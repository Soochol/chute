use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::{Ctx, FORWARD_TIMEOUT, REFUSED, forward_to_hub};
use crate::config::{Config, DEFAULT_MAX_PARALLEL};
use crate::env::{self, EnvSection};
use crate::feedback::{self, Feedback, Subject, Written};
use crate::gc;
use crate::gitops::{Gitops, ItemResult, MergeItem, PrHead, Slot};
use crate::model::{
    Blocked, ConflictFile, EnvRecord, EpochMs, Failure, FailureKind, FailureShape, ItemState, Mode,
    NonEmpty, Partner, PrNumber, PreflightPolicy, PreflightRecord, PreflightResult, RepoInfo, Sha,
    StatusView, StepFailure, VerifyMode,
};
use crate::pc_limit::Wait;
use crate::store::RepoStore;
use crate::verify::{self, Cause, Detail, FlakyStep, Options, Outcome};

const RECORD_KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SHOWN_PATHS: usize = 5;
const BASE_BROKEN: u8 = 4;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 결과를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
    /// 다른 PC의 preflight가 결과를 넘길 때 쓴다. 검증하지 않고 이 결과만 기록한다
    #[arg(long, hide = true, value_name = "JSON", conflicts_with = "json")]
    pub record: Option<String>,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    if let Some(text) = &args.record {
        let entry: Recorded =
            serde_json::from_str(text).context("넘겨받은 preflight 결과를 읽지 못했다")?;
        let (_, store) = ctx.registered_repo()?;
        deliver(ctx, &store, &entry)?;
        return Ok(ExitCode::SUCCESS);
    }
    let report = preflight(ctx)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    Ok(match report.result {
        PreflightResult::Pass | PreflightResult::Skipped => ExitCode::SUCCESS,
        PreflightResult::Fail | PreflightResult::Conflict => ExitCode::from(REFUSED),
        PreflightResult::BaseBroken => ExitCode::from(BASE_BROKEN),
    })
}

#[derive(Debug, Serialize)]
struct Report {
    result: PreflightResult,
    head: Sha,
    base: Sha,
    with: Vec<PrNumber>,
    skipped: Vec<Skipped>,
    skipped_steps: Vec<String>,
    flaky: Vec<FlakyReport>,
    kind: Option<FailureKind>,
    mode: Option<Mode>,
    summary: Option<String>,
    report: Option<PathBuf>,
    report_json: Option<PathBuf>,
    base_broken: Option<BrokenBase>,
    blocked: Option<Blocked>,
    skip_reason: Option<SkipReason>,
    at_ms: EpochMs,
    /// Absent when no slot was opened to record it.
    env: Option<EnvRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SkipReason {
    /// No other PR waits in the queue, so the batch runs the same checks.
    NoQueue,
    /// `chute.toml` says `preflight = "never"`.
    Never,
}

impl SkipReason {
    fn label(self) -> &'static str {
        match self {
            SkipReason::NoQueue => {
                "함께 합칠 PR이 큐에 없어 검사하지 않는다. batch가 병합 전에 검증한다"
            }
            SkipReason::Never => {
                "chute.toml의 preflight가 \"never\"라 검사하지 않는다. batch가 병합 전에 검증한다"
            }
        }
    }
}

fn skip_reason(policy: PreflightPolicy, others_queued: usize) -> Option<SkipReason> {
    match policy {
        PreflightPolicy::Always => None,
        PreflightPolicy::Never => Some(SkipReason::Never),
        PreflightPolicy::WhenQueued => (others_queued == 0).then_some(SkipReason::NoQueue),
    }
}

#[derive(Debug, Serialize)]
struct BrokenBase {
    step: String,
    log: PathBuf,
}

#[derive(Debug, Serialize)]
struct Skipped {
    pr: PrNumber,
    why: Why,
    detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Why {
    HeadChanged,
    FetchFailed,
    Conflict,
    InBase,
}

impl Why {
    fn label(self) -> &'static str {
        match self {
            Why::HeadChanged => "등록한 뒤 HEAD가 바뀌었다",
            Why::FetchFailed => "HEAD를 가져오지 못했다",
            Why::Conflict => "앞의 큐 항목과 충돌한다",
            Why::InBase => "이미 기준에 들어 있다",
        }
    }
}

#[derive(Debug, Serialize)]
struct FlakyReport {
    step: String,
    tests: Vec<String>,
}

struct Steps {
    outcome: Outcome,
    skipped: Vec<String>,
}

struct Target {
    head: Sha,
    branch: Option<String>,
}

enum Alone {
    Conflicts(NonEmpty<ConflictFile>),
    Passes,
    Fails(verify::Failed, Sha),
}

struct Judged {
    result: PreflightResult,
    with: Vec<PrNumber>,
    flaky: Vec<FlakyStep>,
    skipped_steps: Vec<String>,
    written: Option<Written>,
    base_broken: Option<BrokenBase>,
}

fn preflight(ctx: &Ctx) -> Result<Report> {
    let dir = ctx.target_dir()?;
    let (info, store) = ctx.registered_repo()?;
    let target = read_target(ctx, &dir)?;
    let Queue { waiting, blocked } = read_queue(ctx, &store)?;
    if let Some(blocked) = &blocked {
        eprintln!("{}", blocked_notice(blocked));
    }
    let queued: Vec<&Waiting> = waiting
        .iter()
        .filter(|item| Some(item.branch.as_str()) != target.branch.as_deref())
        .collect();

    let last_fetched_config = Config::load_remote_branch(
        &ctx.ext,
        &info.path,
        &info.remote_name,
        &info.default_branch,
    )
    .ok()
    .flatten();
    // Without a readable config the full run below fetches the base and reports why.
    let skip_why = last_fetched_config
        .as_ref()
        .and_then(|c| skip_reason(c.preflight, queued.len()));
    if let Some(reason) = skip_why {
        let base = remote_base(ctx, &info)?;
        let report = Report::skipped(reason, &target, base, blocked, EpochMs::now());
        if let Err(e) = record(ctx, &store, &target, &report) {
            eprintln!("경고: 결과를 상태에 기록하지 못했다: {e:#}");
        }
        return Ok(report);
    }
    let max_parallel = last_fetched_config
        .as_ref()
        .map_or(DEFAULT_MAX_PARALLEL, |c| c.max_parallel);
    let seed = last_fetched_config
        .map(|c| c.cache.seed)
        .unwrap_or_default();
    let gitops = Gitops::new(&ctx.ext, &ctx.home, &info).with_cache_seed(seed);
    let slot = gitops.lock_preflight_slot(max_parallel)?.open()?;
    let base = slot.fetch_base()?;
    let config = Config::load_remote_branch(
        &ctx.ext,
        &info.path,
        &info.remote_name,
        &info.default_branch,
    )?
    .ok_or_else(|| {
        anyhow!(
            "{}/{}에서 chute.toml을 읽지 못했다. 기본 브랜치에 올렸는지 확인한다",
            info.remote_name,
            info.default_branch
        )
    })?;

    slot.checkout(&base)?;
    let at = EpochMs::now();
    let log_dir = log_dir(&store, &target.head, at)?;
    let env = env::record(
        &ctx.ext,
        slot.path(),
        &log_dir.join("env"),
        &config.env_check,
        &verify::Cancel::new(),
    );

    let mut skipped = Vec::new();
    let mut items = fetch_queue(&slot, &queued, &mut skipped);
    items.push(MergeItem::Local {
        head: target.head.clone(),
    });
    let merged = slot.merge(&base, &items)?;
    let Some((own, queue_outcomes)) = merged.items.split_last() else {
        bail!("합치기 결과가 비었다");
    };
    let mut together: Vec<(PrNumber, String, Sha)> = Vec::new();
    for outcome in queue_outcomes {
        let MergeItem::Pr {
            pr, title, head, ..
        } = &outcome.item
        else {
            bail!("큐 항목이 아닌 것이 큐 자리에 있다");
        };
        match outcome.result {
            ItemResult::Merged { .. } => together.push((*pr, title.clone(), head.clone())),
            ItemResult::AlreadyMerged => skipped.push(skip(*pr, Why::InBase, None)),
            ItemResult::Conflict(_) => skipped.push(skip(*pr, Why::Conflict, None)),
        }
    }

    let worktrees_root = feedback::worktrees_root(|key| std::env::var_os(key)).unwrap_or_default();
    let feedback = Feedback {
        ext: &ctx.ext,
        repo: &info,
        store: &store,
        config: &config,
        worktrees_root: &worktrees_root,
    };
    let run = Run {
        ctx,
        info: &info,
        store: &store,
        config: &config,
        feedback: &feedback,
        gitops: &gitops,
        slot: &slot,
        target: &target,
        base: &base,
        together: &together,
        at,
        log_dir,
        env: &env,
    };
    let mut judged = match &own.result {
        ItemResult::Conflict(files) => run.conflict(files, run.with())?,
        ItemResult::Merged { .. } | ItemResult::AlreadyMerged => run.verify(&merged.tip)?,
    };

    let report = Report::new(&mut judged, &target, &base, skipped, blocked, at, env);
    if let Err(e) = record(ctx, &store, &target, &report) {
        eprintln!("경고: 결과를 상태에 기록하지 못했다: {e:#}");
    }
    gc::auto(
        &gc::Site {
            ext: &ctx.ext,
            home: &ctx.home,
            info: &info,
            store: &store,
        },
        false,
    );
    Ok(report)
}

/// The default-branch tip on the remote, read without fetching into a slot.
fn remote_base(ctx: &Ctx, info: &RepoInfo) -> Result<Sha> {
    let branch = format!("refs/heads/{}", info.default_branch);
    let listed = ctx
        .ext
        .git(
            &info.path,
            ["ls-remote", info.remote_name.as_str(), branch.as_str()],
        )?
        .ok()
        .with_context(|| {
            format!(
                "{}/{}의 기준 커밋을 묻지 못했다",
                info.remote_name, info.default_branch
            )
        })?;
    let first = listed
        .lines()
        .find_map(|line| line.split_whitespace().next())
        .ok_or_else(|| {
            anyhow!(
                "{}/{}의 기준 커밋을 찾지 못했다",
                info.remote_name,
                info.default_branch
            )
        })?;
    Sha::try_from(first.to_owned()).map_err(|e| anyhow!(e))
}

struct Waiting {
    pr: PrNumber,
    branch: String,
    title: String,
    head: Sha,
}

struct Queue {
    waiting: Vec<Waiting>,
    blocked: Option<Blocked>,
}

fn read_queue(ctx: &Ctx, store: &RepoStore) -> Result<Queue> {
    let answer = forward_to_hub(ctx, &["status", "--json"], FORWARD_TIMEOUT)
        .context("허브에 닿지 않아 큐를 알 수 없다. 큐 없이 검증하면 batch의 결과와 달라질 수 있어 검증하지 않는다")?;
    let Some(answer) = answer else {
        let state = store.load()?;
        let waiting = state
            .queue
            .iter()
            .filter(|item| matches!(item.state, ItemState::Queued(_) | ItemState::InBatch))
            .map(|item| Waiting {
                pr: item.pr,
                branch: item.branch.clone(),
                title: item.title.clone(),
                head: item.head.clone(),
            })
            .collect();
        return Ok(Queue {
            waiting,
            blocked: state.blocked,
        });
    };
    if answer.code != 0 || answer.truncated {
        bail!(
            "허브가 큐를 알려 주지 않아 검증하지 않는다 (종료 코드 {}): {}",
            answer.code,
            String::from_utf8_lossy(&answer.stderr).trim()
        );
    }
    let view: StatusView = serde_json::from_slice(&answer.stdout)
        .context("허브의 상태를 읽지 못해 큐를 알 수 없다. 검증하지 않는다")?;
    let waiting = view
        .queue
        .into_iter()
        .filter(|item| matches!(item.state, ItemState::Queued(_) | ItemState::InBatch))
        .map(|item| Waiting {
            pr: item.pr,
            branch: item.branch,
            title: item.title,
            head: item.head,
        })
        .collect();
    Ok(Queue {
        waiting,
        blocked: view.blocked,
    })
}

fn read_target(ctx: &Ctx, dir: &Path) -> Result<Target> {
    let status = ctx
        .ext
        .git(
            dir,
            [
                "--no-optional-locks",
                "status",
                "--porcelain",
                "--untracked-files=normal",
                "--ignore-submodules=untracked",
            ],
        )?
        .ok()?;
    if !status.trim().is_empty() {
        let lines: Vec<&str> = status.lines().collect();
        let shown = lines
            .iter()
            .take(SHOWN_PATHS)
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let more = lines.len().saturating_sub(SHOWN_PATHS);
        let tail = if more > 0 {
            format!("\n  …외 {more}개")
        } else {
            String::new()
        };
        bail!(
            "커밋되지 않은 변경이 있어 검증하지 않는다. 변경을 커밋한 뒤 다시 실행한다:\n{shown}{tail}"
        );
    }
    let head = ctx
        .ext
        .git(dir, ["rev-parse", "--verify", "HEAD^{commit}"])?
        .ok()?;
    let head = Sha::try_from(head.trim().to_owned()).map_err(|e| anyhow!(e))?;
    let named = ctx.ext.git(dir, ["symbolic-ref", "--quiet", "HEAD"])?;
    let branch = named
        .success()
        .then(|| named.stdout_text().trim_end_matches('\n').to_owned())
        .and_then(|full| full.strip_prefix("refs/heads/").map(str::to_owned))
        .filter(|name| !name.is_empty());
    Ok(Target { head, branch })
}

fn fetch_queue(slot: &Slot, queued: &[&Waiting], skipped: &mut Vec<Skipped>) -> Vec<MergeItem> {
    let mut items = Vec::new();
    for item in queued {
        match slot.fetch_pr_head(item.pr, &item.head) {
            Ok(PrHead::Unchanged) => items.push(MergeItem::Pr {
                pr: item.pr,
                branch: item.branch.clone(),
                title: item.title.clone(),
                head: item.head.clone(),
            }),
            Ok(PrHead::Changed { .. }) => skipped.push(skip(item.pr, Why::HeadChanged, None)),
            Err(e) => skipped.push(skip(item.pr, Why::FetchFailed, Some(format!("{e:#}")))),
        }
    }
    items
}

fn skip(pr: PrNumber, why: Why, detail: Option<String>) -> Skipped {
    Skipped { pr, why, detail }
}

struct Run<'a> {
    ctx: &'a Ctx,
    info: &'a RepoInfo,
    store: &'a RepoStore,
    config: &'a Config,
    feedback: &'a Feedback<'a>,
    gitops: &'a Gitops,
    slot: &'a Slot,
    target: &'a Target,
    base: &'a Sha,
    together: &'a [(PrNumber, String, Sha)],
    at: EpochMs,
    log_dir: PathBuf,
    env: &'a EnvRecord,
}

impl Run<'_> {
    fn subject(&self) -> Subject {
        Subject::Commit {
            commit: self.target.head.clone(),
        }
    }

    fn with(&self) -> Vec<PrNumber> {
        self.together.iter().map(|(pr, ..)| *pr).collect()
    }

    fn conflict(&self, files: &NonEmpty<ConflictFile>, with: Vec<PrNumber>) -> Result<Judged> {
        let owners = self.commit_owners()?;
        let named: Vec<ConflictFile> = files
            .iter()
            .map(|file| ConflictFile {
                path: file.path.clone(),
                main_commits: file
                    .main_commits
                    .iter()
                    .map(|commit| {
                        let mut commit = commit.clone();
                        if let Some(pr) = owners.get(&commit.sha) {
                            commit.title = format!("큐의 PR {pr}: {}", commit.title);
                        }
                        commit
                    })
                    .collect(),
            })
            .collect();
        let files = NonEmpty::new(named).ok_or_else(|| anyhow!("충돌한 파일 목록이 비었다"))?;
        let written = self.write_report(&Failure::Conflict { files }, None)?;
        Ok(Judged {
            result: PreflightResult::Conflict,
            with,
            flaky: Vec::new(),
            skipped_steps: Vec::new(),
            written: Some(written),
            base_broken: None,
        })
    }

    fn commit_owners(&self) -> Result<HashMap<Sha, PrNumber>> {
        let mut owners = HashMap::new();
        for (pr, _, head) in self.together {
            let range = format!("{}..{head}", self.base);
            let listed = self
                .ctx
                .ext
                .git(self.slot.path(), ["log", "--format=%H", range.as_str()])?
                .ok()?;
            for line in listed.lines() {
                let sha = Sha::try_from(line.trim().to_owned()).map_err(|e| anyhow!(e))?;
                owners.entry(sha).or_insert(*pr);
            }
        }
        Ok(owners)
    }

    fn verify(&self, tip: &Sha) -> Result<Judged> {
        let with = self.with();
        let Steps {
            outcome,
            skipped: skipped_steps,
        } = self.run_steps(&self.log_dir.join("with-queue"), true)?;
        let failed = match outcome {
            Outcome::Passed { flaky } => {
                return Ok(Judged {
                    result: PreflightResult::Pass,
                    with,
                    flaky,
                    skipped_steps,
                    written: None,
                    base_broken: None,
                });
            }
            Outcome::Failed(failed) => failed,
            Outcome::Cancelled => bail!("검증이 취소됐다"),
        };
        let (failure, tree) = if with.is_empty() {
            (step_failure(&failed, VerifyMode::Solo, tip), tip.clone())
        } else {
            let alone = self.verify_alone().with_context(|| {
                format!(
                    "큐와 함께 한 검증은 {} 단계에서 실패했다. 이 HEAD만으로는 검증하지 못해 보고서를 쓰지 못했다",
                    failed.step
                )
            })?;
            match alone {
                Alone::Conflicts(files) => return self.conflict(&files, Vec::new()),
                Alone::Passes => {
                    let mode = VerifyMode::Combo {
                        partners: self.partners(tip)?,
                    };
                    (step_failure(&failed, mode, tip), tip.clone())
                }
                Alone::Fails(alone, solo_tip) => {
                    (step_failure(&alone, VerifyMode::Solo, &solo_tip), solo_tip)
                }
            }
        };
        if is_solo(&failure)
            && let Some(broken) = self.broken_base()?
        {
            return Ok(Judged {
                result: PreflightResult::BaseBroken,
                with,
                flaky: Vec::new(),
                skipped_steps,
                written: None,
                base_broken: Some(broken),
            });
        }
        let written = self.report_failure(&failure, &tree)?;
        Ok(Judged {
            result: PreflightResult::Fail,
            with,
            flaky: Vec::new(),
            skipped_steps,
            written: Some(written),
            base_broken: None,
        })
    }

    fn broken_base(&self) -> Result<Option<BrokenBase>> {
        if self.store.load()?.verified_bases.contains(self.base) {
            return Ok(None);
        }
        self.slot.checkout(self.base)?;
        match self.run_steps(&self.log_dir.join("base"), false)?.outcome {
            Outcome::Passed { .. } => Ok(None),
            Outcome::Failed(failed) => Ok(Some(BrokenBase {
                step: failed.step,
                log: failed.log,
            })),
            Outcome::Cancelled => bail!("검증이 취소됐다"),
        }
    }

    fn verify_alone(&self) -> Result<Alone> {
        let solo = self.slot.merge(
            self.base,
            &[MergeItem::Local {
                head: self.target.head.clone(),
            }],
        )?;
        if let Some(ItemResult::Conflict(files)) = solo.items.first().map(|item| &item.result) {
            return Ok(Alone::Conflicts(files.clone()));
        }
        match self.run_steps(&self.log_dir.join("solo"), true)?.outcome {
            Outcome::Passed { .. } => Ok(Alone::Passes),
            Outcome::Failed(failed) => Ok(Alone::Fails(failed, solo.tip)),
            Outcome::Cancelled => bail!("검증이 취소됐다"),
        }
    }

    fn partners(&self, tip: &Sha) -> Result<NonEmpty<Partner>> {
        let subject = self.subject();
        let partners = self
            .together
            .iter()
            .map(|(pr, title, _)| {
                self.feedback
                    .partner_or_placeholder(&subject, tip, *pr, title)
            })
            .collect::<Result<Vec<_>>>()?;
        NonEmpty::new(partners).ok_or_else(|| anyhow!("함께 검증한 PR이 없는데 조합 실패로 봤다"))
    }

    fn report_failure(&self, failure: &Failure, tree: &Sha) -> Result<Written> {
        let name = failed_ref_name(self.at, tree);
        self.gitops.set_failed_ref(&name, tree)?;
        self.write_report(failure, Some(&name))
    }

    fn write_report(&self, failure: &Failure, failed_ref: Option<&str>) -> Result<Written> {
        let section = EnvSection {
            env: self.env.clone(),
            preflight: None,
        };
        self.feedback
            .write_report_with_env(&self.subject(), failure, failed_ref, Some(section))
    }

    fn run_steps(&self, log_dir: &Path, only_changed: bool) -> Result<Steps> {
        let changed = if only_changed {
            Some(self.slot.changed_files(self.base)?)
        } else {
            None
        };
        let skipped = RefCell::new(Vec::new());
        let on_step = |name: &str| eprintln!("검증 단계: {name}");
        let on_machine_wait = |wait: Wait| {
            if let Wait::Begin { running } = wait {
                eprintln!("이 PC에서 검증 {running}개가 돌고 있어 기다린다");
            }
        };
        let on_skip = |name: &str| {
            eprintln!("건너뛴 검증 단계: {name} (바뀐 파일이 paths에 없다)");
            skipped.borrow_mut().push(name.to_owned());
        };
        let options = Options {
            cache_dir: Some(self.gitops.cache_dir()),
            on_step: Some(&on_step),
            on_machine_wait: Some(&on_machine_wait),
            changed: changed.as_deref(),
            on_skip: Some(&on_skip),
            ..Options::default()
        };
        let outcome = verify::run(self.config, self.store, self.slot.path(), log_dir, &options)
            .with_context(|| format!("검증을 돌리지 못했다 (저장소 {})", self.info.slug))?;
        drop(options);
        Ok(Steps {
            outcome,
            skipped: skipped.into_inner(),
        })
    }
}

fn is_solo(failure: &Failure) -> bool {
    matches!(
        failure.shape(),
        FailureShape::Verify(Mode::Solo) | FailureShape::Timeout(Mode::Solo)
    )
}

fn step_failure(failed: &verify::Failed, mode: VerifyMode, tree: &Sha) -> Failure {
    let step = StepFailure {
        step: failed.step.clone(),
        mode,
        failed_tests: match &failed.detail {
            Detail::Tests(tests) => tests.to_vec(),
            Detail::Tail(_) => Vec::new(),
        },
        log: failed.log.clone(),
        tree: tree.clone(),
    };
    match failed.cause {
        Cause::TimedOut => Failure::Timeout(step),
        Cause::Exited(_) | Cause::Signaled(_) => Failure::Verify(step),
    }
}

fn failed_ref_name(at: EpochMs, tree: &Sha) -> String {
    format!("{}{}-{tree}", gc::PREFLIGHT_REF_STEM, at.0)
}

fn log_dir(store: &RepoStore, head: &Sha, at: EpochMs) -> Result<PathBuf> {
    let short = &head.as_str()[..head.as_str().len().min(12)];
    let dir = store
        .dir()
        .join(gc::PREFLIGHT_LOG_DIR)
        .join(format!("{short}-{}", at.0));
    std::path::absolute(&dir)
        .with_context(|| format!("경로를 절대 경로로 바꾸지 못했다: {}", dir.display()))
}

impl Report {
    fn new(
        judged: &mut Judged,
        target: &Target,
        base: &Sha,
        skipped: Vec<Skipped>,
        blocked: Option<Blocked>,
        at_ms: EpochMs,
        env: EnvRecord,
    ) -> Report {
        let shape = judged
            .written
            .as_ref()
            .map(|written| written.doc.failure.shape());
        let mode = shape.and_then(|shape| match shape {
            crate::model::FailureShape::Verify(mode)
            | crate::model::FailureShape::Timeout(mode) => Some(mode),
            crate::model::FailureShape::Conflict | crate::model::FailureShape::Ci => None,
        });
        Report {
            result: judged.result,
            head: target.head.clone(),
            base: base.clone(),
            with: judged.with.clone(),
            skipped,
            skipped_steps: std::mem::take(&mut judged.skipped_steps),
            flaky: judged
                .flaky
                .iter()
                .map(|flaky| FlakyReport {
                    step: flaky.step.clone(),
                    tests: flaky.tests.clone(),
                })
                .collect(),
            kind: shape.map(|shape| shape.kind()),
            mode,
            summary: judged
                .written
                .as_ref()
                .map(|written| written.doc.summary.clone()),
            report: judged.written.as_ref().map(|written| written.md.clone()),
            report_json: judged.written.as_ref().map(|written| written.json.clone()),
            base_broken: judged.base_broken.take(),
            blocked,
            skip_reason: None,
            at_ms,
            env: Some(env),
        }
    }

    fn skipped(
        reason: SkipReason,
        target: &Target,
        base: Sha,
        blocked: Option<Blocked>,
        at_ms: EpochMs,
    ) -> Report {
        Report {
            result: PreflightResult::Skipped,
            head: target.head.clone(),
            base,
            with: Vec::new(),
            skipped: Vec::new(),
            skipped_steps: Vec::new(),
            flaky: Vec::new(),
            kind: None,
            mode: None,
            summary: None,
            report: None,
            report_json: None,
            base_broken: None,
            blocked,
            skip_reason: Some(reason),
            at_ms,
            env: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Recorded {
    branch: String,
    record: PreflightRecord,
}

fn record(ctx: &Ctx, store: &RepoStore, target: &Target, report: &Report) -> Result<()> {
    let Some(branch) = &target.branch else {
        eprintln!("경고: 브랜치가 아닌 HEAD라 결과를 상태에 기록하지 않는다");
        return Ok(());
    };
    let entry = Recorded {
        branch: branch.clone(),
        record: PreflightRecord {
            head: report.head.clone(),
            result: report.result,
            report: report.report.clone(),
            at_ms: report.at_ms,
            base: Some(report.base.clone()),
            with: report.with.clone(),
            env: report.env.clone(),
        },
    };
    deliver(ctx, store, &entry)
}

fn deliver(ctx: &Ctx, store: &RepoStore, entry: &Recorded) -> Result<()> {
    let text = serde_json::to_string(entry)?;
    match forward_to_hub(ctx, &["preflight", "--record", &text], FORWARD_TIMEOUT)? {
        None => write_record(store, entry),
        Some(answer) if answer.code == 0 => Ok(()),
        Some(answer) => bail!(
            "허브가 결과를 받지 않았다 (종료 코드 {}): {}",
            answer.code,
            String::from_utf8_lossy(&answer.stderr).trim()
        ),
    }
}

fn write_record(store: &RepoStore, entry: &Recorded) -> Result<()> {
    let cutoff = EpochMs::now()
        .0
        .saturating_sub(RECORD_KEEP_FOR.as_millis() as u64);
    store.update(|state| {
        let queued: Vec<String> = state.queue.iter().map(|i| i.branch.clone()).collect();
        state
            .preflights
            .retain(|name, old| old.at_ms.0 >= cutoff || queued.contains(name));
        state
            .preflights
            .insert(entry.branch.clone(), entry.record.clone());
        Ok(())
    })
}

fn blocked_notice(blocked: &Blocked) -> String {
    format!(
        "경고: 저장소가 base_broken 상태다. 기준 커밋 {}의 검증이 실패해 큐가 멈춰 있다. 이 실행의 실패는 이 HEAD의 변경 탓이 아닐 수 있다",
        short(&blocked.base)
    )
}

fn short(sha: &Sha) -> &str {
    &sha.as_str()[..sha.as_str().len().min(12)]
}

fn render(report: &Report) -> String {
    let mut out = String::new();
    let with = if report.with.is_empty() {
        "없음".to_owned()
    } else {
        report
            .with
            .iter()
            .map(|pr| pr.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (&report.summary, &report.base_broken) {
        _ if report.skip_reason.is_some() => {
            let reason = report.skip_reason.map_or("", SkipReason::label);
            out.push_str(&format!("생략: {reason}\n"));
        }
        (Some(summary), _) => out.push_str(&format!("{summary}\n")),
        (None, Some(broken)) => out.push_str(&format!(
            "기준이 깨졌다. 이 커밋 탓이 아니다: 기준 {}의 {} 단계가 실패한다. 이 HEAD는 판정하지 못했고 고칠 것도 없다. 기준을 고친 뒤 다시 실행한다\n검증 로그: {}\n",
            short(&report.base),
            broken.step,
            broken.log.display()
        )),
        (None, None) => out.push_str(&format!(
            "통과: HEAD {} (기준 {}, 함께 검증한 PR {with})\n",
            short(&report.head),
            short(&report.base)
        )),
    }
    if let Some(path) = &report.report {
        out.push_str(&format!("보고서: {}\n", path.display()));
    }
    if report.result != PreflightResult::Pass {
        out.push_str(&format!(
            "HEAD {} · 기준 {} · 함께 검증한 PR {with}\n",
            short(&report.head),
            short(&report.base)
        ));
    }
    for skipped in &report.skipped {
        let detail = skipped
            .detail
            .as_ref()
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "건너뛴 PR {}: {}{detail}\n",
            skipped.pr,
            skipped.why.label()
        ));
    }
    for step in &report.skipped_steps {
        out.push_str(&format!(
            "건너뛴 검증 단계 {step}: 건너뜀(바뀐 파일이 paths에 없다)\n"
        ));
    }
    if let Some(env) = &report.env {
        out.push_str(&format!("검사 환경: {}\n", env::machine_line(env)));
        for check in &env.checks {
            out.push_str(&format!("  {}: {}\n", check.run, env::result_text(check)));
        }
    }
    for flaky in &report.flaky {
        let tests = if flaky.tests.is_empty() {
            String::new()
        } else {
            format!(" (시험 {})", flaky.tests.join(", "))
        };
        out.push_str(&format!(
            "흔들린 단계: {}{tests}. 한 번 실패한 뒤 다시 돌리니 통과했다. 통과로 본다\n",
            flaky.step
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(text: &str) -> Sha {
        Sha::try_from(text.to_owned()).unwrap()
    }

    fn passed(with: &[u64]) -> Report {
        Report {
            result: PreflightResult::Pass,
            head: sha("0123456789abcdef0123456789abcdef01234567"),
            base: sha("fedcba9876543210fedcba9876543210fedcba98"),
            with: with.iter().map(|n| PrNumber(*n)).collect(),
            skipped: Vec::new(),
            skipped_steps: Vec::new(),
            flaky: Vec::new(),
            kind: None,
            mode: None,
            summary: None,
            report: None,
            report_json: None,
            base_broken: None,
            blocked: None,
            skip_reason: None,
            at_ms: EpochMs(1),
            env: Some(EnvRecord {
                machine_id: None,
                host: "pc".into(),
                checks: Vec::new(),
            }),
        }
    }

    #[test]
    fn a_failed_ref_name_carries_its_time_and_tree() {
        let tree = sha("0123456789abcdef0123456789abcdef01234567");
        let name = failed_ref_name(EpochMs(1700), &tree);
        assert_eq!(name, format!("preflight-1700-{tree}"));
        assert_eq!(gc::preflight_ref_made_at(&name), Some(EpochMs(1700)));
    }

    #[test]
    fn a_pass_says_what_it_was_verified_with() {
        let text = render(&passed(&[3, 4]));
        assert!(text.starts_with("통과: HEAD 0123456789ab"));
        assert!(text.contains("기준 fedcba987654"));
        assert!(text.contains("#3, #4"));
        assert!(render(&passed(&[])).contains("함께 검증한 PR 없음"));
    }

    #[test]
    fn the_policy_decides_from_the_number_of_other_queued_prs() {
        use PreflightPolicy::{Always, Never, WhenQueued};
        assert_eq!(skip_reason(WhenQueued, 0), Some(SkipReason::NoQueue));
        assert_eq!(skip_reason(WhenQueued, 1), None);
        assert_eq!(skip_reason(Always, 0), None);
        assert_eq!(skip_reason(Never, 3), Some(SkipReason::Never));
    }

    #[test]
    fn a_skip_says_why_and_keeps_head_and_base() {
        let mut report = passed(&[]);
        report.result = PreflightResult::Skipped;
        report.skip_reason = Some(SkipReason::NoQueue);
        report.env = None;
        let text = render(&report);
        assert!(text.starts_with("생략: 함께 합칠 PR이 큐에 없어"), "{text}");
        assert!(text.contains("batch가 병합 전에 검증한다"));
        assert!(text.contains("HEAD 0123456789ab · 기준 fedcba987654"));
        assert!(!text.contains("검사 환경"));
    }

    #[test]
    fn skipped_prs_and_flaky_steps_are_listed() {
        let mut report = passed(&[]);
        report.skipped.push(skip(PrNumber(5), Why::Conflict, None));
        report
            .skipped
            .push(skip(PrNumber(6), Why::FetchFailed, Some("없다".into())));
        report.flaky.push(FlakyReport {
            step: "test".into(),
            tests: vec!["a::b".into()],
        });
        let text = render(&report);
        assert!(text.contains("건너뛴 PR #5: 앞의 큐 항목과 충돌한다\n"));
        assert!(text.contains("건너뛴 PR #6: HEAD를 가져오지 못했다: 없다\n"));
        assert!(text.contains("흔들린 단계: test (시험 a::b)"));
    }

    #[test]
    fn a_failure_prints_its_summary_and_report_path() {
        let mut report = passed(&[]);
        report.result = PreflightResult::Fail;
        report.summary = Some("검증 실패(단독): test 단계".into());
        report.report = Some(PathBuf::from("/s/reports/commit-x/1.md"));
        let text = render(&report);
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("검증 실패(단독): test 단계"));
        assert_eq!(lines.next(), Some("보고서: /s/reports/commit-x/1.md"));
    }
}
