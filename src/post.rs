use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::{Config, PostVerify, VerifyStep};
use crate::engine::reap_orphans;
use crate::ext::Ext;
use crate::flaky;
use crate::gitops::{Gitops, Slot, SlotLock, SlotName};
use crate::model::{
    Agent, BatchId, Culprit, EpochMs, Event, EventKind, GreenMark, MergeRef, NonEmpty, PostPhase,
    PostRed, PostRun, PostState, PrNumber, RepoInfo, Sha,
};
use crate::notify::{Line, Notifier};
use crate::pc_limit::Wait;
use crate::store::{Home, RepoStore};
use crate::verify::{self, Cancel, Detail, Failed, FlakyStep, Options, Outcome, Wanted};

const SLOT_LOCK_TRIES: u32 = 40;
const SLOT_LOCK_PAUSE: Duration = Duration::from_millis(50);
const MINUTE_MS: u64 = 60 * 1000;
const REPORT_TAIL_CHARS: usize = 3000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostPass {
    Idle,
    Passed,
    Failed,
    Cancelled,
}

enum Checked {
    Passed { flaky: Vec<FlakyStep> },
    Failed(Failed),
    Cancelled,
}

struct Scope<'a> {
    slot: &'a Slot,
    config: &'a Config,
    post: &'a PostVerify,
    baseline: Option<&'a Sha>,
}

enum Search {
    Found(Option<Culprit>),
    Cancelled,
}

pub struct PostVerifier {
    ext: Ext,
    home: Home,
    info: RepoInfo,
    store: RepoStore,
    gitops: Gitops,
    cancel: Cancel,
}

impl PostVerifier {
    pub fn new(ext: Ext, home: Home, info: RepoInfo) -> PostVerifier {
        let store = home.repo(&info.slug);
        let gitops = Gitops::new(&ext, &home, &info);
        PostVerifier {
            ext,
            home,
            info,
            store,
            gitops,
            cancel: Cancel::new(),
        }
    }

    pub fn with_cancel(mut self, cancel: Cancel) -> PostVerifier {
        self.cancel = cancel;
        self
    }

    fn slot_dir(&self) -> PathBuf {
        let dir = self.home.slots_dir(&self.info.slug).join("post");
        std::path::absolute(&dir).unwrap_or(dir)
    }

    pub fn recover(&self) -> Result<()> {
        let lock = self.lock_slot(&[])?;
        reap_orphans(&self.slot_dir())?;
        drop(lock);
        self.store.update(|s| {
            s.post.run = None;
            Ok(())
        })
    }

    /// A process this one starts holds every lock file this one has open until it execs, so
    /// a slot that looks held may be free a moment later.
    fn lock_slot(&self, cache_seed: &[String]) -> Result<SlotLock> {
        let gitops = self.gitops.clone().with_cache_seed(cache_seed.to_vec());
        for _ in 0..SLOT_LOCK_TRIES {
            if let Some(lock) = gitops.lock_slot(SlotName::Post)? {
                return Ok(lock);
            }
            thread::sleep(SLOT_LOCK_PAUSE);
        }
        bail!("머지 뒤 검사 슬롯을 다른 프로세스가 쥐고 있다")
    }

    pub fn run_once(&self) -> Result<PostPass> {
        if self.cancel.is_cancelled() {
            return Ok(PostPass::Cancelled);
        }
        let known = Config::load_remote_branch(
            &self.ext,
            &self.info.path,
            &self.info.remote_name,
            &self.info.default_branch,
        );
        if !matches!(known, Ok(Some(Config { post: Some(_), .. }))) {
            return Ok(PostPass::Idle);
        }
        let tip = self.gitops.fetch_base_tip()?;
        let Ok(config) = Config::load_at(&self.ext, &self.info.path, tip.as_str()) else {
            return Ok(PostPass::Idle);
        };
        let Some(post) = config.post.clone() else {
            return Ok(PostPass::Idle);
        };
        let before = self.store.load()?.post;
        if !is_due(&before, &tip, &post, EpochMs::now()) {
            return Ok(PostPass::Idle);
        }
        let slot = self.lock_slot(&config.cache.seed)?.open()?;
        self.store.update(|s| {
            let now = EpochMs::now();
            s.post.last_started_ms = Some(now);
            s.post.run = Some(PostRun {
                tip: tip.clone(),
                phase: PostPhase::Verifying,
                commit: tip.clone(),
                step: None,
                started_ms: now,
                waiting_machine: false,
            });
            Ok(())
        })?;
        let baseline = before.last_green.as_ref().map(|g| &g.commit);
        let scope = Scope {
            slot: &slot,
            config: &config,
            post: &post,
            baseline,
        };
        let ran = self.check_tip(&scope, &tip, &before);
        let cleared = self.store.update(|s| {
            s.post.run = None;
            Ok(())
        });
        let pass = ran?;
        cleared?;
        Ok(pass)
    }

    fn check_tip(&self, scope: &Scope, tip: &Sha, before: &PostState) -> Result<PostPass> {
        let started = Instant::now();
        match self.check(scope, tip, PostPhase::Verifying, None)? {
            Checked::Cancelled => Ok(PostPass::Cancelled),
            Checked::Passed { flaky } => {
                self.record_flaky(scope, tip, flaky)?;
                self.green(tip, started.elapsed())?;
                Ok(PostPass::Passed)
            }
            Checked::Failed(failed) => {
                if self.fail(scope, tip, before, &failed)? {
                    Ok(PostPass::Failed)
                } else {
                    Ok(PostPass::Cancelled)
                }
            }
        }
    }

    fn check(
        &self,
        scope: &Scope,
        commit: &Sha,
        phase: PostPhase,
        wanted: Option<Wanted>,
    ) -> Result<Checked> {
        let slot = scope.slot;
        slot.checkout(commit)?;
        let changed = scope
            .baseline
            .and_then(|base| slot.changed_files(base).ok());
        self.store.update(|s| {
            if let Some(run) = &mut s.post.run {
                run.phase = phase;
                run.commit = commit.clone();
                run.step = None;
            }
            Ok(())
        })?;
        let on_step = |name: &str| {
            let _ = self.store.update(|s| {
                if let Some(run) = &mut s.post.run {
                    run.step = Some(name.to_owned());
                }
                Ok(())
            });
        };
        let on_machine_wait = |wait: Wait| {
            let _ = self.store.update(|s| {
                if let Some(run) = &mut s.post.run {
                    run.waiting_machine = matches!(wait, Wait::Begin { .. });
                }
                Ok(())
            });
        };
        let steps = Config {
            serialize: false,
            verify: self.current_steps(scope),
            ..scope.config.clone()
        };
        let outcome = verify::run(
            &steps,
            &self.store,
            slot.path(),
            &self.logs_dir(commit),
            &Options {
                cancel: self.cancel.clone(),
                cache_dir: Some(self.gitops.cache_dir()),
                on_step: Some(&on_step),
                on_machine_wait: Some(&on_machine_wait),
                rerun_with_probe: true,
                wanted,
                changed: changed.as_deref(),
                ..Options::default()
            },
        )?;
        Ok(match outcome {
            Outcome::Passed { flaky } => Checked::Passed { flaky },
            Outcome::Failed(failed) => Checked::Failed(failed),
            Outcome::Cancelled => Checked::Cancelled,
        })
    }

    fn current_steps(&self, scope: &Scope) -> NonEmpty<VerifyStep> {
        let latest = self
            .gitops
            .fetch_base_tip()
            .and_then(|tip| Config::load_at(&self.ext, &self.info.path, tip.as_str()))
            .ok()
            .and_then(|config| config.post);
        let steps = scope.post.steps.iter().map(|step| {
            let now = latest
                .iter()
                .flat_map(|post| post.steps.iter())
                .find(|now| now.name == step.name);
            match now {
                Some(now) => VerifyStep {
                    probe: now.probe.clone(),
                    failed: now.failed.clone(),
                    ..step.clone()
                },
                None => step.clone(),
            }
        });
        NonEmpty::new(steps.collect()).unwrap_or_else(|| scope.post.steps.clone())
    }

    fn report_dir(&self, commit: &Sha) -> PathBuf {
        self.store.commit_report_dir(commit.short()).join("post")
    }

    fn logs_dir(&self, commit: &Sha) -> PathBuf {
        self.report_dir(commit).join("verify")
    }

    fn history(&self, kind: EventKind) -> Result<EpochMs> {
        let at_ms = EpochMs::now();
        self.store.append_history(&Event { at_ms, kind })?;
        Ok(at_ms)
    }

    fn record_flaky(&self, scope: &Scope, commit: &Sha, flaky: Vec<FlakyStep>) -> Result<()> {
        if flaky.is_empty() {
            return Ok(());
        }
        let touched: Vec<flaky::Key> = flaky
            .iter()
            .flat_map(|f| flaky::keys_of(&f.step, &f.tests))
            .collect();
        for step in flaky {
            self.history(EventKind::Flaky {
                batch: BatchId(format!("post-{}", commit.short())),
                step: step.step,
                tests: step.tests,
                tree: scope.slot.tree_of(commit)?,
            })?;
        }
        flaky::refresh_chronic(
            &self.store,
            &self.store.history()?.events,
            EpochMs::now(),
            &scope.config.flaky,
            &touched,
        )?;
        Ok(())
    }

    fn green(&self, tip: &Sha, took: Duration) -> Result<()> {
        let now = EpochMs::now();
        let (previous, was_red) = self.store.update(|s| {
            let previous = s.post.last_green.replace(GreenMark {
                commit: tip.clone(),
                at_ms: now,
            });
            Ok((previous.map(|g| g.commit), s.post.red.take()))
        })?;
        self.history(EventKind::PostGreen {
            commit: tip.clone(),
            previous,
            seconds: took.as_secs(),
        })?;
        let Some(red) = was_red else { return Ok(()) };
        let pr = red.culprit.as_ref().and_then(|c| c.pr);
        let at = self.history(EventKind::PostFixed {
            commit: tip.clone(),
            was: red.commit.clone(),
            pr,
        })?;
        let line = Line::PostFixed {
            commit: tip.clone(),
            was: red.commit,
            pr,
        };
        let merge = red
            .culprit
            .as_ref()
            .map(|c| self.merge_of(&c.commit))
            .transpose()?
            .flatten();
        self.tell(merge.as_ref().map(|(_, m)| m), pr, at, &line);
        Ok(())
    }

    fn fail(&self, scope: &Scope, tip: &Sha, before: &PostState, failed: &Failed) -> Result<bool> {
        let baseline = scope.baseline;
        let tests = failed.tests();
        if let Some(old) = &before.red
            && old.step == failed.step
            && old.tests == tests
            && self.still_there(scope.slot, old, tip)
        {
            let log = failed.log.clone();
            self.store.update(|s| {
                if let Some(red) = &mut s.post.red {
                    red.commit = tip.clone();
                    red.log = log;
                }
                Ok(())
            })?;
            return Ok(true);
        }
        let culprit = match self.locate(scope, tip, failed)? {
            Search::Found(culprit) => culprit,
            Search::Cancelled => return Ok(false),
        };
        let merge = culprit
            .as_ref()
            .map(|c| self.merge_of(&c.commit))
            .transpose()?
            .flatten();
        let culprit = culprit.map(|c| Culprit {
            pr: merge.as_ref().map(|(_, m)| m.pr),
            batch: merge.as_ref().map(|(batch, _)| batch.clone()),
            ..c
        });
        let report = self.write_report(tip, baseline, failed, &tests, culprit.as_ref(), &merge)?;
        let red = PostRed {
            commit: tip.clone(),
            step: failed.step.clone(),
            tests: tests.clone(),
            log: failed.log.clone(),
            report: Some(report.clone()),
            since_ms: EpochMs::now(),
            last_green: baseline.cloned(),
            culprit: culprit.clone(),
            holding: scope.post.hold,
        };
        self.store.update(|s| {
            s.post.red = Some(red);
            Ok(())
        })?;
        let at = self.history(EventKind::PostFailed {
            commit: tip.clone(),
            step: failed.step.clone(),
            tests: tests.clone(),
            last_green: baseline.cloned(),
            culprit: culprit.clone(),
        })?;
        let pr = culprit.as_ref().and_then(|c| c.pr);
        let line = Line::PostFailed {
            commit: tip.clone(),
            step: failed.step.clone(),
            tests,
            last_green: baseline.cloned(),
            pr,
            batch: culprit.and_then(|c| c.batch),
            log: failed.log.clone(),
            report: Some(report),
        };
        self.tell(merge.as_ref().map(|(_, m)| m), pr, at, &line);
        Ok(true)
    }

    fn still_there(&self, slot: &Slot, old: &PostRed, tip: &Sha) -> bool {
        old.culprit
            .as_ref()
            .is_none_or(|c| slot.is_ancestor(&c.commit, tip).unwrap_or(false))
    }

    fn locate(&self, scope: &Scope, tip: &Sha, failure: &Failed) -> Result<Search> {
        let wanted = failure.tests();
        let same = |other: &Failed| {
            other.step == failure.step && {
                let seen = other.confirmed.clone().unwrap_or_else(|| other.tests());
                wanted.iter().all(|test| seen.contains(test))
            }
        };
        let Some(good) = scope.baseline else {
            return Ok(Search::Found(None));
        };
        if !scope.slot.is_ancestor(good, tip).unwrap_or(false) {
            return Ok(Search::Found(None));
        }
        let commits = scope.slot.first_parent_range(good, tip)?;
        if commits.is_empty() {
            return Ok(Search::Found(None));
        }
        let mut passing: isize = -1;
        let mut bad = commits.len() - 1;
        let mut different: Vec<isize> = Vec::new();
        while bad as isize - passing > 1 {
            let mid = (passing + bad as isize) / 2;
            let mut decided = false;
            for probe in probes(passing, bad as isize, mid, &different) {
                let commit = &commits[probe as usize];
                let search = Wanted {
                    step: &failure.step,
                    tests: &wanted,
                };
                match self.check(scope, commit, PostPhase::Searching, Some(search))? {
                    Checked::Cancelled => return Ok(Search::Cancelled),
                    Checked::Passed { flaky } => {
                        self.record_flaky(scope, commit, flaky)?;
                        passing = probe;
                        decided = true;
                    }
                    Checked::Failed(other) if same(&other) => {
                        bad = probe as usize;
                        decided = true;
                    }
                    Checked::Failed(_) => different.push(probe),
                }
                if decided {
                    break;
                }
            }
            if !decided {
                return Ok(Search::Found(None));
            }
        }
        Ok(Search::Found(Some(Culprit {
            commit: commits[bad].clone(),
            pr: None,
            batch: None,
        })))
    }

    fn merge_of(&self, commit: &Sha) -> Result<Option<(BatchId, MergeRef)>> {
        for id in self.store.batch_ids()?.into_iter().rev() {
            let Ok(Some(record)) = self.store.read_batch(&id) else {
                continue;
            };
            if let Some(found) = record.merges.into_iter().find(|m| m.commit == *commit) {
                return Ok(Some((id, found)));
            }
        }
        Ok(None)
    }

    fn tell(&self, merge: Option<&MergeRef>, pr: Option<PrNumber>, at: EpochMs, line: &Line) {
        let notifier = Notifier::new(&self.ext, &self.info, &self.store);
        let (Some(merge), Some(pr)) = (merge, pr) else {
            notifier.notify(None, at, line);
            return;
        };
        let targets: Vec<(Option<&Agent>, PrNumber)> =
            vec![(merge.submitter.as_ref(), pr), (merge.agent.as_ref(), pr)];
        notifier.notify_each(&targets, at, |_| line.clone());
    }

    fn write_report(
        &self,
        tip: &Sha,
        baseline: Option<&Sha>,
        failed: &Failed,
        tests: &[String],
        culprit: Option<&Culprit>,
        merge: &Option<(BatchId, MergeRef)>,
    ) -> Result<PathBuf> {
        let dir = self.report_dir(tip);
        fs::create_dir_all(&dir).with_context(|| format!("만들지 못했다: {}", dir.display()))?;
        let mut text = format!(
            "# 머지 뒤 검사 실패\n\n- 기본 브랜치 커밋: {tip}\n- 실패한 단계: {}\n- 로그: {}\n",
            failed.step,
            failed.log.display()
        );
        match baseline {
            Some(green) => text.push_str(&format!("- 마지막으로 통과한 커밋: {green}\n")),
            None => text
                .push_str("- 마지막으로 통과한 커밋: 없음(처음 도는 검사라 범인을 찾지 못했다)\n"),
        }
        if !tests.is_empty() {
            text.push_str(&format!("- 실패한 시험: {}\n", tests.join(", ")));
        }
        match (culprit, merge) {
            (Some(c), Some((batch, m))) => {
                text.push_str(&format!(
                    "- 처음 실패한 머지: PR {} (batch {}, 커밋 {})\n\n되돌리려면 오케스트레이터가 정한다. chute는 자동으로 되돌리지 않는다. 되돌리는 명령: `chute revert {}`\n",
                    m.pr.0, batch.0, c.commit, batch.0
                ));
            }
            (Some(c), None) => text.push_str(&format!(
                "- 처음 실패한 커밋: {} (chute가 머지한 것이 아니라 batch 기록에 없다)\n",
                c.commit
            )),
            (None, _) => {}
        }
        if let Detail::Tail(tail) = &failed.detail {
            let tail: String = tail.chars().rev().take(REPORT_TAIL_CHARS).collect();
            let tail: String = tail.chars().rev().collect();
            text.push_str(&format!("\n## 로그 끝부분\n\n```\n{tail}\n```\n"));
        }
        let path = dir.join("report.md");
        fs::write(&path, text).with_context(|| format!("쓰지 못했다: {}", path.display()))?;
        Ok(path)
    }
}

/// The commits strictly between `passing` and `bad`, nearest to `mid` first, leaving out those
/// already known to fail differently. Such a commit says nothing about the failure being
/// located, so the search moves to its neighbours.
fn probes(passing: isize, bad: isize, mid: isize, skip: &[isize]) -> Vec<isize> {
    let mut order = Vec::new();
    for distance in 0..(bad - passing) {
        for probe in [mid + distance, mid - distance] {
            if probe > passing && probe < bad && !skip.contains(&probe) && !order.contains(&probe) {
                order.push(probe);
            }
        }
    }
    order
}

fn is_due(state: &PostState, tip: &Sha, post: &PostVerify, now: EpochMs) -> bool {
    if state.last_green.as_ref().is_some_and(|g| g.commit == *tip) {
        return false;
    }
    if state.red.as_ref().is_some_and(|r| r.commit == *tip) {
        return false;
    }
    match state.last_started_ms {
        Some(started) if post.interval_minutes > 0 => {
            now.0 >= started.0 + u64::from(post.interval_minutes) * MINUTE_MS
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(hex: &str) -> Sha {
        Sha::try_from(hex.to_owned()).unwrap()
    }

    fn post(interval_minutes: u32) -> PostVerify {
        let config = Config::parse(
            "[[verify]]\nname = \"a\"\nrun = \"true\"\n[[post_verify]]\nname = \"b\"\nrun = \"true\"\n",
        )
        .unwrap();
        PostVerify {
            interval_minutes,
            ..config.post.unwrap()
        }
    }

    fn green(commit: &str) -> Option<GreenMark> {
        Some(GreenMark {
            commit: sha(commit),
            at_ms: EpochMs(1),
        })
    }

    #[test]
    fn probes_start_at_the_middle_and_stay_inside_the_open_range() {
        assert_eq!(probes(-1, 5, 2, &[]), [2, 3, 1, 4, 0]);
        assert_eq!(probes(1, 4, 2, &[]), [2, 3]);
        assert!(probes(2, 3, 2, &[]).is_empty());
        assert_eq!(probes(-1, 5, 2, &[2, 3]), [1, 4, 0]);
    }

    #[test]
    fn a_tip_is_due_unless_it_was_the_last_green_or_the_last_failure() {
        let tip = sha("abc1234");
        let mut state = PostState::default();
        assert!(is_due(&state, &tip, &post(0), EpochMs(5)));
        state.last_green = green("abc1234");
        assert!(!is_due(&state, &tip, &post(0), EpochMs(5)));
        assert!(is_due(&state, &sha("def5678"), &post(0), EpochMs(5)));
        state.last_green = None;
        state.red = Some(PostRed {
            commit: tip.clone(),
            step: "b".into(),
            tests: Vec::new(),
            log: "/log".into(),
            report: None,
            since_ms: EpochMs(1),
            last_green: None,
            culprit: None,
            holding: false,
        });
        assert!(!is_due(&state, &tip, &post(0), EpochMs(5)));
    }

    #[test]
    fn the_interval_counts_from_the_start_of_the_last_run() {
        let tip = sha("abc1234");
        let state = PostState {
            last_started_ms: Some(EpochMs(1_000)),
            ..PostState::default()
        };
        let at = |minutes: u64, ms: u64| EpochMs(1_000 + minutes * 60_000 + ms);
        assert!(is_due(&state, &tip, &post(0), EpochMs(1_000)));
        assert!(!is_due(
            &state,
            &tip,
            &post(10),
            EpochMs(1_000 + 10 * 60_000 - 1)
        ));
        assert!(is_due(&state, &tip, &post(10), at(10, 0)));
    }
}
