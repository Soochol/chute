use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::ext::{Ext, ExtError, Tool};
use crate::machine::{self, Machines};
use crate::model::{HubView, MachineId, Slug};
use crate::store::Home;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Local,
    Remote { id: MachineId, name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forwarded {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// hc kept only the start of the output.
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardErrorKind {
    UnknownMachine,
    Unreachable,
    NoChute,
    TimedOut,
    Misconfigured,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ForwardError {
    pub kind: ForwardErrorKind,
    pub message: String,
}

impl ForwardError {
    fn new(kind: ForwardErrorKind, message: String) -> ForwardError {
        ForwardError { kind, message }
    }
}

pub fn elsewhere(home: &Home, ext: &Ext, slug: &Slug) -> Result<Option<MachineId>> {
    let Some(id) = home.repo(slug).hub()? else {
        return Ok(None);
    };
    Ok((machine::core(ext)?.id != id).then_some(id))
}

pub fn route(home: &Home, ext: &Ext, slug: &Slug) -> Result<Route> {
    let Some(id) = elsewhere(home, ext, slug)? else {
        return Ok(Route::Local);
    };
    let name = machine::list(ext)?
        .into_iter()
        .find(|m| m.id == id)
        .map(|m| m.name)
        .ok_or_else(|| ForwardError::new(ForwardErrorKind::UnknownMachine, unknown_hub(&id)))?;
    Ok(Route::Remote { id, name })
}

fn unknown_hub(id: &MachineId) -> String {
    format!(
        "허브 머신({id})을 hc가 모른다. 명령은 허브에 기록되지 않았다. \
         `hc machine ls`로 등록된 머신을 확인하고, 없으면 `hc machine add`로 더한다"
    )
}

pub fn forward(
    home: &Home,
    ext: &Ext,
    slug: &Slug,
    route: &Route,
    args: &[&str],
    timeout: Duration,
) -> Result<Forwarded> {
    let Route::Remote { name, .. } = route else {
        bail!("이 PC가 허브라 넘길 곳이 없다");
    };
    let slug_text = slug.to_string();
    let argv: Vec<&str> = ["chute", "--slug", &slug_text]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    let env = [("CHUTE_FORWARDED", "1")];
    let first = exec_on(ext, name, &env, &argv, timeout)?;
    let Some(hub) = not_hub(&first) else {
        return Ok(first);
    };

    let machines = Machines::load(ext)?;
    if hub == machines.local().id {
        return Err(ForwardError::new(
            ForwardErrorKind::Misconfigured,
            format!(
                "허브 설정이 서로를 가리킨다. 허브 `{name}`은 이 PC를 허브라고 하고, 이 PC는 `{name}`을 허브로 안다. \
                 허브가 어디여야 하는지 정해 허브에서 `chute hub set --local`을, 다른 PC에서 `chute hub set <허브>`를 실행한다"
            ),
        )
        .into());
    }
    let next = machines
        .by_id(&hub)
        .ok_or_else(|| ForwardError::new(ForwardErrorKind::UnknownMachine, unknown_hub(&hub)))?;
    let second = exec_on(ext, &next.name, &env, &argv, timeout)?;
    if not_hub(&second).is_some() {
        return Err(ForwardError::new(
            ForwardErrorKind::Misconfigured,
            format!(
                "허브를 찾지 못했다. `{name}`은 `{next}`를 허브라고 하고 `{next}`는 다시 다른 머신을 허브라고 한다. \
                 명령은 기록되지 않았다. 각 머신에서 `chute hub`로 허브를 확인한다",
                next = next.name
            ),
        )
        .into());
    }
    home.repo(slug).set_hub(hub)?;
    Ok(second)
}

fn not_hub(answer: &Forwarded) -> Option<MachineId> {
    #[derive(Deserialize)]
    struct Body {
        not_hub: Named,
    }
    #[derive(Deserialize)]
    struct Named {
        hub: MachineId,
    }
    if answer.code != 3 {
        return None;
    }
    serde_json::from_slice::<Body>(&answer.stdout)
        .ok()
        .map(|b| b.not_hub.hub)
}

/// Exit code reported when a signal ended the program and hc gave none.
const ENDED_BY_SIGNAL: i32 = 2;

#[derive(Deserialize)]
struct HcExec {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    #[serde(default)]
    timed_out: bool,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct HcFailure {
    error: HcError,
}

#[derive(Deserialize)]
struct HcError {
    kind: String,
    #[serde(default)]
    message: String,
}

pub fn exec_on(
    ext: &Ext,
    name: &str,
    env: &[(&str, &str)],
    argv: &[&str],
    timeout: Duration,
) -> Result<Forwarded, ForwardError> {
    let seconds = timeout.as_secs().clamp(1, 600);
    let mut args = vec![
        "exec".to_owned(),
        "--json".into(),
        "--on".into(),
        name.to_owned(),
        "--timeout".into(),
        seconds.to_string(),
    ];
    for (k, v) in env {
        args.push("--env".into());
        args.push(format!("{k}={v}"));
    }
    args.push("--".into());
    args.extend(argv.iter().map(|a| (*a).to_owned()));

    let slack = Duration::from_secs(10);
    let out = ext
        .call(Tool::Hc, None, &args, Duration::from_secs(seconds) + slack)
        .map_err(|e| match e {
            ExtError::Timeout { .. } => timed_out(name, seconds),
            e => ForwardError::new(
                ForwardErrorKind::Unreachable,
                format!(
                    "허브 `{name}`로 명령을 넘기지 못했다: {e}. 명령은 기록되지 않았다. hc가 설치돼 있고 돌고 있는지 확인한다"
                ),
            ),
        })?;
    if !out.success() {
        return Err(hc_failure(name, &out.stderr_text()));
    }
    let exec: HcExec = serde_json::from_slice(&out.stdout).map_err(|e| {
        ForwardError::new(
            ForwardErrorKind::Unreachable,
            format!("허브 `{name}`의 응답을 읽지 못했다: {e}. 명령이 기록됐는지 알 수 없다. `chute status`로 확인한다"),
        )
    })?;
    if exec.timed_out {
        return Err(timed_out(name, seconds));
    }
    let unreadable = |what: &str| {
        ForwardError::new(
            ForwardErrorKind::Unreachable,
            format!(
                "허브 `{name}`의 {what} 출력을 읽지 못했다. 명령이 기록됐는지 알 수 없다. `chute status`로 확인한다"
            ),
        )
    };
    Ok(Forwarded {
        code: exec.code.unwrap_or(ENDED_BY_SIGNAL),
        stdout: base64_decode(&exec.stdout).ok_or_else(|| unreadable("stdout"))?,
        stderr: base64_decode(&exec.stderr).ok_or_else(|| unreadable("stderr"))?,
        truncated: exec.truncated,
    })
}

fn timed_out(name: &str, seconds: u64) -> ForwardError {
    ForwardError::new(
        ForwardErrorKind::TimedOut,
        format!(
            "허브 `{name}`가 {seconds}초 안에 답하지 않았다. 명령이 기록됐는지 알 수 없다. \
             `chute status`로 확인한 뒤 필요하면 다시 실행한다"
        ),
    )
}

fn hc_failure(name: &str, stderr: &str) -> ForwardError {
    use ForwardErrorKind::*;
    let parsed = serde_json::from_str::<HcFailure>(stderr.trim()).ok();
    let detail = parsed
        .as_ref()
        .map(|f| f.error.message.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| stderr.trim().to_owned());
    match parsed.as_ref().map(|f| f.error.kind.as_str()) {
        Some("machine_unknown") => ForwardError::new(
            UnknownMachine,
            format!(
                "hc가 머신 `{name}`을 모른다. 명령은 허브에 기록되지 않았다. \
                 `hc machine ls`로 등록된 머신을 확인한다"
            ),
        ),
        Some("spawn_failed") => ForwardError::new(
            NoChute,
            format!(
                "허브 `{name}`에서 chute를 실행하지 못했다({detail}). 명령은 기록되지 않았다. \
                 그 머신의 로그인 환경 PATH에 chute가 있는지 확인한다"
            ),
        ),
        Some("timeout") => ForwardError::new(
            TimedOut,
            format!(
                "허브 `{name}`가 제때 답하지 않았다({detail}). 명령이 기록됐는지 알 수 없다. \
                 `chute status`로 확인한 뒤 필요하면 다시 실행한다"
            ),
        ),
        _ => ForwardError::new(
            Unreachable,
            format!(
                "허브 `{name}`에 닿지 않는다({detail}). 명령은 기록되지 않았다. \
                 `hc machine ls`로 상태를 보고, 로그인이 필요하면 `hc machine login {name}`을 실행한다"
            ),
        ),
    }
}

pub struct Seen {
    pub view: HubView,
    pub machines: Option<Machines>,
    pub note: Option<String>,
}

pub fn view(home: &Home, ext: &Ext, slug: &Slug, repo_path: PathBuf) -> Result<Seen> {
    let configured = home.repo(slug).hub()?;
    let core = match machine::core(ext) {
        Ok(core) => core,
        Err(e) => {
            let view = HubView {
                local: configured.is_none(),
                machine_id: configured,
                host: None,
                machine: None,
                repo_path: Some(repo_path),
                reachable: true,
                error: None,
            };
            return Ok(Seen {
                view,
                machines: None,
                note: Some(format!("이 PC의 머신 id를 읽지 못했다: {e:#}")),
            });
        }
    };
    let (others, listing_failed) = match machine::list(ext) {
        Ok(others) => (others, None),
        Err(e) => (Vec::new(), Some(format!("{e:#}"))),
    };
    let machines = Machines::new(core.clone(), others);
    let view = match configured.filter(|id| *id != core.id) {
        None => HubView {
            local: true,
            machine_id: Some(core.id),
            host: Some(core.name),
            machine: None,
            repo_path: Some(repo_path),
            reachable: true,
            error: None,
        },
        Some(id) => {
            let found = machines.by_id(&id);
            let error = match (found, &listing_failed) {
                (_, Some(failed)) => Some(failed.clone()),
                (None, None) => Some(unknown_hub(&id)),
                (Some(m), None) if !m.connected() => {
                    Some(format!("머신 `{}`의 상태가 {}이다", m.name, m.state))
                }
                (Some(_), None) => None,
            };
            return Ok(Seen {
                view: HubView {
                    local: false,
                    host: found.and_then(|m| m.host.clone()),
                    machine: found.map(|m| m.name.clone()),
                    repo_path: None,
                    reachable: error.is_none(),
                    error,
                    machine_id: Some(id),
                },
                machines: Some(machines),
                note: None,
            });
        }
    };
    Ok(Seen {
        view,
        machines: Some(machines),
        note: listing_failed,
    })
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut bits = 0u32;
    let mut count = 0;
    let mut padding = 0;
    for byte in text.trim().bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            _ => return None,
        };
        if padding > 0 {
            return None;
        }
        bits = bits << 6 | u32::from(value);
        count += 1;
        if count == 4 {
            out.extend([(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]);
            (bits, count) = (0, 0);
        }
    }
    match (count, padding) {
        (0, 0) => {}
        (2, 2) => out.push((bits >> 4) as u8),
        (3, 1) => out.extend([(bits >> 10) as u8, (bits >> 2) as u8]),
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_with_and_without_padding() {
        for (text, bytes) in [
            ("", &b""[..]),
            ("aGkK", b"hi\n"),
            ("aGk=", b"hi"),
            ("aA==", b"h"),
            ("7IqI66CI7Yq4", "슈레트".as_bytes()),
        ] {
            assert_eq!(base64_decode(text).as_deref(), Some(bytes), "{text}");
        }
        for bad in ["a", "aGk", "aG=k", "aGk!", "=aGk"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn hc_failures_are_told_apart() {
        let kind = |stderr: &str| hc_failure("desk", stderr).kind;
        let err = |k: &str| format!(r#"{{"error":{{"kind":"{k}","message":"m","hint":""}}}}"#);
        assert_eq!(
            kind(&err("machine_unknown")),
            ForwardErrorKind::UnknownMachine
        );
        assert_eq!(kind(&err("spawn_failed")), ForwardErrorKind::NoChute);
        assert_eq!(kind(&err("timeout")), ForwardErrorKind::TimedOut);
        assert_eq!(kind(&err("unreachable")), ForwardErrorKind::Unreachable);
        assert_eq!(kind(&err("needs_login")), ForwardErrorKind::Unreachable);
        assert_eq!(kind("not json"), ForwardErrorKind::Unreachable);
    }

    #[test]
    fn every_failure_says_what_to_do_in_korean() {
        for stderr in [
            r#"{"error":{"kind":"machine_unknown","message":"x"}}"#,
            r#"{"error":{"kind":"unreachable","message":"x"}}"#,
            r#"{"error":{"kind":"spawn_failed","message":"x"}}"#,
        ] {
            let message = hc_failure("desk", stderr).message;
            assert!(message.contains("기록되지 않았다"), "{message}");
            assert!(
                message.contains("hc") || message.contains("PATH"),
                "{message}"
            );
        }
    }
}
