use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{Ctx, Refusal, finish};
use crate::config::{self, Config};
use crate::model::{Agent, EpochMs, ItemState, PrNumber, RepoInfo, Sha, TerminalId};
use crate::queue::{self, Change, Submission};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호. 없으면 지금 브랜치의 PR
    pub pr: Option<PrNumber>,
    /// 반송을 받을 hc 터미널 주소. 없으면 HC_TERMINAL_ID
    #[arg(long, value_name = "주소")]
    pub agent: Option<String>,
    /// 결과를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    finish(submit(&args, ctx))
}

fn submit(args: &Args, ctx: &Ctx) -> Result<()> {
    let dir = ctx.target_dir()?;
    let (info, store) = ctx.registered_repo()?;
    let config = load_default_branch_config(ctx, &info)?;
    let pr = read_pr(ctx, &dir, args.pr)?;
    check_open_and_ready(&pr, &info)?;
    let agent = identify_agent(ctx, &dir, args.agent.as_deref());

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
        touches_config: config_edit != ConfigEdit::No,
        approval: config.approval,
        now: EpochMs::now(),
    };
    let done = store.update(|state| Ok(queue::submit(state, submission)?))?;
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
    Ok(())
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
    Config::load_remote_branch(&ctx.ext, &info.path, branch)?.ok_or_else(|| {
        Refusal(format!(
            "{}: chute.toml을 origin/{branch}에서 읽지 못했다. 기본 브랜치에 올렸는지 확인하고 `git fetch`를 먼저 실행한다",
            info.slug
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

fn identify_agent(ctx: &Ctx, dir: &Path, flag: Option<&str>) -> Option<Agent> {
    let Some(address) = flag.filter(|a| !a.is_empty()).or(ctx.terminal()) else {
        eprintln!(
            "경고: 에이전트를 알 수 없다. --agent도 HC_TERMINAL_ID도 없다. 에이전트 없이 등록한다"
        );
        return None;
    };
    match lookup_agent(ctx, address, dir) {
        Ok(agent) => Some(agent),
        Err(e) => {
            eprintln!("경고: 에이전트를 확인하지 못했다({address}): {e:#}. 에이전트 없이 등록한다");
            None
        }
    }
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
