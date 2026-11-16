use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{Ctx, FORWARD_TIMEOUT, Refusal, finish, relay};
use crate::config::{self, Config};
use crate::ext::Ext;
use crate::hub::{self, Forwarded, Route};
use crate::machine;
use crate::model::{
    Agent, EpochMs, Event, EventKind, ItemState, MachineId, PrNumber, RepoInfo, Sha, TerminalId,
};
use crate::queue::{self, Change, Submission};
use crate::store::RepoStore;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호. 없으면 지금 브랜치의 PR
    pub pr: Option<PrNumber>,
    /// 반송을 받을 hc 터미널 주소. 없으면 HC_TERMINAL_ID
    #[arg(long, value_name = "주소")]
    pub agent: Option<String>,
    /// 확인한 커밋. GitHub의 PR HEAD가 이 값과 다르면 거절한다. 7자 이상의 짧은 SHA도 된다
    #[arg(long, value_name = "SHA", value_parser = parse_head)]
    pub head: Option<Sha>,
    /// 급한 PR로 표시해 batch에 먼저 넣는다. 사용자의 승인 범위 안에서만 쓴다
    #[arg(long)]
    pub priority: bool,
    /// 이 PR이 머지된 뒤에 batch에 넣는다. 여러 번 줄 수 있다
    #[arg(long, value_name = "PR")]
    pub after: Vec<PrNumber>,
    /// 멈춘 큐를 풀 복구 PR로 넣는다. 큐가 멈춰 있을 때만, 한 번에 하나만 받는다
    #[arg(long)]
    pub repair: bool,
    /// 결과를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
    #[arg(long, hide = true)]
    pub relayed_no_head: bool,
    #[arg(long, hide = true, value_name = "JSON", value_parser = parse_relayed)]
    pub relayed_agent: Option<Relayed>,
    #[arg(long, hide = true, value_name = "JSON", value_parser = parse_relayed)]
    pub relayed_submitter: Option<Relayed>,
}

#[derive(Clone, Debug)]
pub struct Relayed(Option<Box<Agent>>);

impl Relayed {
    fn agent(&self) -> Option<Agent> {
        self.0.as_deref().cloned()
    }
}

fn parse_relayed(text: &str) -> Result<Relayed, String> {
    serde_json::from_str(text)
        .map(Relayed)
        .map_err(|e| format!("에이전트 JSON을 읽지 못했다: {e}"))
}

const MIN_HEAD_LEN: usize = 7;

fn parse_head(text: &str) -> Result<Sha, String> {
    let sha = Sha::try_from(text.to_ascii_lowercase())?;
    if sha.as_str().len() < MIN_HEAD_LEN {
        return Err(format!(
            "커밋 SHA는 {MIN_HEAD_LEN}자 이상이어야 한다: {text}"
        ));
    }
    Ok(sha)
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    match submit(&args, ctx) {
        Ok(None) => Ok(ExitCode::SUCCESS),
        Ok(Some(answer)) => Ok(relay(&answer)),
        Err(e) => finish(Err(e)),
    }
}

fn submit(args: &Args, ctx: &Ctx) -> Result<Option<Forwarded>> {
    let dir = ctx.target_dir()?;
    let (info, store) = ctx.registered_repo()?;
    if !ctx.forwarded() {
        let route = hub::route(&ctx.home, &ctx.ext, &info.slug)?;
        if let Route::Remote { .. } = route {
            return submit_through_hub(args, ctx, &dir, &info, &route).map(Some);
        }
    }
    let config = load_default_branch_config(ctx, &info)?;
    if config.require_head && (args.head.is_none() || args.relayed_no_head) {
        return Err(Refusal(REQUIRE_HEAD_MESSAGE.to_owned()).into());
    }
    if args.repair && !args.after.is_empty() {
        return Err(Refusal(REPAIR_AFTER_MESSAGE.to_owned()).into());
    }
    let pr = read_pr(ctx, &dir, args.pr)?;
    check_open_and_ready(&pr, &info)?;
    if let Some(head) = &args.head {
        check_head(&pr, head)?;
    }
    let after = resolve_after(ctx, &dir, &store, pr.number, &args.after)?;
    let this_pc = || this_machine(ctx);
    let agent = match &args.relayed_agent {
        Some(relayed) => {
            let agent = relayed.agent();
            if let Some(agent) = &agent {
                check_agent_machine(&ctx.ext, agent, this_pc().as_ref())?;
            }
            agent
        }
        None => identify_agent(ctx, &dir, args.agent.as_deref(), this_pc),
    };
    let submitter = match (&args.relayed_submitter, &args.relayed_agent) {
        (Some(relayed), _) => relayed.agent(),
        (None, Some(_)) => None,
        (None, None) if args.agent.as_deref().is_none_or(str::is_empty) => agent.clone(),
        (None, None) => identify_submitter(ctx, &dir, this_pc),
    };

    let config_edit = pr.config_edit();
    if config_edit == ConfigEdit::Unknown {
        eprintln!(
            "경고: 바뀐 파일 {}개 가운데 {}개만 읽었다. chute.toml을 고치는지 알 수 없어 사람 승인을 요구한다",
            pr.changed_files,
            pr.files.len()
        );
    }
    let submission = Submission {
        pr: pr.number,
        title: pr.title,
        branch: pr.head_ref_name,
        head: pr.head_ref_oid.clone(),
        agent,
        submitter,
        touches_config: config_edit != ConfigEdit::No,
        approval: config.approval,
        priority: args.priority,
        after,
        repair: args.repair,
        now: EpochMs::now(),
    };
    let (done, kept_after) = store.update(|state| {
        let done = queue::submit(state, submission)?;
        let kept = state
            .queue
            .iter()
            .find(|i| i.pr == pr.number)
            .map(|i| i.after.clone())
            .unwrap_or_default();
        Ok((done, kept))
    })?;
    if done.change != Change::Unchanged {
        record_submitted(
            &store,
            &pr.head_ref_oid,
            pr.number,
            args.priority,
            kept_after,
            args.repair,
        );
    }
    if let Err(e) = super::up::ensure_daemon(ctx, &info, &store) {
        eprintln!("경고: 데몬을 띄우지 못했다. 큐에는 올라갔다. `chute up`으로 띄운다: {e:#}");
    }

    let report = Report {
        pr: pr.number,
        head: pr.head_ref_oid,
        state: done.state,
        position: done.position,
        change: done.change,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report, config_edit == ConfigEdit::Yes));
    }
    Ok(None)
}

fn submit_through_hub(
    args: &Args,
    ctx: &Ctx,
    dir: &Path,
    info: &RepoInfo,
    route: &Route,
) -> Result<Forwarded> {
    let pr = read_pr(ctx, dir, args.pr)?;
    check_open_and_ready(&pr, info)?;
    let this_pc = machine::core(&ctx.ext)?.id;
    let agent = identify_agent(ctx, dir, args.agent.as_deref(), || Some(this_pc.clone()));
    let submitter = if args.agent.as_deref().is_none_or(str::is_empty) {
        agent.clone()
    } else {
        identify_submitter(ctx, dir, || Some(this_pc))
    };
    let head = match &args.head {
        Some(head) => head.clone(),
        None => local_head(&ctx.ext, dir)?,
    };
    let mut forwarded = vec![
        "submit".to_owned(),
        pr.number.0.to_string(),
        "--head".to_owned(),
        head.to_string(),
        "--relayed-agent".to_owned(),
        serde_json::to_string(&agent)?,
        "--relayed-submitter".to_owned(),
        serde_json::to_string(&submitter)?,
    ];
    if args.head.is_none() {
        forwarded.push("--relayed-no-head".to_owned());
    }
    if args.priority {
        forwarded.push("--priority".to_owned());
    }
    if args.repair {
        forwarded.push("--repair".to_owned());
    }
    for n in &args.after {
        forwarded.extend(["--after".to_owned(), n.0.to_string()]);
    }
    if args.json {
        forwarded.push("--json".to_owned());
    }
    let argv: Vec<&str> = forwarded.iter().map(String::as_str).collect();
    hub::forward(
        &ctx.home,
        &ctx.ext,
        &info.slug,
        route,
        &argv,
        FORWARD_TIMEOUT,
    )
}

const REPAIR_AFTER_MESSAGE: &str = "복구 PR은 `--after`와 함께 쓸 수 없다. 큐가 멈춰 있는 동안 다른 PR은 머지되지 않아 \
     기다리는 PR이 영영 풀리지 않는다. `--after`를 뺀다";

const REQUIRE_HEAD_MESSAGE: &str = "이 저장소는 확인한 HEAD(`--head`) 없이 submit을 받지 않는다. \
     오케스트레이터 아래에서 일한다면 submit하지 말고 PR과 HEAD를 보고한다";

fn record_submitted(
    store: &RepoStore,
    head: &Sha,
    pr: PrNumber,
    priority: bool,
    after: Vec<PrNumber>,
    repair: bool,
) {
    let recorded = store.append_history(&Event {
        at_ms: EpochMs::now(),
        kind: EventKind::Submitted {
            pr,
            head: head.clone(),
            priority,
            after,
            repair,
        },
    });
    if let Err(e) = recorded {
        eprintln!("경고: submit을 history.jsonl에 남기지 못했다: {e:#}");
    }
}

fn resolve_after(
    ctx: &Ctx,
    dir: &Path,
    store: &RepoStore,
    pr: PrNumber,
    wanted: &[PrNumber],
) -> Result<Vec<PrNumber>> {
    let mut kept: Vec<PrNumber> = Vec::new();
    for &n in wanted {
        if n == pr {
            return Err(Refusal(format!(
                "{pr}: 자기 자신 뒤에 둘 수 없다. `--after`에서 뺀다"
            ))
            .into());
        }
        let queued = store.load()?.queue.iter().any(|i| i.pr == n);
        if queued {
            kept.push(n);
            continue;
        }
        match merged_on_github(ctx, dir, n)? {
            true => {}
            false => {
                return Err(Refusal(format!(
                    "{pr}: --after {n}를 받지 않았다. {n}는 큐에 없고 머지되지도 않았다. \
                     {n}를 먼저 submit하거나 `--after`에서 뺀다"
                ))
                .into());
            }
        }
    }
    Ok(kept)
}

fn merged_on_github(ctx: &Ctx, dir: &Path, n: PrNumber) -> Result<bool> {
    #[derive(Deserialize)]
    struct Found {
        state: PrState,
    }
    let out = ctx
        .ext
        .gh(dir, ["pr", "view", &n.0.to_string(), "--json", "state"])?;
    let err = out.stderr_text().to_lowercase();
    if !out.success() && (err.contains("could not resolve to a pullrequest")) {
        return Err(Refusal(format!(
            "{n}: 큐에 없고 GitHub에서도 찾지 못했다. `--after`의 PR 번호를 확인한다"
        ))
        .into());
    }
    let text = out.ok()?;
    let found: Found = serde_json::from_str(&text).context("gh pr view의 출력을 읽지 못했다")?;
    Ok(found.state == PrState::Merged)
}

fn this_machine(ctx: &Ctx) -> Option<MachineId> {
    match machine::core(&ctx.ext) {
        Ok(core) => Some(core.id),
        Err(e) => {
            eprintln!("경고: 이 PC의 머신 id를 읽지 못했다: {e:#}. 에이전트의 머신을 비워 둔다");
            None
        }
    }
}

fn local_head(ext: &Ext, dir: &Path) -> Result<Sha> {
    let text = ext
        .git(dir, ["rev-parse", "HEAD"])?
        .ok()
        .context("이 PC의 HEAD를 읽지 못했다")?;
    Sha::try_from(text.trim().to_owned()).map_err(anyhow::Error::msg)
}

fn check_head(pr: &Pr, expected: &Sha) -> Result<(), Refusal> {
    if pr.head_ref_oid.as_str().starts_with(expected.as_str()) {
        return Ok(());
    }
    Err(Refusal(format!(
        "{}: GitHub의 PR HEAD는 {}인데 확인한 HEAD는 {expected}다. \
         push하지 않은 커밋이 있거나 그사이 다른 커밋이 push됐다. 확인할 HEAD를 push한 뒤 다시 submit한다",
        pr.number, pr.head_ref_oid
    )))
}

fn check_agent_machine(
    ext: &Ext,
    agent: &Agent,
    this_pc: Option<&MachineId>,
) -> Result<(), Refusal> {
    let Some(machine) = &agent.machine else {
        return Ok(());
    };
    if Some(machine) == this_pc {
        return Ok(());
    }
    let known = machine::list(ext).map_err(|e| Refusal(format!("{e:#}")))?;
    if known.iter().any(|m| m.id == *machine) {
        return Ok(());
    }
    Err(Refusal(format!(
        "에이전트가 있는 머신({machine})을 이 허브가 모른다. 반송이 갈 길이 없어 받지 않는다. \
         허브에서 `hc machine add`로 그 PC를 더한 뒤 다시 submit한다"
    )))
}

#[derive(Serialize)]
struct Report {
    pr: PrNumber,
    head: Sha,
    #[serde(flatten)]
    state: ItemState,
    position: NonZeroUsize,
    change: Change,
}

fn render(report: &Report, touches_config: bool) -> String {
    let headline = match (report.change, report.state) {
        (Change::Added, _) => "등록했다",
        (Change::Requeued, ItemState::AwaitingApproval) => "HEAD가 바뀌어 다시 승인을 기다린다",
        (Change::Requeued, _) => "다시 큐에 올렸다",
        (Change::HeadUpdated, _) => "HEAD를 바꿔 다시 등록했다",
        (Change::OptionsUpdated, _) => "옵션을 바꿔 다시 등록했다",
        (Change::Unchanged, _) => "이미 등록돼 있고 바뀐 것이 없다",
    };
    let mut text = format!(
        "{headline}: {}\nHEAD {} · {} · 큐 {}번째\n",
        report.pr,
        report.head,
        queue::label(report.state),
        report.position
    );
    if report.state == ItemState::AwaitingApproval {
        if touches_config {
            text.push_str("chute.toml을 고치는 PR이라 신뢰 모드에서도 사람 승인이 필요하다.\n");
        }
        text.push_str(&format!(
            "사람이 승인해야 큐에 들어간다. Herdr 현황판의 승인 버튼이나 `chute approve {}`로 한다.\n",
            report.pr.0
        ));
    }
    text
}

fn load_default_branch_config(ctx: &Ctx, info: &RepoInfo) -> Result<Config> {
    let branch = &info.default_branch;
    Config::load_remote_branch(&ctx.ext, &info.path, &info.remote_name, branch)?.ok_or_else(|| {
        Refusal(format!(
            "{}: chute.toml을 {}/{branch}에서 읽지 못했다. 기본 브랜치에 올렸는지 확인하고 `git fetch`를 먼저 실행한다",
            info.slug, info.remote_name
        ))
        .into()
    })
}

const PR_FIELDS: &str =
    "number,title,headRefName,headRefOid,baseRefName,state,isDraft,files,changedFiles";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: PrNumber,
    title: String,
    head_ref_name: String,
    head_ref_oid: Sha,
    base_ref_name: String,
    state: PrState,
    is_draft: bool,
    files: Vec<PrFile>,
    changed_files: usize,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Deserialize)]
struct PrFile {
    path: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigEdit {
    No,
    Yes,
    /// `gh` lists at most 100 files, so a longer PR may hide `chute.toml`.
    Unknown,
}

impl Pr {
    fn config_edit(&self) -> ConfigEdit {
        if self.files.iter().any(|f| f.path == config::FILE_NAME) {
            ConfigEdit::Yes
        } else if self.files.len() < self.changed_files {
            ConfigEdit::Unknown
        } else {
            ConfigEdit::No
        }
    }
}

fn read_pr(ctx: &Ctx, dir: &Path, number: Option<PrNumber>) -> Result<Pr> {
    let mut args = vec!["pr".to_owned(), "view".to_owned()];
    args.extend(number.map(|n| n.0.to_string()));
    args.extend(["--json".to_owned(), PR_FIELDS.to_owned()]);
    let text = ctx
        .ext
        .gh(dir, &args)?
        .ok()
        .context("PR을 읽지 못했다. 번호를 인자로 주거나 PR이 있는 브랜치에서 실행한다")?;
    serde_json::from_str(&text).context("gh pr view의 출력을 읽지 못했다")
}

fn check_open_and_ready(pr: &Pr, info: &RepoInfo) -> Result<(), Refusal> {
    let n = pr.number;
    match pr.state {
        PrState::Open => {}
        PrState::Closed => {
            return Err(Refusal(format!(
                "{n}: 닫힌 PR이다. 열린 PR만 큐에 올릴 수 있다"
            )));
        }
        PrState::Merged => return Err(Refusal(format!("{n}: 이미 머지된 PR이다"))),
    }
    if pr.is_draft {
        return Err(Refusal(format!(
            "{n}: draft PR이다. ready for review로 바꾼 뒤 다시 submit한다"
        )));
    }
    if pr.base_ref_name != info.default_branch {
        return Err(Refusal(format!(
            "{n}: `{}` 브랜치를 향한 PR이다. chute는 기본 브랜치 `{}`를 향한 PR만 받는다",
            pr.base_ref_name, info.default_branch
        )));
    }
    Ok(())
}

fn identify_agent(
    ctx: &Ctx,
    dir: &Path,
    flag: Option<&str>,
    this_pc: impl FnOnce() -> Option<MachineId>,
) -> Option<Agent> {
    let Some(address) = flag.filter(|a| !a.is_empty()).or(ctx.terminal()) else {
        eprintln!(
            "경고: 에이전트를 알 수 없다. --agent도 HC_TERMINAL_ID도 없다. 에이전트 없이 등록한다"
        );
        return None;
    };
    match resolve_terminal(ctx, address, dir, this_pc) {
        Ok(agent) => Some(agent),
        Err(e) => {
            eprintln!("경고: 에이전트를 확인하지 못했다({address}): {e:#}. 에이전트 없이 등록한다");
            None
        }
    }
}

fn identify_submitter(
    ctx: &Ctx,
    dir: &Path,
    this_pc: impl FnOnce() -> Option<MachineId>,
) -> Option<Agent> {
    let address = ctx.terminal().filter(|a| !a.is_empty())?;
    match resolve_terminal(ctx, address, dir, this_pc) {
        Ok(agent) => Some(agent),
        Err(e) => {
            eprintln!(
                "경고: submit한 터미널을 확인하지 못했다({address}): {e:#}. 결과 알림을 받을 터미널 없이 등록한다"
            );
            None
        }
    }
}

fn resolve_terminal(
    ctx: &Ctx,
    address: &str,
    dir: &Path,
    this_pc: impl FnOnce() -> Option<MachineId>,
) -> Result<Agent> {
    let agent = lookup_agent(ctx, address, dir)?;
    let machine = machine_of(ctx, &agent.terminal, this_pc);
    Ok(Agent { machine, ..agent })
}

fn machine_of(
    ctx: &Ctx,
    terminal: &TerminalId,
    this_pc: impl FnOnce() -> Option<MachineId>,
) -> Option<MachineId> {
    let Some((name, _)) = terminal.0.split_once('/') else {
        return this_pc();
    };
    let found = machine::list(&ctx.ext)
        .ok()
        .and_then(|all| all.into_iter().find(|m| m.name == name))
        .map(|m| m.id);
    if found.is_none() {
        eprintln!("경고: 터미널 주소의 머신 `{name}`을 hc가 모른다. 에이전트의 머신을 비워 둔다");
    }
    found
}

#[derive(Deserialize)]
struct HcTerminal {
    address: String,
    state: String,
    created_ms: u64,
    name: Option<String>,
    #[serde(rename = "cwd")]
    launch_cwd: Option<PathBuf>,
    foreground: Option<HcForeground>,
}

#[derive(Deserialize)]
struct HcForeground {
    cwd: Option<PathBuf>,
}

fn lookup_agent(ctx: &Ctx, address: &str, fallback_dir: &Path) -> Result<Agent> {
    let text = ctx.ext.hc(["get", "--json", address])?.ok()?;
    let terminal: HcTerminal =
        serde_json::from_str(&text).context("hc get의 출력을 읽지 못했다")?;
    if terminal.address.trim().is_empty() {
        bail!("hc get의 응답에 주소가 없다");
    }
    if terminal.state != "live" {
        bail!("터미널이 끝났다(state: {})", terminal.state);
    }
    let workdir = terminal
        .foreground
        .and_then(|f| f.cwd)
        .into_iter()
        .chain(terminal.launch_cwd)
        .find(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| fallback_dir.to_owned());
    Ok(Agent {
        terminal: TerminalId(terminal.address),
        created_ms: EpochMs(terminal.created_ms),
        name: terminal.name.filter(|n| !n.is_empty()),
        workdir,
        alive: true,
        machine: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sha(s: &str) -> Sha {
        Sha::try_from(s.to_owned()).unwrap()
    }

    fn report(state: ItemState, change: Change, position: usize) -> Report {
        Report {
            pr: PrNumber(12),
            head: sha("12e5678"),
            state,
            position: NonZeroUsize::new(position).unwrap(),
            change,
        }
    }

    #[test]
    fn the_text_says_what_happened_and_what_to_do_next() {
        let queued = render(&report(ItemState::Queued(None), Change::Added, 3), false);
        assert_eq!(queued, "등록했다: #12\nHEAD 12e5678 · 대기 · 큐 3번째\n");

        let waiting = render(&report(ItemState::AwaitingApproval, Change::Added, 1), true);
        assert!(waiting.contains("chute.toml을 고치는 PR"), "{waiting}");
        assert!(waiting.contains("`chute approve 12`"), "{waiting}");
        let plain = render(
            &report(ItemState::AwaitingApproval, Change::Added, 1),
            false,
        );
        assert!(!plain.contains("chute.toml을"), "{plain}");

        let back = render(
            &report(ItemState::AwaitingApproval, Change::Requeued, 1),
            false,
        );
        assert!(
            back.starts_with("HEAD가 바뀌어 다시 승인을 기다린다: #12"),
            "{back}"
        );
        let requeued = render(&report(ItemState::Queued(None), Change::Requeued, 1), false);
        assert!(requeued.starts_with("다시 큐에 올렸다: #12"), "{requeued}");

        let same = render(
            &report(ItemState::Queued(None), Change::Unchanged, 1),
            false,
        );
        assert!(
            same.starts_with("이미 등록돼 있고 바뀐 것이 없다: #12"),
            "{same}"
        );
    }

    #[test]
    fn the_json_report_has_pr_head_state_and_place() {
        let report = report(
            ItemState::Queued(Some(crate::model::QueuedReason::CiPending)),
            Change::HeadUpdated,
            2,
        );
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            json!({
                "pr": 12, "head": "12e5678", "state": "queued", "state_reason": "ci_pending",
                "position": 2, "change": "head_updated",
            })
        );
    }

    fn pr_with(files: &[&str], changed_files: usize) -> Pr {
        serde_json::from_value(json!({
            "number": 12, "title": "t", "headRefName": "b", "headRefOid": "12e5678",
            "baseRefName": "main", "state": "OPEN", "isDraft": false,
            "files": files.iter().map(|p| json!({"path": p})).collect::<Vec<_>>(),
            "changedFiles": changed_files,
        }))
        .unwrap()
    }

    #[test]
    fn a_file_list_shorter_than_the_changed_file_count_cannot_prove_chute_toml_is_untouched() {
        assert!(pr_with(&["a", "b"], 2).config_edit() == ConfigEdit::No);
        assert!(pr_with(&["a", "chute.toml"], 2).config_edit() == ConfigEdit::Yes);
        assert!(pr_with(&["a"], 101).config_edit() == ConfigEdit::Unknown);
        assert!(pr_with(&["chute.toml"], 101).config_edit() == ConfigEdit::Yes);
        assert!(pr_with(&["docs/chute.toml"], 1).config_edit() == ConfigEdit::No);
    }
}
