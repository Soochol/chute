use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::Deserialize;

use crate::ci::{self, Verdict as CiVerdict};
use crate::clock::Clock;
use crate::config::Config;
use crate::env::{self, EnvSection};
use crate::ext::Ext;
use crate::feedback::{Delivery, Feedback, Notice, Subject};
use crate::flaky;
use crate::gitops::{
    FAILED_REF_PREFIX, Gitops, ItemResult, MergeItem, PrHead, PushOutcome, Slot, SlotLock, SlotName,
};
use crate::model::{
    Agent, BaseDoubt, Batch, BatchId, BatchRecord, BlockReason, Blocked, BounceReason, EpochMs,
    Event, EventKind, Failure, Flaky, ItemState, MergeRef, NonEmpty, Phase, PrNumber,
    ProbeEvidence, QueueItem, QueuedReason, RemovedReason, RepoInfo, Rerun, RerunSignal, ResumedBy,
    Reused, Sha, SkippedStep, State, StepFailure, TreeKey, VerifiedBy, VerifiedTree, VerifyMode,
    Warning, WarningKind,
};
use crate::notify::{BounceDelivery, Line, Notifier, Removal, WaitingWhy, mode_of};
use crate::pc_limit::Wait;
use crate::queue::{self, Recorded};
use crate::reuse;
use crate::store::{Home, RepoStore};
use crate::verify::{self, Cancel, Cause, Detail, Failed, FlakyStep, Options, Outcome};

const PUSH_ROUNDS: u32 = 3;
const SLOT_LOCK_TRIES: u32 = 40;
const SLOT_LOCK_PAUSE: Duration = Duration::from_millis(50);
pub const VERIFIED_BASES_KEPT: usize = 100;
const NO_CHECKS_WARNING_AFTER_MS: u64 = 10 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct Timing {
    pub merged_check: Duration,
    pub merged_poll: Duration,
    pub ci_recheck: Duration,
    pub clock: Clock,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            merged_check: Duration::from_secs(20),
            merged_poll: Duration::from_secs(1),
            ci_recheck: Duration::from_secs(30),
            clock: Clock::System,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pass {
    Idle,
    Blocked,
    Held,
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
    ci_looked: Mutex<BTreeMap<PrNumber, Instant>>,
    pr_files: Mutex<BTreeMap<(PrNumber, Sha), Vec<String>>>,
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
            ci_looked: Mutex::new(BTreeMap::new()),
            pr_files: Mutex::new(BTreeMap::new()),
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

    /// Appends an event, except one already written: a PR is merged once per submission and a
    /// batch finishes once, however many times a run cut short repeats the step.
    fn history(&self, kind: EventKind) -> Result<()> {
        self.history_at(kind).map(drop)
    }

    fn history_at(&self, kind: EventKind) -> Result<EpochMs> {
        let once = matches!(
            kind,
            EventKind::Merged { .. } | EventKind::BatchFinished { .. }
        );
        if once {
            let seen = self
                .store
                .history()?
                .events
                .iter()
                .find(|e| match (&e.kind, &kind) {
                    (
                        EventKind::Merged {
                            pr, submitted_ms, ..
                        },
                        EventKind::Merged {
                            pr: p,
                            submitted_ms: s,
                            ..
                        },
                    ) => pr == p && submitted_ms == s,
                    (
                        EventKind::BatchFinished { batch, .. },
                        EventKind::BatchFinished { batch: b, .. },
                    ) => batch == b,
                    _ => false,
                })
                .map(|e| e.at_ms);
            if let Some(at_ms) = seen {
                return Ok(at_ms);
            }
        }
        let at_ms = EpochMs::now();
        self.store.append_history(&Event { at_ms, kind })?;
        Ok(at_ms)
    }

    /// A PR that left the queue without merging through chute may still have been merged on
    /// GitHub, which satisfies the items waiting for it. An unreadable state fails the pass.
    fn release_merged_after(&self) -> Result<()> {
        let state = self.store.load()?;
        let gone: BTreeSet<PrNumber> = state
            .queue
            .iter()
            .flat_map(|i| i.after.iter().copied())
            .filter(|n| !state.queue.iter().any(|i| i.pr == *n))
            .collect();
        for n in gone {
            if self.merged_on_github(n)? {
                self.store.update(|s| {
                    queue::release_after(s, n);
                    Ok(())
                })?;
            }
        }
        Ok(())
    }

    fn merged_on_github(&self, pr: PrNumber) -> Result<bool> {
        #[derive(serde::Deserialize)]
        struct Found {
            state: String,
        }
        let text = self
            .ext
            .gh(
                &self.info.path,
                ["pr", "view", &pr.0.to_string(), "--json", "state"],
            )?
            .ok()
            .with_context(|| format!("{pr}의 상태를 gh로 읽지 못했다"))?;
        let found: Found =
            serde_json::from_str(&text).with_context(|| format!("{pr}의 gh 출력을 읽지 못했다"))?;
        Ok(found.state == "MERGED")
    }

    fn notifier(&self) -> Notifier<'_> {
        Notifier::new(&self.ext, &self.info, &self.store)
    }

    fn tell_merged(&self, item: &QueueItem, batch: &BatchId, commit: &Sha, at: EpochMs) {
        let line = Line::Merged {
            pr: item.pr,
            head: item.head.clone(),
            batch: batch.clone(),
            commit: commit.clone(),
            repair: item.repair,
        };
        self.notifier().notify(item.submitter.as_ref(), at, &line);
    }

    fn tell_halted(&self, at: EpochMs) {
        let state = match self.store.load() {
            Ok(state) => state,
            Err(e) => {
                eprintln!("경고: 멈춤 알림을 보낼 큐를 읽지 못했다: {e:#}");
                return;
            }
        };
        let Some(blocked) = &state.blocked else {
            return;
        };
        let filled = blocked.step.is_some()
            || blocked.log.is_some()
            || blocked.pushed_by_chute
            || blocked.rechecked;
        let line = Line::Halted {
            base: blocked.base.clone(),
            step: blocked.step.clone(),
            pushed_by_chute: filled.then_some(blocked.pushed_by_chute),
            rechecked: filled.then_some(blocked.rechecked),
            log: blocked.log.clone(),
            suspect: state
                .base_doubt
                .as_ref()
                .filter(|d| blocked.rechecked && d.base == blocked.base)
                .and_then(|d| d.prs.first().copied()),
        };
        let targets: Vec<(Option<&Agent>, PrNumber)> = state
            .queue
            .iter()
            .map(|i| (i.submitter.as_ref(), i.pr))
            .collect();
        self.notifier().notify_each(&targets, at, |_| line.clone());
    }

    fn tell_resumed(&self, base: &Sha, by: ResumedBy, repair: Option<&QueueItem>, at: EpochMs) {
        let state = match self.store.load() {
            Ok(state) => state,
            Err(e) => {
                eprintln!("경고: 멈춤 풀림 알림을 보낼 큐를 읽지 못했다: {e:#}");
                return;
            }
        };
        let pr = repair.map(|i| i.pr);
        let targets: Vec<(Option<&Agent>, PrNumber)> = state
            .queue
            .iter()
            .chain(repair)
            .map(|i| (i.submitter.as_ref(), i.pr))
            .collect();
        let line = Line::Resumed {
            base: base.clone(),
            by,
            pr,
        };
        self.notifier().notify_each(&targets, at, |_| line.clone());
    }

    fn resume_for_main_moved(&self, base: &Sha) -> Result<()> {
        self.store.update(|s| {
            s.blocked = None;
            Ok(())
        })?;
        let at = self.history_at(EventKind::Resumed {
            base: base.clone(),
            by: ResumedBy::MainMoved,
            pr: None,
        })?;
        self.tell_resumed(base, ResumedBy::MainMoved, None, at);
        Ok(())
    }

    fn tell_removed(&self, item: &QueueItem, removal: Removal, at: EpochMs) {
        let line = Line::Removed {
            pr: item.pr,
            head: item.head.clone(),
            removal,
        };
        self.notifier().notify(item.submitter.as_ref(), at, &line);
    }

    fn load_config(&self) -> Result<Config> {
        let branch = &self.info.default_branch;
        let remote = &self.info.remote_name;
        Config::load_remote_branch(&self.ext, &self.info.path, remote, branch)?.ok_or_else(|| {
            anyhow!(
                "chute.toml을 {remote}/{branch}에서 읽지 못했다. 기본 브랜치에 올렸는지 확인한다"
            )
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
        let main = format!(
            "refs/remotes/{}/{}",
            self.info.remote_name, self.info.default_branch
        );
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
        let mut newly_merged: Vec<(&QueueItem, Sha, EpochMs)> = Vec::new();
        for (item, commit) in &settled {
            if !merged_prs.contains(&item.pr) {
                let commit = commit.clone().unwrap_or_else(|| item.head.clone());
                let at = self.history_at(EventKind::Merged {
                    pr: item.pr,
                    batch: id_for_events.clone(),
                    commit: commit.clone(),
                    submitted_ms: item.submitted_ms,
                    title: Some(item.title.clone()),
                })?;
                newly_merged.push((item, commit, at));
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
            if let Some(mut record) = record {
                let now = record.finished_ms.unwrap_or_else(EpochMs::now);
                for (item, commit) in &settled {
                    let known = record.merges.iter().any(|m| m.pr == item.pr);
                    if let (Some(commit), false) = (commit, known) {
                        record.merges.push(MergeRef {
                            pr: item.pr,
                            commit: commit.clone(),
                            submitter: item.submitter.clone(),
                            agent: item.agent.clone(),
                        });
                    }
                }
                let told_bounced = bounced_prs.clone();
                for pr in &record.bounced {
                    if !bounced_prs.contains(pr) {
                        bounced_prs.push(*pr);
                    }
                }
                for pr in &bounced_prs {
                    let sent_back = state.queue.iter().find(|i| i.pr == *pr);
                    if let Some(QueueItem {
                        state: ItemState::Bounced(reason),
                        attempts,
                        ..
                    }) = sent_back
                        && !told_bounced.contains(pr)
                    {
                        self.history(EventKind::Bounced {
                            pr: *pr,
                            batch: Some(id.clone()),
                            reason: *reason,
                            attempts: *attempts,
                            unreachable: None,
                        })?;
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
        for (item, commit, at) in &newly_merged {
            self.tell_merged(item, &id_for_events, commit, *at);
        }
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

    fn due_ci_waiters(&self, state: &State) -> BTreeSet<PrNumber> {
        let mut looked = self.ci_looked.lock().unwrap_or_else(|e| e.into_inner());
        looked.retain(|pr, _| state.queue.iter().any(|i| i.pr == *pr));
        state
            .queue
            .iter()
            .filter(|i| i.state == ItemState::Queued(Some(QueuedReason::CiPending)))
            .filter(|i| {
                looked.get(&i.pr).is_none_or(|at| {
                    self.timing.clock.now().duration_since(*at) >= self.timing.ci_recheck
                })
            })
            .map(|i| i.pr)
            .collect()
    }

    fn prune_no_checks_warnings(&self, require_ci: Option<bool>) -> Result<()> {
        let queued = |s: &State, pr: Option<PrNumber>| {
            s.queue
                .iter()
                .any(|i| Some(i.pr) == pr && matches!(i.state, ItemState::Queued(_)))
        };
        let stale = |s: &State| {
            s.warnings.iter().any(|w| {
                w.kind == WarningKind::NoChecks && (require_ci == Some(false) || !queued(s, w.pr))
            })
        };
        if !stale(&self.store.load()?) {
            return Ok(());
        }
        self.store.update(|s| {
            let keep: Vec<bool> = s
                .warnings
                .iter()
                .map(|w| {
                    w.kind != WarningKind::NoChecks
                        || (require_ci != Some(false) && queued(s, w.pr))
                })
                .collect();
            let mut keep = keep.into_iter();
            s.warnings.retain(|_| keep.next().unwrap_or(true));
            Ok(())
        })
    }

    pub fn run_once(&self) -> Result<Pass> {
        self.release_merged_after()?;
        self.prune_no_checks_warnings(None)?;
        let due_waiters = self.due_ci_waiters(&self.store.load()?);
        let waiting = |s: &crate::model::State| {
            s.queue.iter().any(|i| match i.state {
                ItemState::Queued(None) => true,
                ItemState::Queued(Some(_)) => !due_waiters.is_empty(),
                _ => false,
            })
        };
        if !waiting(&self.store.load()?) {
            return Ok(Pass::Idle);
        }
        let slot = self.lock_batch_slot()?.open()?;
        let base = slot.fetch_base()?;
        let mut repairing = false;
        if let Some(blocked) = self.store.load()?.blocked {
            if blocked.base != base {
                self.resume_for_main_moved(&base)?;
            } else if queue::repair_ready(&self.store.load()?) {
                repairing = true;
            } else {
                return Ok(Pass::Blocked);
            }
        }
        let config = self.load_config()?;
        self.prune_no_checks_warnings(Some(config.require_ci))?;
        let held = self.held_by_post_verify(&slot, &base, &config)?;
        let id = self.new_batch_id();
        let (taken, was_waiting_on_ci, queue_position): (
            Vec<QueueItem>,
            BTreeSet<PrNumber>,
            BTreeMap<PrNumber, usize>,
        ) = self.store.update(|s| {
            let waiting_on_ci: BTreeSet<PrNumber> = s
                .queue
                .iter()
                .filter(|i| i.state == ItemState::Queued(Some(QueuedReason::CiPending)))
                .map(|i| i.pr)
                .collect();
            let prs = {
                let due = if config.require_ci {
                    &due_waiters
                } else {
                    &waiting_on_ci
                };
                if repairing {
                    queue::take_repair(s, due)
                } else {
                    queue::take_batch_except(s, config.max_batch, due, &held)
                }
            };
            if !prs.is_empty() {
                s.batch = Some(Batch {
                    id: id.clone(),
                    base: base.clone(),
                    prs: prs.clone(),
                    phase: Phase::Building,
                    step: None,
                    steps: Vec::new(),
                    round: 0,
                    started_ms: EpochMs::now(),
                    waiting_machine: false,
                    reused: None,
                });
            }
            let items = prs
                .iter()
                .filter_map(|pr| s.queue.iter().find(|i| i.pr == *pr).cloned())
                .collect();
            let position = queue::batch_order(&s.queue)
                .into_iter()
                .enumerate()
                .map(|(n, pr)| (pr, n))
                .collect();
            Ok((items, waiting_on_ci, position))
        })?;
        if taken.is_empty() {
            return Ok(if repairing {
                Pass::Blocked
            } else if !held.is_empty() {
                Pass::Held
            } else {
                Pass::Idle
            });
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
                env: None,
                skipped: Vec::new(),
                reused: Vec::new(),
                reruns: Vec::new(),
            },
            start_base: base,
            items: taken.iter().map(|i| (i.pr, i.clone())).collect(),
            fetched: BTreeSet::new(),
            conflicted: BTreeSet::new(),
            sent_back: BTreeSet::new(),
            landed: Vec::new(),
            merged: Vec::new(),
            bounced: Vec::new(),
            deferred: BTreeSet::new(),
            closed_any: false,
            was_waiting_on_ci,
            queue_position,
            bisecting: false,
            verify_time: Duration::ZERO,
            repairing,
            reran: BTreeSet::new(),
            rerunning: None,
        };
        let result = run.execute(&taken);
        let only_deferred = run.only_deferred();
        let wound = run.wind_down();
        if wound.is_ok() && result.is_ok() {
            run.settle_flaky();
        }
        let summary = Summary {
            batch: id,
            merged: run.merged.clone(),
            bounced: run.bounced.clone(),
        };
        match result {
            Ok(()) if only_deferred => wound.map(|()| Pass::Idle),
            Ok(()) => wound.map(|()| Pass::Done(summary)),
            Err(e) => match e.downcast::<Halt>() {
                Ok(Halt::Cancelled) => wound.map(|()| Pass::Cancelled),
                Ok(Halt::Blocked) => wound.map(|()| Pass::Blocked),
                Err(e) => Err(e),
            },
        }
    }

    fn held_by_post_verify(
        &self,
        slot: &Slot,
        base: &Sha,
        config: &Config,
    ) -> Result<BTreeSet<PrNumber>> {
        let state = self.store.load()?;
        let (Some(post), Some(red)) = (&config.post, &state.post.red) else {
            return Ok(BTreeSet::new());
        };
        let Some(step) = post.steps.iter().find(|s| s.name == red.step) else {
            return Ok(BTreeSet::new());
        };
        if !post.hold || !red.holding {
            return Ok(BTreeSet::new());
        }
        let mut held = BTreeSet::new();
        for item in state
            .queue
            .iter()
            .filter(|i| matches!(i.state, ItemState::Queued(_)) && !i.repair)
        {
            match self.files_of(slot, base, item) {
                Ok(files) if step.runs_for(&files) => {
                    held.insert(item.pr);
                }
                Ok(_) => {}
                Err(e) => eprintln!(
                    "경고: {}의 바뀐 파일을 읽지 못해 잡아 두지 않는다: {e:#}",
                    item.pr
                ),
            }
        }
        Ok(held)
    }

    fn files_of(&self, slot: &Slot, base: &Sha, item: &QueueItem) -> Result<Vec<String>> {
        let key = (item.pr, item.head.clone());
        if let Some(files) = self
            .pr_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Ok(files.clone());
        }
        let head = match slot.fetch_pr_head(item.pr, &item.head)? {
            PrHead::Unchanged => item.head.clone(),
            PrHead::Changed { actual } => actual,
        };
        let files = slot.changed_since_fork(base, &head)?;
        self.pr_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, files.clone());
        Ok(files)
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
                    "state,headRefOid,mergeCommit,isDraft,baseRefName",
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
            PrState::Open if view.is_draft => Inspection::Draft,
            PrState::Open if view.base_ref_name != self.info.default_branch => {
                Inspection::Retargeted
            }
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
    is_draft: bool,
    base_ref_name: String,
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

#[derive(Clone)]
enum Inspection {
    Ready,
    Closed,
    Merged { commit: Sha },
    HeadChanged { current: Sha },
    Draft,
    Retargeted,
}

#[derive(Clone, Debug)]
struct Landed {
    pr: PrNumber,
    title: String,
}

#[derive(Clone, Copy)]
enum Scope {
    Changed,
    Whole,
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

struct Told<'a> {
    reason: BounceReason,
    failure: Option<&'a Failure>,
    attempts: u32,
    report: Option<&'a Path>,
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
    deferred: BTreeSet<PrNumber>,
    closed_any: bool,
    was_waiting_on_ci: BTreeSet<PrNumber>,
    queue_position: BTreeMap<PrNumber, usize>,
    bisecting: bool,
    verify_time: Duration,
    repairing: bool,
    reran: BTreeSet<PrNumber>,
    rerunning: Option<Rerunning>,
}

struct Rerunning {
    step: FlakyStep,
    failed_tree: Sha,
}

impl Run<'_> {
    fn id(&self) -> &BatchId {
        &self.record.id
    }

    fn execute(&mut self, taken: &[QueueItem]) -> Result<()> {
        self.save_record()?;
        let mut by_queue = taken.to_vec();
        by_queue.sort_by_key(|i| self.queue_position[&i.pr]);
        let group = self.admit(&by_queue)?;
        if !group.is_empty() {
            self.ensure_env()?;
        }
        self.solve(group)?;
        self.recheck_conflicts()
    }

    fn admit(&mut self, taken: &[QueueItem]) -> Result<Vec<PrNumber>> {
        let mut ready = Vec::new();
        for item in taken {
            match self.engine.inspect(item)? {
                Inspection::Ready if ready.len() >= self.config.max_batch as usize => {
                    self.release(item.pr)?
                }
                Inspection::Ready if self.passes_ci_gate(item)? => ready.push(item.pr),
                Inspection::Ready => {}
                other => self.set_aside(item, other)?,
            }
        }
        Ok(ready)
    }

    fn set_aside(&mut self, item: &QueueItem, why: Inspection) -> Result<()> {
        match why {
            Inspection::Ready => {}
            Inspection::Closed => {
                self.engine
                    .store
                    .update(|s| Ok(queue::closed(s, item.pr)?))?;
                let at = self.engine.history_at(EventKind::Closed { pr: item.pr })?;
                self.engine
                    .notifier()
                    .notify_waiters(item.pr, WaitingWhy::Closed, at);
                self.closed_any = true;
            }
            Inspection::Merged { commit } => {
                let at = self.engine.history_at(EventKind::Merged {
                    pr: item.pr,
                    batch: self.id().clone(),
                    commit: commit.clone(),
                    submitted_ms: item.submitted_ms,
                    title: Some(item.title.clone()),
                })?;
                self.engine
                    .store
                    .update(|s| Ok(queue::merged(s, item.pr)?))?;
                self.engine.tell_merged(item, self.id(), &commit, at);
                self.merged.push(item.pr);
            }
            Inspection::HeadChanged { current } => self.head_changed(item, &current)?,
            Inspection::Draft | Inspection::Retargeted => {
                self.engine.store.update(|s| {
                    queue::requeue(s, item.pr)?;
                    queue::drop_item(s, item.pr)?;
                    if let Some(batch) = &mut s.batch {
                        batch.prs.retain(|p| *p != item.pr);
                    }
                    Ok(())
                })?;
                let at = self.engine.history_at(EventKind::Dropped { pr: item.pr })?;
                self.engine
                    .notifier()
                    .notify_waiters(item.pr, WaitingWhy::Dropped, at);
                self.closed_any = true;
            }
        }
        Ok(())
    }

    fn all_still_fit(&mut self, b: &Build, group: &mut Vec<PrNumber>) -> Result<bool> {
        let mut aside = Vec::new();
        for (pr, _) in &b.landing {
            let item = self.items[pr].clone();
            let seen = self
                .engine
                .inspect(&item)
                .with_context(|| format!("push 직전에 {pr}의 상태를 읽지 못해 push하지 않았다"))?;
            if !matches!(seen, Inspection::Ready) {
                aside.push((item, seen));
            }
        }
        for (item, seen) in &aside {
            self.set_aside(item, seen.clone())?;
        }
        group.retain(|pr| !aside.iter().any(|(item, _)| item.pr == *pr));
        Ok(aside.is_empty())
    }

    fn passes_ci_gate(&mut self, item: &QueueItem) -> Result<bool> {
        if !self.config.require_ci {
            return Ok(true);
        }
        let engine = self.engine;
        let pr = item.pr;
        let verdict = ci::read(&engine.ext, &engine.info.path, pr)?;
        let mut looked = engine.ci_looked.lock().unwrap_or_else(|e| e.into_inner());
        match verdict {
            CiVerdict::Pending | CiVerdict::NoChecks => {
                looked.insert(pr, engine.timing.clock.now())
            }
            CiVerdict::Passed | CiVerdict::Failed(_) => looked.remove(&pr),
        };
        drop(looked);
        match verdict {
            CiVerdict::Passed => {
                self.clear_no_checks_warning(pr)?;
                Ok(true)
            }
            CiVerdict::Pending => {
                self.clear_no_checks_warning(pr)?;
                self.defer(pr, false)?;
                Ok(false)
            }
            CiVerdict::NoChecks => {
                let since = item
                    .last_failure
                    .as_ref()
                    .map_or(item.submitted_ms, |f| f.at_ms.max(item.submitted_ms));
                let waited = EpochMs::now().0.saturating_sub(since.0);
                self.defer(pr, waited > NO_CHECKS_WARNING_AFTER_MS)?;
                Ok(false)
            }
            CiVerdict::Failed(checks) => {
                self.clear_no_checks_warning(pr)?;
                self.bounce(pr, Failure::Ci { checks }, None)?;
                Ok(false)
            }
        }
    }

    fn release(&mut self, pr: PrNumber) -> Result<()> {
        let keep_mark = self.config.require_ci && self.was_waiting_on_ci.contains(&pr);
        self.engine.store.update(|s| {
            queue::requeue(s, pr)?;
            if keep_mark {
                queue::set_ci_pending(s, pr)?;
            }
            if let Some(batch) = &mut s.batch {
                batch.prs.retain(|p| *p != pr);
            }
            Ok(())
        })?;
        self.record.prs.retain(|p| *p != pr);
        self.save_record()
    }

    fn defer(&mut self, pr: PrNumber, warn_no_checks: bool) -> Result<()> {
        self.engine.store.update(|s| {
            queue::requeue(s, pr)?;
            queue::set_ci_pending(s, pr)?;
            if let Some(batch) = &mut s.batch {
                batch.prs.retain(|p| *p != pr);
            }
            let warned = s
                .warnings
                .iter()
                .any(|w| w.kind == WarningKind::NoChecks && w.pr == Some(pr));
            if warn_no_checks && !warned {
                s.warnings.push(Warning {
                    kind: WarningKind::NoChecks,
                    message: format!(
                        "{pr}: 10분이 넘도록 GitHub check가 하나도 없다. 이 저장소에 CI가 없으면 `chute.toml`의 `require_ci`를 끈다"
                    ),
                    pr: Some(pr),
                    at_ms: EpochMs::now(),
                });
            }
            Ok(())
        })?;
        self.deferred.insert(pr);
        self.record.prs.retain(|p| *p != pr);
        self.save_record()
    }

    fn clear_no_checks_warning(&self, pr: PrNumber) -> Result<()> {
        let warned = self
            .engine
            .store
            .load()?
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::NoChecks && w.pr == Some(pr));
        if !warned {
            return Ok(());
        }
        self.engine.store.update(|s| {
            s.warnings
                .retain(|w| !(w.kind == WarningKind::NoChecks && w.pr == Some(pr)));
            Ok(())
        })
    }

    fn only_deferred(&self) -> bool {
        !self.items.is_empty()
            && self
                .items
                .keys()
                .all(|pr| self.was_waiting_on_ci.contains(pr) || self.deferred.contains(pr))
            && !self.closed_any
            && self.merged.is_empty()
            && self.bounced.is_empty()
            && self.landed.is_empty()
            && self.record.verifications == 0
            && self.record.pushed.is_empty()
    }

    fn head_changed(&mut self, item: &QueueItem, current: &Sha) -> Result<()> {
        let pr = item.pr;
        let notice = Notice::HeadChanged {
            registered: &item.head,
            current,
        };
        let ((), delivery) =
            self.deliver_and_record(item, &notice, |s| Ok(queue::head_changed(s, pr)?))?;
        self.bounced.push(pr);
        self.sent_back.insert(pr);
        self.conflicted.remove(&pr);
        self.save_record()?;
        let at = self.engine.history_at(EventKind::Bounced {
            pr,
            batch: Some(self.id().clone()),
            reason: BounceReason::HeadChanged,
            attempts: item.attempts,
            unreachable: unreachable_of(&delivery),
        })?;
        let told = Told {
            reason: BounceReason::HeadChanged,
            failure: None,
            attempts: item.attempts,
            report: None,
        };
        self.tell_bounce(item, told, &delivery, at);
        self.tell_waiters(pr, WaitingWhy::Bounced, at);
        Ok(())
    }

    fn tell_bounce(&self, item: &QueueItem, told: Told<'_>, delivery: &Delivery, at: EpochMs) {
        let notifier = self.engine.notifier();
        let (worker, how) = match delivery {
            Delivery::Sent { .. } => (item.agent.clone(), BounceDelivery::Sent),
            Delivery::Spawned(agent) => (Some(agent.clone()), BounceDelivery::Spawned),
            Delivery::Unreachable(why) => (None, BounceDelivery::Unreachable(why.to_string())),
        };
        let receiver = match how {
            BounceDelivery::Unreachable(_) => item.submitter.as_ref(),
            BounceDelivery::Sent | BounceDelivery::Spawned => worker.as_ref(),
        };
        let (mode, partners) = told.failure.map(mode_of).unwrap_or_default();
        let line = Line::Bounced {
            pr: item.pr,
            head: item.head.clone(),
            reason: told.reason,
            mode,
            partners,
            attempts: told.attempts,
            budget: self.config.budget,
            to: receiver.map(|r| notifier.address_of(r)),
            delivered: how,
            report: told.report.map(Path::to_path_buf),
        };
        match delivery {
            Delivery::Unreachable(_) => {
                notifier.notify(item.submitter.as_ref(), at, &line);
            }
            Delivery::Sent { .. } | Delivery::Spawned(_) => {
                notifier.notify_unless_same(item.submitter.as_ref(), worker.as_ref(), at, &line);
            }
        }
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

    fn deliver_and_record<T>(
        &self,
        item: &QueueItem,
        notice: &Notice<'_>,
        record: impl FnOnce(&mut State) -> Result<T>,
    ) -> Result<(T, Delivery)> {
        let delivery = self.feedback().deliver(item, notice);
        let pr = item.pr;
        let recorded = self.engine.store.update(|s| {
            let recorded = record(s)?;
            if let Delivery::Spawned(agent) = &delivery {
                queue::record_agent(s, pr, agent.clone())?;
            }
            Ok(recorded)
        })?;
        Ok((recorded, delivery))
    }

    fn tell_waiters(&self, pr: PrNumber, why: WaitingWhy, at: EpochMs) {
        self.engine.notifier().notify_waiters(pr, why, at);
    }

    fn ensure_env(&mut self) -> Result<()> {
        if self.record.env.is_none() {
            self.slot.checkout(&self.start_base)?;
            let env = env::record(
                &self.engine.ext,
                self.slot.path(),
                &self.engine.store.batch_dir(self.id()).join("env"),
                &self.config.env_check,
                &self.engine.cancel,
            );
            if self.engine.cancel.is_cancelled() {
                return Ok(());
            }
            self.record.env = Some(env);
            self.save_record()?;
        }
        Ok(())
    }

    fn env_section(&mut self, item: &QueueItem) -> Result<Option<EnvSection>> {
        self.ensure_env()?;
        let Some(env) = self.record.env.clone() else {
            return Ok(None);
        };
        let state = self.engine.store.load()?;
        let preflight = state
            .preflights
            .get(&item.branch)
            .filter(|p| p.head == item.head)
            .and_then(|p| p.env.clone());
        Ok(Some(EnvSection { env, preflight }))
    }

    fn bounce(&mut self, pr: PrNumber, failure: Failure, failed_ref: Option<&str>) -> Result<()> {
        let engine = self.engine;
        let item = self.items[&pr].clone();
        let section = self.env_section(&item)?;
        let written = self.feedback().write_report_with_env(
            &Subject::from(&item),
            &failure,
            failed_ref,
            section,
        )?;
        let budget = self.config.budget;
        let record = |s: &mut State| Ok(queue::record_failure(s, pr, written.record(), budget)?);
        let (failed, delivery) = if item.attempts < budget {
            let (failed, delivery) =
                self.deliver_and_record(&item, &Notice::Bounced(&written), record)?;
            (failed, Some(delivery))
        } else {
            (engine.store.update(record)?, None)
        };
        if let Recorded::OverBudget(gone) = &failed {
            let at = engine.history_at(EventKind::Removed {
                pr,
                reason: RemovedReason::Budget,
                detail: Some(format!("반송 한도 {budget}번을 넘었다")),
                report: Some(written.md.clone()),
            })?;
            let removal = Removal::Budget {
                attempts: gone.attempts,
                budget,
                report: Some(written.md.clone()),
            };
            engine.tell_removed(gone, removal, at);
            self.tell_waiters(pr, WaitingWhy::Removed, at);
        }
        self.bounced.push(pr);
        self.conflicted.remove(&pr);
        self.save_record()?;
        if let Recorded::Bounced(reason) = failed {
            let at = engine.history_at(EventKind::Bounced {
                pr,
                batch: Some(self.id().clone()),
                reason,
                attempts: item.attempts + 1,
                unreachable: delivery.as_ref().and_then(unreachable_of),
            })?;
            if let Some(delivery) = &delivery {
                let told = Told {
                    reason,
                    failure: Some(&written.doc.failure),
                    attempts: item.attempts + 1,
                    report: Some(&written.md),
                };
                self.tell_bounce(&item, told, delivery, at);
            }
            self.tell_waiters(pr, WaitingWhy::Bounced, at);
        }
        Ok(())
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

    fn solve(&mut self, mut group: Vec<PrNumber>) -> Result<()> {
        let mut rounds = 0;
        while rounds < PUSH_ROUNDS && !group.is_empty() {
            let b = self.build(&group)?;
            if b.landing.is_empty() {
                return self.land(&b);
            }
            let reusable = !self.bisecting && self.rerunning.is_none();
            match self.verify(&b.tip, Scope::Changed, reusable)? {
                Verdict::Pass => {
                    if let Some(rerun) = self.rerunning.take() {
                        self.record_flaky(vec![rerun.step], &rerun.failed_tree)?;
                    }
                    if !self.all_still_fit(&b, &mut group)? {
                        continue;
                    }
                    self.set_phase(Phase::Pushing)?;
                    if self.slot.head()? != b.tip {
                        bail!(
                            "검증한 커밋 {}이 슬롯의 HEAD가 아니라 push하지 않았다",
                            b.tip
                        );
                    }
                    match self.slot.push(&b.tip)? {
                        PushOutcome::Pushed => return self.land(&b),
                        PushOutcome::MainMoved => rounds += 1,
                    }
                }
                Verdict::Fail(failed) => return self.split_or_convict(&b, failed),
            }
        }
        if group.is_empty() {
            return Ok(());
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

    fn verify(&mut self, tree: &Sha, scope: Scope, reusable: bool) -> Result<Verdict> {
        let changed = match scope {
            Scope::Changed => Some(self.slot.changed_files(&self.start_base)?),
            Scope::Whole => None,
        };
        let key = self.tree_key(tree, changed.as_deref())?;
        if let (true, Some(key)) = (reusable, &key)
            && self.reuse_pass(key)?
        {
            return Ok(Verdict::Pass);
        }
        self.record.verifications += 1;
        let round = self.record.verifications;
        let steps = self.config.steps_for(changed.as_deref());
        self.update_batch(|b| {
            b.round = round;
            b.step = None;
            b.steps = steps.clone();
            b.reused = None;
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
        let on_machine_wait = |wait: Wait| {
            let _ = engine.store.update(|s| {
                if let Some(batch) = &mut s.batch {
                    batch.waiting_machine = matches!(wait, Wait::Begin { .. });
                }
                Ok(())
            });
        };
        let log_dir = engine
            .store
            .batch_dir(self.id())
            .join(format!("verify-{round}"));
        let skipped = RefCell::new(Vec::new());
        let on_skip = |name: &str| skipped.borrow_mut().push(name.to_owned());
        let started = Instant::now();
        let outcome = verify::run(
            &self.config,
            &engine.store,
            self.slot.path(),
            &log_dir,
            &Options {
                cancel: engine.cancel.clone(),
                cache_dir: Some(engine.gitops.cache_dir()),
                on_step: Some(&on_step),
                on_machine_wait: Some(&on_machine_wait),
                changed: changed.as_deref(),
                on_skip: Some(&on_skip),
                ..Options::default()
            },
        )?;
        self.verify_time += started.elapsed();
        self.record.skipped.extend(
            skipped
                .into_inner()
                .into_iter()
                .map(|step| SkippedStep { round, step }),
        );
        match outcome {
            Outcome::Passed { flaky } => {
                self.clear_base_doubt()?;
                let steady = flaky.is_empty();
                self.record_flaky(flaky, tree)?;
                if let (true, Some(key)) = (steady, key) {
                    self.remember_pass(key)?;
                }
                Ok(Verdict::Pass)
            }
            Outcome::Failed(failed) => Ok(Verdict::Fail(failed)),
            Outcome::Cancelled => Err(Halt::Cancelled.into()),
        }
    }

    fn tree_key(&self, commit: &Sha, changed: Option<&[String]>) -> Result<Option<TreeKey>> {
        if !self.config.reuse.enabled {
            return Ok(None);
        }
        Ok(Some(TreeKey {
            tree: self.slot.tree_of(commit)?,
            config: reuse::fingerprint(&self.engine.ext, &self.engine.info.path, &self.config)?,
            skipped: self.config.skipped_steps(changed),
        }))
    }

    fn reuse_pass(&mut self, key: &TreeKey) -> Result<bool> {
        let engine = self.engine;
        let now = EpochMs::now();
        let flaky_steps: Vec<String> = flaky::ledger(
            &engine.store.history()?.events,
            now,
            &self.config.flaky,
            self.config.flaky.chronic_days,
        )
        .into_iter()
        .map(|entry| entry.key.step)
        .collect();
        let lookup = reuse::Lookup {
            config: &self.config,
            now,
            here: self.record.env.as_ref(),
            flaky_steps: &flaky_steps,
        };
        let state = engine.store.load()?;
        let Some(found) = lookup.find(&state.verified_trees, key) else {
            return Ok(false);
        };
        let reused = Reused {
            tree: key.tree.clone(),
            by: found.by,
            verified_ms: found.at_ms,
        };
        engine.history(EventKind::Reused {
            batch: self.id().clone(),
            tree: reused.tree.clone(),
            by: reused.by,
            verified_ms: reused.verified_ms,
        })?;
        self.update_batch(|b| b.reused = Some(reused.by))?;
        self.record.reused.push(reused);
        self.save_record()?;
        self.clear_base_doubt()?;
        Ok(true)
    }

    fn remember_pass(&self, key: TreeKey) -> Result<()> {
        let now = EpochMs::now();
        let verified = VerifiedTree {
            key,
            at_ms: now,
            by: VerifiedBy::Batch,
            env: self.record.env.clone(),
        };
        self.engine
            .store
            .update(|s| {
                reuse::remember(s, verified, self.config.reuse.hours, now);
                Ok(())
            })
            .map(drop)
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
            let item = &self.items[pr];
            self.record.merges.push(MergeRef {
                pr: *pr,
                commit: commit.clone(),
                submitter: item.submitter.clone(),
                agent: item.agent.clone(),
            });
        }
        self.save_record()?;
        let mut merged_at = Vec::new();
        for pr in &b.members {
            let item = &self.items[pr];
            merged_at.push(engine.history_at(EventKind::Merged {
                pr: *pr,
                batch: self.id().clone(),
                commit: commits.get(pr).unwrap_or(&item.head).clone(),
                submitted_ms: item.submitted_ms,
                title: Some(item.title.clone()),
            })?);
        }
        let resumes = self.repairing && pushed;
        engine.store.update(|s| {
            for pr in &b.members {
                queue::merged(s, *pr)?;
            }
            if pushed {
                s.verified_bases.push(b.tip.clone());
                let excess = s.verified_bases.len().saturating_sub(VERIFIED_BASES_KEPT);
                s.verified_bases.drain(..excess);
            }
            if resumes {
                s.blocked = None;
            }
            Ok(())
        })?;
        for (pr, at) in b.members.iter().zip(merged_at) {
            let item = &self.items[pr];
            let commit = commits.get(pr).unwrap_or(&item.head);
            engine.tell_merged(item, self.id(), commit, at);
        }
        if let (true, Some((pr, _))) = (resumes, b.landing.first()) {
            let at = engine.history_at(EventKind::Resumed {
                base: b.tip.clone(),
                by: ResumedBy::Repair,
                pr: Some(*pr),
            })?;
            engine.tell_resumed(&b.tip, ResumedBy::Repair, Some(&self.items[pr]), at);
        }
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
        if let Blame::Solo { .. } = &blame
            && let Some((signal, tests)) = self.rerun_signal(pr, &failed)?
        {
            return self.rerun(pr, &failed, signal, tests, &tip);
        }
        if !self.repairing {
            match &blame {
                Blame::Solo { base } => self.confirm_solo_base(pr, base)?,
                Blame::Combo(_) => self.require_sound_base(&b.base)?,
            }
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

    fn rerun_signal(
        &self,
        pr: PrNumber,
        failed: &Failed,
    ) -> Result<Option<(RerunSignal, Vec<String>)>> {
        if self.reran.contains(&pr) {
            return Ok(None);
        }
        Ok(flaky::rerun_signal(
            failed,
            &self.engine.store.history()?.events,
            EpochMs::now(),
            self.config.flaky.chronic_days,
            self.id(),
        ))
    }

    fn rerun(
        &mut self,
        pr: PrNumber,
        failed: &Failed,
        signal: RerunSignal,
        tests: Vec<String>,
        failed_tree: &Sha,
    ) -> Result<()> {
        self.reran.insert(pr);
        self.record.reruns.push(Rerun {
            pr,
            step: failed.step.clone(),
            failed_round: self.record.verifications,
            signal,
            tests: tests.clone(),
        });
        self.save_record()?;
        self.rerunning = Some(Rerunning {
            step: FlakyStep {
                step: failed.step.clone(),
                tests,
            },
            failed_tree: failed_tree.clone(),
        });
        let result = self.solve(vec![pr]);
        self.rerunning = None;
        result
    }

    fn check_alone(&mut self, pr: PrNumber, base: &Sha) -> Result<Alone> {
        let item = self.merge_item(pr);
        let result = self.slot.merge(base, &[item])?;
        match result.items.into_iter().next().map(|o| o.result) {
            Some(ItemResult::Merged { .. }) => {
                match self.verify(&result.tip, Scope::Changed, false)? {
                    Verdict::Pass => Ok(Alone::Pass),
                    Verdict::Fail(failed) => Ok(Alone::Fail {
                        failed,
                        tip: result.tip,
                    }),
                }
            }
            Some(ItemResult::AlreadyMerged) => Ok(Alone::Pass),
            Some(ItemResult::Conflict(_)) => Ok(Alone::Conflict),
            None => bail!("합치기 결과가 비어 있다"),
        }
    }

    fn check_base(&mut self, base: &Sha, again: bool) -> Result<Option<Failed>> {
        let engine = self.engine;
        if !again && engine.store.load()?.verified_bases.contains(base) {
            return Ok(None);
        }
        self.slot.checkout(base)?;
        match self.verify(base, Scope::Whole, false)? {
            Verdict::Pass => {
                engine.store.update(|s| {
                    if !s.verified_bases.contains(base) {
                        s.verified_bases.push(base.clone());
                    }
                    Ok(())
                })?;
                Ok(None)
            }
            Verdict::Fail(failed) => Ok(Some(failed)),
        }
    }

    fn base_sound(&mut self, base: &Sha) -> Result<bool> {
        Ok(self.check_base(base, false)?.is_none())
    }

    fn require_sound_base(&mut self, base: &Sha) -> Result<()> {
        match self.check_base(base, false)? {
            None => Ok(()),
            Some(failed) => self.halt(base, &failed, false),
        }
    }

    fn confirm_solo_base(&mut self, pr: PrNumber, base: &Sha) -> Result<()> {
        let engine = self.engine;
        let doubted = engine
            .store
            .load()?
            .base_doubt
            .is_some_and(|d| d.base == *base && d.prs.iter().any(|p| *p != pr));
        if doubted {
            return match self.check_base(base, true)? {
                None => Ok(()),
                Some(failed) => self.halt(base, &failed, true),
            };
        }
        self.require_sound_base(base)?;
        engine.store.update(|s| {
            let mut doubt = match s.base_doubt.take() {
                Some(d) if d.base == *base => d,
                _ => BaseDoubt {
                    base: base.clone(),
                    prs: Vec::new(),
                },
            };
            if !doubt.prs.contains(&pr) {
                doubt.prs.push(pr);
            }
            s.base_doubt = Some(doubt);
            Ok(())
        })
    }

    fn clear_base_doubt(&self) -> Result<()> {
        let store = &self.engine.store;
        if store.load()?.base_doubt.is_none() {
            return Ok(());
        }
        store.update(|s| {
            s.base_doubt = None;
            Ok(())
        })
    }

    fn halt(&mut self, base: &Sha, failed: &Failed, rechecked: bool) -> Result<()> {
        let engine = self.engine;
        let pushed_by_chute = self.pushed_by_chute(base);
        engine.store.update(|s| {
            s.blocked = Some(Blocked {
                reason: BlockReason::BaseBroken,
                base: base.clone(),
                since_ms: EpochMs::now(),
                step: Some(failed.step.clone()),
                log: Some(failed.log.clone()),
                pushed_by_chute,
                rechecked,
            });
            self.release_items(s)
        })?;
        let at = engine.history_at(EventKind::Blocked {
            reason: BlockReason::BaseBroken,
            base: base.clone(),
        })?;
        engine.tell_halted(at);
        Err(Halt::Blocked.into())
    }

    fn pushed_by_chute(&self, commit: &Sha) -> bool {
        let store = &self.engine.store;
        let ids = store.batch_ids().unwrap_or_else(|e| {
            eprintln!("경고: batch 기록의 목록을 읽지 못했다: {e:#}");
            Vec::new()
        });
        ids.iter().any(|id| match store.read_batch(id) {
            Ok(record) => record.is_some_and(|r| r.pushed.contains(commit)),
            Err(e) => {
                eprintln!("경고: batch {} 기록을 읽지 못했다: {e:#}", id.0);
                false
            }
        })
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
                    let at = engine.history_at(EventKind::Merged {
                        pr,
                        batch: self.id().clone(),
                        commit: held.head.clone(),
                        submitted_ms: held.submitted_ms,
                        title: Some(held.title.clone()),
                    })?;
                    engine.store.update(|s| Ok(queue::merged(s, pr)?))?;
                    engine.tell_merged(&held, self.id(), &held.head, at);
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

    fn settle_flaky(&mut self) {
        if let Err(e) = self.gather_flaky_evidence() {
            eprintln!("경고: 흔들림의 근거를 모으지 못했다: {e:#}");
        }
    }

    fn gather_flaky_evidence(&mut self) -> Result<()> {
        let engine = self.engine;
        let flaky = self.record.flaky.clone();
        let chronic_warned = engine
            .store
            .load()?
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::FlakyChronic);
        if flaky.is_empty() && !chronic_warned {
            return Ok(());
        }
        let mut made: Vec<Option<Vec<String>>> = vec![None; flaky.len()];
        for (n, entry) in flaky.iter().enumerate() {
            let command = self
                .config
                .verify
                .iter()
                .find(|s| s.name == entry.step)
                .and_then(|s| s.probe.as_deref());
            let landed = self.record.pushed.contains(&entry.tree);
            let Some(command) = command.filter(|_| landed && !entry.tests.is_empty()) else {
                continue;
            };
            let found = match flaky::probe(&flaky::ProbeJob {
                config: &self.config,
                store: &engine.store,
                slot: &self.slot,
                batch: self.id(),
                command,
                step: &entry.step,
                tests: &entry.tests,
                base: &self.start_base,
                flaked: &entry.tree,
                cancel: &engine.cancel,
                cache_dir: engine.gitops.cache_dir(),
            }) {
                Ok(found) => found,
                Err(e) => {
                    eprintln!(
                        "경고: 단계 {}의 흔들림을 되풀이하지 못했다: {e:#}",
                        entry.step
                    );
                    continue;
                }
            };
            engine.history(EventKind::FlakyProbe {
                batch: self.id().clone(),
                step: entry.step.clone(),
                tests: found.tests.clone(),
                evidence: found.evidence,
                runs: found.runs,
                failures: found.failures,
            })?;
            if found.evidence == ProbeEvidence::Made {
                made[n] = Some(found.tests);
            }
        }
        let touched: Vec<flaky::Key> = flaky
            .iter()
            .flat_map(|f| flaky::keys_of(&f.step, &f.tests))
            .collect();
        let mut due = flaky::refresh_chronic(
            &engine.store,
            &engine.store.history()?.events,
            EpochMs::now(),
            &self.config.flaky,
            &touched,
        )?;
        let targets: Vec<(Option<&Agent>, PrNumber)> = self
            .merged
            .iter()
            .filter_map(|pr| self.items.get(pr))
            .map(|i| (i.submitter.as_ref(), i.pr))
            .collect();
        for (entry, made) in flaky.iter().zip(made) {
            let keys = flaky::keys_of(&entry.step, &entry.tests);
            let chronic = keys.iter().any(|k| due.contains(k));
            due.retain(|k| !keys.contains(k));
            if (made.is_some() || chronic) && !targets.is_empty() {
                engine
                    .notifier()
                    .notify_each(&targets, EpochMs::now(), |prs| Line::Flaky {
                        batch: self.id().clone(),
                        prs: prs.to_vec(),
                        step: entry.step.clone(),
                        tests: made.clone().unwrap_or_else(|| entry.tests.clone()),
                        made: made.is_some(),
                        chronic,
                    });
            }
        }
        Ok(())
    }

    fn release_items(&self, s: &mut State) -> Result<()> {
        for pr in self.items.keys() {
            let held = s.queue.iter().find(|i| i.pr == *pr);
            if held.is_some_and(|i| i.state == ItemState::InBatch) {
                queue::requeue(s, *pr)?;
                if self.config.require_ci && self.was_waiting_on_ci.contains(pr) {
                    queue::set_ci_pending(s, *pr)?;
                }
                if let Some(batch) = &mut s.batch {
                    batch.prs.retain(|p| p != pr);
                }
            }
        }
        Ok(())
    }

    fn wind_down(&mut self) -> Result<()> {
        let engine = self.engine;
        let now = EpochMs::now();
        self.record.finished_ms = Some(now);
        if self.only_deferred() {
            engine.store.remove_batch(self.id())?;
        } else {
            self.save_record()?;
        }
        if !self.merged.is_empty() || !self.bounced.is_empty() {
            engine.history(EventKind::BatchFinished {
                batch: self.id().clone(),
                merged: self.merged.clone(),
                bounced: self.bounced.clone(),
                seconds: now.0.saturating_sub(self.record.started_ms.0) / 1000,
                verify_s: self.record.verify_s,
            })?;
        }
        engine.store.update(|s| {
            self.release_items(s)?;
            s.batch = None;
            Ok(())
        })?;
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

fn unreachable_of(delivery: &Delivery) -> Option<String> {
    match delivery {
        Delivery::Unreachable(why) => Some(why.to_string()),
        Delivery::Sent { .. } | Delivery::Spawned(_) => None,
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

pub(crate) fn reap_orphans(slot: &Path) -> Result<()> {
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
            confirmed: None,
            first_attempt_tests: None,
            named_by: None,
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
