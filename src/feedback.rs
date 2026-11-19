use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::env::{self, EnvSection};
use crate::ext::{Ext, Output};
use crate::gitops::FAILED_REF_PREFIX;
use crate::hub::{ForwardErrorKind, exec_on_input};
use crate::machine::{self, Machine};
use crate::model::{
    Agent, BaseCheck, ConflictFile, EpochMs, FailedCheck, Failure, FailureRecord, Partner,
    PrNumber, QueueItem, RepoInfo, RunOutcome, Sha, Slug, SoloEvidence, StepFailure, TerminalId,
    VerifyMode,
};
use crate::repo;
use crate::store::RepoStore;

mod parcel;

pub use parcel::{Limits, PARCEL_LIMIT, Parcel};
use parcel::{ParcelNotice, Rebuild};

pub(crate) const HC_NOT_FOUND: i32 = 3;
pub(crate) const HC_BLOCKED: i32 = 9;
pub(crate) const HC_FINISHED: i32 = 11;

const LOG_TAIL_BYTES: u64 = 16 * 1024;
const LOG_TAIL_LINES: usize = 40;
const LISTED_FILES: usize = 30;
const LISTED_TESTS: usize = 50;
const LISTED_COMMITS: usize = 10;
const HC_NAME_LIMIT: usize = 32;
const NAME_TRIES: u32 = 5;
const SIBLING_TRIES: u32 = 20;
const SHELLS: [&str; 15] = [
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "tcsh",
    "csh",
    "nu",
    "ash",
    "rc",
    "elvish",
    "xonsh",
    "pwsh",
    "powershell",
];
pub(crate) const PLAIN_MARKS: &str = ".,:/_-+=@%";
const PARTNER_SEARCH_DEPTH: usize = 1000;
const REMOTE_DELIVER_TIMEOUT: Duration = Duration::from_secs(180);

pub fn worktrees_root(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf> {
    var("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".herdr/worktrees"))
        .ok_or_else(|| anyhow!("HOME이 없어 새 worktree를 둘 곳을 정하지 못했다"))
}

#[derive(Clone, Copy)]
pub struct Feedback<'a> {
    pub ext: &'a Ext,
    pub repo: &'a RepoInfo,
    pub store: &'a RepoStore,
    pub config: &'a Config,
    pub worktrees_root: &'a Path,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Subject {
    Pr {
        pr: PrNumber,
        title: String,
        branch: String,
        head: Sha,
    },
    Commit {
        commit: Sha,
    },
}

impl From<&QueueItem> for Subject {
    fn from(item: &QueueItem) -> Subject {
        Subject::Pr {
            pr: item.pr,
            title: item.title.clone(),
            branch: item.branch.clone(),
            head: item.head.clone(),
        }
    }
}

impl Subject {
    fn this(&self) -> &'static str {
        match self {
            Subject::Pr { .. } => "이 PR",
            Subject::Commit { .. } => "이 커밋",
        }
    }

    fn label(&self) -> String {
        match self {
            Subject::Pr { pr, .. } => format!("PR {pr}"),
            Subject::Commit { commit } => format!("커밋 {}", short(commit)),
        }
    }

    fn key(&self) -> String {
        match self {
            Subject::Pr { pr, .. } => format!("pr{}", pr.0),
            Subject::Commit { commit } => short(commit).to_owned(),
        }
    }
}

fn short(sha: &Sha) -> &str {
    &sha.as_str()[..sha.as_str().len().min(12)]
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportDoc {
    pub seq: u32,
    pub at_ms: EpochMs,
    pub subject: Subject,
    pub summary: String,
    pub failure: Failure,
    pub step: Option<StepReport>,
    pub next: Vec<String>,
    #[serde(default)]
    pub env: Option<EnvSection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepReport {
    pub name: String,
    pub command: String,
    pub timeout_s: u64,
    pub log_tail: Option<String>,
    pub tree: Sha,
    pub git_ref: Option<String>,
    pub repro: Vec<String>,
    pub cleanup: String,
}

#[derive(Clone, Debug)]
pub struct Written {
    pub md: PathBuf,
    pub json: PathBuf,
    pub doc: ReportDoc,
}

impl Written {
    pub fn record(&self) -> FailureRecord {
        FailureRecord {
            failure: self.doc.failure.clone(),
            summary: self.doc.summary.clone(),
            report: self.md.clone(),
            at_ms: self.doc.at_ms,
        }
    }
}

enum Cause<'a> {
    Conflict(&'a [ConflictFile]),
    Step {
        timed_out: bool,
        step: &'a StepFailure,
    },
    Ci(&'a [FailedCheck]),
}

impl<'a> Cause<'a> {
    fn of(failure: &'a Failure) -> Result<Cause<'a>> {
        match failure {
            Failure::Conflict { files } => Ok(Cause::Conflict(files)),
            Failure::Verify(step) => Ok(Cause::Step {
                timed_out: false,
                step,
            }),
            Failure::Timeout(step) => Ok(Cause::Step {
                timed_out: true,
                step,
            }),
            Failure::Ci { checks } => Ok(Cause::Ci(checks)),
        }
    }
}

impl Feedback<'_> {
    pub fn write_report(
        &self,
        subject: &Subject,
        failure: &Failure,
        failed_ref: Option<&str>,
    ) -> Result<Written> {
        self.write_report_with_env(subject, failure, failed_ref, None)
    }

    pub fn write_report_with_env(
        &self,
        subject: &Subject,
        failure: &Failure,
        failed_ref: Option<&str>,
        env: Option<EnvSection>,
    ) -> Result<Written> {
        self.write_report_at(subject, failure, failed_ref, env, Placement::Here)
    }

    fn write_report_at(
        &self,
        subject: &Subject,
        failure: &Failure,
        failed_ref: Option<&str>,
        env: Option<EnvSection>,
        placement: Placement<'_>,
    ) -> Result<Written> {
        let cause = Cause::of(failure)?;
        let step = match &cause {
            Cause::Step { step, timed_out } => {
                Some(self.step_report(subject, step, failed_ref, *timed_out, placement)?)
            }
            Cause::Conflict(_) | Cause::Ci(_) => None,
        };
        let dir = self.report_dir(subject);
        fs::create_dir_all(&dir).with_context(|| format!("만들지 못했다: {}", dir.display()))?;
        let (seq, md) = claim_sequence(&dir)?;
        let json = dir.join(format!("{seq}.json"));
        let write = || -> Result<ReportDoc> {
            let doc = ReportDoc {
                seq,
                at_ms: EpochMs::now(),
                subject: subject.clone(),
                summary: summarize(&cause),
                next: next_steps(
                    subject,
                    &cause,
                    &self.repo.remote_name,
                    &self.repo.default_branch,
                    self.config.require_head,
                ),
                step,
                failure: failure.clone(),
                env,
            };
            let mut text = serde_json::to_string_pretty(&doc)?;
            text.push('\n');
            write_file(&json, &text)?;
            write_file(&md, render(&doc, &cause))?;
            Ok(doc)
        };
        match write() {
            Ok(doc) => Ok(Written { md, json, doc }),
            Err(e) => {
                let _ = fs::remove_file(&md);
                let _ = fs::remove_file(&json);
                Err(e)
            }
        }
    }

    pub fn partner_or_placeholder(
        &self,
        subject: &Subject,
        tree: &Sha,
        pr: PrNumber,
        title: &str,
    ) -> Result<Partner> {
        let dir = self.report_dir(subject);
        fs::create_dir_all(&dir).with_context(|| format!("만들지 못했다: {}", dir.display()))?;
        let diff = dir.join(format!("partner-pr{}-{}.diff", pr.0, short(tree)));
        let (files, text) = match self.merge_change(tree, pr) {
            Ok(change) => change,
            Err(e) => (
                Vec::new(),
                format!("diff를 만들지 못했다: {e:#}\n").into_bytes(),
            ),
        };
        write_file(&diff, text)?;
        Ok(Partner {
            pr,
            title: title.to_owned(),
            files,
            diff,
        })
    }

    fn failed_ref_at(&self, name: &str, tree: &Sha) -> Result<String> {
        if !plain_ref_name(name) {
            bail!("실패한 트리 참조 이름이 아니다: {name:?}");
        }
        let full = if name.starts_with("refs/") {
            name.to_owned()
        } else {
            format!("{FAILED_REF_PREFIX}{name}")
        };
        let out = self.ext.git(
            &self.repo.path,
            [
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{full}^{{commit}}"),
            ],
        )?;
        if out.success() && out.stdout_text().trim().starts_with(tree.as_str()) {
            Ok(full)
        } else {
            bail!("실패한 트리 참조 {full}이 커밋 {tree}를 가리키지 않는다")
        }
    }

    fn report_dir(&self, subject: &Subject) -> PathBuf {
        let dir = match subject {
            Subject::Pr { pr, .. } => self.store.report_dir(*pr),
            Subject::Commit { commit } => self.store.commit_report_dir(short(commit)),
        };
        std::path::absolute(&dir).unwrap_or(dir)
    }

    fn step_report(
        &self,
        subject: &Subject,
        step: &StepFailure,
        failed_ref: Option<&str>,
        timed_out: bool,
        placement: Placement<'_>,
    ) -> Result<StepReport> {
        let configured = self
            .config
            .verify
            .iter()
            .find(|s| s.name == step.step)
            .ok_or_else(|| anyhow!("chute.toml에 검증 단계 {:?}이 없다", step.step))?;
        let (key, tree) = (subject.key(), short(&step.tree));
        let slug = self.repo.slug.dir_name();
        let id = if key == tree {
            format!("{slug}-{key}")
        } else {
            format!("{slug}-{key}-{tree}")
        };
        let dir = format!("\"${{TMPDIR:-/tmp}}/chute-repro-{id}\"");
        let repo = shell_word(&self.repo.path.to_string_lossy());
        let failed_ref = failed_ref
            .map(|name| self.failed_ref_at(name, &step.tree))
            .transpose()?;
        let run_id: String = id
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let limit = if timed_out {
            format!("timeout -k 5 {} ", configured.timeout.as_secs())
        } else {
            String::new()
        };
        let run = format!(
            "CHUTE_RUN_ID=repro_{run_id}_$$_$(date +%s) {limit}sh -c {}",
            shell_word(&configured.run)
        );
        let git = format!("git -c core.hooksPath=/dev/null -C {repo}");
        let remote = &self.repo.remote_name;
        let at_tree = format!(
            "{git} worktree add --detach {dir} {} && (cd {dir} && {run})",
            step.tree
        );
        let repro = match (placement, &failed_ref) {
            (Placement::Here, _) | (Placement::Delivered { .. }, Some(_)) => vec![at_tree],
            (
                Placement::Delivered {
                    rebuild: Some(rebuild),
                },
                None,
            ) => {
                let merges: String = rebuild
                    .heads
                    .iter()
                    .map(|head| format!("git merge --no-ff --no-edit {head} && "))
                    .collect();
                vec![
                    format!(
                        "# 허브의 실패한 트리를 이 머신에 가져오지 못했다. {remote}에서 같은 트리를 만든다"
                    ),
                    format!("{git} fetch {remote}"),
                    format!(
                        "{git} worktree add --detach {dir} {} && (cd {dir} && {merges}{run})",
                        rebuild.base
                    ),
                ]
            }
            (Placement::Delivered { rebuild: None }, None) => vec![
                format!(
                    "# 허브의 실패한 트리를 이 머신에 가져오지 못했고 {remote}에서 만드는 방법도 알지 못했다. 아래 커밋은 허브에만 있을 수 있다"
                ),
                at_tree,
            ],
        };
        let log = match placement {
            Placement::Here => step.log.clone(),
            Placement::Delivered { .. } => self.report_dir(subject).join(&step.log),
        };
        Ok(StepReport {
            name: step.step.clone(),
            command: configured.run.clone(),
            timeout_s: configured.timeout.as_secs(),
            log_tail: step.failed_tests.is_empty().then(|| log_tail(&log)),
            tree: step.tree.clone(),
            git_ref: failed_ref.clone(),
            repro,
            cleanup: format!("git -C {repo} worktree remove --force {dir}"),
        })
    }

    fn merge_change(&self, tree: &Sha, pr: PrNumber) -> Result<(Vec<String>, Vec<u8>)> {
        let git = |args: &[&str]| -> Result<String> {
            Ok(self.ext.git(&self.repo.path, args.iter().copied())?.ok()?)
        };
        let log = git(&[
            "log",
            "--first-parent",
            "--format=%H\t%s",
            "-n",
            &PARTNER_SEARCH_DEPTH.to_string(),
            tree.as_str(),
        ])?;
        let prefix = github_merge_subject_prefix(pr);
        let merge = log
            .lines()
            .find_map(|line| {
                let (sha, subject) = line.split_once('\t')?;
                subject.starts_with(&prefix).then_some(sha)
            })
            .ok_or_else(|| anyhow!("{tree}의 이력에서 {pr}의 머지 커밋을 찾지 못했다"))?;
        let parent = format!("{merge}^1");
        let files = git(&[
            "diff",
            "--name-only",
            "-z",
            "--no-ext-diff",
            "--no-renames",
            &parent,
            merge,
        ])?;
        let files: Vec<String> = files
            .split('\0')
            .filter(|f| !f.is_empty())
            .map(str::to_owned)
            .collect();
        let diff = self.ext.git(
            &self.repo.path,
            [
                "-c",
                "core.quotePath=false",
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                &parent,
                merge,
            ],
        );
        let text = match diff {
            Err(e) => format!("diff를 만들지 못했다: {e}\n").into_bytes(),
            Ok(diff) if !diff.success() => format!(
                "diff를 만들지 못했다: git이 실패했다({}). {}\n",
                exit_text(diff.code),
                diff.stderr_text().trim()
            )
            .replace(". \n", ".\n")
            .into_bytes(),
            Ok(diff) if diff.stdout.is_empty() => {
                "이 머지 커밋은 바뀐 파일이 없다\n".as_bytes().to_vec()
            }
            Ok(diff) => diff.stdout,
        };
        Ok((files, text))
    }
}

fn plain_ref_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
}

#[derive(Clone, Copy)]
enum Placement<'a> {
    Here,
    Delivered { rebuild: Option<&'a Rebuild> },
}

fn github_merge_subject_prefix(pr: PrNumber) -> String {
    format!("Merge pull request {pr} from ")
}

fn claim_sequence(dir: &Path) -> Result<(u32, PathBuf)> {
    let exhausted = || anyhow!("보고서 번호가 끝에 닿았다: {}", dir.display());
    let mut next = highest_sequence(dir)?
        .checked_add(1)
        .ok_or_else(exhausted)?;
    loop {
        let md = dir.join(format!("{next}.md"));
        match OpenOptions::new().write(true).create_new(true).open(&md) {
            Ok(_) => return Ok((next, md)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                next = next.checked_add(1).ok_or_else(exhausted)?
            }
            Err(e) => return Err(e).with_context(|| format!("만들지 못했다: {}", md.display())),
        }
    }
}

fn highest_sequence(dir: &Path) -> Result<u32> {
    let mut highest = 0;
    for entry in fs::read_dir(dir).with_context(|| format!("읽지 못했다: {}", dir.display()))?
    {
        let path = entry?.path();
        if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("md" | "json")
        ) && let Some(seq) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        {
            highest = highest.max(seq);
        }
    }
    Ok(highest)
}

fn write_file(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("쓰지 못했다: {}", path.display()))
}

fn log_tail(path: &Path) -> String {
    let read = || -> io::Result<String> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let start = len.saturating_sub(LOG_TAIL_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let text = strip_terminal_noise(&String::from_utf8_lossy(&bytes));
        let mut lines: Vec<&str> = text.trim_end().lines().collect();
        if start > 0 && lines.len() > 1 {
            lines.remove(0);
        }
        let from = lines.len().saturating_sub(LOG_TAIL_LINES);
        let tail = lines[from..].join("\n");
        Ok(match tail.trim_end() {
            "" => "(로그가 비어 있다)".to_owned(),
            tail => tail.to_owned(),
        })
    };
    read().unwrap_or_else(|e| format!("(로그를 읽지 못했다: {e})"))
}

fn strip_terminal_noise(text: &str) -> String {
    const CSI: &str = r"\x1b\[[0-?]*[ -/]*[@-~]";
    const OSC: &str = r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)";
    const CHARSET: &str = r"\x1b[()][0-9A-Za-z]";
    static ESCAPES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!("{CSI}|{OSC}|{CHARSET}")).expect("valid regex"));
    let lines = text.replace("\r\n", "\n").replace('\r', "\n");
    ESCAPES
        .replace_all(&lines, "")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:=@%+-".contains(&b));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn one_line(text: &str) -> String {
    strip_terminal_noise(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn longest_backtick_run(text: &str) -> usize {
    text.split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default()
}

fn fenced(out: &mut Vec<String>, lang: &str, text: &str) {
    let fence = fence_for(text);
    out.push(format!("{fence}{lang}"));
    out.push(text.to_owned());
    out.push(fence);
}

fn fence_for(text: &str) -> String {
    "`".repeat((longest_backtick_run(text) + 1).max(3))
}

fn code_span(text: &str) -> String {
    let text: String = strip_terminal_noise(text)
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let longest = longest_backtick_run(&text);
    let ticks = "`".repeat(longest + 1);
    if longest == 0 && !text.starts_with(' ') && !text.ends_with(' ') {
        format!("{ticks}{text}{ticks}")
    } else {
        format!("{ticks} {text} {ticks}")
    }
}

fn mode_label(mode: &VerifyMode) -> String {
    match mode {
        VerifyMode::Solo => "단독".to_owned(),
        VerifyMode::Combo { partners } => {
            let prs: Vec<String> = partners.iter().map(|p| p.pr.to_string()).collect();
            format!("조합, 상대 PR {}", prs.join(", "))
        }
    }
}

fn file_names(files: &[ConflictFile]) -> String {
    let shown: Vec<String> = files.iter().take(3).map(|f| one_line(&f.path)).collect();
    match files.len().saturating_sub(shown.len()) {
        0 => shown.join(", "),
        more => format!("{} 외 {more}개", shown.join(", ")),
    }
}

fn check_names(checks: &[FailedCheck]) -> String {
    let shown: Vec<String> = checks.iter().take(3).map(|c| code_span(&c.name)).collect();
    match checks.len().saturating_sub(shown.len()) {
        0 => shown.join(", "),
        more => format!("{} 외 {more}개", shown.join(", ")),
    }
}

fn summarize(cause: &Cause) -> String {
    match cause {
        Cause::Conflict(files) => format!("충돌: {}", file_names(files)),
        Cause::Ci(checks) => format!("원격 CI 실패: {}", check_names(checks)),
        Cause::Step { timed_out, step } => {
            let what = if *timed_out {
                "시간 초과"
            } else {
                "검증 실패"
            };
            let mut text = format!(
                "{what}({}): {} 단계",
                mode_label(&step.mode),
                one_line(&step.step)
            );
            if !step.failed_tests.is_empty() {
                text.push_str(&format!(", 실패한 시험 {}개", step.failed_tests.len()));
            }
            text
        }
    }
}

fn next_steps(
    subject: &Subject,
    cause: &Cause,
    remote: &str,
    default_branch: &str,
    require_head: bool,
) -> Vec<String> {
    let this = subject.this();
    let merge =
        format!("`git fetch {remote}`, `git merge {remote}/{default_branch}` 순서로 실행해");
    let (fix, give_up) = match subject {
        Subject::Pr { .. } if require_head => (
            "고쳐서 커밋하고 push한 뒤 PR 번호와 새 HEAD를 보고한다. 이 저장소는 `require_head = true`라 submit하지 않는다.",
            "못 풀겠으면 `chute giveup --reason \"<이유>\"`를 실행한다. `--reason` 값에 막힌 까닭을 따옴표로 묶어 적는다. PR이 큐에서 빠지고 submit한 터미널에 알린다.",
        ),
        Subject::Pr { .. } => (
            "고쳐서 커밋하고 push한 뒤 `chute submit`을 실행한다.",
            "못 풀겠으면 `chute giveup --reason \"<이유>\"`를 실행한다. `--reason` 값에 막힌 까닭을 따옴표로 묶어 적는다. PR이 큐에서 빠지고 submit한 터미널에 알린다.",
        ),
        Subject::Commit { .. } if require_head => (
            "고쳐서 커밋한 뒤 `chute preflight`를 다시 실행한다. 통과하면 push하고 PR 번호와 새 HEAD를 보고한다. 이 저장소는 `require_head = true`라 submit하지 않는다.",
            "못 풀겠으면 하던 일을 멈추고 사용자에게 이유를 알린다.",
        ),
        Subject::Commit { .. } => (
            "고쳐서 커밋한 뒤 `chute preflight`를 다시 실행한다. 통과하면 push하고 `chute submit`을 실행한다.",
            "못 풀겠으면 하던 일을 멈추고 사용자에게 이유를 알린다.",
        ),
    };
    let mut steps = match cause {
        Cause::Conflict(_) => vec![format!(
            "{merge} 기준 브랜치를 합치고 충돌한 파일을 푼다. 충돌 상대가 아직 머지되지 않은 큐의 PR이면 그 PR이 머지된 뒤에 합쳐도 된다."
        )],
        Cause::Ci(_) => vec![
            "보고서의 링크에서 실패한 check의 로그를 보고 원인을 찾는다. 로컬 검증이 아니라 GitHub의 check가 실패한 것이라 재현 명령이 없다.".to_owned(),
        ],
        Cause::Step { timed_out, step } => {
            let mut steps =
                vec!["보고서의 재현 방법으로 같은 단계를 돌려 실패를 직접 확인한다.".to_owned()];
            if *timed_out {
                steps.push("시간 제한은 `chute.toml`의 `timeout` 값이다. 값을 늘려서 풀지 않는다. 로그 끝부분에서 멈춘 곳을 찾는다.".to_owned());
            }
            if let VerifyMode::Combo { .. } = step.mode {
                steps.push(format!(
                    "보고서의 diff 파일에서 상대 PR의 변경과 {this}의 변경이 어디서 어긋나는지 찾아 {this}을 맞춘다. 상대 PR이 이미 머지됐으면 {merge} 합친 뒤 확인한다."
                ));
            }
            steps
        }
    };
    steps.push(fix.to_owned());
    steps.push(give_up.to_owned());
    steps
}

fn render(doc: &ReportDoc, cause: &Cause) -> String {
    let mut out: Vec<String> = Vec::new();
    let label = doc.subject.label();
    match &doc.subject {
        Subject::Pr {
            title,
            branch,
            head,
            ..
        } => {
            out.push(format!("# {label} 반송 보고서: {}", one_line(title)));
            out.push(String::new());
            out.push(format!("- 대상: {label}, 브랜치 {}", code_span(branch)));
            out.push(format!("- 등록한 HEAD: `{head}`"));
        }
        Subject::Commit { commit } => {
            out.push(format!("# {label} 검증 보고서"));
            out.push(String::new());
            out.push(format!("- 대상: 커밋 `{commit}` (PR 없음, preflight)"));
        }
    }
    out.push(format!("- 결과: {}", doc.summary));
    out.push(format!("- 보고서 번호: {}", doc.seq));
    out.push(String::new());
    out.push("이 파일만 읽고 고칠 수 있게 썼다. 같은 내용이 `<번호>.json`에도 있다.".to_owned());
    out.push(String::new());

    match (cause, &doc.step) {
        (Cause::Conflict(files), _) => render_conflict(&mut out, files),
        (Cause::Step { timed_out, step }, Some(report)) => {
            render_step(&mut out, &doc.subject, step, report, *timed_out)
        }
        (Cause::Step { .. }, None) => {}
        (Cause::Ci(checks), _) => render_ci(&mut out, checks),
    }

    if let Some(section) = &doc.env {
        render_env(&mut out, section);
    }

    out.push("## 다음에 할 일".to_owned());
    out.push(String::new());
    for (i, step) in doc.next.iter().enumerate() {
        out.push(format!("{}. {step}", i + 1));
    }
    out.push(String::new());
    out.join("\n")
}

fn render_env(out: &mut Vec<String>, section: &EnvSection) {
    out.push("## 검사 환경".to_owned());
    out.push(String::new());
    let hub = &section.env;
    out.push(format!("- 머신: {}", code_span(&env::machine_line(hub))));
    for check in &hub.checks {
        out.push(format!(
            "- {}: {}",
            code_span(&check.run),
            code_span(&env::result_text(check))
        ));
    }
    match &section.preflight {
        None => {}
        Some(preflight) => {
            let rows = env::differences(preflight, hub);
            out.push(String::new());
            if rows.is_empty() {
                out.push("preflight와 같은 환경이다.".to_owned());
            } else {
                out.push(
                    "같은 HEAD의 preflight와 다른 줄만 나란히 둔다. 없는 쪽은 그 명령을 돌리지 않았다.".to_owned(),
                );
                out.push(String::new());
                out.push("| 항목 | preflight를 돌린 PC | 허브 |".to_owned());
                out.push("| --- | --- | --- |".to_owned());
                let cell = |text: &Option<String>| match text {
                    Some(text) => code_span(text).replace('|', "\\|"),
                    None => "(없음)".to_owned(),
                };
                for row in &rows {
                    out.push(format!(
                        "| {} | {} | {} |",
                        code_span(&row.what).replace('|', "\\|"),
                        cell(&row.preflight),
                        cell(&row.hub)
                    ));
                }
            }
        }
    }
    out.push(String::new());
}

fn beside_report(path: &Path) -> &'static str {
    if path.is_relative() {
        " (이 보고서와 같은 폴더)"
    } else {
        ""
    }
}

fn render_ci(out: &mut Vec<String>, checks: &[FailedCheck]) {
    out.push("## 실패한 check".to_owned());
    out.push(String::new());
    out.push(
        "GitHub가 이 PR의 등록한 HEAD에서 실패하거나 취소됐다고 알린 check다. 합친 트리의 로컬 검증은 이 PR에 돌지 않았다."
            .to_owned(),
    );
    out.push(String::new());
    for check in checks.iter().take(LISTED_FILES) {
        let link = if check.link.is_empty() {
            "링크 없음".to_owned()
        } else {
            code_span(&check.link)
        };
        out.push(format!("- {}: {link}", code_span(&check.name)));
    }
    if checks.len() > LISTED_FILES {
        out.push(format!(
            "- …외 {}개. 전부 `.json`에 있다",
            checks.len() - LISTED_FILES
        ));
    }
    out.push(String::new());
}

fn render_conflict(out: &mut Vec<String>, files: &[ConflictFile]) {
    out.push("## 충돌한 파일".to_owned());
    out.push(String::new());
    for file in files.iter().take(LISTED_FILES) {
        out.push(format!("- {}", code_span(&file.path)));
        if file.main_commits.is_empty() {
            out.push("  - 이 파일을 고친 쪽의 커밋을 찾지 못했다".to_owned());
        }
        for commit in file.main_commits.iter().take(LISTED_COMMITS) {
            out.push(format!(
                "  - 충돌 상대가 고친 커밋: `{}` {}",
                short(&commit.sha),
                one_line(&commit.title)
            ));
        }
        if file.main_commits.len() > LISTED_COMMITS {
            out.push(format!(
                "  - …외 {}개. 전부 `.json`에 있다",
                file.main_commits.len() - LISTED_COMMITS
            ));
        }
    }
    if files.len() > LISTED_FILES {
        out.push(format!(
            "- …외 {}개. 전부 `.json`에 있다",
            files.len() - LISTED_FILES
        ));
    }
    out.push(String::new());
}

/// `{result}({N}번 돌려 {breakdown})`. A report without per-run outcomes says every run ended
/// the way the last one did. Old evidence with no runs at all has nothing to break down, so it
/// says only how the step ended, with no parenthesis.
fn solo_runs(evidence: &SoloEvidence, timed_out: bool) -> String {
    let legacy;
    let outcomes: &[RunOutcome] = if evidence.run_outcomes.is_empty() {
        let last = if timed_out {
            RunOutcome::TimedOut
        } else {
            RunOutcome::Failed
        };
        legacy = vec![last; evidence.runs as usize];
        &legacy
    } else {
        &evidence.run_outcomes
    };
    let count = |kind: RunOutcome| outcomes.iter().filter(|o| **o == kind).count();
    let (failed, timeouts) = (count(RunOutcome::Failed), count(RunOutcome::TimedOut));
    let result = match (failed, timeouts) {
        (0, 0) if outcomes.is_empty() && timed_out => "시간 초과로 끝났다",
        (_, 0) => "실패했다",
        (0, _) => "시간 초과로 끝났다",
        _ => "통과하지 못했다",
    };
    let parts: Vec<String> = [
        (failed, "번 실패"),
        (timeouts, "번 시간 초과"),
        (count(RunOutcome::Passed), "번 통과"),
        (
            count(RunOutcome::NotRun),
            "번은 앞 단계가 실패해 이 단계를 돌리지 않았다",
        ),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n}{what}"))
    .collect();
    if outcomes.is_empty() {
        return result.to_owned();
    }
    format!("{result}({}번 돌려 {})", outcomes.len(), parts.join(", "))
}

fn render_solo_scope(
    out: &mut Vec<String>,
    this: &str,
    evidence: Option<&SoloEvidence>,
    timed_out: bool,
) {
    let Some(evidence) = evidence else {
        out.push(format!(
            "- 범위: 단독. 기준 브랜치에 {this}만 얹은 트리에서 같은 단계가 실패했다"
        ));
        return;
    };
    out.push(format!(
        "- 범위: 단독. 기준 브랜치에 {this}만 얹은 트리에서 {}",
        solo_runs(evidence, timed_out)
    ));
    out.push(match evidence.base {
        Some(BaseCheck::RanInThisBatch) => "- 기준 브랜치: 이 batch에서 돌려 통과했다".to_owned(),
        Some(BaseCheck::VerifiedEarlier) => {
            "- 기준 브랜치: chute가 앞서 검증한 커밋이라 이 batch에서는 돌리지 않았다".to_owned()
        }
        Some(BaseCheck::PushedSkippedByPaths) => "- 기준 브랜치: 이 batch가 push한 커밋이다. 그 검증은 변경 범위(`paths`)에 들지 않아 이 단계를 건너뛰었다".to_owned(),
        Some(BaseCheck::PushedReused) => "- 기준 브랜치: 이 batch가 push한 커밋이다. 같은 트리를 앞서 검증한 기록이 있어 이 batch에서는 돌리지 않았다".to_owned(),
        None => "- 기준 브랜치: 확인하지 않았다".to_owned(),
    });
    if evidence.attempts.iter().all(Vec::is_empty) {
        out.push("- 장부: 실패한 시험 이름을 뽑지 못해 확인하지 않았다".to_owned());
    } else if evidence.ledger_tests.is_empty() {
        out.push(format!(
            "- 장부: 최근 {}일 장부에 이 단계의 흔들림 기록이 있는 실패 시험은 없다",
            evidence.ledger_days
        ));
    } else {
        let names: Vec<String> = evidence.ledger_tests.iter().map(|t| code_span(t)).collect();
        out.push(format!(
            "- 장부: 최근 {}일 장부에 이 단계의 흔들림 기록이 있는 실패 시험: {}",
            evidence.ledger_days,
            names.join(", ")
        ));
    }
    if evidence.attempts.len() < 2 {
        return;
    }
    if evidence.attempts.iter().any(Vec::is_empty) {
        out.push(format!(
            "- 시도별 실패 시험({n}번 시도): 시험 이름을 뽑지 못한 시도가 있어 서로 비교하지 못했다",
            n = evidence.attempts.len()
        ));
        return;
    }
    let same = evidence.attempts.windows(2).all(|pair| {
        let later: HashSet<&String> = pair[1].iter().collect();
        pair[0].len() == pair[1].len() && pair[0].iter().all(|t| later.contains(t))
    });
    if same {
        out.push(format!(
            "- 시도별 실패 시험({}번 시도): 모든 시도에서 같았다",
            evidence.attempts.len()
        ));
        return;
    }
    out.push(format!(
        "- 시도별 실패 시험({}번 시도): 서로 달랐다",
        evidence.attempts.len()
    ));
    for (i, tests) in evidence.attempts.iter().enumerate() {
        let names: Vec<String> = tests.iter().map(|t| code_span(t)).collect();
        out.push(format!("  - {}번째 시도: {}", i + 1, names.join(", ")));
    }
}

fn render_step(
    out: &mut Vec<String>,
    subject: &Subject,
    step: &StepFailure,
    report: &StepReport,
    timed_out: bool,
) {
    let this = subject.this();
    out.push("## 실패한 단계".to_owned());
    out.push(String::new());
    out.push(format!("- 단계: {}", code_span(&report.name)));
    let command_fits_inline = one_line(&report.command) == report.command;
    if command_fits_inline {
        out.push(format!(
            "- 명령: {} (검증 worktree에서 `sh -c`로 실행한다)",
            code_span(&report.command)
        ));
    } else {
        out.push("- 명령: 아래 블록과 같다(검증 worktree에서 `sh -c`로 실행한다)".to_owned());
    }
    if timed_out {
        out.push(format!(
            "- 결과: **시간 초과**. {}초 안에 끝나지 않아 chute가 멈췄다",
            report.timeout_s
        ));
    } else {
        out.push("- 결과: 실패".to_owned());
    }
    match &step.mode {
        VerifyMode::Solo => render_solo_scope(out, this, step.evidence.as_deref(), timed_out),
        VerifyMode::Combo { .. } => out.push(format!(
            "- 범위: 조합. 기준 브랜치에 {this}만 얹으면 통과하지만, 아래 PR과 함께이면 실패한다"
        )),
    }
    out.push(format!(
        "- 로그: {}{}",
        code_span(&step.log.display().to_string()),
        beside_report(&step.log)
    ));
    out.push(String::new());
    if !command_fits_inline {
        fenced(out, "sh", &report.command);
        out.push(String::new());
    }

    if let Some(tail) = &report.log_tail {
        out.push("실패한 시험 이름을 뽑지 못했다. 로그 끝부분이다.".to_owned());
        out.push(String::new());
        fenced(out, "text", tail);
    } else {
        out.push(format!("### 실패한 시험 ({}개)", step.failed_tests.len()));
        out.push(String::new());
        for name in step.failed_tests.iter().take(LISTED_TESTS) {
            out.push(format!("- {}", code_span(name)));
        }
        if step.failed_tests.len() > LISTED_TESTS {
            out.push(format!(
                "- …외 {}개. 전부 `.json`에 있다",
                step.failed_tests.len() - LISTED_TESTS
            ));
        }
    }
    out.push(String::new());

    if let VerifyMode::Combo { partners } = &step.mode {
        out.push("## 함께 묶였던 PR".to_owned());
        out.push(String::new());
        for partner in partners.iter() {
            out.push(format!("### {} {}", partner.pr, one_line(&partner.title)));
            out.push(String::new());
            let mut files: Vec<String> = partner
                .files
                .iter()
                .take(LISTED_FILES)
                .map(|f| code_span(f))
                .collect();
            if partner.files.len() > LISTED_FILES {
                files.push(format!("…외 {}개", partner.files.len() - LISTED_FILES));
            }
            if files.is_empty() {
                out.push("- 바뀐 파일: 알 수 없다. 사정은 diff 파일에 있다".to_owned());
            } else {
                out.push(format!("- 바뀐 파일: {}", files.join(", ")));
            }
            out.push(format!(
                "- diff: {}{}",
                code_span(&partner.diff.display().to_string()),
                beside_report(&partner.diff)
            ));
            out.push(String::new());
        }
    }

    out.push("## 재현".to_owned());
    out.push(String::new());
    match &report.git_ref {
        Some(git_ref) => out.push(format!(
            "실패한 트리: 커밋 `{}`, 참조 {}",
            report.tree,
            code_span(git_ref)
        )),
        None => out.push(format!("실패한 트리: 커밋 `{}`", report.tree)),
    }
    out.push(String::new());
    out.push("저장소의 아무 worktree에서 실행한다.".to_owned());
    out.push(String::new());
    fenced(out, "sh", &report.repro.join("\n"));
    out.push(String::new());
    out.push("다 쓴 뒤 만든 worktree를 지운다.".to_owned());
    out.push(String::new());
    fenced(out, "sh", &report.cleanup);
    out.push(String::new());
}

#[derive(Clone, Copy, Debug)]
pub enum Notice<'a> {
    Bounced(&'a Written),
    HeadChanged {
        registered: &'a Sha,
        current: &'a Sha,
    },
}

impl Notice<'_> {
    fn line(&self, item: &QueueItem, require_head: bool) -> String {
        let label = format!("PR {}", item.pr);
        match self {
            Notice::Bounced(written) => {
                let steps: Vec<String> = written
                    .doc
                    .next
                    .iter()
                    .enumerate()
                    .map(|(i, step)| format!("{}. {step}", i + 1))
                    .collect();
                format!(
                    "{} {}{}",
                    without_shell_syntax(&format!(
                        "chute 반송: {label}. {}. 보고서:",
                        line_summary(&written.doc)
                    )),
                    shell_word(&written.md.to_string_lossy()),
                    without_shell_syntax(&format!(". 할 일: {}", steps.join(" ")))
                )
            }
            Notice::HeadChanged {
                registered,
                current,
            } => {
                let again = if require_head {
                    "고칠 것이 끝났으면 PR 번호와 새 HEAD를 보고한다."
                } else {
                    "고칠 것이 끝났으면 `chute submit`을 다시 실행한다."
                };
                without_shell_syntax(&format!(
                    "chute 알림: {label}의 HEAD가 바뀌었다. 등록한 HEAD: {}, 지금 HEAD: {}. 머지는 등록한 HEAD만 한다. {again}",
                    short(registered),
                    short(current)
                ))
            }
        }
    }
}

fn line_summary(doc: &ReportDoc) -> String {
    match &doc.failure {
        Failure::Conflict { files } => format!("충돌: 파일 {}개", files.len()),
        Failure::Ci { checks } => format!("원격 CI 실패: check {}개", checks.len()),
        _ => doc.summary.clone(),
    }
}

/// `hc send` pastes the text into the terminal and submits it with Enter.
pub(crate) fn without_shell_syntax(text: &str) -> String {
    let plain: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() || PLAIN_MARKS.contains(c) {
                c
            } else {
                ' '
            }
        })
        .collect();
    one_line(&plain)
        .replace(" :", ":")
        .replace(" ,", ",")
        .replace(" .", ".")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Delivery {
    Sent { terminal: TerminalId },
    Spawned(Agent),
    Unreachable(Unreachable),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unreachable {
    Blocked {
        terminal: TerminalId,
    },
    NoRespawn,
    FirstMessageNotSent {
        terminal: TerminalId,
        reason: String,
    },
    Failed(String),
}

impl fmt::Display for Unreachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unreachable::Blocked { terminal } => write!(
                f,
                "{}이 권한 확인을 기다려 보내지 않았다. 사람이 풀어야 한다",
                terminal.0
            ),
            Unreachable::NoRespawn => {
                f.write_str("chute.toml의 respawn이 비어 있어 새 에이전트를 띄우지 않았다")
            }
            Unreachable::FirstMessageNotSent { terminal, reason } => write!(
                f,
                "새 에이전트 {}에게 첫 일이 전달되지 않았다: {reason}",
                terminal.0
            ),
            Unreachable::Failed(why) => f.write_str(why),
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct TerminalInfo {
    state: TerminalState,
    created_ms: EpochMs,
    #[serde(default, deserialize_with = "status_of_program")]
    status: ProgramStatus,
    #[serde(default)]
    reported: bool,
    #[serde(default)]
    address: Option<TerminalId>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    foreground: Option<Foreground>,
}

#[derive(Deserialize)]
struct Foreground {
    name: Option<String>,
}

#[derive(Default)]
enum ProgramStatus {
    #[default]
    NotGiven,
    NoProgram,
    Reported,
}

fn status_of_program<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ProgramStatus, D::Error> {
    Ok(match Option::<serde::de::IgnoredAny>::deserialize(d)? {
        None => ProgramStatus::NoProgram,
        Some(_) => ProgramStatus::Reported,
    })
}

impl TerminalInfo {
    pub(crate) fn is_live_agent(&self, recorded: EpochMs) -> bool {
        self.state == TerminalState::Live
            && self.created_ms == recorded
            && !self.has_left_for_a_shell()
    }

    fn has_left_for_a_shell(&self) -> bool {
        if !matches!(self.status, ProgramStatus::NoProgram) {
            return false;
        }
        match self.foreground.as_ref().and_then(|f| f.name.as_deref()) {
            Some(name) => SHELLS.contains(&name),
            None => self.reported,
        }
    }
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TerminalState {
    Live,
    Finished,
}

#[derive(Deserialize)]
struct Launched {
    address: TerminalId,
    first_message: Option<FirstMessage>,
    state: Option<TerminalState>,
    created_ms: Option<EpochMs>,
}

#[derive(Deserialize)]
struct FirstMessage {
    sent: bool,
    reason: Option<String>,
}

enum Started {
    Launched { name: String, out: Output },
    Earlier(Agent),
}

enum Attempt {
    Sent,
    Gone,
    Stuck(Unreachable),
}

impl Feedback<'_> {
    pub fn deliver(&self, item: &QueueItem, notice: &Notice<'_>) -> Delivery {
        if let Notice::Bounced(written) = notice
            && !matches!(&written.doc.subject, Subject::Pr { pr, .. } if *pr == item.pr)
        {
            return Delivery::Unreachable(Unreachable::Failed(format!(
                "{} 보고서는 PR {}에게 보낼 수 없다: {}",
                written.doc.subject.label(),
                item.pr,
                written.md.display()
            )));
        }
        let machine = match self.machine_of(item) {
            Ok(machine) => machine,
            Err(why) => return Delivery::Unreachable(why),
        };
        match machine {
            Some(machine) => self.deliver_remote(item, notice, &machine),
            None => self.deliver_here(item, notice),
        }
    }

    fn machine_of(&self, item: &QueueItem) -> Result<Option<Machine>, Unreachable> {
        let Some(id) = item.agent.as_ref().and_then(|a| a.machine.as_ref()) else {
            return Ok(None);
        };
        let local = machine::core(self.ext).map_err(|e| {
            Unreachable::Failed(format!(
                "에이전트가 있는 머신({id})이 이 PC인지 확인하지 못했다: {e:#}"
            ))
        })?;
        if local.id == *id {
            return Ok(None);
        }
        let others = machine::list(self.ext).map_err(|e| {
            Unreachable::Failed(format!("에이전트가 있는 머신({id})을 찾지 못했다: {e:#}"))
        })?;
        others.into_iter().find(|m| m.id == *id).map(Some).ok_or_else(|| {
            Unreachable::Failed(format!(
                "에이전트가 있는 머신({id})을 hc가 모른다. 사람이 `hc machine add`로 더하고 전달한다"
            ))
        })
    }

    fn deliver_remote(&self, item: &QueueItem, notice: &Notice<'_>, machine: &Machine) -> Delivery {
        let name = machine.name.as_str();
        let slug = self.repo.slug.to_string();
        let sent = self
            .parcel(item, notice, &Limits::default())
            .and_then(|parcel| parcel.encode())
            .map_err(|e| {
                Unreachable::Failed(format!("머신 `{name}`로 보낼 묶음을 만들지 못했다: {e:#}"))
            })
            .and_then(|bytes| {
                exec_on_input(
                    self.ext,
                    name,
                    &[],
                    &["chute", "--slug", &slug, "deliver"],
                    Some(&bytes),
                    REMOTE_DELIVER_TIMEOUT,
                )
                .map_err(|e| Unreachable::Failed(unreached(name, e.kind)))
            })
            .and_then(|answer| self.answer_of(name, &answer));
        match sent {
            Ok(Delivery::Spawned(mut agent)) => {
                agent.machine = Some(machine.id.clone());
                Delivery::Spawned(agent)
            }
            Ok(Delivery::Unreachable(why)) => {
                Delivery::Unreachable(Unreachable::Failed(format!("머신 `{name}`: {why}")))
            }
            Ok(sent) => sent,
            Err(why) => Delivery::Unreachable(why),
        }
    }

    /// Receiving end of `deliver_remote`: files the parcel under this machine's state folder,
    /// brings the failed tree in, and runs the v1 delivery here. The caller has fetched origin.
    pub fn receive(&self, parcel: &Parcel) -> Result<Delivery> {
        let item = &parcel.item;
        let (failure, failed_ref, rebuild, section) = match &parcel.notice {
            ParcelNotice::HeadChanged {
                registered,
                current,
            } => {
                return Ok(self.deliver_here(
                    item,
                    &Notice::HeadChanged {
                        registered,
                        current,
                    },
                ));
            }
            ParcelNotice::Bounced {
                failure,
                failed_ref,
                rebuild,
                env: section,
            } => (failure, failed_ref, rebuild, section),
        };
        let subject = Subject::from(item);
        let dir = self.report_dir(&subject);
        fs::create_dir_all(&dir).with_context(|| format!("만들지 못했다: {}", dir.display()))?;
        let files: Vec<(&str, Vec<u8>)> = parcel
            .files
            .iter()
            .map(|file| Ok((file.name.as_str(), file.bytes()?)))
            .collect::<Result<_>>()?;
        for (name, bytes) in files {
            write_file(&dir.join(name), bytes)?;
        }
        let tree = match failure {
            Failure::Verify(step) | Failure::Timeout(step) => Some(&step.tree),
            Failure::Conflict { .. } | Failure::Ci { .. } => None,
        };
        let imported = match (&parcel.bundle, failed_ref, tree) {
            (Some(bundle), Some(name), Some(tree)) => self.import_tree(&dir, bundle, name, tree),
            _ => None,
        };
        let written = self.write_report_at(
            &subject,
            failure,
            imported.as_deref(),
            section.as_deref().cloned(),
            Placement::Delivered {
                rebuild: rebuild.as_ref(),
            },
        )?;
        Ok(self.deliver_here(item, &Notice::Bounced(&written)))
    }

    fn import_tree(&self, dir: &Path, bundle: &str, name: &str, tree: &Sha) -> Option<String> {
        let bytes = crate::hub::base64_decode(bundle)?;
        let file = dir.join("failed-tree.bundle");
        write_file(&file, bytes).ok()?;
        let staging = format!("refs/chute/incoming/{}", short(tree));
        let fetched = self.ext.git(
            &self.repo.path,
            [
                OsString::from("fetch"),
                "--quiet".into(),
                file.as_os_str().to_owned(),
                format!("+{name}:{staging}").into(),
            ],
        );
        let _ = fs::remove_file(&file);
        let imported = fetched
            .ok()
            .filter(|out| out.success())
            .and_then(|_| self.failed_ref_at(&staging, tree).ok())
            .and_then(|_| {
                let moved = self
                    .ext
                    .git(&self.repo.path, ["update-ref", name, tree.as_str()])
                    .ok()?;
                moved.success().then_some(())
            })
            .and_then(|()| self.failed_ref_at(name, tree).ok());
        let _ = self
            .ext
            .git(&self.repo.path, ["update-ref", "-d", &staging]);
        imported
    }

    fn deliver_here(&self, item: &QueueItem, notice: &Notice<'_>) -> Delivery {
        let line = notice.line(item, self.config.require_head);
        if let Some(agent) = &item.agent {
            match self.send(agent, &line) {
                Attempt::Sent => {
                    return Delivery::Sent {
                        terminal: agent.terminal.clone(),
                    };
                }
                Attempt::Stuck(why) => return Delivery::Unreachable(why),
                Attempt::Gone => {}
            }
        }
        match self.respawn(item, &line) {
            Ok(delivery) => delivery,
            Err(why) => Delivery::Unreachable(why),
        }
    }

    fn run_hc<I, S>(&self, args: I) -> Result<Output, Unreachable>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.ext
            .hc(args)
            .map_err(|e| Unreachable::Failed(e.to_string()))
    }

    fn terminal(&self, id: &TerminalId) -> Result<Option<TerminalInfo>, Unreachable> {
        terminal(self.ext, id)
    }

    fn send(&self, agent: &Agent, line: &str) -> Attempt {
        match self.terminal(&agent.terminal) {
            Err(why) => return Attempt::Stuck(why),
            Ok(Some(info)) if info.is_live_agent(agent.created_ms) => {}
            Ok(_) => return Attempt::Gone,
        }
        let out = match self.run_hc(["send", agent.terminal.0.as_str(), line]) {
            Ok(out) => out,
            Err(why) => return Attempt::Stuck(why),
        };
        match out.code {
            Some(0) => Attempt::Sent,
            Some(HC_BLOCKED) => Attempt::Stuck(Unreachable::Blocked {
                terminal: agent.terminal.clone(),
            }),
            Some(HC_NOT_FOUND | HC_FINISHED) => Attempt::Gone,
            _ => Attempt::Stuck(Unreachable::Failed(hc_failure("send", &out))),
        }
    }

    fn respawn(&self, item: &QueueItem, line: &str) -> Result<Delivery, Unreachable> {
        if self.config.respawn.is_empty() {
            return Err(Unreachable::NoRespawn);
        }
        let cwd = self
            .workdir(item)
            .map_err(|e| Unreachable::Failed(format!("{e:#}")))?;
        let (fetch, push) = match self.pr_remote(item.pr, &cwd) {
            Some(remote) => (
                format!("`git fetch {remote}`과 `git status`로"),
                format!("`git push -u {remote} <브랜치>`로"),
            ),
            None => (
                "PR 브랜치가 있는 원격에서 fetch하고 `git status`로".to_owned(),
                "PR 브랜치가 있는 원격으로".to_owned(),
            ),
        };
        let task = format!(
            "너는 chute(머지 큐)가 돌려보낸 PR의 담당 에이전트다. PR: {}, 브랜치: {}. 지금 폴더가 그 브랜치의 worktree다. 먼저 {fetch} 이 폴더가 PR의 최신 HEAD인지 본다. push는 {push} 한다. chute 사용법은 `chute --skill`로 읽는다. {line}",
            item.pr,
            code_span(&item.branch),
        );
        let (name, out) = match self.run_with_free_name(item.pr, &cwd, task)? {
            Started::Launched { name, out } => (name, out),
            Started::Earlier(agent) => {
                return match self.send(&agent, line) {
                    Attempt::Sent => Ok(Delivery::Spawned(agent)),
                    Attempt::Stuck(why) => Err(why),
                    Attempt::Gone => Err(Unreachable::Failed(format!(
                        "이미 떠 있는 에이전트 {}에게 보내지 못했다",
                        agent.terminal.0
                    ))),
                };
            }
        };
        let launched: Launched = serde_json::from_slice(&out.stdout)
            .map_err(|e| Unreachable::Failed(format!("hc run의 출력을 읽지 못했다: {e}")))?;
        if let Some(FirstMessage {
            sent: false,
            reason,
        }) = launched.first_message
        {
            return Err(Unreachable::FirstMessageNotSent {
                terminal: launched.address,
                reason: reason.unwrap_or_else(|| "이유를 알 수 없다".to_owned()),
            });
        }
        let (created_ms, state) = match (launched.created_ms, launched.state) {
            (Some(created_ms), Some(state)) => (created_ms, state),
            _ => {
                let started = |why: &dyn fmt::Display| {
                    Unreachable::Failed(format!(
                        "에이전트 {}는 떴지만 created_ms를 읽지 못했다: {why}",
                        launched.address.0
                    ))
                };
                let info = self
                    .terminal(&launched.address)
                    .map_err(|why| started(&why))?
                    .ok_or_else(|| started(&"hc get이 찾지 못했다"))?;
                (info.created_ms, info.state)
            }
        };
        if state != TerminalState::Live {
            return Err(Unreachable::Failed(format!(
                "새 에이전트 {}가 곧바로 끝났다",
                launched.address.0
            )));
        }
        Ok(Delivery::Spawned(Agent {
            terminal: launched.address,
            created_ms,
            name: Some(name),
            workdir: cwd,
            alive: true,
            machine: None,
        }))
    }

    fn pr_remote(&self, pr: PrNumber, cwd: &Path) -> Option<String> {
        #[derive(Deserialize)]
        struct Owner {
            login: String,
        }
        #[derive(Deserialize)]
        struct Head {
            name: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct View {
            head_repository_owner: Owner,
            head_repository: Head,
        }
        let text = self
            .ext
            .gh(
                cwd,
                [
                    "pr",
                    "view",
                    &pr.0.to_string(),
                    "--json",
                    "headRepositoryOwner,headRepository",
                ],
            )
            .ok()?
            .ok()
            .ok()?;
        let view: View = serde_json::from_str(&text).ok()?;
        let head = Slug::new(
            &view.head_repository_owner.login,
            &view.head_repository.name,
        )
        .ok()?;
        let mut holders = repo::remotes(self.ext, cwd)
            .ok()?
            .into_iter()
            .filter(|r| r.slug.same_repo(&head));
        let first = holders.next()?;
        let managed = self.repo.remote_name.as_str();
        if first.name == managed {
            return Some(first.name);
        }
        Some(holders.find(|r| r.name == managed).unwrap_or(first).name)
    }

    fn run_with_free_name(
        &self,
        pr: PrNumber,
        cwd: &Path,
        task: String,
    ) -> Result<Started, Unreachable> {
        for attempt in 1..=NAME_TRIES {
            let name = self.agent_name(pr, attempt);
            let mut args: Vec<OsString> = ["run", "--bg", "--json", "--no-parent", "--name"]
                .into_iter()
                .map(OsString::from)
                .collect();
            args.extend([
                OsString::from(&name),
                OsString::from("--cwd"),
                cwd.as_os_str().to_owned(),
                OsString::from("--"),
            ]);
            args.extend(self.config.respawn.iter().map(OsString::from));
            args.push(OsString::from(&task));
            let out = self.run_hc(args)?;
            if out.success() {
                return Ok(Started::Launched { name, out });
            }
            if !(is_name_taken(&out) && attempt < NAME_TRIES) {
                return Err(Unreachable::Failed(hc_failure("run", &out)));
            }
            if let Some(earlier) = self.live_agent_named(&name, cwd)? {
                return Ok(Started::Earlier(earlier));
            }
        }
        unreachable!("the last attempt returns")
    }

    /// The agent chute started earlier under `name` for this folder, if it is still alive.
    fn live_agent_named(&self, name: &str, cwd: &Path) -> Result<Option<Agent>, Unreachable> {
        let Some(info) = self.terminal(&TerminalId(name.to_owned()))? else {
            return Ok(None);
        };
        let is_agent = info.state == TerminalState::Live && !info.has_left_for_a_shell();
        let (Some(terminal), Some(launched_in)) = (info.address, info.cwd) else {
            return Ok(None);
        };
        let same_folder = resolved(&launched_in) == resolved(cwd);
        Ok((is_agent && same_folder).then(|| Agent {
            terminal,
            created_ms: info.created_ms,
            name: Some(name.to_owned()),
            workdir: cwd.to_owned(),
            alive: true,
            machine: None,
        }))
    }

    fn agent_name(&self, pr: PrNumber, attempt: u32) -> String {
        let number = pr.0.to_string();
        let longest_suffix = format!("-{NAME_TRIES}");
        let room = HC_NAME_LIMIT - "chute--pr".len() - number.len() - longest_suffix.len();
        let repo: String = self
            .repo
            .slug
            .name()
            .chars()
            .map(|c| match c {
                c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
                '_' => '_',
                _ => '-',
            })
            .take(room)
            .collect();
        match attempt {
            1 => format!("chute-{repo}-pr{number}"),
            n => format!("chute-{repo}-pr{number}-{n}"),
        }
    }

    fn workdir(&self, item: &QueueItem) -> Result<PathBuf> {
        if let Some(agent) = &item.agent
            && self.on_branch(&agent.workdir, &item.branch)
            && self.in_repo(&agent.workdir)
        {
            return Ok(agent.workdir.clone());
        }
        let checkouts = self.checkouts()?;
        let holders: Vec<&Checkout> = checkouts
            .iter()
            .filter(|c| c.branch.as_deref() == Some(item.branch.as_str()))
            .collect();
        if let Some(live) = holders
            .iter()
            .find(|c| c.path.is_dir() && self.is_worktree_root(&c.path))
        {
            return Ok(live.path.clone());
        }
        if let Some(locked) = holders.iter().find(|c| c.locked) {
            bail!(
                "PR 브랜치를 쥔 worktree가 잠겨 있어 폴더가 없어도 가져오지 않는다: {}",
                locked.path.display()
            );
        }
        let dest = free_dest(&self.worktree_dest(item)?, &checkouts)?;
        let record_without_folder = !holders.is_empty()
            || checkouts
                .iter()
                .any(|c| resolved(&c.path) == resolved(&dest));
        self.add_worktree(item, &dest, record_without_folder)
    }

    fn on_branch(&self, dir: &Path, branch: &str) -> bool {
        self.ext
            .git(dir, ["symbolic-ref", "--short", "-q", "HEAD"])
            .is_ok_and(|out| out.success() && out.stdout_text().trim() == branch)
    }

    fn is_worktree_root(&self, dir: &Path) -> bool {
        self.ext
            .git(dir, ["rev-parse", "--show-toplevel"])
            .is_ok_and(|out| {
                out.success()
                    && resolved(Path::new(out.stdout_text().trim_end_matches('\n')))
                        == resolved(dir)
            })
    }

    /// True unless git says `commit` is not reachable from `reference`; a commit git cannot
    /// find at all counts as reachable, so the reference is used.
    fn contains_commit(&self, reference: &str, commit: &Sha) -> Result<bool> {
        let out = self.ext.git(
            &self.repo.path,
            ["merge-base", "--is-ancestor", commit.as_str(), reference],
        )?;
        Ok(out.code != Some(1))
    }

    fn in_repo(&self, dir: &Path) -> bool {
        let common_dir = |cwd: &Path| -> Option<PathBuf> {
            let out = self
                .ext
                .git(
                    cwd,
                    ["rev-parse", "--path-format=absolute", "--git-common-dir"],
                )
                .ok()?;
            out.success()
                .then(|| PathBuf::from(out.stdout_text().trim_end_matches('\n')))?
                .canonicalize()
                .ok()
        };
        matches!(
            (common_dir(dir), common_dir(&self.repo.path)),
            (Some(a), Some(b)) if a == b
        )
    }

    fn checkouts(&self) -> Result<Vec<Checkout>> {
        let text = self
            .ext
            .git(&self.repo.path, ["worktree", "list", "--porcelain"])?
            .ok()?;
        Ok(text
            .split("\n\n")
            .filter_map(|block| {
                let path = PathBuf::from(block.lines().find_map(|l| l.strip_prefix("worktree "))?);
                let branch = block
                    .lines()
                    .find_map(|l| l.strip_prefix("branch refs/heads/"))
                    .map(str::to_owned)
                    .or_else(|| self.rebasing_branch(&path));
                let locked = block
                    .lines()
                    .any(|l| l == "locked" || l.starts_with("locked "));
                Some(Checkout {
                    path,
                    branch,
                    locked,
                })
            })
            .collect())
    }

    /// A worktree stopped in the middle of a rebase has a detached HEAD, yet git still treats
    /// the rebased branch as checked out there.
    fn rebasing_branch(&self, dir: &Path) -> Option<String> {
        ["rebase-merge", "rebase-apply"].iter().find_map(|state| {
            let out = self
                .ext
                .git(
                    dir,
                    ["rev-parse", "--git-path", &format!("{state}/head-name")],
                )
                .ok()?;
            let file = dir.join(out.stdout_text().trim_end_matches('\n'));
            fs::read_to_string(file)
                .ok()?
                .trim()
                .strip_prefix("refs/heads/")
                .map(str::to_owned)
        })
    }

    fn worktree_dest(&self, item: &QueueItem) -> Result<PathBuf> {
        let branch = item.branch.as_str();
        let plain = !branch.starts_with('-')
            && !branch.is_empty()
            && Path::new(branch)
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
        if !plain {
            bail!("브랜치 이름으로 worktree 자리를 정할 수 없다: {branch:?}");
        }
        Ok(self.worktrees_root.join(self.repo.slug.name()).join(branch))
    }

    fn add_worktree(
        &self,
        item: &QueueItem,
        dest: &Path,
        holder_folder_gone: bool,
    ) -> Result<PathBuf> {
        if dest.exists() {
            bail!(
                "worktree를 만들 자리에 이미 무엇이 있다: {}",
                dest.display()
            );
        }
        let branch = item.branch.as_str();
        let has_ref = |name: String| -> Result<bool> {
            Ok(self
                .ext
                .git(&self.repo.path, ["rev-parse", "--verify", "--quiet", &name])?
                .success())
        };
        let remote = format!("refs/remotes/{}/{branch}", self.repo.remote_name);
        let mut args: Vec<OsString> = vec!["worktree".into(), "add".into()];
        // git refuses a branch that a worktree record still holds, even once that folder is gone.
        if holder_folder_gone {
            args.push("--force".into());
        }
        let mut upstream = None;
        if has_ref(format!("refs/heads/{branch}"))? {
            args.extend([dest.as_os_str().to_owned(), branch.into()]);
        } else {
            let remote_exists = has_ref(remote.clone())?;
            let start = if remote_exists && self.contains_commit(&remote, &item.head)? {
                remote.as_str()
            } else {
                item.head.as_str()
            };
            args.extend([
                "-b".into(),
                branch.into(),
                dest.as_os_str().to_owned(),
                start.into(),
            ]);
            upstream = remote_exists.then_some(remote);
        }
        self.ext.git(&self.repo.path, args)?.ok()?;
        if let Some(upstream) = upstream {
            let _ = self
                .ext
                .git(dest, ["branch", &format!("--set-upstream-to={upstream}")]);
        }
        Ok(dest.to_owned())
    }
}

fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = at.canonicalize() {
            return rest.iter().rev().fold(real, |dir, part| dir.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

fn free_dest(wanted: &Path, checkouts: &[Checkout]) -> Result<PathBuf> {
    let taken = |path: &Path| {
        path.exists()
            || checkouts
                .iter()
                .any(|c| c.locked && resolved(&c.path) == resolved(path))
    };
    if !taken(wanted) {
        return Ok(wanted.to_owned());
    }
    (2..=SIBLING_TRIES)
        .map(|n| {
            let mut name = wanted.as_os_str().to_owned();
            name.push(format!("-chute-{n}"));
            PathBuf::from(name)
        })
        .find(|candidate| !taken(candidate))
        .ok_or_else(|| anyhow!("worktree를 만들 빈 자리가 없다: {}", wanted.display()))
}

struct Checkout {
    path: PathBuf,
    branch: Option<String>,
    locked: bool,
}

fn is_name_taken(out: &Output) -> bool {
    #[derive(Deserialize)]
    struct Failure {
        error: Kind,
    }
    #[derive(Deserialize)]
    struct Kind {
        kind: String,
    }
    serde_json::from_slice::<Failure>(&out.stderr).is_ok_and(|f| f.error.kind == "name_taken")
}

fn unreached(machine: &str, kind: ForwardErrorKind) -> String {
    match kind {
        ForwardErrorKind::UnknownMachine => format!(
            "hc가 머신 `{machine}`을 모른다. 보고서를 보내지 못했다. `hc machine ls`로 등록된 머신을 확인한다"
        ),
        ForwardErrorKind::NoChute => format!(
            "머신 `{machine}`에서 chute를 실행하지 못했다. 보고서를 보내지 못했다. 그 머신의 PATH에 chute가 있는지 확인한다"
        ),
        ForwardErrorKind::TimedOut => format!(
            "머신 `{machine}`가 제때 답하지 않았다. 보고서가 전달됐는지 알 수 없다. `chute status`로 확인한다"
        ),
        ForwardErrorKind::Unreachable | ForwardErrorKind::Misconfigured => format!(
            "머신 `{machine}`에 닿지 못했다. 보고서를 보내지 못했다. `hc machine ls`로 상태를 보고, 로그인이 필요하면 `hc machine login {machine}`을 실행한다"
        ),
    }
}

pub(crate) fn terminal(ext: &Ext, id: &TerminalId) -> Result<Option<TerminalInfo>, Unreachable> {
    let out = ext
        .hc(["get", id.0.as_str(), "--json"])
        .map_err(|e| Unreachable::Failed(e.to_string()))?;
    match out.code {
        Some(0) => serde_json::from_slice(&out.stdout)
            .map(Some)
            .map_err(|e| Unreachable::Failed(format!("hc get {}의 출력을 읽지 못했다: {e}", id.0))),
        Some(HC_NOT_FOUND) => Ok(None),
        _ => Err(Unreachable::Failed(hc_failure("get", &out))),
    }
}

fn exit_text(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("종료 코드 {code}"),
        None => "신호로 끝남".to_owned(),
    }
}

fn hc_failure(verb: &str, out: &Output) -> String {
    format!(
        "hc {verb}가 실패했다({}): {}",
        exit_text(out.code),
        out.stderr_text().trim()
    )
}

#[cfg(test)]
mod tests;
