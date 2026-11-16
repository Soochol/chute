use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

use super::{Ctx, FORWARD_TIMEOUT, Refusal, finish};
use crate::hub;
use crate::machine::{self, Machines};
use crate::model::{BatchRecord, Event, HubView, MachineId, RevertRecord, State};
use crate::store::RepoStore;

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
    /// 옛 허브에서 대기열을 받아 이 PC를 허브로 만든다
    Take(TakeArgs),
    /// 허브를 다른 머신으로 넘긴다. `hub take`가 부른다
    #[command(hide = true)]
    Release(ReleaseArgs),
    /// 얼어 있는 대기열을 내보낸다. `hub take`가 부른다
    #[command(hide = true)]
    Export,
}

#[derive(clap::Args, Debug)]
pub struct TakeArgs {
    /// 옛 허브의 hc 이름. `hc machine ls`로 본다
    pub machine: String,
}

#[derive(clap::Args, Debug)]
pub struct ReleaseArgs {
    /// 새 허브의 머신 id
    #[arg(long)]
    pub to: String,
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
        Some(Sub::Take(take)) => finish(take_hub(&take, ctx)),
        Some(Sub::Release(release)) => finish(release_hub(&release, ctx)),
        Some(Sub::Export) => finish(export_hub(ctx)),
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

#[derive(Debug, Serialize, Deserialize)]
struct Handover {
    state: State,
    history: Vec<Event>,
    batches: Vec<HandoverBatch>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HandoverBatch {
    record: BatchRecord,
    revert: Option<RevertRecord>,
}

fn release_hub(args: &ReleaseArgs, ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    let to = MachineId::try_from(args.to.clone())
        .map_err(|e| Refusal(format!("`--to`가 머신 id가 아니다: {e}")))?;
    match hub::elsewhere(&ctx.home, &ctx.ext, &info.slug)? {
        Some(current) if current == to => return Ok(()),
        Some(current) => {
            return Err(Refusal(format!(
                "이 PC는 허브가 아니다. 허브는 {current}다. 허브인 머신에서 실행한다"
            ))
            .into());
        }
        None => {}
    }
    if store.daemon_running()? {
        return Err(Refusal(format!(
            "이 PC에서 {}의 데몬이 돌고 있다. 먼저 `chute down`으로 멈춘다",
            info.slug
        ))
        .into());
    }
    let machines = Machines::load(&ctx.ext)?;
    if to == machines.local().id {
        return Err(Refusal(
            "이 PC로는 넘길 수 없다. 받을 머신에서 `chute hub take`를 실행한다".to_owned(),
        )
        .into());
    }
    if machines.by_id(&to).is_none() {
        return Err(Refusal(format!(
            "허브를 받을 머신({to})을 hc가 모른다. `hc machine ls`로 확인하고, 없으면 `hc machine add`로 더한다"
        ))
        .into());
    }
    store.set_hub(to)
}

fn export_hub(ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    if hub::elsewhere(&ctx.home, &ctx.ext, &info.slug)?.is_none() {
        return Err(Refusal(
            "이 PC가 아직 허브다. 대기열이 얼어 있지 않아 내보내지 않는다. `chute hub release`가 먼저다"
                .to_owned(),
        )
        .into());
    }
    let me = machine::core(&ctx.ext)?.id;
    let mut state = store.load()?;
    for item in &mut state.queue {
        if let Some(agent) = &mut item.agent {
            agent.machine.get_or_insert_with(|| me.clone());
        }
    }
    let history = store.history()?.events;
    let mut batches = Vec::new();
    for id in store.batch_ids()? {
        if let Some(record) = store.read_batch(&id)? {
            batches.push(HandoverBatch {
                revert: store.read_revert(&id)?,
                record,
            });
        }
    }
    let handover = Handover {
        state,
        history,
        batches,
    };
    println!("{}", serde_json::to_string(&handover)?);
    Ok(())
}

fn take_hub(args: &TakeArgs, ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    let name = &args.machine;
    let machines = Machines::load(&ctx.ext)?;
    let old = machines.by_name(name).ok_or_else(|| {
        Refusal(format!(
            "hc가 머신 `{name}`을 모른다. `hc machine ls`로 등록된 머신을 확인한다"
        ))
    })?;
    if !old.connected() {
        return Err(Refusal(format!(
            "머신 `{name}`에 연결돼 있지 않다(상태: {}). `hc machine ls`로 보고, 로그인이 필요하면 `hc machine login {name}`을 실행한다",
            old.state
        ))
        .into());
    }
    let me = machines.local().id.clone();
    if old.id == me {
        return Err(Refusal("이 PC에서 이 PC로 옮길 수 없다".to_owned()).into());
    }
    if let Some(other) = hub::elsewhere(&ctx.home, &ctx.ext, &info.slug)?
        && other != old.id
    {
        return Err(Refusal(format!(
            "이 PC는 `{name}`이 아니라 {other}를 허브로 안다. `chute hub set --local`로 먼저 정리한다"
        ))
        .into());
    }
    if store.daemon_running()? {
        return Err(Refusal(format!(
            "이 PC에서 {}의 데몬이 돌고 있다. 먼저 `chute down`으로 멈춘다",
            info.slug
        ))
        .into());
    }

    let slug = info.slug.to_string();
    if store.taken_from()? == Some(old.id.clone()) {
        let seen = hub::exec_on(
            &ctx.ext,
            name,
            &[],
            &["chute", "--slug", &slug, "hub", "--json"],
            FORWARD_TIMEOUT,
        )?;
        let there: HubView = serde_json::from_slice(&seen.stdout).map_err(|e| {
            Refusal(format!(
                "`{name}`의 `chute hub --json` 출력을 읽지 못했다({e}). 허브 상태를 확인하지 못해 멈춘다"
            ))
        })?;
        if there.local {
            return Err(Refusal(format!(
                "허브는 이미 이 PC로 옮겨 왔지만 `{name}`이 다시 스스로를 허브로 안다. `{name}`에서 `chute hub set <이 PC>`로 이 PC를 허브로 정한다. 그곳의 큐에 새 항목이 있으면 먼저 정리한다"
            ))
            .into());
        }
        store.clear_hub()?;
        println!("{slug}의 허브는 이미 이 PC로 옮겨 왔다. 할 일이 없다");
        return Ok(());
    }
    let to = me.to_string();
    let released = hub::exec_on(
        &ctx.ext,
        name,
        &[],
        &["chute", "--slug", &slug, "hub", "release", "--to", &to],
        FORWARD_TIMEOUT,
    )?;
    if released.code != 0 {
        return Err(Refusal(format!(
            "`{name}`이 허브를 넘기지 않았다: {}",
            String::from_utf8_lossy(&released.stderr).trim()
        ))
        .into());
    }

    let exported = hub::exec_on(
        &ctx.ext,
        name,
        &[],
        &["chute", "--slug", &slug, "hub", "export"],
        FORWARD_TIMEOUT,
    )?;
    if exported.code != 0 {
        return Err(Refusal(format!(
            "`{name}`이 대기열을 내보내지 않았다: {}",
            String::from_utf8_lossy(&exported.stderr).trim()
        ))
        .into());
    }
    if exported.truncated {
        return Err(Refusal(format!(
            "`{name}`의 대기열이 너무 커서 hc가 뒤를 잘랐다. 허브는 이미 이 PC로 정해졌으니, 항목을 줄인 뒤 같은 명령을 다시 실행한다"
        ))
        .into());
    }
    let handover: Handover = serde_json::from_slice(&exported.stdout)
        .with_context(|| format!("`{name}`의 `chute hub export` 출력을 읽지 못했다"))?;

    let items = handover.state.queue.len();
    let Handover {
        state,
        history,
        batches,
    } = handover;
    import_state(&store, state)?;
    import_records(&store, &history, &batches)?;
    store.mark_taken(old.id.clone())?;
    store.clear_hub()?;
    println!(
        "{slug}의 허브를 이 PC로 옮겼다. 큐 항목 {items}개를 `{name}`에서 가져왔다. \
         보고서와 검증 로그는 옮기지 않는다. `{name}`에 남아 있다"
    );
    Ok(())
}

fn import_state(store: &RepoStore, state: State) -> Result<()> {
    store.update(|mine| {
        if *mine == State::default() {
            *mine = state;
        } else if mine.queue != state.queue || mine.batch != state.batch {
            return Err(Refusal(
                "이 PC에 다른 항목이나 상태가 있다. 가져오면 섞이거나 잃는다. 옛 허브는 이미 이 PC를 허브로 가리키며 얼어 있다. 이 PC의 항목이 머지되거나 `chute drop`으로 빠진 뒤 같은 명령을 다시 실행한다"
                    .to_owned(),
            )
            .into());
        }
        Ok(())
    })
}

fn import_records(store: &RepoStore, history: &[Event], batches: &[HandoverBatch]) -> Result<()> {
    let mut unmatched = store.history()?.events;
    for event in history {
        match unmatched.iter().position(|e| e == event) {
            Some(i) => {
                unmatched.remove(i);
            }
            None => store.append_history(event)?,
        }
    }
    for batch in batches {
        let id = &batch.record.id;
        if store.read_batch(id)?.is_none() {
            store.write_batch(&batch.record)?;
        }
        if let Some(revert) = &batch.revert
            && store.read_revert(id)?.is_none()
        {
            store.write_revert(id, revert)?;
        }
    }
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
