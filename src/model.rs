use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrNumber(pub u64);

impl fmt::Display for PrNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl std::str::FromStr for PrNumber {
    type Err = String;

    /// Accepts `12` and `#12`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.strip_prefix('#')
            .unwrap_or(s)
            .parse()
            .map(PrNumber)
            .map_err(|_| format!("PR 번호가 아니다: {s:?}"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha(String);

impl Sha {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let hex = s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if hex && (4..=64).contains(&s.len()) {
            Ok(Sha(s))
        } else {
            Err(format!("커밋 SHA가 아니다: {s:?}"))
        }
    }
}

impl From<Sha> for String {
    fn from(s: Sha) -> String {
        s.0
    }
}

impl fmt::Display for Sha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EpochMs(pub u64);

impl EpochMs {
    pub fn now() -> EpochMs {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        EpochMs(since.as_millis() as u64)
    }
}

/// GitHub `owner/name`. The state folder name is `owner__name`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Slug {
    owner: String,
    name: String,
}

impl Slug {
    pub fn new(owner: &str, name: &str) -> Result<Slug, String> {
        let valid = |s: &str| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        if valid(owner) && valid(name) {
            Ok(Slug {
                owner: owner.to_owned(),
                name: name.to_owned(),
            })
        } else {
            Err(format!("저장소 이름이 아니다: {owner}/{name}"))
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn dir_name(&self) -> String {
        format!("{}__{}", self.owner, self.name)
    }
}

impl TryFrom<String> for Slug {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.split_once('/') {
            Some((owner, name)) => Slug::new(owner, name),
            None => Err(format!("owner/name 형식이 아니다: {s:?}")),
        }
    }
}

impl From<Slug> for String {
    fn from(s: Slug) -> String {
        s.to_string()
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    #[default]
    Agent,
    Human,
}

/// `repo.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub slug: Slug,
    pub path: PathBuf,
    pub remote: String,
    pub default_branch: String,
}

/// `daemon.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub terminal: Option<TerminalId>,
    pub doing: Option<String>,
    pub started_ms: EpochMs,
}

/// A queue item's state and its reason, written as the flat pair
/// `"state"` / `"state_reason"` so a reason can only appear where the state allows one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawItemState", into = "RawItemState")]
pub enum ItemState {
    AwaitingApproval,
    Queued(Option<QueuedReason>),
    InBatch,
    Bounced(BounceReason),
    NeedsHuman(HumanReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedReason {
    CiPending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BounceReason {
    Conflict,
    Verify,
    Timeout,
    HeadChanged,
    Ci,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanReason {
    Budget,
    Unreachable,
    GaveUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StateName {
    AwaitingApproval,
    Queued,
    InBatch,
    Bounced,
    NeedsHuman,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum AnyReason {
    Queued(QueuedReason),
    Bounce(BounceReason),
    Human(HumanReason),
}

#[derive(Serialize, Deserialize)]
struct RawItemState {
    state: StateName,
    state_reason: Option<AnyReason>,
}

impl TryFrom<RawItemState> for ItemState {
    type Error = String;

    fn try_from(raw: RawItemState) -> Result<Self, Self::Error> {
        use AnyReason as R;
        match (raw.state, raw.state_reason) {
            (StateName::AwaitingApproval, None) => Ok(ItemState::AwaitingApproval),
            (StateName::Queued, None) => Ok(ItemState::Queued(None)),
            (StateName::Queued, Some(R::Queued(r))) => Ok(ItemState::Queued(Some(r))),
            (StateName::InBatch, None) => Ok(ItemState::InBatch),
            (StateName::Bounced, Some(R::Bounce(r))) => Ok(ItemState::Bounced(r)),
            (StateName::NeedsHuman, Some(R::Human(r))) => Ok(ItemState::NeedsHuman(r)),
            (state, reason) => Err(format!("상태 {state:?}에 사유 {reason:?}가 올 수 없다")),
        }
    }
}

impl From<ItemState> for RawItemState {
    fn from(s: ItemState) -> Self {
        let (state, state_reason) = match s {
            ItemState::AwaitingApproval => (StateName::AwaitingApproval, None),
            ItemState::Queued(r) => (StateName::Queued, r.map(AnyReason::Queued)),
            ItemState::InBatch => (StateName::InBatch, None),
            ItemState::Bounced(r) => (StateName::Bounced, Some(AnyReason::Bounce(r))),
            ItemState::NeedsHuman(r) => (StateName::NeedsHuman, Some(AnyReason::Human(r))),
        };
        RawItemState {
            state,
            state_reason,
        }
    }
}

/// The agent that submitted a PR. An hc address alone may be reused, so a bounce
/// goes to the same agent only when `terminal` and `created_ms` both match a live terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub terminal: TerminalId,
    pub created_ms: EpochMs,
    pub name: Option<String>,
    pub workdir: PathBuf,
    pub alive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub pr: PrNumber,
    pub title: String,
    pub branch: String,
    /// The HEAD recorded at submit. Only this commit is merged.
    pub head: Sha,
    #[serde(flatten)]
    pub state: ItemState,
    pub agent: Option<Agent>,
    /// Consecutive bounces. Reset on merge and on `chute retry`.
    pub attempts: u32,
    pub submitted_ms: EpochMs,
    pub touches_config: bool,
    pub last_failure: Option<FailureRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Conflict,
    Verify,
    Timeout,
    Ci,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Failure {
    Conflict { files: Vec<ConflictFile> },
    Verify(StepFailure),
    Timeout(StepFailure),
    Ci { checks: Vec<FailedCheck> },
}

impl Failure {
    pub fn kind(&self) -> FailureKind {
        match self {
            Failure::Conflict { .. } => FailureKind::Conflict,
            Failure::Verify(_) => FailureKind::Verify,
            Failure::Timeout(_) => FailureKind::Timeout,
            Failure::Ci { .. } => FailureKind::Ci,
        }
    }

    pub fn mode(&self) -> Option<Mode> {
        match self {
            Failure::Verify(step) | Failure::Timeout(step) => Some(match step.mode {
                VerifyMode::Solo => Mode::Solo,
                VerifyMode::Combo { .. } => Mode::Combo,
            }),
            Failure::Conflict { .. } | Failure::Ci { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictFile {
    pub path: String,
    /// Commits on the default branch that changed this file.
    pub main_commits: Vec<CommitRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRef {
    pub sha: Sha,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepFailure {
    pub step: String,
    #[serde(flatten)]
    pub mode: VerifyMode,
    /// Names matched by the step's `failed` regex. Empty when the step has none.
    pub failed_tests: Vec<String>,
    pub log: PathBuf,
    /// The failing tree, kept under `refs/chute/failed/…` for reproduction.
    pub tree: Sha,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum VerifyMode {
    Solo,
    Combo { partners: Vec<Partner> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Solo,
    Combo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partner {
    pub pr: PrNumber,
    pub title: String,
    pub files: Vec<String>,
    pub diff: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCheck {
    pub name: String,
    pub link: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRecord {
    #[serde(flatten)]
    pub failure: Failure,
    pub summary: String,
    pub report: PathBuf,
    pub at_ms: EpochMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Building,
    Verifying,
    Bisecting,
    Pushing,
}

/// The batch in progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub id: BatchId,
    pub base: Sha,
    pub prs: Vec<PrNumber>,
    pub phase: Phase,
    pub step: Option<String>,
    pub round: u32,
    pub started_ms: EpochMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    BaseBroken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocked {
    pub reason: BlockReason,
    pub base: Sha,
    pub since_ms: EpochMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightResult {
    Pass,
    Fail,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightRecord {
    pub head: Sha,
    pub result: PreflightResult,
    pub report: Option<PathBuf>,
    pub at_ms: EpochMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    /// After a push, GitHub did not report the PR as MERGED.
    NotMerged,
    /// `require_ci` is on but a PR has had no checks for over ten minutes.
    NoChecks,
    /// `chute.toml` on the default branch could not be read.
    Config,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
    pub pr: Option<PrNumber>,
    pub at_ms: EpochMs,
}

/// `state.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub queue: Vec<QueueItem>,
    pub batch: Option<Batch>,
    pub blocked: Option<Blocked>,
    /// Base commits whose verification passed. Commits chute pushed are verified by construction.
    pub verified_bases: Vec<Sha>,
    /// Latest preflight per branch.
    pub preflights: BTreeMap<String, PreflightRecord>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRef {
    pub pr: PrNumber,
    pub commit: Sha,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flaky {
    pub step: String,
    pub tests: Vec<String>,
    pub tree: Sha,
}

/// `batches/<id>/batch.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRecord {
    pub id: BatchId,
    pub base: Sha,
    pub prs: Vec<PrNumber>,
    pub started_ms: EpochMs,
    pub finished_ms: Option<EpochMs>,
    /// Merge commits pushed to the default branch, in push order.
    pub merges: Vec<MergeRef>,
    pub bounced: Vec<PrNumber>,
    pub flaky: Vec<Flaky>,
    pub verify_s: u64,
}

/// One line of `history.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at_ms: EpochMs,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    Merged {
        pr: PrNumber,
        batch: BatchId,
        commit: Sha,
        submitted_ms: EpochMs,
    },
    Bounced {
        pr: PrNumber,
        batch: Option<BatchId>,
        reason: BounceReason,
        attempts: u32,
    },
    Flaky {
        batch: BatchId,
        step: String,
        tests: Vec<String>,
        tree: Sha,
    },
    BatchFinished {
        batch: BatchId,
        merged: Vec<PrNumber>,
        bounced: Vec<PrNumber>,
        seconds: u64,
        verify_s: u64,
    },
    Reverted {
        batch: BatchId,
        prs: Vec<PrNumber>,
        commit: Sha,
    },
}

/// The `schema` field of the status JSON. Only version 1 exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct SchemaV1;

impl TryFrom<u32> for SchemaV1 {
    type Error = String;

    fn try_from(v: u32) -> Result<Self, Self::Error> {
        if v == 1 {
            Ok(SchemaV1)
        } else {
            Err(format!("상태 JSON schema {v}는 모른다"))
        }
    }
}

impl From<SchemaV1> for u32 {
    fn from(_: SchemaV1) -> u32 {
        1
    }
}

/// `chute status --json`: the contract between the engine and the Herdr mod.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusView {
    pub schema: SchemaV1,
    pub repo: RepoView,
    pub approval: Approval,
    pub daemon: DaemonView,
    pub blocked: Option<Blocked>,
    pub batch: Option<Batch>,
    pub queue: Vec<ItemView>,
    pub today: Today,
    pub recent: Vec<RecentBatch>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoView {
    pub slug: Slug,
    pub path: PathBuf,
    pub default_branch: String,
    pub state_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonView {
    pub running: bool,
    pub terminal: Option<TerminalId>,
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemView {
    pub pr: PrNumber,
    pub title: String,
    pub branch: String,
    pub head: Sha,
    #[serde(flatten)]
    pub state: ItemState,
    pub agent: Option<AgentView>,
    pub attempts: u32,
    pub budget: u32,
    pub submitted_ms: EpochMs,
    pub touches_config: bool,
    pub preflight: Option<PreflightView>,
    pub last_failure: Option<FailureView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    pub terminal: TerminalId,
    pub name: Option<String>,
    pub alive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightView {
    pub result: PreflightResult,
    pub head: Sha,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureView {
    pub kind: FailureKind,
    pub mode: Option<Mode>,
    pub summary: String,
    pub report: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Today {
    pub merged: u32,
    pub bounced: u32,
    pub flaky: u32,
    pub batches: u32,
    pub median_lead_s: Option<u64>,
    pub verify_s: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentBatch {
    pub batch: BatchId,
    pub merged: Vec<PrNumber>,
    pub bounced: Vec<PrNumber>,
    pub seconds: u64,
    pub finished_ms: EpochMs,
}

impl ItemView {
    pub fn new(item: &QueueItem, budget: u32, preflight: Option<&PreflightRecord>) -> ItemView {
        ItemView {
            pr: item.pr,
            title: item.title.clone(),
            branch: item.branch.clone(),
            head: item.head.clone(),
            state: item.state,
            agent: item.agent.as_ref().map(|a| AgentView {
                terminal: a.terminal.clone(),
                name: a.name.clone(),
                alive: a.alive,
            }),
            attempts: item.attempts,
            budget,
            submitted_ms: item.submitted_ms,
            touches_config: item.touches_config,
            preflight: preflight.map(|p| PreflightView {
                result: p.result,
                head: p.head.clone(),
            }),
            last_failure: item.last_failure.as_ref().map(|f| FailureView {
                kind: f.failure.kind(),
                mode: f.failure.mode(),
                summary: f.summary.clone(),
                report: f.report.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const FIXTURE: &str = include_str!("../tests/fixtures/status-full.json");

    #[test]
    fn status_fixture_round_trips() {
        let original: Value = serde_json::from_str(FIXTURE).unwrap();
        let view: StatusView = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&view).unwrap(), original);
    }

    #[test]
    fn status_fixture_covers_every_state_and_reason() {
        let view: StatusView = serde_json::from_str(FIXTURE).unwrap();
        let states: Vec<ItemState> = view.queue.iter().map(|i| i.state).collect();
        let expected = [
            ItemState::AwaitingApproval,
            ItemState::Queued(None),
            ItemState::Queued(Some(QueuedReason::CiPending)),
            ItemState::InBatch,
            ItemState::Bounced(BounceReason::Conflict),
            ItemState::Bounced(BounceReason::Verify),
            ItemState::Bounced(BounceReason::Timeout),
            ItemState::Bounced(BounceReason::HeadChanged),
            ItemState::Bounced(BounceReason::Ci),
            ItemState::NeedsHuman(HumanReason::Budget),
            ItemState::NeedsHuman(HumanReason::Unreachable),
            ItemState::NeedsHuman(HumanReason::GaveUp),
        ];
        for state in expected {
            assert!(states.contains(&state), "fixture에 {state:?}가 없다");
        }
        let kinds: Vec<FailureKind> = view
            .queue
            .iter()
            .filter_map(|i| i.last_failure.as_ref().map(|f| f.kind))
            .collect();
        for kind in [
            FailureKind::Conflict,
            FailureKind::Verify,
            FailureKind::Timeout,
            FailureKind::Ci,
        ] {
            assert!(kinds.contains(&kind), "fixture에 실패 {kind:?}가 없다");
        }
        assert!(view.queue.iter().any(|i| i.agent.is_none()));
        assert!(view.blocked.is_some());
        assert!(view.batch.is_some());
        assert!(!view.warnings.is_empty());
    }

    #[test]
    fn item_state_rejects_impossible_reason() {
        let parse = |v: Value| serde_json::from_value::<ItemState>(v);
        assert!(parse(json!({"state": "bounced", "state_reason": null})).is_err());
        assert!(parse(json!({"state": "bounced", "state_reason": "budget"})).is_err());
        assert!(parse(json!({"state": "in_batch", "state_reason": "verify"})).is_err());
        assert!(parse(json!({"state": "needs_human", "state_reason": "conflict"})).is_err());
        assert_eq!(
            parse(json!({"state": "bounced", "state_reason": "head_changed"})).unwrap(),
            ItemState::Bounced(BounceReason::HeadChanged)
        );
    }

    #[test]
    fn stored_state_round_trips_every_failure() {
        let sha = |s: &str| Sha::try_from(s.to_owned()).unwrap();
        let step = |mode| StepFailure {
            step: "test".into(),
            mode,
            failed_tests: vec!["a::b".into()],
            log: "/s/log".into(),
            tree: sha("abc1234"),
        };
        let failures = [
            Failure::Conflict {
                files: vec![ConflictFile {
                    path: "src/x.rs".into(),
                    main_commits: vec![CommitRef {
                        sha: sha("abc1234"),
                        title: "t".into(),
                    }],
                }],
            },
            Failure::Verify(step(VerifyMode::Solo)),
            Failure::Timeout(step(VerifyMode::Combo {
                partners: vec![Partner {
                    pr: PrNumber(3),
                    title: "p".into(),
                    files: vec!["a".into()],
                    diff: "/s/d.diff".into(),
                }],
            })),
            Failure::Ci {
                checks: vec![FailedCheck {
                    name: "build".into(),
                    link: "https://x".into(),
                }],
            },
        ];
        let state = State {
            queue: failures
                .into_iter()
                .enumerate()
                .map(|(i, failure)| QueueItem {
                    pr: PrNumber(i as u64),
                    title: "t".into(),
                    branch: "b".into(),
                    head: sha("def5678"),
                    state: ItemState::Bounced(BounceReason::Verify),
                    agent: None,
                    attempts: 1,
                    submitted_ms: EpochMs(1),
                    touches_config: false,
                    last_failure: Some(FailureRecord {
                        failure,
                        summary: "s".into(),
                        report: "/s/r.md".into(),
                        at_ms: EpochMs(2),
                    }),
                })
                .collect(),
            ..State::default()
        };
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
    }
}
