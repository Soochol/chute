use std::fs;
use std::path::Path;
use std::time::Duration;

use nix::unistd::gethostname;
use serde::{Deserialize, Serialize};

use crate::ext::Ext;
use crate::machine;
use crate::model::{EnvCheck, EnvRecord};
use crate::verify::{self, End};

pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSection {
    pub env: EnvRecord,
    #[serde(default)]
    pub preflight: Option<EnvRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    pub what: String,
    pub preflight: Option<String>,
    pub hub: Option<String>,
}

pub fn record(ext: &Ext, dir: &Path, log_dir: &Path, commands: &[String]) -> EnvRecord {
    record_within(ext, dir, log_dir, commands, CHECK_TIMEOUT)
}

pub fn record_within(
    ext: &Ext,
    dir: &Path,
    log_dir: &Path,
    commands: &[String],
    timeout: Duration,
) -> EnvRecord {
    let _ = fs::create_dir_all(log_dir);
    EnvRecord {
        machine_id: machine::core(ext).ok().map(|core| core.id),
        host: gethostname()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        checks: commands
            .iter()
            .enumerate()
            .map(|(n, run)| {
                check(
                    dir,
                    run,
                    &log_dir.join(format!("env-{}.log", n + 1)),
                    timeout,
                )
            })
            .collect(),
    }
}

fn check(dir: &Path, run: &str, log: &Path, timeout: Duration) -> EnvCheck {
    let (ok, note) = match verify::run_command(dir, run, log, timeout) {
        Ok(End::Passed) => (true, None),
        Ok(End::Failed(verify::Cause::TimedOut)) => (
            false,
            Some(format!("{}초 안에 끝나지 않아 멈췄다", timeout.as_secs())),
        ),
        Ok(End::Failed(_) | End::Cancelled) => (false, None),
        Err(e) => (false, Some(format!("{e:#}"))),
    };
    let first = fs::read(log)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .and_then(|text| text.lines().map(str::trim).find(|l| !l.is_empty()).map(cut));
    let output = match (first, note) {
        (Some(line), Some(note)) => format!("{line} ({note})"),
        (Some(line), None) => line,
        (None, Some(note)) => note,
        (None, None) => String::new(),
    };
    EnvCheck {
        run: run.to_owned(),
        output,
        ok,
    }
}

fn cut(line: &str) -> String {
    line.chars().take(OUTPUT_CHARS).collect()
}

pub fn differences(preflight: &EnvRecord, hub: &EnvRecord) -> Vec<Difference> {
    let mut rows = Vec::new();
    let (before, now) = (machine_line(preflight), machine_line(hub));
    if before != now {
        rows.push(Difference {
            what: "머신".to_owned(),
            preflight: Some(before),
            hub: Some(now),
        });
    }
    let mut runs: Vec<&str> = Vec::new();
    for check in preflight.checks.iter().chain(&hub.checks) {
        if !runs.contains(&check.run.as_str()) {
            runs.push(&check.run);
        }
    }
    for run in runs {
        let (before, now) = (result_of(preflight, run), result_of(hub, run));
        if before != now {
            rows.push(Difference {
                what: run.to_owned(),
                preflight: before,
                hub: now,
            });
        }
    }
    rows
}

pub fn machine_line(env: &EnvRecord) -> String {
    match &env.machine_id {
        Some(id) => format!("{} ({id})", env.host),
        None => env.host.clone(),
    }
}

pub fn result_text(check: &EnvCheck) -> String {
    if check.ok {
        check.output.clone()
    } else {
        format!("실패: {}", check.output)
    }
}

fn result_of(env: &EnvRecord, run: &str) -> Option<String> {
    env.checks
        .iter()
        .find(|check| check.run == run)
        .map(result_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sandbox;
    use std::time::Instant;

    fn commands(list: &[&str]) -> Vec<String> {
        list.iter().map(|c| (*c).to_owned()).collect()
    }

    struct Fixture {
        sb: Sandbox,
        ext: Ext,
        dir: std::path::PathBuf,
        logs: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let sb = Sandbox::new();
        let ext = Ext::from_vars(|k| sb.var(k));
        let dir = sb.root().join("slot");
        let logs = sb.root().join("logs");
        fs::create_dir_all(&dir).unwrap();
        Fixture { sb, ext, dir, logs }
    }

    fn env_of(f: &Fixture, list: &[&str], timeout: Duration) -> EnvRecord {
        record_within(&f.ext, &f.dir, &f.logs, &commands(list), timeout)
    }

    #[test]
    fn the_machine_is_kept_when_env_check_is_empty() {
        let f = fixture();
        let env = env_of(&f, &[], CHECK_TIMEOUT);
        assert!(env.checks.is_empty());
        assert_eq!(
            env.machine_id.unwrap().as_str(),
            crate::testkit::SANDBOX_MACHINE_ID
        );
        assert!(!env.host.is_empty());
    }

    #[test]
    fn an_unreadable_core_leaves_the_machine_id_empty() {
        let f = fixture();
        f.sb.fake("hc").clear_rules().respond(
            "server status --json",
            crate::testkit::Reply::ok("{}").code(1),
        );
        let env = env_of(&f, &[], CHECK_TIMEOUT);
        assert_eq!(env.machine_id, None);
        assert!(!env.host.is_empty());
    }

    #[test]
    fn a_check_keeps_the_first_line_cut_to_200_chars() {
        let f = fixture();
        let long = "가".repeat(300);
        let env = env_of(
            &f,
            &[
                "printf 'first\\nsecond\\n'",
                &format!("echo {long}"),
                "echo oops >&2; echo late",
            ],
            CHECK_TIMEOUT,
        );
        assert_eq!(env.checks[0].output, "first");
        assert_eq!(env.checks[1].output.chars().count(), 200);
        assert_eq!(env.checks[2].output, "oops");
        assert!(env.checks.iter().all(|c| c.ok));
    }

    #[test]
    fn a_failing_check_is_recorded_and_the_next_one_still_runs() {
        let f = fixture();
        let env = env_of(&f, &["echo broken; exit 3", "echo fine"], CHECK_TIMEOUT);
        assert_eq!(
            env.checks,
            [
                EnvCheck {
                    run: "echo broken; exit 3".into(),
                    output: "broken".into(),
                    ok: false
                },
                EnvCheck {
                    run: "echo fine".into(),
                    output: "fine".into(),
                    ok: true
                },
            ]
        );
    }

    #[test]
    fn a_check_that_outlasts_the_limit_is_cut_and_the_next_one_still_runs() {
        let f = fixture();
        let started = Instant::now();
        let env = env_of(
            &f,
            &["echo start; sleep 30", "echo after"],
            Duration::from_millis(300),
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!env.checks[0].ok);
        assert!(env.checks[0].output.starts_with("start"));
        assert_eq!(env.checks[1].output, "after");
    }

    #[test]
    fn checks_run_in_the_given_folder() {
        let f = fixture();
        fs::write(f.dir.join("marker"), "here").unwrap();
        let env = env_of(&f, &["cat marker"], CHECK_TIMEOUT);
        assert_eq!(env.checks[0].output, "here");
    }

    fn env(host: &str, checks: &[(&str, &str, bool)]) -> EnvRecord {
        EnvRecord {
            machine_id: None,
            host: host.into(),
            checks: checks
                .iter()
                .map(|(run, output, ok)| EnvCheck {
                    run: (*run).into(),
                    output: (*output).into(),
                    ok: *ok,
                })
                .collect(),
        }
    }

    #[test]
    fn differences_list_only_rows_that_disagree() {
        let pre = env(
            "laptop",
            &[("a", "1", true), ("b", "x", true), ("c", "p", true)],
        );
        let hub = env(
            "hub",
            &[("a", "1", true), ("b", "y", true), ("d", "q", false)],
        );
        let rows = differences(&pre, &hub);
        let whats: Vec<&str> = rows.iter().map(|r| r.what.as_str()).collect();
        assert_eq!(whats, ["머신", "b", "c", "d"]);
        assert_eq!(rows[2].hub, None);
        assert_eq!(rows[3].preflight, None);
        assert!(differences(&hub, &hub).is_empty());
    }
}
