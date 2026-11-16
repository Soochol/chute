use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

use super::{Ctx, FORWARD_TIMEOUT, Refusal, finish};
use crate::hub;
use crate::machine::Machines;
use crate::model::HubView;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Sub>,
    /// 허브를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum Sub {
    /// 허브를 다른 머신으로 정한다
    Set(SetArgs),
}

#[derive(clap::Args, Debug)]
pub struct SetArgs {
    /// 허브로 정할 머신의 hc 이름. `hc machine ls`로 본다
    #[arg(required_unless_present = "local", conflicts_with = "local")]
    pub machine: Option<String>,
    /// 이 PC를 허브로 되돌린다
    #[arg(long)]
    pub local: bool,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    match args.command {
        None => show(args.json, ctx).map(|()| ExitCode::SUCCESS),
        Some(Sub::Set(set)) => finish(set_hub(&set, ctx)),
    }
}

fn show(json: bool, ctx: &Ctx) -> Result<()> {
    let (info, _) = ctx.registered_repo()?;
    let seen = hub::view(&ctx.home, &ctx.ext, &info.slug, info.path)?;
    if let Some(note) = &seen.note {
        eprintln!("경고: {note}");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&seen.view)?);
    } else {
        println!("{}", describe(&seen.view));
    }
    Ok(())
}

fn describe(view: &HubView) -> String {
    if view.local {
        return match &view.host {
            Some(host) => format!("허브: 이 PC ({host})"),
            None => "허브: 이 PC".to_owned(),
        };
    }
    let name = view.machine.as_deref().or(view.host.as_deref());
    let who = match (name, &view.machine_id) {
        (Some(name), _) => name.to_owned(),
        (None, Some(id)) => format!("모르는 머신 {id}"),
        (None, None) => "모르는 머신".to_owned(),
    };
    match &view.error {
        None => format!("허브: {who} (연결됨)"),
        Some(error) => format!("허브: {who}\n닿지 않는다: {error}"),
    }
}

fn set_hub(args: &SetArgs, ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    let Some(name) = &args.machine else {
        store.clear_hub()?;
        println!("이 PC를 허브로 정했다: {}", info.slug);
        return Ok(());
    };

    let machines = Machines::load(&ctx.ext)?;
    let target = machines.by_name(name).ok_or_else(|| {
        Refusal(format!(
            "hc가 머신 `{name}`을 모른다. `hc machine ls`로 등록된 머신을 확인한다"
        ))
    })?;
    if !target.connected() {
        return Err(Refusal(format!(
            "머신 `{name}`에 연결돼 있지 않다(상태: {}). `hc machine ls`로 보고, 로그인이 필요하면 `hc machine login {name}`을 실행한다",
            target.state
        ))
        .into());
    }
    let state = store.load()?;
    if !state.queue.is_empty() || state.batch.is_some() {
        let what = match state.queue.len() {
            0 => "진행 중인 batch가".to_owned(),
            n => format!("큐에 항목이 {n}개"),
        };
        return Err(Refusal(format!(
            "이 PC에 {what} 남아 있다. 허브를 옮기면 잃는다. 머지되거나 `chute drop`으로 빠지고 batch가 끝날 때까지 기다린다"
        ))
        .into());
    }

    let version = format!("chute {}", env!("CARGO_PKG_VERSION"));
    let there = hub::exec_on(
        &ctx.ext,
        name,
        &[],
        &["chute", "--version"],
        FORWARD_TIMEOUT,
    )?;
    if there.code != 0 || String::from_utf8_lossy(&there.stdout).trim() != version {
        return Err(Refusal(format!(
            "`{name}`의 chute 버전이 이 PC와 다르다(이 PC: {version}, 그곳: {}). 두 머신의 chute를 같은 버전으로 맞춘다",
            String::from_utf8_lossy(&there.stdout).trim()
        ))
        .into());
    }

    let slug = info.slug.to_string();
    let answer = hub::exec_on(
        &ctx.ext,
        name,
        &[],
        &["chute", "--slug", &slug, "hub", "--json"],
        FORWARD_TIMEOUT,
    )?;
    if answer.code != 0 {
        return Err(Refusal(format!(
            "`{name}`에서 `chute hub`가 실패했다: {}. 그곳에서 `chute init`으로 {slug}를 등록했는지 확인한다",
            String::from_utf8_lossy(&answer.stderr).trim()
        ))
        .into());
    }
    let there: HubView = serde_json::from_slice(&answer.stdout)
        .map_err(|e| anyhow::anyhow!("`{name}`의 `chute hub --json` 출력을 읽지 못했다: {e}"))?;
    if !there.local {
        return Err(Refusal(format!(
            "`{name}`도 허브가 아니다. 그곳은 {}를 허브로 안다. 허브가 될 머신에서 `chute hub set --local`을 먼저 실행한다",
            there.machine.as_deref().unwrap_or("다른 머신")
        ))
        .into());
    }

    store.set_hub(target.id.clone())?;
    println!("허브를 `{name}`으로 정했다: {slug}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MachineId;

    fn view(local: bool, machine: Option<&str>, error: Option<&str>) -> HubView {
        HubView {
            local,
            machine_id: Some(MachineId::try_from("abc".to_owned()).unwrap()),
            host: Some("desk-host".into()),
            machine: machine.map(str::to_owned),
            repo_path: None,
            reachable: error.is_none(),
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn describes_where_the_hub_is() {
        assert_eq!(describe(&view(true, None, None)), "허브: 이 PC (desk-host)");
        assert_eq!(
            describe(&view(false, Some("desk"), None)),
            "허브: desk (연결됨)"
        );
        let down = describe(&view(false, Some("desk"), Some("상태가 unreachable이다")));
        assert!(
            down.contains("닿지 않는다: 상태가 unreachable이다"),
            "{down}"
        );
        assert!(describe(&view(false, None, Some("x"))).starts_with("허브: desk-host"));
    }
}
