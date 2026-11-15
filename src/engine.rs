use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::Deserialize;

use crate::config::Config;
use crate::ext::Ext;
use crate::feedback::{Delivery, Feedback, Notice, Subject};
use crate::gitops::{
    FAILED_REF_PREFIX, Gitops, ItemResult, MergeItem, PrHead, PushOutcome, Slot, SlotLock, SlotName,
};
use crate::model::{
    Batch, BatchId, BatchRecord, BlockReason, Blocked, BounceReason, EpochMs, Event, EventKind,
    Failure, Flaky, HumanReason, ItemState, MergeRef, NonEmpty, Phase, PrNumber, QueueItem,
    RepoInfo, Sha, State, StepFailure, VerifyMode, Warning, WarningKind,
};
use crate::queue;
use crate::store::{Home, RepoStore};
use crate::verify::{self, Cancel, Cause, Detail, Failed, FlakyStep, Options, Outcome};

const PUSH_ROUNDS: u32 = 3;
const SLOT_LOCK_TRIES: u32 = 40;
const SLOT_LOCK_PAUSE: Duration = Duration::from_millis(50);
const VERIFIED_BASES_KEPT: usize = 100;

#[derive(Clone, Debug)]
pub struct Timing {
    pub merged_check: Duration,
    pub merged_poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            merged_check: Duration::from_secs(20),
            merged_poll: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pass {
    Idle,
    Blocked,
    Cancelled,
    Done(Summary),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub batch: BatchId,
    pub merged: Vec<PrNumber>,
    pub bounced: Vec<PrNumber>,
}

#[derive(Debug, thiserror::Error)]
enum Halt {
    #[error("검증이 취소됐다")]
    Cancelled,
    #[error("기준 커밋이 검증을 통과하지 못해 큐를 멈췄다")]
    Blocked,
}

pub struct Engine {
    ext: Ext,
    home: Home,
    info: RepoInfo,
    store: RepoStore,
    gitops: Gitops,
    worktrees_root: PathBuf,
    cancel: Cancel,
    timing: Timing,
}

impl Engine {
    pub fn new(ext: Ext, home: Home, info: RepoInfo, worktrees_root: PathBuf) -> Engine {
        let store = home.repo(&info.slug);
        let gitops = Gitops::new(&ext, &home, &info);
        Engine {
            ext,
            home,
            info,
            store,
            gitops,
            worktrees_root,
            cancel: Cancel::new(),
            timing: Timing::default(),
        }
    }

    pub fn with_cancel(mut self, cancel: Cancel) -> Engine {
        self.cancel = cancel;
        self
    }

    pub fn with_timing(mut self, timing: Timing) -> Engine {
        self.timing = timing;
        self
    }

    fn batch_slot_dir(&self) -> PathBuf {
        let dir = self.home.slots_dir(&self.info.slug).join("batch");
        std::path::absolute(&dir).unwrap_or(dir)
    }

    /// A process this one starts holds every lock file this one has open until it execs, so
    /// a slot that looks held may be free a moment later.
    fn lock_batch_slot(&self) -> Result<SlotLock> {
        for _ in 0..SLOT_LOCK_TRIES {
            if let Some(lock) = self.gitops.lock_slot(SlotName::Batch)? {
                return Ok(lock);
            }
            thread::sleep(SLOT_LOCK_PAUSE);
        }
        bail!("batch 슬롯을 다른 프로세스가 쥐고 있다")
    }

    fn history(&self, kind: EventKind) -> Result<()> {
        self.store.append_history(&Event {
            at_ms: EpochMs::now(),
            kind,
        })
    }

    fn load_config(&self) -> Result<Config> {
        let branch = &self.info.default_branch;
        Config::load_remote_branch(&self.ext, &self.info.path, branch)?.ok_or_else(|| {
            anyhow!("chute.toml을 origin/{branch}에서 읽지 못했다. 기본 브랜치에 올렸는지 확인한다")
        })
    }

    /// Cleans up what an earlier run left behind. It can run any number of times and at any
    /// point after a crash, and ends in the same state: `in_batch` items go back to the queue
    /// unless their head is already in the default branch, in which case they are merged.
    /// The caller holds the daemon lock, so no verification of this repository is alive and
    /// whatever still runs in the batch slot is an orphan.
    pub fn recover(&self) -> Result<()> {
        let lock = self.lock_batch_slot()?;
        reap_orphans(&self.batch_slot_dir())?;
        let slot = lock.open()?;
        slot.fetch_base()?;
        let state = self.store.load()?;
        let main = format!("refs/remotes/origin/{}", self.info.default_branch);
        let mut settled: Vec<(QueueItem, Option<Sha>)> = Vec::new();
        for item in state.queue.iter().filter(|i| i.state == ItemState::InBatch) {
            if self.is_ancestor(&item.head, &main)? {
                settled.push((item.clone(), self.merge_commit_of(item, &main)?));
            }
        }
        // The queue is written last, so a run cut short anywhere in here still finds its
        // `batch` and `in_batch` items, and repeats what is left. Each step before it can be
        // repeated without a trace.
        let batch_id = state.batch.as_ref().map(|b| b.id.clone());
        let id_for_events = batch_id.clone().unwrap_or(BatchId(String::new()));
        let events = self.store.history()?.events;
        let mut merged_prs: Vec<PrNumber> = Vec::new();
        let mut bounced_prs: Vec<PrNumber> = Vec::new();
        let mut finished_already = false;
        for event in &events {
            match &event.kind {
                EventKind::Merged { pr, batch, .. } if *batch == id_for_events => {
                    merged_prs.push(*pr)
                }
                EventKind::Bounced {
                    pr,
                    batch: Some(batch),
                    ..
                } if *batch == id_for_events => bounced_prs.push(*pr),
                EventKind::BatchFinished { batch, .. } if *batch == id_for_events => {
                    finished_already = true
                }
                _ => {}
            }
        }
        for (item, commit) in &settled {
            if !merged_prs.contains(&item.pr) {
                self.history(EventKind::Merged {
                    pr: item.pr,
                    batch: id_for_events.clone(),
                    commit: commit.clone().unwrap_or_else(|| item.head.clone()),
                    submitted_ms: item.submitted_ms,
                })?;
                merged_prs.push(item.pr);
            }
        }
        if let Some(id) = &batch_id {
            let record = match self.store.read_batch(id) {
                Ok(record) => record,
                Err(e) => {
                    eprintln!("chute: batch 기록을 읽지 못해 건너뛴다: {e:#}");
                    None
                }
            };
            if let Some(mut record) = record
                && record.finished_ms.is_none()
            {
                let now = EpochMs::now();
                for (item, commit) in &settled {
                    let known = record.merges.iter().any(|m| m.pr == item.pr);
                    if let (Some(commit), false) = (commit, known) {
                        record.merges.push(MergeRef {
                            pr: item.pr,
                            commit: commit.clone(),
                        });
                    }
                }
                for pr in &record.bounced {
                    if !bounced_prs.contains(pr) {
                        bounced_prs.push(*pr);
                    }
                }
                for merge in &record.merges {
                    if !merged_prs.contains(&merge.pr) {
                        merged_prs.push(merge.pr);
                    }
                }
                record.bounced = bounced_prs.clone();
                if !finished_already && (!merged_prs.is_empty() || !bounced_prs.is_empty()) {
                    self.history(EventKind::BatchFinished {
                        batch: id.clone(),
                        merged: merged_prs,
                        bounced: bounced_prs,
                        seconds: now.0.saturating_sub(record.started_ms.0) / 1000,
                        verify_s: record.verify_s,
                    })?;
                }
                record.finished_ms = Some(now);
                self.store.write_batch(&record)?;
            }
        }
        self.store.update(|s| {
            for (item, _) in &settled {
                queue::merged(s, item.pr)?;
            }
            let leftovers: Vec<PrNumber> = s
                .queue
                .iter()
                .filter(|i| i.state == ItemState::InBatch)
                .map(|i| i.pr)
                .collect();
            for pr in leftovers {
                queue::requeue(s, pr)?;
            }
            s.batch = None;
            Ok(())
        })?;
        self.prune_failed_refs()
    }

    fn is_ancestor(&self, commit: &Sha, of: &str) -> Result<bool> {
        let out = self.ext.git(
            &self.info.path,
            ["merge-base", "--is-ancestor", commit.as_str(), of],
        )?;
        Ok(out.code == Some(0))
    }

    fn merge_commit_of(&self, item: &QueueItem, main: &str) -> Result<Option<Sha>> {
        let title = format!("Merge pull request #{} from ", item.pr.0);
        let out = self.ext.git(
            &self.info.path,
            [
                "log",
                "--first-parent",
                "--fixed-strings",
                "--format=%H%x09%s",
                &format!("--grep={title}"),
                main,
            ],
        )?;
        let found = out
            .stdout_text()
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .find(|(_, subject)| subject.starts_with(&title))
            .and_then(|(sha, _)| Sha::try_from(sha.to_owned()).ok());
        Ok(found)
    }

    fn prune_failed_refs(&self) -> Result<()> {
        let queued: BTreeSet<u64> = self.store.load()?.queue.iter().map(|i| i.pr.0).collect();
        let listed = self
            .ext
            .git(
                &self.info.path,
                ["for-each-ref", "--format=%(refname)", FAILED_REF_PREFIX],
            )?
            .ok()?;
        for full in listed.lines() {
            let name = full.strip_prefix(FAILED_REF_PREFIX).unwrap_or(full);
            let pr = name
                .strip_prefix("pr")
                .and_then(|rest| rest.split('-').next())
                .and_then(|n| n.parse::<u64>().ok());
            if pr.is_some_and(|n| !queued.contains(&n)) {
                self.gitops.delete_failed_ref(name)?;
            }
        }
        Ok(())
    }

    pub fn run_once(&self) -> Result<Pass> {
        let waiting =
            |s: &crate::model::State| s.queue.iter().any(|i| i.state == ItemState::Queued(None));
        if !waiting(&self.store.load()?) {
            return Ok(Pass::Idle);
        }
        let slot = self.lock_batch_slot()?.open()?;
        let base = slot.fetch_base()?;
        if let Some(blocked) = self.store.load()?.blocked {
            if blocked.base == base {
                return Ok(Pass::Blocked);
            }
            self.store.update(|s| {
                s.blocked = None;
                Ok(())
            })?;
        }
        let config = self.load_config()?;
        let id = self.new_batch_id();
        let taken: Vec<QueueItem> = self.store.update(|s| {
            let prs = queue::take_batch(s, config.max_batch);
            if !prs.is_empty() {
                s.batch = Some(Batch {
                    id: id.clone(),
                    base: base.clone(),
                    prs: prs.clone(),
                    phase: Phase::Building,
                    step: None,
                    round: 0,
                    started_ms: EpochMs::now(),
                });
            }
            Ok(s.queue
                .iter()
                .filter(|i| prs.contains(&i.pr))
                .cloned()
                .collect())
        })?;
        if taken.is_empty() {
            return Ok(Pass::Idle);
        }
        let mut run = Run {
            engine: self,
            slot,
            config,
            record: BatchRecord {
                id: id.clone(),
                base: base.clone(),
                prs: taken.iter().map(|i| i.pr).collect(),
                started_ms: EpochMs::now(),
                finished_ms: None,
                merges: Vec::new(),
                bounced: Vec::new(),
                flaky: Vec::new(),
                verify_s: 0,
                pushed: Vec::new(),
                verifications: 0,
            },
            start_base: base,
            items: taken.iter().map(|i| (i.pr, i.clone())).collect(),
            fetched: BTreeSet::new(),
            conflicted: BTreeSet::new(),
            sent_back: BTreeSet::new(),
            landed: Vec::new(),
            merged: Vec::new(),
            bounced: Vec::new(),
            bisecting: false,
            verify_time: Duration::ZERO,
        };
        let result = run.execute(&taken);
        let wound = run.wind_down();
        let summary = Summary {
            batch: id,
            merged: run.merged.clone(),
            bounced: run.bounced.clone(),
        };
        match result {
            Ok(()) => wound.map(|()| Pass::Done(summary)),
            Err(e) => match e.downcast::<Halt>() {
                Ok(Halt::Cancelled) => wound.map(|()| Pass::Cancelled),
                Ok(Halt::Blocked) => wound.map(|()| Pass::Blocked),
                Err(e) => Err(e),
            },
        }
    }

    fn new_batch_id(&self) -> BatchId {
        let stem = Local::now().format("b%Y%m%d-%H%M%S").to_string();
        let mut id = BatchId(stem.clone());
        for n in 2.. {
            if !self.store.batch_dir(&id).exists() {
                break;
            }
            id = BatchId(format!("{stem}-{n}"));
        }
        id
    }

    fn view(&self, pr: PrNumber) -> Result<PrView> {
        let text = self
            .ext
            .gh(
                &self.info.path,
                [
                    "pr",
                    "view",
                    &pr.0.to_string(),
                    "--json",
                    "state,headRefOid,mergeCommit",
                ],
            )?
            .ok()
            .with_context(|| format!("{pr}를 gh로 읽지 못했다"))?;
        serde_json::from_str(&text).with_context(|| format!("gh pr view {pr}의 출력을 읽지 못했다"))
    }

    fn inspect(&self, item: &QueueItem) -> Result<Inspection> {
        let view = self.view(item.pr)?;
        Ok(match view.state {
            PrState::Closed => Inspection::Closed,
            PrState::Merged => Inspection::Merged {
                commit: view
                    .merge_commit
                    .map_or_else(|| item.head.clone(), |m| m.oid),
            },
            PrState::Open if view.head_ref_oid != item.head => Inspection::HeadChanged {
                current: view.head_ref_oid,
            },
            PrState::Open => Inspection::Ready,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrView {
    state: PrState,
    head_ref_oid: Sha,
    merge_commit: Option<MergeCommit>,
}

#[derive(Deserialize)]
struct MergeCommit {
    oid: Sha,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
enum PrState {
    Open,
    Closed,
    Merged,
}

enum Inspection {
    Ready,
    Closed,
    Merged { commit: Sha },
    HeadChanged { current: Sha },
}

#[derive(Clone, Debug)]
struct Landed {
    pr: PrNumber,
    title: String,
}

struct Build {
    base: Sha,
    tip: Sha,
    landing: Vec<(PrNumber, Sha)>,
    members: Vec<PrNumber>,
}

enum Verdict {
    Pass,
    Fail(Failed),
}

enum Alone {
    Pass,
    Fail { failed: Failed, tip: Sha },
    Conflict,
}

enum Blame {
    Solo { base: Sha },
    Combo(Vec<Landed>),
}

struct Run<'e> {
    engine: &'e Engine,
    slot: Slot,
    config: Config,
    record: BatchRecord,
    start_base: Sha,
    items: BTreeMap<PrNumber, QueueItem>,
    fetched: BTreeSet<PrNumber>,
    conflicted: BTreeSet<PrNumber>,
    sent_back: BTreeSet<PrNumber>,
    landed: Vec<Landed>,
    merged: Vec<PrNumber>,
    bounced: Vec<PrNumber>,
    bisecting: bool,
    verify_time: Duration,
}

impl Run<'_> {
    fn id(&self) -> &BatchId {
        &self.record.id
    }

    fn execute(&mut self, taken: &[QueueItem]) -> Result<()> {
        self.save_record()?;
        let group = self.admit(taken)?;
        self.solve(group)?;
        self.recheck_conflicts()
    }

    fn admit(&mut self, taken: &[QueueItem]) -> Result<Vec<PrNumber>> {
        let mut ready = Vec::new();
        for item in taken {
            match self.engine.inspect(item)? {
                Inspection::Ready => ready.push(item.pr),
                Inspection::Closed => {
                    self.engine
                        .store
                        .update(|s| Ok(queue::closed(s, item.pr)?))?;
                    self.engine.history(EventKind::Closed { pr: item.pr })?;
                }
                Inspection::Merged { commit } => {
                    self.engine.history(EventKind::Merged {
                        pr: item.pr,
                        batch: self.id().clone(),
                        commit,
                        submitted_ms: item.submitted_ms,
                    })?;
                    self.engine
                        .store
                        .update(|s| Ok(queue::merged(s, item.pr)?))?;
                    self.merged.push(item.pr);
                }
                Inspection::HeadChanged { current } => self.head_changed(item, &current)?,
            }
        }
        Ok(ready)
    }

    fn head_changed(&mut self, item: &QueueItem, current: &Sha) -> Result<()> {
        let pr = item.pr;
        let notice = Notice::HeadChanged {
            registered: &item.head,
            current,
        };
        let (_, unreachable) = self.deliver_and_record(item, &notice, |s| {
            queue::head_changed(s, pr)?;
            Ok(ItemState::Bounced(BounceReason::HeadChanged))
        })?;
        self.bounced.push(pr);
        self.sent_back.insert(pr);
        self.conflicted.remove(&pr);
        self.save_record()?;
        self.engine.history(EventKind::Bounced {
            pr,
            batch: Some(self.id().clone()),
            reason: BounceReason::HeadChanged,
            attempts: item.attempts,
        })?;
        self.note_unreachable(pr, unreachable)
    }

    fn feedback(&self) -> Feedback<'_> {
        Feedback {
            ext: &self.engine.ext,
            repo: &self.engine.info,
            store: &self.engine.store,
            config: &self.config,
            worktrees_root: &self.engine.worktrees_root,
        }
    }

    fn deliver_and_record(
        &self,
        item: &QueueItem,
        notice: &Notice<'_>,
        record: impl FnOnce(&mut State) -> Result<ItemState>,
    ) -> Result<(ItemState, Option<String>)> {
        let delivery = self.feedback().deliver(item, notice);
        let pr = item.pr;
        self.engine.store.update(|s| {
            let to = record(s)?;
            let mut unreachable = None;
            match &delivery {
                Delivery::Sent { .. } => {}
                Delivery::Spawned(agent) => queue::record_agent(s, pr, agent.clone())?,
                Delivery::Unreachable(why) => {
                    queue::unreachable(s, pr)?;
                    unreachable = Some(why.to_string());
                }
            }
            Ok((to, unreachable))
        })
    }

    fn note_unreachable(&self, pr: PrNumber, why: Option<String>) -> Result<()> {
        match why {
            Some(detail) => self.engine.history(EventKind::NeedsHuman {
                pr,
                reason: HumanReason::Unreachable,
                detail: Some(detail),
            }),
            None => Ok(()),
        }
    }

    fn bounce(&mut self, pr: PrNumber, failure: Failure, failed_ref: Option<&str>) -> Result<()> {
        let engine = self.engine;
        let item = self.items[&pr].clone();
        let written = self
            .feedback()
            .write_report(&Subject::from(&item), &failure, failed_ref)?;
        let budget = self.config.budget;
        let record = |s: &mut State| Ok(queue::record_failure(s, pr, written.record(), budget)?);
        let (to, unreachable) = if item.attempts < budget {
            self.deliver_and_record(&item, &Notice::Bounced(&written), record)?
        } else {
            (engine.store.update(record)?, None)
        };
        self.bounced.push(pr);
        self.conflicted.remove(&pr);
        self.save_record()?;
        match to {
            ItemState::Bounced(reason) => engine.history(EventKind::Bounced {
                pr,
                batch: Some(self.id().clone()),
                reason,
                attempts: item.attempts + 1,
            })?,
            ItemState::NeedsHuman(reason) => engine.history(EventKind::NeedsHuman {
                pr,
                reason,
                detail: Some(format!("재시도 예산 {budget}번을 모두 썼다")),
            })?,
            other => bail!("{pr}의 실패 처리가 뜻밖의 상태로 끝났다: {other:?}"),
        }
        self.note_unreachable(pr, unreachable)
    }

    fn save_record(&mut self) -> Result<()> {
        self.record.bounced = self.bounced.clone();
        self.record.verify_s = self.verify_time.as_secs();
        self.engine.store.write_batch(&self.record)
    }

    fn set_phase(&self, phase: Phase) -> Result<()> {
        self.update_batch(|b| {
            b.phase = phase;
            b.step = None;
        })
    }

    fn update_batch(&self, change: impl FnOnce(&mut Batch)) -> Result<()> {
        self.engine.store.update(|s| {
            if let Some(batch) = &mut s.batch {
                change(batch);
            }
            Ok(())
        })
    }

    fn merge_item(&self, pr: PrNumber) -> MergeItem {
        let item = &self.items[&pr];
        MergeItem::Pr {
            pr,
            branch: item.branch.clone(),
            title: item.title.clone(),
            head: item.head.clone(),
        }
    }

    fn fetch_heads(&mut self, group: &[PrNumber]) -> Result<Vec<PrNumber>> {
        let mut fit = Vec::new();
        for &pr in group {
            if self.sent_back.contains(&pr) {
                continue;
            }
            if self.fetched.contains(&pr) {
                fit.push(pr);
                continue;
            }
            let item = self.items[&pr].clone();
            match self.slot.fetch_pr_head(pr, &item.head)? {
                PrHead::Unchanged => {
                    self.fetched.insert(pr);
                    fit.push(pr);
                }
                PrHead::Changed { actual } => self.head_changed(&item, &actual)?,
            }
        }
        Ok(fit)
    }

    fn build(&mut self, group: &[PrNumber]) -> Result<Build> {
        self.set_phase(if self.bisecting {
            Phase::Bisecting
        } else {
            Phase::Building
        })?;
        let base = self.slot.fetch_base()?;
        let fit = self.fetch_heads(group)?;
        let merge_items: Vec<MergeItem> = fit.iter().map(|pr| self.merge_item(*pr)).collect();
        let result = self.slot.merge(&base, &merge_items)?;
        let mut landing = Vec::new();
        let mut members = Vec::new();
        for outcome in result.items {
            let MergeItem::Pr { pr, .. } = outcome.item else {
                bail!("PR이 아닌 항목이 batch에 있다");
            };
            match outcome.result {
                ItemResult::Merged { commit } => {
                    self.conflicted.remove(&pr);
                    landing.push((pr, commit));
                    members.push(pr);
                }
                ItemResult::AlreadyMerged => {
                    self.conflicted.remove(&pr);
                    members.push(pr);
                }
                ItemResult::Conflict(_) => {
                    self.conflicted.insert(pr);
                }
            }
        }
        Ok(Build {
            base,
            tip: result.tip,
            landing,
            members,
        })
    }

    fn solve(&mut self, group: Vec<PrNumber>) -> Result<()> {
        if group.is_empty() {
            return Ok(());
        }
        for _ in 0..PUSH_ROUNDS {
            let b = self.build(&group)?;
            if b.landing.is_empty() {
                return self.land(&b);
            }
            match self.verify(&b.tip)? {
                Verdict::Pass => {
                    self.set_phase(Phase::Pushing)?;
                    if self.slot.head()? != b.tip {
                        bail!(
                            "검증한 커밋 {}이 슬롯의 HEAD가 아니라 push하지 않았다",
                            b.tip
                        );
                    }
                    match self.slot.push(&b.tip)? {
                        PushOutcome::Pushed => return self.land(&b),
                        PushOutcome::MainMoved => continue,
                    }
                }
                Verdict::Fail(failed) => return self.split_or_convict(&b, failed),
            }
        }
        bail!(
            "기본 브랜치가 계속 움직여 {PUSH_ROUNDS}번 다시 만들었지만 push하지 못했다. 큐에 되돌리고 다시 시도한다"
        )
    }

    fn split_or_convict(&mut self, b: &Build, failed: Failed) -> Result<()> {
        match b.members.as_slice() {
            [] => Ok(()),
            [only] => self.convict(*only, b, failed),
            members => {
                self.bisecting = true;
                let (front, back) = members.split_at(members.len() / 2);
                self.solve(front.to_vec())?;
                self.solve(back.to_vec())
            }
        }
    }

    fn verify(&mut self, tree: &Sha) -> Result<Verdict> {
        self.record.verifications += 1;
        let round = self.record.verifications;
        self.update_batch(|b| {
            b.round = round;
            b.step = None;
            b.phase = if b.phase == Phase::Building {
                Phase::Verifying
            } else {
                b.phase
            };
        })?;
        let engine = self.engine;
        let on_step = |name: &str| {
            let _ = engine.store.update(|s| {
                if let Some(batch) = &mut s.batch {
                    batch.step = Some(name.to_owned());
                }
                Ok(())
            });
        };
        let log_dir = engine
            .store
            .batch_dir(self.id())
            .join(format!("verify-{round}"));
        let started = Instant::now();
        let outcome = verify::run(
            &self.config,
            &engine.store,
            self.slot.path(),
            &log_dir,
            &Options {
                cancel: engine.cancel.clone(),
                on_step: Some(&on_step),
                ..Options::default()
            },
        )?;
        self.verify_time += started.elapsed();
        match outcome {
            Outcome::Passed { flaky } => {
                self.record_flaky(flaky, tree)?;
                Ok(Verdict::Pass)
            }
            Outcome::Failed(failed) => Ok(Verdict::Fail(failed)),
            Outcome::Cancelled => Err(Halt::Cancelled.into()),
        }
    }

    fn record_flaky(&mut self, flaky: Vec<FlakyStep>, tree: &Sha) -> Result<()> {
        for step in flaky {
            self.engine.history(EventKind::Flaky {
                batch: self.id().clone(),
                step: step.step.clone(),
                tests: step.tests.clone(),
                tree: tree.clone(),
            })?;
            self.record.flaky.push(Flaky {
                step: step.step,
                tests: step.tests,
                tree: tree.clone(),
            });
        }
        Ok(())
    }

    /// The PRs of `b` are in the default branch now. The record and the history come before
    /// the queue changes, so a run cut short in here is finished by `recover`, which sees the
    /// PRs still `in_batch` and finds the events already written.
    fn land(&mut self, b: &Build) -> Result<()> {
        let engine = self.engine;
        let pushed = !b.landing.is_empty();
        let commits: BTreeMap<PrNumber, Sha> = b.landing.iter().cloned().collect();
        if pushed {
            self.record.pushed.push(b.tip.clone());
        }
        for (pr, commit) in &b.landing {
            self.record.merges.push(MergeRef {
                pr: *pr,
                commit: commit.clone(),
            });
        }
        self.save_record()?;
        for pr in &b.members {
            let item = &self.items[pr];
            engine.history(EventKind::Merged {
                pr: *pr,
                batch: self.id().clone(),
                commit: commits.get(pr).unwrap_or(&item.head).clone(),
                submitted_ms: item.submitted_ms,
            })?;
        }
        engine.store.update(|s| {
            for pr in &b.members {
                queue::merged(s, *pr)?;
            }
            if pushed {
                s.verified_bases.push(b.tip.clone());
                let excess = s.verified_bases.len().saturating_sub(VERIFIED_BASES_KEPT);
                s.verified_bases.drain(..excess);
            }
            Ok(())
        })?;
        for pr in &b.members {
            if commits.contains_key(pr) {
                self.landed.push(Landed {
                    pr: *pr,
                    title: self.items[pr].title.clone(),
                });
            }
            self.merged.push(*pr);
        }
        Ok(())
    }

    fn convict(&mut self, pr: PrNumber, b: &Build, failed: Failed) -> Result<()> {
        let together = (failed.clone(), b.tip.clone());
        let (failed, tip, blame) = if b.base == self.start_base || self.landed.is_empty() {
            (
                failed,
                b.tip.clone(),
                Blame::Solo {
                    base: b.base.clone(),
                },
            )
        } else {
            let start = self.start_base.clone();
            match self.check_alone(pr, &start)? {
                Alone::Pass => (together.0, together.1, Blame::Combo(self.landed.clone())),
                Alone::Conflict => (
                    together.0,
                    together.1,
                    Blame::Solo {
                        base: b.base.clone(),
                    },
                ),
                Alone::Fail {
                    failed: alone,
                    tip: alone_tip,
                } => {
                    if self.base_sound(&start)? {
                        (alone, alone_tip, Blame::Solo { base: start })
                    } else {
                        (
                            together.0,
                            together.1,
                            Blame::Solo {
                                base: b.base.clone(),
                            },
                        )
                    }
                }
            }
        };
        match &blame {
            Blame::Solo { base } => self.require_sound_base(base)?,
            Blame::Combo(_) => self.require_sound_base(&b.base)?,
        }
        let item = self.items[&pr].clone();
        let mode = match blame {
            Blame::Solo { .. } => VerifyMode::Solo,
            Blame::Combo(partners) => {
                let subject = Subject::from(&item);
                let described = partners
                    .iter()
                    .map(|p| {
                        self.feedback()
                            .partner_or_placeholder(&subject, &tip, p.pr, &p.title)
                    })
                    .collect::<Result<Vec<_>>>()?;
                VerifyMode::Combo {
                    partners: NonEmpty::new(described)
                        .ok_or_else(|| anyhow!("조합 실패인데 상대 PR이 없다"))?,
                }
            }
        };
        let failure = step_failure(&failed, mode, &tip);
        let name = format!("pr{}-{}", pr.0, self.id().0);
        self.engine.gitops.set_failed_ref(&name, &tip)?;
        self.bounce(pr, failure, Some(&name))
    }

    fn check_alone(&mut self, pr: PrNumber, base: &Sha) -> Result<Alone> {
        let item = self.merge_item(pr);
        let result = self.slot.merge(base, &[item])?;
        match result.items.into_iter().next().map(|o| o.result) {
            Some(ItemResult::Merged { .. }) => match self.verify(&result.tip)? {
                Verdict::Pass => Ok(Alone::Pass),
                Verdict::Fail(failed) => Ok(Alone::Fail {
                    failed,
                    tip: result.tip,
                }),
            },
            Some(ItemResult::AlreadyMerged) => Ok(Alone::Pass),
            Some(ItemResult::Conflict(_)) => Ok(Alone::Conflict),
            None => bail!("합치기 결과가 비어 있다"),
        }
    }

    fn base_sound(&mut self, base: &Sha) -> Result<bool> {
        let engine = self.engine;
        if engine.store.load()?.verified_bases.contains(base) {
            return Ok(true);
        }
        self.slot.checkout(base)?;
        match self.verify(base)? {
            Verdict::Pass => {
                engine.store.update(|s| {
                    s.verified_bases.push(base.clone());
                    Ok(())
                })?;
                Ok(true)
            }
            Verdict::Fail(_) => Ok(false),
        }
    }

    fn require_sound_base(&mut self, base: &Sha) -> Result<()> {
        if self.base_sound(base)? {
            return Ok(());
        }
        let engine = self.engine;
        engine.store.update(|s| {
            s.blocked = Some(Blocked {
                reason: BlockReason::BaseBroken,
                base: base.clone(),
                since_ms: EpochMs::now(),
            });
            Ok(())
        })?;
        engine.history(EventKind::Blocked {
            reason: BlockReason::BaseBroken,
            base: base.clone(),
        })?;
        Err(Halt::Blocked.into())
    }

    fn recheck_conflicts(&mut self) -> Result<()> {
        let conflicted: Vec<PrNumber> = std::mem::take(&mut self.conflicted).into_iter().collect();
        if conflicted.is_empty() {
            return Ok(());
        }
        let engine = self.engine;
        for pr in conflicted {
            let base = self.slot.fetch_base()?;
            let item = self.merge_item(pr);
            let result = self.slot.merge(&base, &[item])?;
            match result.items.into_iter().next().map(|o| o.result) {
                Some(ItemResult::Conflict(files)) => {
                    self.bounce(pr, Failure::Conflict { files }, None)?;
                }
                Some(ItemResult::AlreadyMerged) => {
                    let held = self.items[&pr].clone();
                    engine.history(EventKind::Merged {
                        pr,
                        batch: self.id().clone(),
                        commit: held.head.clone(),
                        submitted_ms: held.submitted_ms,
                    })?;
                    engine.store.update(|s| Ok(queue::merged(s, pr)?))?;
                    self.merged.push(pr);
                }
                Some(ItemResult::Merged { .. }) => {
                    engine.store.update(|s| Ok(queue::requeue(s, pr)?))?;
                }
                None => bail!("합치기 결과가 비어 있다"),
            }
        }
        Ok(())
    }

    fn wind_down(&mut self) -> Result<()> {
        let engine = self.engine;
        let prs: Vec<PrNumber> = self.items.keys().copied().collect();
        engine.store.update(|s| {
            for pr in &prs {
                let held = s.queue.iter().find(|i| i.pr == *pr);
                if held.is_some_and(|i| i.state == ItemState::InBatch) {
                    queue::requeue(s, *pr)?;
                }
            }
            s.batch = None;
            Ok(())
        })?;
        let now = EpochMs::now();
        self.record.finished_ms = Some(now);
        self.record.verify_s = self.verify_time.as_secs();
        self.save_record()?;
        if !self.merged.is_empty() || !self.bounced.is_empty() {
            engine.history(EventKind::BatchFinished {
                batch: self.id().clone(),
                merged: self.merged.clone(),
                bounced: self.bounced.clone(),
                seconds: now.0.saturating_sub(self.record.started_ms.0) / 1000,
                verify_s: self.record.verify_s,
            })?;
        }
        engine.prune_failed_refs()?;
        reap_orphans(&engine.batch_slot_dir())?;
        if engine.cancel.is_cancelled() {
            return Ok(());
        }
        self.confirm_merged()
    }

    fn confirm_merged(&self) -> Result<()> {
        let engine = self.engine;
        let mut waiting: Vec<PrNumber> = self.landed.iter().map(|l| l.pr).collect();
        let deadline = Instant::now() + engine.timing.merged_check;
        loop {
            waiting.retain(|pr| !engine.view(*pr).is_ok_and(|v| v.state == PrState::Merged));
            if waiting.is_empty() || Instant::now() >= deadline {
                break;
            }
            thread::sleep(engine.timing.merged_poll);
        }
        if waiting.is_empty() {
            return Ok(());
        }
        engine.store.update(|s| {
            for pr in &waiting {
                s.warnings
                    .retain(|w| !(w.kind == WarningKind::NotMerged && w.pr == Some(*pr)));
                s.warnings.push(Warning {
                    kind: WarningKind::NotMerged,
                    message: format!(
                        "{pr}: 기본 브랜치에 push했지만 GitHub이 아직 머지된 PR로 보지 않는다"
                    ),
                    pr: Some(*pr),
                    at_ms: EpochMs::now(),
                });
            }
            Ok(())
        })
    }
}

fn step_failure(failed: &Failed, mode: VerifyMode, tree: &Sha) -> Failure {
    let failed_tests = match &failed.detail {
        Detail::Tests(tests) => tests.to_vec(),
        Detail::Tail(_) => Vec::new(),
    };
    let step = StepFailure {
        step: failed.step.clone(),
        mode,
        failed_tests,
        log: failed.log.clone(),
        tree: tree.clone(),
    };
    match failed.cause {
        Cause::TimedOut => Failure::Timeout(step),
        Cause::Exited(_) | Cause::Signaled(_) => Failure::Verify(step),
    }
}

fn reap_orphans(slot: &Path) -> Result<()> {
    let Ok(slot) = fs::canonicalize(slot) else {
        return Ok(());
    };
    let Ok(procs) = fs::read_dir("/proc") else {
        return Ok(());
    };
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if pid as u32 == std::process::id() {
            continue;
        }
        let Ok(cwd) = fs::read_link(entry.path().join("cwd")) else {
            continue;
        };
        let carries_run_id = fs::read(entry.path().join("environ")).is_ok_and(|env| {
            env.split(|b| *b == 0)
                .any(|v| v.starts_with(b"CHUTE_RUN_ID="))
        });
        if cwd.starts_with(&slot) && carries_run_id {
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(cause: Cause, detail: Detail) -> Failed {
        Failed {
            step: "test".to_owned(),
            cause,
            detail,
            log: "/logs/test.1.log".into(),
        }
    }

    fn tree() -> Sha {
        Sha::try_from("abc1234".to_owned()).unwrap()
    }

    #[test]
    fn a_timeout_is_a_timeout_failure_and_the_other_causes_are_verify_failures() {
        let tests = NonEmpty::new(vec!["a::b".to_owned()]).unwrap();
        let timed_out = failed(Cause::TimedOut, Detail::Tail("...".into()));
        assert!(matches!(
            step_failure(&timed_out, VerifyMode::Solo, &tree()),
            Failure::Timeout(_)
        ));
        for cause in [Cause::Exited(1), Cause::Signaled(9)] {
            let one = failed(cause, Detail::Tests(tests.clone()));
            let Failure::Verify(step) = step_failure(&one, VerifyMode::Solo, &tree()) else {
                panic!("검증 실패여야 한다");
            };
            assert_eq!(step.failed_tests, ["a::b"]);
            assert_eq!(step.tree, tree());
            assert_eq!(step.log, one.log);
        }
    }

    #[test]
    fn a_failure_without_test_names_carries_none() {
        let one = failed(Cause::Exited(2), Detail::Tail("끝부분".into()));
        let Failure::Verify(step) = step_failure(&one, VerifyMode::Solo, &tree()) else {
            panic!("검증 실패여야 한다");
        };
        assert!(step.failed_tests.is_empty());
    }
}
