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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MachineId(String);

impl MachineId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineId {
    type Error = String;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        if id.is_empty() || id.contains(|c: char| c.is_whitespace() || c.is_control() || c == '/') {
            return Err(format!("머신 id가 올바르지 않다: {id:?}"));
        }
        Ok(MachineId(id))
    }
}

impl From<MachineId> for String {
    fn from(id: MachineId) -> String {
        id.0
    }
}

impl fmt::Display for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubConfig {
    pub machine_id: MachineId,
}

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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Slug {
    owner: String,
    name: String,
}

impl Slug {
    pub fn new(owner: &str, name: &str) -> Result<Slug, String> {
        let owner_ok = !owner.is_empty()
            && !owner.starts_with('_')
            && !owner.ends_with('_')
            && !owner.contains("__")
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b));
        let name_ok = !name.is_empty()
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if owner_ok && name_ok {
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

    /// GitHub compares owner and name without regard to case.
    pub fn same_repo(&self, other: &Slug) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub slug: Slug,
    pub path: PathBuf,
    pub remote: String,
    #[serde(default = "default_remote_name")]
    pub remote_name: String,
    pub default_branch: String,
}

fn default_remote_name() -> String {
    "origin".to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub terminal: Option<TerminalId>,
    pub doing: Option<String>,
    pub started_ms: EpochMs,
}

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

/// hc does not promise that terminal addresses are never reused, so an agent is the same
/// only when `terminal` and `created_ms` both match a live terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub terminal: TerminalId,
    pub created_ms: EpochMs,
    pub name: Option<String>,
    pub workdir: PathBuf,
    pub alive: bool,
    #[serde(default)]
    pub machine: Option<MachineId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub pr: PrNumber,
    pub title: String,
    pub branch: String,
    pub head: Sha,
    #[serde(flatten)]
    pub state: ItemState,
    pub agent: Option<Agent>,
    pub attempts: u32,
    pub submitted_ms: EpochMs,
    pub touches_config: bool,
    pub last_failure: Option<FailureRecord>,
    #[serde(default)]
    pub submitter: Option<Agent>,
    #[serde(default)]
    pub priority: bool,
    #[serde(default)]
    pub after: Vec<PrNumber>,
    #[serde(default)]
    pub repair: bool,
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
    Conflict { files: NonEmpty<ConflictFile> },
    Verify(StepFailure),
    Timeout(StepFailure),
    Ci { checks: NonEmpty<FailedCheck> },
}

impl Failure {
    pub fn shape(&self) -> FailureShape {
        let mode = |step: &StepFailure| match step.mode {
            VerifyMode::Solo => Mode::Solo,
            VerifyMode::Combo { .. } => Mode::Combo,
        };
        match self {
            Failure::Conflict { .. } => FailureShape::Conflict,
            Failure::Verify(step) => FailureShape::Verify(mode(step)),
            Failure::Timeout(step) => FailureShape::Timeout(mode(step)),
            Failure::Ci { .. } => FailureShape::Ci,
        }
    }
}

/// A failure's kind and mode, written as the flat pair `"kind"` / `"mode"`:
/// verification failures have a mode, conflicts and CI failures do not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawFailureShape", into = "RawFailureShape")]
pub enum FailureShape {
    Conflict,
    Verify(Mode),
    Timeout(Mode),
    Ci,
}

impl FailureShape {
    pub fn kind(self) -> FailureKind {
        match self {
            FailureShape::Conflict => FailureKind::Conflict,
            FailureShape::Verify(_) => FailureKind::Verify,
            FailureShape::Timeout(_) => FailureKind::Timeout,
            FailureShape::Ci => FailureKind::Ci,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RawFailureShape {
    kind: FailureKind,
    mode: Option<Mode>,
}

impl TryFrom<RawFailureShape> for FailureShape {
    type Error = String;

    fn try_from(raw: RawFailureShape) -> Result<Self, Self::Error> {
        match (raw.kind, raw.mode) {
            (FailureKind::Conflict, None) => Ok(FailureShape::Conflict),
            (FailureKind::Ci, None) => Ok(FailureShape::Ci),
            (FailureKind::Verify, Some(m)) => Ok(FailureShape::Verify(m)),
            (FailureKind::Timeout, Some(m)) => Ok(FailureShape::Timeout(m)),
            (kind, mode) => Err(format!("실패 {kind:?}에 mode {mode:?}가 올 수 없다")),
        }
    }
}

impl From<FailureShape> for RawFailureShape {
    fn from(shape: FailureShape) -> Self {
        let mode = match shape {
            FailureShape::Verify(m) | FailureShape::Timeout(m) => Some(m),
            FailureShape::Conflict | FailureShape::Ci => None,
        };
        RawFailureShape {
            kind: shape.kind(),
            mode,
        }
    }
}

/// A list with at least one element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonEmpty<T>(Vec<T>);

impl<T> NonEmpty<T> {
    pub fn new(items: Vec<T>) -> Option<NonEmpty<T>> {
        (!items.is_empty()).then_some(NonEmpty(items))
    }

    pub fn first(&self) -> &T {
        &self.0[0]
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl<T> std::ops::Deref for NonEmpty<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<T: Serialize> Serialize for NonEmpty<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for NonEmpty<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        NonEmpty::new(Vec::deserialize(d)?)
            .ok_or_else(|| serde::de::Error::custom("빈 목록이 올 수 없다"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictFile {
    pub path: String,
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
    pub failed_tests: Vec<String>,
    pub log: PathBuf,
    pub tree: Sha,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum VerifyMode {
    Solo,
    Combo { partners: NonEmpty<Partner> },
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
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub log: Option<PathBuf>,
    #[serde(default)]
    pub pushed_by_chute: bool,
    #[serde(default)]
    pub rechecked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseDoubt {
    pub base: Sha,
    pub prs: Vec<PrNumber>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvRecord {
    #[serde(default)]
    pub machine_id: Option<MachineId>,
    pub host: String,
    pub checks: Vec<EnvCheck>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvCheck {
    pub run: String,
    pub output: String,
    pub ok: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightResult {
    Pass,
    Fail,
    Conflict,
    /// The base commit itself fails verification, so no commit can be judged on it.
    BaseBroken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightRecord {
    pub head: Sha,
    pub result: PreflightResult,
    pub report: Option<PathBuf>,
    pub at_ms: EpochMs,
    /// The default-branch tip the run started from. Records written before it was kept have none.
    #[serde(default)]
    pub base: Option<Sha>,
    /// The queued PRs the head was verified together with.
    #[serde(default)]
    pub with: Vec<PrNumber>,
    #[serde(default)]
    pub env: Option<EnvRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    NotMerged,
    NoChecks,
    Config,
    History,
    Engine,
    Hub,
    FlakyChronic,
    Gc,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
    pub pr: Option<PrNumber>,
    pub at_ms: EpochMs,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub queue: Vec<QueueItem>,
    pub batch: Option<Batch>,
    pub blocked: Option<Blocked>,
    pub verified_bases: Vec<Sha>,
    pub preflights: BTreeMap<String, PreflightRecord>,
    pub warnings: Vec<Warning>,
    #[serde(default)]
    pub base_doubt: Option<BaseDoubt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRef {
    pub pr: PrNumber,
    pub commit: Sha,
    #[serde(default)]
    pub submitter: Option<Agent>,
    #[serde(default)]
    pub agent: Option<Agent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flaky {
    pub step: String,
    pub tests: Vec<String>,
    pub tree: Sha,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRecord {
    pub id: BatchId,
    pub base: Sha,
    pub prs: Vec<PrNumber>,
    pub started_ms: EpochMs,
    pub finished_ms: Option<EpochMs>,
    pub merges: Vec<MergeRef>,
    pub bounced: Vec<PrNumber>,
    pub flaky: Vec<Flaky>,
    pub verify_s: u64,
    #[serde(default)]
    pub pushed: Vec<Sha>,
    #[serde(default)]
    pub verifications: u32,
    #[serde(default)]
    pub env: Option<EnvRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertRecord {
    pub at_ms: EpochMs,
    pub tip: Sha,
    pub prs: Vec<RevertedPr>,
    pub verified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertedPr {
    pub pr: PrNumber,
    pub merge: Sha,
    pub revert: Sha,
}

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
    GaveUp {
        pr: PrNumber,
        reason: String,
    },
    Closed {
        pr: PrNumber,
    },
    NeedsHuman {
        pr: PrNumber,
        reason: HumanReason,
        detail: Option<String>,
    },
    Blocked {
        reason: BlockReason,
        base: Sha,
    },
    Submitted {
        pr: PrNumber,
        head: Sha,
        priority: bool,
        after: Vec<PrNumber>,
        repair: bool,
    },
    Dropped {
        pr: PrNumber,
    },
    Notified {
        kind: NotifyKind,
        pr: Option<PrNumber>,
        to: Option<TerminalId>,
        delivered: Delivered,
    },
    Resumed {
        base: Sha,
        by: ResumedBy,
        pr: Option<PrNumber>,
    },
    FlakyProbe {
        batch: BatchId,
        step: String,
        tests: Vec<String>,
        evidence: ProbeEvidence,
        runs: u32,
        failures: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyKind {
    Merged,
    Bounced,
    Human,
    Halted,
    Resumed,
    Reverted,
    Waiting,
    Flaky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivered {
    Sent,
    SameAsAgent,
    NoTerminal,
    Gone,
    Blocked,
    UnknownMachine,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumedBy {
    Repair,
    MainMoved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeEvidence {
    Base,
    Made,
    Unresolved,
}

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
    pub hub: HubView,
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
pub struct HubView {
    pub local: bool,
    pub machine_id: Option<MachineId>,
    pub host: Option<String>,
    pub machine: Option<String>,
    pub repo_path: Option<PathBuf>,
    pub reachable: bool,
    pub error: Option<String>,
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
    #[serde(default)]
    pub submitter: Option<AgentView>,
    #[serde(default)]
    pub priority: bool,
    #[serde(default)]
    pub after: Vec<PrNumber>,
    #[serde(default)]
    pub repair: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    pub terminal: TerminalId,
    pub name: Option<String>,
    pub alive: bool,
    pub machine_id: Option<MachineId>,
    pub host: Option<String>,
    pub machine: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightView {
    pub result: PreflightResult,
    pub head: Sha,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureView {
    #[serde(flatten)]
    pub shape: FailureShape,
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
    pub fn new(
        item: &QueueItem,
        budget: u32,
        preflight: Option<&PreflightRecord>,
        agent: Option<AgentView>,
        submitter: Option<AgentView>,
    ) -> ItemView {
        ItemView {
            pr: item.pr,
            title: item.title.clone(),
            branch: item.branch.clone(),
            head: item.head.clone(),
            state: item.state,
            agent,
            attempts: item.attempts,
            budget,
            submitted_ms: item.submitted_ms,
            touches_config: item.touches_config,
            preflight: preflight.map(|p| PreflightView {
                result: p.result,
                head: p.head.clone(),
            }),
            last_failure: item.last_failure.as_ref().map(|f| FailureView {
                shape: f.failure.shape(),
                summary: f.summary.clone(),
                report: f.report.clone(),
            }),
            submitter,
            priority: item.priority,
            after: item.after.clone(),
            repair: item.repair,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const FIXTURE: &str = include_str!("../tests/fixtures/status-full.json");
    const UNREACHABLE: &str = include_str!("../tests/fixtures/status-hub-unreachable.json");

    #[test]
    fn status_fixtures_round_trip() {
        for fixture in [FIXTURE, UNREACHABLE] {
            let original: Value = serde_json::from_str(fixture).unwrap();
            let view: StatusView = serde_json::from_value(original.clone()).unwrap();
            assert_eq!(serde_json::to_value(&view).unwrap(), original);
        }
    }

    #[test]
    fn status_fixture_has_a_remote_hub_and_agents_on_three_kinds_of_machine() {
        let view: StatusView = serde_json::from_str(FIXTURE).unwrap();
        assert!(!view.hub.local && view.hub.reachable && view.hub.error.is_none());
        assert_eq!(view.hub.machine.as_deref(), Some("desk"));
        let agents: Vec<&AgentView> = view.queue.iter().filter_map(|i| i.agent.as_ref()).collect();
        assert!(agents.iter().any(|a| a.machine.as_deref() == Some("desk")));
        assert!(
            agents
                .iter()
                .any(|a| a.machine.is_none() && a.host.is_some())
        );
        assert!(
            agents
                .iter()
                .any(|a| a.machine.is_none() && a.host.is_none())
        );
        assert!(agents.iter().all(|a| a.machine_id.is_some()));
    }

    #[test]
    fn status_fixture_of_an_unreachable_hub_has_no_queue_and_says_why() {
        let view: StatusView = serde_json::from_str(UNREACHABLE).unwrap();
        assert!(!view.hub.reachable && !view.hub.local);
        assert!(view.hub.error.is_some());
        assert!(view.queue.is_empty() && !view.daemon.running);
        assert!(view.warnings.iter().any(|w| w.kind == WarningKind::Hub));
    }

    #[test]
    fn a_v1_agent_without_a_machine_is_read_and_kept_without_one() {
        let v1 = json!({"terminal": "t1rs", "created_ms": 5, "name": "impl-a",
                        "workdir": "/w/cfg", "alive": false});
        let agent: Agent = serde_json::from_value(v1).unwrap();
        assert_eq!(agent.machine, None);
        let id = MachineId::try_from("9a0c".to_owned()).unwrap();
        let again: Agent = serde_json::from_value(
            serde_json::to_value(Agent {
                machine: Some(id.clone()),
                ..agent
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(again.machine, Some(id));
    }

    #[test]
    fn a_blank_machine_id_is_refused_when_read() {
        assert!(serde_json::from_value::<HubConfig>(json!({"machine_id": ""})).is_err());
        assert!(serde_json::from_value::<HubConfig>(json!({"machine_id": "a/b"})).is_err());
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
            .filter_map(|i| i.last_failure.as_ref().map(|f| f.shape.kind()))
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
        let submitted: Vec<EpochMs> = view.queue.iter().map(|i| i.submitted_ms).collect();
        assert!(submitted.is_sorted(), "큐는 submit 순서여야 한다");
        let merged: Vec<PrNumber> = view.recent.iter().flat_map(|r| r.merged.clone()).collect();
        assert!(
            view.queue.iter().all(|i| !merged.contains(&i.pr)),
            "머지된 PR은 큐에 남지 않는다"
        );
        assert!(view.blocked.is_some());
        assert!(view.batch.is_some());
        assert!(!view.warnings.is_empty());
    }

    #[test]
    fn status_fixture_covers_the_v3_fields() {
        let view: StatusView = serde_json::from_str(FIXTURE).unwrap();
        let item = |pr: u64| view.queue.iter().find(|i| i.pr == PrNumber(pr)).unwrap();
        assert!(view.queue.iter().any(|i| i.priority && !i.repair));
        assert!(view.queue.iter().any(|i| i.repair));
        let held = view.queue.iter().find(|i| !i.after.is_empty()).unwrap();
        assert!(
            held.after
                .iter()
                .all(|n| view.queue.iter().any(|i| i.pr == *n))
        );
        let apart = view
            .queue
            .iter()
            .filter(|i| i.submitter.is_some() && i.submitter != i.agent)
            .count();
        assert!(apart >= 1);
        let same = item(23);
        assert_eq!(same.submitter, same.agent);
        assert!(view.queue.iter().any(|i| i.submitter.is_none()));
        let blocked = view.blocked.unwrap();
        assert!(blocked.step.is_some() && blocked.log.is_some());
        assert!(blocked.pushed_by_chute && blocked.rechecked);
    }

    #[test]
    fn an_item_view_from_before_v3_reads_with_empty_new_fields() {
        let mut old: Value = serde_json::from_str(FIXTURE).unwrap();
        let first = old["queue"][0].as_object_mut().unwrap();
        for key in ["submitter", "priority", "after", "repair"] {
            first.remove(key);
        }
        old["blocked"] = json!({"reason": "base_broken", "base": "0a1b2c3", "since_ms": 1});
        let view: StatusView = serde_json::from_value(old).unwrap();
        let item = &view.queue[0];
        assert!(item.submitter.is_none() && !item.priority && !item.repair);
        assert!(item.after.is_empty());
        let blocked = view.blocked.unwrap();
        assert_eq!((blocked.step, blocked.log), (None, None));
        assert!(!blocked.pushed_by_chute && !blocked.rechecked);
    }

    #[test]
    fn a_v2_state_json_is_read_without_the_new_fields() {
        let v2 = json!({
            "queue": [{
                "pr": 4, "title": "t", "branch": "b", "head": "abc1234",
                "state": "queued", "state_reason": null, "agent": null,
                "attempts": 0, "submitted_ms": 1, "touches_config": false,
                "last_failure": null
            }],
            "batch": null,
            "blocked": {"reason": "base_broken", "base": "abc1234", "since_ms": 2},
            "verified_bases": ["abc1234"],
            "preflights": {"b": {
                "head": "abc1234", "result": "pass", "report": null, "at_ms": 3
            }},
            "warnings": []
        });
        let state: State = serde_json::from_value(v2).unwrap();
        let item = &state.queue[0];
        assert!(item.submitter.is_none() && !item.priority && !item.repair);
        assert!(item.after.is_empty());
        assert_eq!(state.base_doubt, None);
        let blocked = state.blocked.unwrap();
        assert!(blocked.step.is_none() && blocked.log.is_none());
        assert!(!blocked.pushed_by_chute && !blocked.rechecked);
        assert_eq!(state.preflights["b"].env, None);
    }

    #[test]
    fn a_v2_batch_record_is_read_without_the_new_fields() {
        let v2 = json!({
            "id": "b1", "base": "abc1234", "prs": [4], "started_ms": 1,
            "finished_ms": null,
            "merges": [{"pr": 4, "commit": "def5678"}],
            "bounced": [], "flaky": [], "verify_s": 0
        });
        let record: BatchRecord = serde_json::from_value(v2).unwrap();
        assert_eq!(record.env, None);
        assert!(record.merges[0].submitter.is_none() && record.merges[0].agent.is_none());
    }

    #[test]
    fn an_env_record_round_trips_on_batch_and_preflight_records() {
        let env = EnvRecord {
            machine_id: Some(MachineId::try_from("9a0c".to_owned()).unwrap()),
            host: "laptop-host".into(),
            checks: vec![
                EnvCheck {
                    run: "cargo --version".into(),
                    output: "cargo 1.90.0".into(),
                    ok: true,
                },
                EnvCheck {
                    run: "missing-tool".into(),
                    output: "sh: 1: missing-tool: not found".into(),
                    ok: false,
                },
            ],
        };
        let batch = json!({
            "id": "b1", "base": "abc1234", "prs": [], "started_ms": 1,
            "finished_ms": null, "merges": [], "bounced": [], "flaky": [],
            "verify_s": 0,
            "env": {"machine_id": "9a0c", "host": "laptop-host", "checks": [
                {"run": "cargo --version", "output": "cargo 1.90.0", "ok": true},
                {"run": "missing-tool", "output": "sh: 1: missing-tool: not found", "ok": false}
            ]}
        });
        let record: BatchRecord = serde_json::from_value(batch).unwrap();
        assert_eq!(record.env, Some(env.clone()));
        let again: BatchRecord =
            serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
        assert_eq!(again, record);
        let preflight = PreflightRecord {
            head: Sha::try_from("abc1234".to_owned()).unwrap(),
            result: PreflightResult::Pass,
            report: None,
            at_ms: EpochMs(1),
            base: None,
            with: Vec::new(),
            env: Some(env),
        };
        let back: PreflightRecord =
            serde_json::from_value(serde_json::to_value(&preflight).unwrap()).unwrap();
        assert_eq!(back, preflight);
    }

    #[test]
    fn the_new_event_kinds_round_trip_as_the_same_json() {
        let lines = [
            json!({"at_ms": 1, "event": "submitted", "pr": 4, "head": "abc1234",
                   "priority": true, "after": [2, 3], "repair": false}),
            json!({"at_ms": 2, "event": "dropped", "pr": 4}),
            json!({"at_ms": 3, "event": "notified", "kind": "merged", "pr": 4,
                   "to": "desk/t7", "delivered": "sent"}),
            json!({"at_ms": 4, "event": "notified", "kind": "halted", "pr": null,
                   "to": null, "delivered": "no_terminal"}),
            json!({"at_ms": 5, "event": "resumed", "base": "abc1234", "by": "repair", "pr": 9}),
            json!({"at_ms": 6, "event": "resumed", "base": "abc1234", "by": "main_moved",
                   "pr": null}),
            json!({"at_ms": 7, "event": "flaky_probe", "batch": "b1", "step": "test",
                   "tests": ["a::b"], "evidence": "made", "runs": 20, "failures": 1}),
        ];
        for line in lines {
            let event: Event = serde_json::from_value(line.clone()).unwrap();
            assert_eq!(serde_json::to_value(&event).unwrap(), line);
        }
    }

    #[test]
    fn every_notification_kind_delivery_and_evidence_has_one_spelling() {
        let kinds = [
            (NotifyKind::Merged, "merged"),
            (NotifyKind::Bounced, "bounced"),
            (NotifyKind::Human, "human"),
            (NotifyKind::Halted, "halted"),
            (NotifyKind::Resumed, "resumed"),
            (NotifyKind::Reverted, "reverted"),
            (NotifyKind::Waiting, "waiting"),
            (NotifyKind::Flaky, "flaky"),
        ];
        let deliveries = [
            (Delivered::Sent, "sent"),
            (Delivered::SameAsAgent, "same_as_agent"),
            (Delivered::NoTerminal, "no_terminal"),
            (Delivered::Gone, "gone"),
            (Delivered::Blocked, "blocked"),
            (Delivered::UnknownMachine, "unknown_machine"),
            (Delivered::Failed, "failed"),
        ];
        let evidence = [
            (ProbeEvidence::Base, "base"),
            (ProbeEvidence::Made, "made"),
            (ProbeEvidence::Unresolved, "unresolved"),
        ];
        for (kind, name) in kinds {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<NotifyKind>(json!(name)).unwrap(),
                kind
            );
        }
        for (delivered, name) in deliveries {
            assert_eq!(serde_json::to_value(delivered).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<Delivered>(json!(name)).unwrap(),
                delivered
            );
        }
        for (item, name) in evidence {
            assert_eq!(serde_json::to_value(item).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<ProbeEvidence>(json!(name)).unwrap(),
                item
            );
        }
        assert!(serde_json::from_value::<Delivered>(json!("delivered")).is_err());
    }

    #[test]
    fn the_new_warning_kinds_round_trip() {
        for (kind, name) in [
            (WarningKind::FlakyChronic, "flaky_chronic"),
            (WarningKind::Gc, "gc"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<WarningKind>(json!(name)).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn new_stored_fields_round_trip_in_the_state() {
        let sha = |s: &str| Sha::try_from(s.to_owned()).unwrap();
        let agent = |t: &str| Agent {
            terminal: TerminalId(t.into()),
            created_ms: EpochMs(5),
            name: None,
            workdir: "/w".into(),
            alive: true,
            machine: None,
        };
        let state = State {
            queue: vec![QueueItem {
                pr: PrNumber(8),
                title: "t".into(),
                branch: "b".into(),
                head: sha("abc1234"),
                state: ItemState::Queued(None),
                agent: Some(agent("t2")),
                attempts: 0,
                submitted_ms: EpochMs(1),
                touches_config: false,
                last_failure: None,
                submitter: Some(agent("t1")),
                priority: true,
                after: vec![PrNumber(3)],
                repair: true,
            }],
            blocked: Some(Blocked {
                reason: BlockReason::BaseBroken,
                base: sha("def5678"),
                since_ms: EpochMs(2),
                step: Some("test".into()),
                log: Some("/s/test.1.log".into()),
                pushed_by_chute: true,
                rechecked: true,
            }),
            base_doubt: Some(BaseDoubt {
                base: sha("def5678"),
                prs: vec![PrNumber(7)],
            }),
            ..State::default()
        };
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
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

    fn one<T>(item: T) -> NonEmpty<T> {
        NonEmpty::new(vec![item]).unwrap()
    }

    #[test]
    fn failure_view_rejects_a_mode_that_does_not_fit_its_kind() {
        let parse = |kind: &str, mode: Value| {
            serde_json::from_value::<FailureView>(
                json!({"kind": kind, "mode": mode, "summary": "s", "report": "/r"}),
            )
        };
        assert!(parse("conflict", json!("solo")).is_err());
        assert!(parse("ci", json!("combo")).is_err());
        assert!(parse("verify", Value::Null).is_err());
        assert!(parse("timeout", Value::Null).is_err());
        assert_eq!(
            parse("timeout", json!("combo")).unwrap().shape,
            FailureShape::Timeout(Mode::Combo)
        );
    }

    #[test]
    fn failures_need_their_files_checks_and_partners() {
        let record = |failure: Value| {
            let mut v = json!({"summary": "s", "report": "/r", "at_ms": 1});
            v.as_object_mut()
                .unwrap()
                .extend(failure.as_object().unwrap().clone());
            serde_json::from_value::<FailureRecord>(v)
        };
        let step = json!({"step": "t", "failed_tests": [], "log": "/l", "tree": "abc1234"});
        let with = |extra: Value| {
            let mut v = step.clone();
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            v
        };
        assert!(record(json!({"kind": "conflict", "files": []})).is_err());
        assert!(record(json!({"kind": "ci", "checks": []})).is_err());
        assert!(
            record(with(
                json!({"kind": "verify", "mode": "combo", "partners": []})
            ))
            .is_err()
        );
        assert!(record(with(json!({"kind": "verify", "mode": "solo"}))).is_ok());
    }

    #[test]
    fn slug_owner_cannot_make_folder_names_collide() {
        assert!(Slug::new("a__b", "c").is_err());
        assert!(Slug::new("a_", "b").is_err());
        assert_eq!(Slug::new("a", "_b").unwrap().dir_name(), "a___b");
        assert!(Slug::new("octocat_acme", "widget").is_ok());
        assert!(Slug::new("github.com", "solo").is_err());
        assert_eq!(Slug::new("a", "b__c").unwrap().dir_name(), "a__b__c");
        assert!(Slug::new("Soochol", "chute.rs_x-1").is_ok());
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
                files: one(ConflictFile {
                    path: "src/x.rs".into(),
                    main_commits: vec![CommitRef {
                        sha: sha("abc1234"),
                        title: "t".into(),
                    }],
                }),
            },
            Failure::Verify(step(VerifyMode::Solo)),
            Failure::Timeout(step(VerifyMode::Combo {
                partners: one(Partner {
                    pr: PrNumber(3),
                    title: "p".into(),
                    files: vec!["a".into()],
                    diff: "/s/d.diff".into(),
                }),
            })),
            Failure::Ci {
                checks: one(FailedCheck {
                    name: "build".into(),
                    link: "https://x".into(),
                }),
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
                    submitter: None,
                    priority: false,
                    after: Vec::new(),
                    repair: false,
                })
                .collect(),
            ..State::default()
        };
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
    }
}
