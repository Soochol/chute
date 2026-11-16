//! The one rule for what chute leaves behind: verification logs, reports, failed-tree refs
//! and surplus preflight slots. Records (`state.json`, `history.jsonl`, `batch.json`,
//! `revert.json`) and worktrees that respawn made are never touched.
//!
//! Only this PC's state folder and repository are swept. Whatever another PC left is swept
//! by that PC's chute with the same rule.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::config::{Config, Retention};
use crate::ext::Ext;
use crate::gitops::{FAILED_REF_PREFIX, Gitops, SlotLock};
use crate::hub;
use crate::model::{BatchId, EpochMs, PrNumber, RepoInfo, Warning, WarningKind};
use crate::store::{Home, RepoStore};

pub const PREFLIGHT_LOG_DIR: &str = "preflight";
pub const PREFLIGHT_REF_STEM: &str = "preflight-";

const LAST_RUN_FILE: &str = "gc.json";
const AUTO_EVERY_MS: u64 = 60 * 60 * 1000;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const BATCH_ID_LEN: usize = "b20261116-141025".len();

pub struct Site<'a> {
    pub ext: &'a Ext,
    pub home: &'a Home,
    pub info: &'a RepoInfo,
    pub store: &'a RepoStore,
}

pub struct Rules {
    pub retention: Retention,
    pub max_parallel: u32,
    pub queue: Option<BTreeSet<PrNumber>>,
    pub batch: Option<BatchId>,
    pub blocked_log: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    BatchLog,
    PreflightLog,
    PrReport,
    CommitReport,
    BatchRef,
    PreflightRef,
    Slot,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::BatchLog => "batch 검증 로그",
            Kind::PreflightLog => "preflight 로그",
            Kind::PrReport => "반송 보고서",
            Kind::CommitReport => "preflight 보고서",
            Kind::BatchRef => "batch 실패 참조",
            Kind::PreflightRef => "preflight 실패 참조",
            Kind::Slot => "preflight 슬롯",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub kind: Kind,
    pub target: String,
    pub bytes: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Swept {
    /// What was removed, or with `dry_run` what would be.
    pub items: Vec<Item>,
    pub failures: Vec<String>,
}

impl Swept {
    pub fn bytes(&self) -> u64 {
        self.items.iter().map(|item| item.bytes).sum()
    }
}

enum Doomed {
    Dir(PathBuf),
    Ref(String),
    Slot(Box<SlotLock>),
}

struct Planned {
    item: Item,
    doomed: Doomed,
}

pub fn sweep(site: &Site, rules: &Rules, now: EpochMs, dry_run: bool) -> Swept {
    let mut swept = Swept::default();
    let mut planned = Vec::new();
    for found in [
        plan_batch_logs(site, rules, now),
        plan_preflight_logs(site, rules, now),
        plan_reports(site, rules, now),
        plan_failed_refs(site, rules, now),
        plan_slots(site, rules),
    ] {
        match found {
            Ok(more) => planned.extend(more),
            Err(e) => swept.failures.push(format!("{e:#}")),
        }
    }
    let gitops = Gitops::new(site.ext, site.home, site.info);
    for Planned { item, doomed } in planned {
        let removed = if dry_run {
            Ok(())
        } else {
            remove(&gitops, doomed)
        };
        match removed {
            Ok(()) => swept.items.push(item),
            Err(e) => swept.failures.push(format!("{}: {e:#}", item.target)),
        }
    }
    swept
}

fn remove(gitops: &Gitops, doomed: Doomed) -> Result<()> {
    match doomed {
        Doomed::Dir(path) => match fs::remove_dir_all(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        },
        Doomed::Ref(name) => gitops.delete_failed_ref(&name),
        Doomed::Slot(lock) => lock.remove(),
    }
}

fn cutoff(now: EpochMs, days: u32) -> u64 {
    now.0.saturating_sub(u64::from(days) * DAY_MS)
}

fn plan_batch_logs(site: &Site, rules: &Rules, now: EpochMs) -> Result<Vec<Planned>> {
    let cutoff = cutoff(now, rules.retention.logs_days);
    let mut planned = Vec::new();
    for batch in subdirs(&site.store.dir().join("batches"))? {
        if rules.batch.as_ref().map(|id| id.0.as_str()) == name_of(&batch).as_deref() {
            continue;
        }
        planned.extend(old_dirs(&batch, Kind::BatchLog, cutoff, rules, |name| {
            name.starts_with("verify-")
        })?);
    }
    Ok(planned)
}

fn plan_preflight_logs(site: &Site, rules: &Rules, now: EpochMs) -> Result<Vec<Planned>> {
    let root = site.store.dir().join(PREFLIGHT_LOG_DIR);
    let cutoff = cutoff(now, rules.retention.logs_days);
    old_dirs(&root, Kind::PreflightLog, cutoff, rules, |_| true)
}

fn plan_reports(site: &Site, rules: &Rules, now: EpochMs) -> Result<Vec<Planned>> {
    let root = site.store.dir().join("reports");
    let cutoff = cutoff(now, rules.retention.reports_days);
    let mut planned = old_dirs(&root, Kind::PrReport, cutoff, rules, |name| {
        let Some(pr) = name.strip_prefix("pr-").and_then(|n| n.parse().ok()) else {
            return false;
        };
        rules
            .queue
            .as_ref()
            .is_none_or(|queue| !queue.contains(&PrNumber(pr)))
    })?;
    planned.extend(old_dirs(
        &root,
        Kind::CommitReport,
        cutoff,
        rules,
        |name| name.starts_with("commit-"),
    )?);
    Ok(planned)
}

fn plan_failed_refs(site: &Site, rules: &Rules, now: EpochMs) -> Result<Vec<Planned>> {
    let listed = site
        .ext
        .git(
            &site.info.path,
            ["for-each-ref", "--format=%(refname)", FAILED_REF_PREFIX],
        )?
        .ok()?;
    let cutoff = cutoff(now, rules.retention.reports_days);
    let mut planned = Vec::new();
    for full in listed.lines() {
        let Some(name) = full.strip_prefix(FAILED_REF_PREFIX) else {
            continue;
        };
        let (kind, doomed) = match parse_ref(name) {
            Some(Made::Preflight(made)) => (Kind::PreflightRef, made.0 < cutoff),
            Some(Made::Batch { pr, made }) => {
                let doomed = match &rules.queue {
                    Some(queue) => !queue.contains(&pr),
                    None => made.is_some_and(|made| made.0 < cutoff),
                };
                (Kind::BatchRef, doomed)
            }
            None => continue,
        };
        if doomed {
            planned.push(Planned {
                item: Item {
                    kind,
                    target: full.to_owned(),
                    bytes: 0,
                },
                doomed: Doomed::Ref(name.to_owned()),
            });
        }
    }
    Ok(planned)
}

fn plan_slots(site: &Site, rules: &Rules) -> Result<Vec<Planned>> {
    let gitops = Gitops::new(site.ext, site.home, site.info);
    let locks = gitops.lock_surplus_preflight_slots(rules.max_parallel)?;
    Ok(locks
        .into_iter()
        .map(|lock| {
            let path = lock.path();
            Planned {
                item: Item {
                    kind: Kind::Slot,
                    target: path.display().to_string(),
                    bytes: tree_size(&path),
                },
                doomed: Doomed::Slot(Box::new(lock)),
            }
        })
        .collect())
}

fn old_dirs(
    root: &Path,
    kind: Kind,
    cutoff: u64,
    rules: &Rules,
    name_ok: impl Fn(&str) -> bool,
) -> Result<Vec<Planned>> {
    let mut planned = Vec::new();
    for dir in subdirs(root)? {
        let accepted = name_of(&dir).is_some_and(|name| name_ok(&name));
        let holds_blocked_log = rules
            .blocked_log
            .as_ref()
            .is_some_and(|log| log.starts_with(&dir));
        if !accepted || holds_blocked_log || modified_ms(&dir)? >= cutoff {
            continue;
        }
        planned.push(Planned {
            item: Item {
                kind,
                target: dir.display().to_string(),
                bytes: tree_size(&dir),
            },
            doomed: Doomed::Dir(dir),
        });
    }
    Ok(planned)
}

fn subdirs(root: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("읽지 못했다: {}", root.display())),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("읽지 못했다: {}", root.display()))?;
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn name_of(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

fn modified_ms(path: &Path) -> Result<u64> {
    let modified = fs::metadata(path)
        .and_then(|m| m.modified())
        .with_context(|| format!("수정 시각을 읽지 못했다: {}", path.display()))?;
    Ok(modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64)
}

fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| tree_size(&entry.path()))
        .sum()
}

enum Made {
    Preflight(EpochMs),
    Batch { pr: PrNumber, made: Option<EpochMs> },
}

fn parse_ref(name: &str) -> Option<Made> {
    if let Some(made) = preflight_ref_made_at(name) {
        return Some(Made::Preflight(made));
    }
    let (pr, batch) = name.strip_prefix("pr")?.split_once('-')?;
    Some(Made::Batch {
        pr: PrNumber(pr.parse().ok()?),
        made: batch_made_at(batch),
    })
}

pub fn preflight_ref_made_at(name: &str) -> Option<EpochMs> {
    let rest = name.strip_prefix(PREFLIGHT_REF_STEM)?;
    let (at, _tree) = rest.split_once('-')?;
    at.parse().ok().map(EpochMs)
}

fn batch_made_at(batch: &str) -> Option<EpochMs> {
    let stamp = batch.get(..BATCH_ID_LEN)?;
    let naive = NaiveDateTime::parse_from_str(stamp, "b%Y%m%d-%H%M%S").ok()?;
    let local = naive.and_local_timezone(Local).earliest()?;
    u64::try_from(local.timestamp_millis()).ok().map(EpochMs)
}

#[derive(Serialize, Deserialize)]
struct LastRun {
    at_ms: EpochMs,
}

fn last_run(store: &RepoStore) -> Option<EpochMs> {
    let text = fs::read_to_string(store.dir().join(LAST_RUN_FILE)).ok()?;
    serde_json::from_str::<LastRun>(&text).ok().map(|l| l.at_ms)
}

fn write_last_run(store: &RepoStore, at_ms: EpochMs) -> Result<()> {
    let dir = store.dir();
    fs::create_dir_all(dir).with_context(|| format!("만들지 못했다: {}", dir.display()))?;
    let tmp = dir.join(format!("{LAST_RUN_FILE}.{}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec(&LastRun { at_ms })?)
        .with_context(|| format!("쓰지 못했다: {}", tmp.display()))?;
    fs::rename(&tmp, dir.join(LAST_RUN_FILE))
        .with_context(|| format!("이름을 바꾸지 못했다: {}", tmp.display()))
}

fn is_due(store: &RepoStore, now: EpochMs) -> bool {
    last_run(store).is_none_or(|last| last.0 > now.0 || now.0 - last.0 >= AUTO_EVERY_MS)
}

fn rules_of(site: &Site) -> Result<Option<Rules>> {
    let Some(config) = Config::load_remote_branch(
        site.ext,
        &site.info.path,
        &site.info.remote_name,
        &site.info.default_branch,
    )?
    else {
        return Ok(None);
    };
    let state = site.store.load()?;
    let is_hub = hub::elsewhere(site.home, site.ext, &site.info.slug)?.is_none();
    Ok(Some(Rules {
        retention: config.retention,
        max_parallel: config.max_parallel,
        queue: is_hub.then(|| state.queue.iter().map(|item| item.pr).collect()),
        batch: state.batch.map(|batch| batch.id),
        blocked_log: state.blocked.and_then(|blocked| blocked.log),
    }))
}

/// Sweeps this PC. A real run notes its time and the outcome in the state: a failure leaves a
/// `gc` warning, the next clean run removes it. `None` when there is no `chute.toml` on
/// `<remote>/<default branch>` to take the rules from.
pub fn run(site: &Site, dry_run: bool, now: EpochMs) -> Result<Option<Swept>> {
    let outcome = rules_of(site).map(|rules| rules.map(|rules| sweep(site, &rules, now, dry_run)));
    if !dry_run {
        note(site, &outcome, now);
    }
    outcome
}

/// The sweep that rides along with other commands. It never fails the caller. Unless `force`,
/// it runs at most once an hour, going by the time `gc.json` holds.
pub fn auto(site: &Site, force: bool) -> Option<Swept> {
    let now = EpochMs::now();
    if !force && !is_due(site.store, now) {
        return None;
    }
    match run(site, false, now) {
        Ok(swept) => swept,
        Err(e) => {
            eprintln!("경고: 오래된 파일을 정리하지 못했다: {e:#}");
            None
        }
    }
}

fn note(site: &Site, outcome: &Result<Option<Swept>>, now: EpochMs) {
    let problem = match outcome {
        Ok(Some(swept)) => swept.failures.first().map(|first| {
            format!(
                "오래된 파일 {}건을 정리하지 못했다. 처음 실패: {first}",
                swept.failures.len()
            )
        }),
        Ok(None) => return,
        Err(e) => Some(format!("오래된 파일을 정리하지 못했다: {e:#}")),
    };
    if problem.is_none()
        && let Err(e) = write_last_run(site.store, now)
    {
        eprintln!("경고: 정리한 시각을 기록하지 못했다: {e:#}");
    }
    let has_warning =
        |state: &crate::model::State| state.warnings.iter().any(|w| w.kind == WarningKind::Gc);
    let needs_write = match site.store.load() {
        Ok(state) => problem.is_some() || has_warning(&state),
        Err(_) => true,
    };
    if !needs_write {
        return;
    }
    let written = site.store.update(|state| {
        state.warnings.retain(|w| w.kind != WarningKind::Gc);
        if let Some(message) = &problem {
            state.warnings.push(Warning {
                kind: WarningKind::Gc,
                message: message.clone(),
                pr: None,
                at_ms: now,
            });
        }
        Ok(())
    });
    if let Err(e) = written {
        eprintln!("경고: 정리 경고를 상태에 남기지 못했다: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Batch, BlockReason, Blocked, Phase, Sha, Slug};
    use crate::testkit::{Sandbox, TestRepo};
    use std::time::{Duration, SystemTime};

    const NOW_DAYS_AGO: fn(u64) -> SystemTime =
        |days| SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60 + 60);

    struct World {
        sb: Sandbox,
        repo: TestRepo,
        home: Home,
        ext: Ext,
        info: RepoInfo,
        store: RepoStore,
    }

    impl World {
        fn new() -> World {
            let sb = Sandbox::new();
            let repo = TestRepo::new(&sb, "acme/widget");
            let ext = Ext::from_vars(|k| sb.var(k));
            let home = Home::at(sb.chute_home());
            let info = RepoInfo {
                slug: Slug::new("acme", "widget").unwrap(),
                path: repo.path().to_path_buf(),
                remote: "unused".into(),
                remote_name: "origin".into(),
                default_branch: "main".into(),
            };
            let store = home.repo(&info.slug);
            store.register(&info).unwrap();
            World {
                sb,
                repo,
                home,
                ext,
                info,
                store,
            }
        }

        fn site(&self) -> Site<'_> {
            Site {
                ext: &self.ext,
                home: &self.home,
                info: &self.info,
                store: &self.store,
            }
        }

        fn rules(&self) -> Rules {
            Rules {
                retention: Retention {
                    logs_days: 7,
                    reports_days: 30,
                },
                max_parallel: 2,
                queue: Some(BTreeSet::new()),
                batch: None,
                blocked_log: None,
            }
        }

        fn dir(&self, relative: &str, days: u64) -> PathBuf {
            let dir = self.store.dir().join(relative);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("a.log"), "0123456789").unwrap();
            fs::File::open(&dir)
                .unwrap()
                .set_modified(NOW_DAYS_AGO(days))
                .unwrap();
            dir
        }

        fn sweep(&self, rules: &Rules) -> Vec<(Kind, String)> {
            let swept = sweep(&self.site(), rules, EpochMs::now(), false);
            assert_eq!(swept.failures, Vec::<String>::new());
            self.removed(&swept)
        }

        fn removed(&self, swept: &Swept) -> Vec<(Kind, String)> {
            let root = self.store.dir().display().to_string();
            let mut found: Vec<_> = swept
                .items
                .iter()
                .map(|i| {
                    let short = i
                        .target
                        .strip_prefix(&format!("{root}/"))
                        .unwrap_or(&i.target);
                    (i.kind, short.to_owned())
                })
                .collect();
            found.sort_by(|a, b| a.1.cmp(&b.1));
            found
        }

        fn set_ref(&self, name: &str) {
            self.repo
                .git(&["update-ref", &format!("{FAILED_REF_PREFIX}{name}"), "HEAD"]);
        }

        fn refs(&self) -> Vec<String> {
            self.repo
                .git(&[
                    "for-each-ref",
                    "--format=%(refname:strip=3)",
                    FAILED_REF_PREFIX,
                ])
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    fn pr(n: u64) -> PrNumber {
        PrNumber(n)
    }

    fn sha(c: char) -> Sha {
        Sha::try_from(c.to_string().repeat(40)).unwrap()
    }

    #[test]
    fn a_preflight_ref_name_carries_its_time() {
        assert_eq!(
            preflight_ref_made_at("preflight-1700-abc"),
            Some(EpochMs(1700))
        );
        assert_eq!(preflight_ref_made_at("preflight-x-abc"), None);
        assert_eq!(preflight_ref_made_at("other-1700-abc"), None);
    }

    #[test]
    fn a_batch_id_is_read_back_as_the_local_time_it_started() {
        let made = batch_made_at("b20261116-141025").unwrap();
        let back = chrono::DateTime::from_timestamp_millis(made.0 as i64)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(back.format("%Y%m%d-%H%M%S").to_string(), "20261116-141025");
        assert_eq!(batch_made_at("b20261116-141025-2"), Some(made));
        assert_eq!(batch_made_at("nonsense"), None);
    }

    #[test]
    fn batch_logs_go_after_logs_days_and_the_records_beside_them_stay() {
        let w = World::new();
        let old = w.dir("batches/b1/verify-1", 8);
        let young = w.dir("batches/b1/verify-2", 6);
        let other_old = w.dir("batches/b2/verify-1", 30);
        fs::write(old.parent().unwrap().join("batch.json"), "{}").unwrap();
        fs::write(old.parent().unwrap().join("revert.json"), "{}").unwrap();
        fs::create_dir_all(other_old.parent().unwrap().join("revert-logs")).unwrap();

        let gone = w.sweep(&w.rules());

        assert_eq!(
            gone,
            [
                (Kind::BatchLog, "batches/b1/verify-1".to_owned()),
                (Kind::BatchLog, "batches/b2/verify-1".to_owned())
            ]
        );
        assert!(!old.exists() && !other_old.exists());
        assert!(young.exists());
        assert!(old.parent().unwrap().join("batch.json").exists());
        assert!(old.parent().unwrap().join("revert.json").exists());
        assert!(other_old.parent().unwrap().join("revert-logs").exists());
    }

    #[test]
    fn the_running_batchs_logs_and_the_halted_queues_log_stay_however_old() {
        let w = World::new();
        let running = w.dir("batches/b1/verify-1", 90);
        let halting = w.dir("batches/b2/verify-3", 90);
        let halting_log = halting.join("test.1.log");
        let gone = w.dir("batches/b2/verify-1", 90);
        let mut rules = w.rules();
        rules.batch = Some(BatchId("b1".into()));
        rules.blocked_log = Some(halting_log);

        w.sweep(&rules);

        assert!(running.exists() && halting.exists());
        assert!(!gone.exists());

        let preflight_halting = w.dir("preflight/abc-1", 90);

        rules.blocked_log = Some(preflight_halting.join("test.1.log"));
        w.sweep(&rules);
        assert!(
            preflight_halting.exists(),
            "the folder holding the log stays"
        );
        assert!(!halting.exists(), "once the queue moves on, it goes");
    }

    #[test]
    fn the_running_batch_in_the_state_is_found_by_the_real_run() {
        let w = World::new();
        w.repo.commit_file(
            "chute.toml",
            "[[verify]]\nname = \"t\"\nrun = \"true\"\n",
            "chore: 설정",
        );
        w.repo.git(&["push", "--quiet", "origin", "main"]);
        let running = w.dir("batches/b1/verify-1", 90);
        let halting = w.dir("batches/b2/verify-1", 90);
        let gone = w.dir("batches/b3/verify-1", 90);
        w.store
            .update(|s| {
                s.batch = Some(Batch {
                    id: BatchId("b1".into()),
                    base: sha('a'),
                    prs: vec![pr(1)],
                    phase: Phase::Verifying,
                    step: None,
                    round: 1,
                    started_ms: EpochMs(1),
                    waiting_machine: false,
                });
                s.blocked = Some(Blocked {
                    reason: BlockReason::BaseBroken,
                    base: sha('b'),
                    since_ms: EpochMs(1),
                    step: None,
                    log: Some(halting.join("check.1.log")),
                    pushed_by_chute: false,
                    rechecked: false,
                });
                Ok(())
            })
            .unwrap();

        run(&w.site(), false, EpochMs::now()).unwrap().unwrap();

        assert!(running.exists() && halting.exists());
        assert!(!gone.exists());
    }

    #[test]
    fn preflight_logs_go_after_logs_days() {
        let w = World::new();
        let old = w.dir("preflight/0123456789ab-1", 8);
        let young = w.dir("preflight/0123456789ab-2", 1);

        let gone = w.sweep(&w.rules());

        assert_eq!(
            gone,
            [(Kind::PreflightLog, "preflight/0123456789ab-1".to_owned())]
        );
        assert!(!old.exists() && young.exists());
    }

    #[test]
    fn a_pr_report_goes_when_the_pr_left_the_queue_and_is_older_than_reports_days() {
        let w = World::new();
        let queued_old = w.dir("reports/pr-1", 40);
        let left_old = w.dir("reports/pr-2", 40);
        let left_young = w.dir("reports/pr-3", 29 - 1);
        let mut rules = w.rules();
        rules.queue = Some(BTreeSet::from([pr(1)]));

        let gone = w.sweep(&rules);

        assert_eq!(gone, [(Kind::PrReport, "reports/pr-2".to_owned())]);
        assert!(queued_old.exists() && left_young.exists() && !left_old.exists());
    }

    #[test]
    fn a_pc_that_does_not_know_the_queue_judges_a_pr_report_by_its_age_alone() {
        let w = World::new();
        let old = w.dir("reports/pr-1", 40);
        let young = w.dir("reports/pr-2", 3);
        let mut rules = w.rules();
        rules.queue = None;

        let gone = w.sweep(&rules);

        assert_eq!(gone, [(Kind::PrReport, "reports/pr-1".to_owned())]);
        assert!(!old.exists() && young.exists());
    }

    #[test]
    fn a_commit_report_goes_after_reports_days_not_logs_days() {
        let w = World::new();
        let middle = w.dir("reports/commit-aaa", 10);
        let old = w.dir("reports/commit-bbb", 31);

        let gone = w.sweep(&w.rules());

        assert_eq!(
            gone,
            [(Kind::CommitReport, "reports/commit-bbb".to_owned())]
        );
        assert!(middle.exists() && !old.exists());
    }

    #[test]
    fn retention_values_move_the_line() {
        let w = World::new();
        let log = w.dir("preflight/x-1", 2);
        let report = w.dir("reports/commit-x", 2);
        let mut rules = w.rules();
        w.sweep(&rules);
        assert!(log.exists() && report.exists());

        rules.retention = Retention {
            logs_days: 1,
            reports_days: 1,
        };
        w.sweep(&rules);
        assert!(!log.exists() && !report.exists());
    }

    #[test]
    fn the_hub_drops_a_batch_ref_when_its_pr_left_the_queue_whatever_its_age() {
        let w = World::new();
        w.set_ref("pr1-b20990101-000000");
        w.set_ref("pr2-b20990101-000000");
        let mut rules = w.rules();
        rules.queue = Some(BTreeSet::from([pr(1)]));

        let gone = w.sweep(&rules);

        assert_eq!(
            gone,
            [(
                Kind::BatchRef,
                format!("{FAILED_REF_PREFIX}pr2-b20990101-000000")
            )]
        );
        assert_eq!(w.refs(), ["pr1-b20990101-000000"]);
    }

    #[test]
    fn a_pc_without_the_queue_drops_a_batch_ref_by_the_time_in_its_batch_id() {
        let w = World::new();
        w.set_ref("pr1-b20200101-000000");
        w.set_ref("pr2-b20990101-000000");
        w.set_ref("pr3-garbled");
        let mut rules = w.rules();
        rules.queue = None;

        w.sweep(&rules);

        assert_eq!(w.refs(), ["pr2-b20990101-000000", "pr3-garbled"]);
    }

    #[test]
    fn a_preflight_ref_goes_after_reports_days_on_every_pc() {
        let w = World::new();
        let day = DAY_MS;
        let now = EpochMs::now().0;
        w.set_ref(&format!("preflight-{}-abc", now - 40 * day));
        w.set_ref(&format!("preflight-{}-def", now - 10 * day));
        w.set_ref("preflight-garbled");
        for queue in [Some(BTreeSet::new()), None] {
            let mut rules = w.rules();
            rules.queue = queue;
            w.sweep(&rules);
            assert_eq!(
                w.refs(),
                [
                    format!("preflight-{}-def", now - 10 * day),
                    "preflight-garbled".to_owned()
                ]
            );
        }
    }

    #[test]
    fn a_dry_run_lists_the_same_items_and_removes_nothing() {
        let w = World::new();
        let log = w.dir("preflight/x-1", 8);
        let report = w.dir("reports/pr-4", 40);
        w.set_ref("pr4-b20200101-000000");
        let rules = w.rules();

        let planned = sweep(&w.site(), &rules, EpochMs::now(), true);
        assert!(log.exists() && report.exists());
        assert_eq!(w.refs().len(), 1);

        let done = sweep(&w.site(), &rules, EpochMs::now(), false);
        assert_eq!(planned.items, done.items);
        assert_eq!(done.items.len(), 3);
        assert_eq!(planned.bytes(), 20);
        assert!(!log.exists() && !report.exists() && w.refs().is_empty());
    }

    #[test]
    fn records_and_other_worktrees_are_never_swept() {
        let w = World::new();
        let dir = w.store.dir().to_path_buf();
        for file in ["state.json", "history.jsonl", "repo.json"] {
            fs::write(dir.join(file), "{}").unwrap();
            fs::File::open(dir.join(file))
                .unwrap()
                .set_modified(NOW_DAYS_AGO(400))
                .unwrap();
        }
        let respawned = w.sb.home().join(".herdr/worktrees/chute/old");
        fs::create_dir_all(&respawned).unwrap();
        fs::File::open(&respawned)
            .unwrap()
            .set_modified(NOW_DAYS_AGO(400))
            .unwrap();
        let mut rules = w.rules();
        rules.retention = Retention {
            logs_days: 1,
            reports_days: 1,
        };

        w.sweep(&rules);

        for file in ["state.json", "history.jsonl", "repo.json"] {
            assert!(dir.join(file).exists(), "{file}");
        }
        assert!(respawned.exists());
    }

    #[test]
    fn surplus_slots_go_with_their_worktree_registration_and_the_rest_stay() {
        let w = World::new();
        let gitops = Gitops::new(&w.ext, &w.home, &w.info);
        for k in 1..=4 {
            let name = crate::gitops::SlotName::Preflight(k);
            gitops.lock_slot(name).unwrap().unwrap().open().unwrap();
        }
        let slot = |k: u32| gitops.slot_path(crate::gitops::SlotName::Preflight(k));
        fs::write(slot(3).join("big.bin"), vec![0u8; 5000]).unwrap();

        let gone = w.sweep(&w.rules());

        assert_eq!(
            gone.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            [Kind::Slot, Kind::Slot]
        );
        assert!(slot(1).exists() && slot(2).exists());
        assert!(!slot(3).exists() && !slot(4).exists());
        let listed = w.repo.git(&["worktree", "list", "--porcelain"]);
        assert!(!listed.contains("preflight-3") && !listed.contains("preflight-4"));
        assert!(listed.contains("preflight-1"));
        let swept = sweep(&w.site(), &w.rules(), EpochMs::now(), true);
        assert!(swept.items.is_empty(), "nothing is left to sweep");
    }

    #[test]
    fn a_dry_run_reports_a_surplus_slots_size_and_keeps_it() {
        let w = World::new();
        let gitops = Gitops::new(&w.ext, &w.home, &w.info);
        let name = crate::gitops::SlotName::Preflight(3);
        gitops.lock_slot(name).unwrap().unwrap().open().unwrap();
        fs::write(gitops.slot_path(name).join("big.bin"), vec![0u8; 5000]).unwrap();

        let swept = sweep(&w.site(), &w.rules(), EpochMs::now(), true);

        assert_eq!(swept.items.len(), 1);
        assert!(swept.bytes() >= 5000);
        assert!(gitops.slot_path(name).exists());
    }

    #[test]
    fn a_run_is_due_once_an_hour() {
        let w = World::new();
        let at = EpochMs(10 * AUTO_EVERY_MS);
        assert!(is_due(&w.store, at), "never ran");
        write_last_run(&w.store, at).unwrap();
        assert!(!is_due(&w.store, EpochMs(at.0 + AUTO_EVERY_MS - 1)));
        assert!(is_due(&w.store, EpochMs(at.0 + AUTO_EVERY_MS)));
        assert!(
            is_due(&w.store, EpochMs(at.0 - 1)),
            "a clock that went back"
        );
    }

    #[test]
    fn a_failed_run_leaves_a_gc_warning_and_a_later_clean_run_removes_it() {
        let w = World::new();
        let now = EpochMs(5);
        let failed: Result<Option<Swept>> = Err(anyhow::anyhow!("디스크가 가득 찼다"));
        note(&w.site(), &failed, now);
        let warnings = w.store.load().unwrap().warnings;
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, WarningKind::Gc);
        assert!(warnings[0].message.contains("디스크가 가득 찼다"));
        assert_eq!(last_run(&w.store), None, "a failed run is retried");

        let partial = Swept {
            items: Vec::new(),
            failures: vec!["a".into(), "b".into()],
        };
        note(&w.site(), &Ok(Some(partial)), now);
        let warnings = w.store.load().unwrap().warnings;
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("2건"));

        note(&w.site(), &Ok(Some(Swept::default())), EpochMs(9));
        assert!(w.store.load().unwrap().warnings.is_empty());
        assert_eq!(last_run(&w.store), Some(EpochMs(9)));
    }

    #[test]
    fn an_unrelated_warning_survives_the_gc_warning_coming_and_going() {
        let w = World::new();
        w.store
            .update(|s| {
                s.warnings.push(Warning {
                    kind: WarningKind::Hub,
                    message: "허브".into(),
                    pr: None,
                    at_ms: EpochMs(1),
                });
                Ok(())
            })
            .unwrap();
        let failed: Result<Option<Swept>> = Err(anyhow::anyhow!("x"));
        note(&w.site(), &failed, EpochMs(2));
        note(&w.site(), &Ok(Some(Swept::default())), EpochMs(3));
        let kinds: Vec<_> = w
            .store
            .load()
            .unwrap()
            .warnings
            .iter()
            .map(|w| w.kind)
            .collect();
        assert_eq!(kinds, [WarningKind::Hub]);
    }
}
