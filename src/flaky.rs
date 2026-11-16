use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::{Config, FlakyPolicy, PROBE_PLACEHOLDER};
use crate::gitops::Slot;
use crate::model::{BatchId, EpochMs, Event, EventKind, ProbeEvidence, Sha, Warning, WarningKind};
use crate::store::RepoStore;
use crate::verify::{self, Cancel, CommandEnd, Options};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// Linux refuses a single `sh -c` argument of 128 KiB or more.
const MAX_TESTS_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Key {
    pub step: String,
    pub test: Option<String>,
}

impl Key {
    pub fn label(&self) -> String {
        match &self.test {
            Some(test) => format!("단계 {}의 시험 {test}", self.step),
            None => format!("단계 {}", self.step),
        }
    }

    /// Names the key in a warning. The quotes make two different keys never read the same.
    fn identity(&self) -> String {
        match &self.test {
            Some(test) => format!("단계 {:?}의 시험 {test:?}", self.step),
            None => format!("단계 {:?}", self.step),
        }
    }
}

pub fn keys_of(step: &str, tests: &[String]) -> Vec<Key> {
    if tests.is_empty() {
        return vec![Key {
            step: step.to_owned(),
            test: None,
        }];
    }
    tests
        .iter()
        .map(|test| Key {
            step: step.to_owned(),
            test: Some(test.clone()),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    #[serde(flatten)]
    pub key: Key,
    pub count: u32,
    pub last_ms: EpochMs,
    pub last_batch: BatchId,
    pub evidence: Option<ProbeEvidence>,
    pub chronic: bool,
}

struct Tally {
    count: u32,
    last_ms: EpochMs,
    last_batch: BatchId,
}

fn window_start(now: EpochMs, days: u32) -> u64 {
    now.0.saturating_sub(u64::from(days) * DAY_MS)
}

fn tally(events: &[Event], since: u64) -> BTreeMap<Key, Tally> {
    let mut flakes: Vec<(&Event, &BatchId, &str, &[String])> = events
        .iter()
        .filter(|e| e.at_ms.0 >= since)
        .filter_map(|e| match &e.kind {
            EventKind::Flaky {
                batch, step, tests, ..
            } => Some((e, batch, step.as_str(), tests.as_slice())),
            _ => None,
        })
        .collect();
    flakes.sort_by_key(|(e, ..)| e.at_ms);
    let mut counts: BTreeMap<Key, Tally> = BTreeMap::new();
    for (event, batch, step, tests) in flakes {
        for key in keys_of(step, tests) {
            let tally = counts.entry(key).or_insert_with(|| Tally {
                count: 0,
                last_ms: event.at_ms,
                last_batch: batch.clone(),
            });
            tally.count += 1;
            tally.last_ms = event.at_ms;
            tally.last_batch = batch.clone();
        }
    }
    counts
}

fn evidence_for(events: &[Event], batch: &BatchId, key: &Key) -> Option<ProbeEvidence> {
    let test = key.test.as_ref()?;
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::FlakyProbe {
                batch: b,
                step,
                tests,
                evidence,
                ..
            } if b == batch && *step == key.step && tests.contains(test) => {
                Some((e.at_ms, *evidence))
            }
            _ => None,
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, evidence)| evidence)
}

/// Flakes of the last `days` days, most frequent first. `chronic` is judged over the policy's
/// own window, whatever `days` is.
pub fn ledger(events: &[Event], now: EpochMs, policy: &FlakyPolicy, days: u32) -> Vec<Entry> {
    let chronic = chronic_keys(events, now, policy);
    let mut entries: Vec<Entry> = tally(events, window_start(now, days))
        .into_iter()
        .map(|(key, t)| Entry {
            evidence: evidence_for(events, &t.last_batch, &key),
            chronic: chronic.contains_key(&key),
            key,
            count: t.count,
            last_ms: t.last_ms,
            last_batch: t.last_batch,
        })
        .collect();
    entries.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.last_ms.cmp(&a.last_ms))
            .then(a.key.cmp(&b.key))
    });
    entries
}

fn chronic_keys(events: &[Event], now: EpochMs, policy: &FlakyPolicy) -> BTreeMap<Key, u32> {
    tally(events, window_start(now, policy.chronic_days))
        .into_iter()
        .filter(|(_, t)| t.count >= policy.chronic)
        .map(|(key, t)| (key, t.count))
        .collect()
}

fn names(warning: &Warning, key: &Key) -> bool {
    warning.kind == WarningKind::FlakyChronic
        && warning
            .message
            .rsplit_once(": ")
            .is_some_and(|(identity, _)| identity == key.identity())
}

/// Brings the `flaky_chronic` warnings in line with the history: one per chronic key, none for a
/// key that fell under the threshold. A warning's time is when its key was last announced, so
/// the keys returned are those in `touched` that are chronic and were not announced in the last
/// day. The caller tells the submitters about them.
pub fn refresh_chronic(
    store: &RepoStore,
    events: &[Event],
    now: EpochMs,
    policy: &FlakyPolicy,
    touched: &[Key],
) -> Result<Vec<Key>> {
    let chronic = chronic_keys(events, now, policy);
    store.update(|state| {
        state
            .warnings
            .retain(|w| w.kind != WarningKind::FlakyChronic || chronic.keys().any(|k| names(w, k)));
        let mut due = Vec::new();
        for (key, count) in &chronic {
            let message = format!(
                "{}: {}일 안에 {count}번 흔들렸다",
                key.identity(),
                policy.chronic_days
            );
            let held = state.warnings.iter_mut().find(|w| names(w, key));
            match held {
                Some(warning) => {
                    warning.message = message;
                    if touched.contains(key) && now.0.saturating_sub(warning.at_ms.0) >= DAY_MS {
                        warning.at_ms = now;
                        due.push(key.clone());
                    }
                }
                None => {
                    let announced = touched.contains(key);
                    state.warnings.push(Warning {
                        kind: WarningKind::FlakyChronic,
                        message,
                        pr: None,
                        at_ms: if announced {
                            now
                        } else {
                            EpochMs(now.0.saturating_sub(DAY_MS))
                        },
                    });
                    if announced {
                        due.push(key.clone());
                    }
                }
            }
        }
        Ok(due)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub evidence: ProbeEvidence,
    /// The tests the command ran with. A long list is cut to what fits.
    pub tests: Vec<String>,
    pub runs: u32,
    pub failures: u32,
}

pub struct ProbeJob<'a> {
    pub config: &'a Config,
    pub store: &'a RepoStore,
    pub slot: &'a Slot,
    pub batch: &'a BatchId,
    pub command: &'a str,
    pub step: &'a str,
    pub tests: &'a [String],
    pub base: &'a Sha,
    pub flaked: &'a Sha,
    pub cancel: &'a Cancel,
    pub cache_dir: PathBuf,
}

struct Round {
    runs: u32,
    failed: bool,
    cancelled: bool,
}

/// Repeats the flaky tests on the batch's start base, then on the flaked commit. Each round
/// stops at its first failure, after `probe_repeats` runs or after `probe_seconds`, and a
/// cancel ends the probe as unresolved. A failure on the base means the flake was already there.
pub fn probe(job: &ProbeJob) -> Result<ProbeResult> {
    let (quoted, tests) = quote_within(job.tests, MAX_TESTS_BYTES)
        .context("흔들린 시험 이름이 모두 너무 길어 probe 명령에 넣지 못했다")?;
    let command = job.command.replace(PROBE_PLACEHOLDER, &quoted);
    let log_dir = job.store.batch_dir(job.batch).join("probe");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("만들지 못했다: {}", log_dir.display()))?;
    let policy = job.config.flaky;
    let opts = Options {
        cancel: job.cancel.clone(),
        cache_dir: Some(job.cache_dir.clone()),
        ..Options::default()
    };
    let mut runs = 0;
    let rounds = [
        (job.base, "base", ProbeEvidence::Base),
        (job.flaked, "flaked", ProbeEvidence::Made),
    ];
    for (commit, side, found) in rounds {
        job.slot.checkout(commit)?;
        let log = log_dir.join(format!("{}.{side}.log", job.step));
        let round = repeat(job, &command, &log, &policy, &opts)?;
        runs += round.runs;
        if round.failed {
            return Ok(ProbeResult {
                evidence: found,
                tests,
                runs,
                failures: 1,
            });
        }
        if round.cancelled {
            break;
        }
    }
    Ok(ProbeResult {
        evidence: ProbeEvidence::Unresolved,
        tests,
        runs,
        failures: 0,
    })
}

fn repeat(
    job: &ProbeJob,
    command: &str,
    log: &std::path::Path,
    policy: &FlakyPolicy,
    opts: &Options,
) -> Result<Round> {
    let started = Instant::now();
    let limit = Duration::from_secs(u64::from(policy.probe_seconds));
    let mut round = Round {
        runs: 0,
        failed: false,
        cancelled: false,
    };
    while round.runs < policy.probe_repeats {
        let Some(left) = limit
            .checked_sub(started.elapsed())
            .filter(|d| !d.is_zero())
        else {
            break;
        };
        match verify::run_probe(
            job.config,
            job.store,
            job.slot.path(),
            log,
            command,
            left,
            opts,
        )? {
            CommandEnd::Passed => round.runs += 1,
            CommandEnd::Failed => {
                round.runs += 1;
                round.failed = true;
                break;
            }
            CommandEnd::TimedOut => break,
            CommandEnd::Cancelled => {
                round.cancelled = true;
                break;
            }
        }
    }
    Ok(round)
}

/// The names that fit in `limit` bytes, each in single quotes, and the names themselves. A name
/// too long to fit is left out. `None` when no name fits.
fn quote_within(tests: &[String], limit: usize) -> Option<(String, Vec<String>)> {
    let mut text = String::new();
    let mut taken = Vec::new();
    for test in tests {
        let quoted = format!("'{}'", test.replace('\'', r"'\''"));
        let added = if text.is_empty() { 0 } else { 1 } + quoted.len();
        if text.len() + added > limit {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&quoted);
        taken.push(test.clone());
    }
    (!taken.is_empty()).then_some((text, taken))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BatchId;

    const NOW: EpochMs = EpochMs(100 * DAY_MS);

    fn policy() -> FlakyPolicy {
        FlakyPolicy {
            probe_repeats: 20,
            probe_seconds: 180,
            chronic: 3,
            chronic_days: 30,
        }
    }

    fn flaky(days_ago: u64, batch: &str, step: &str, tests: &[&str]) -> Event {
        Event {
            at_ms: EpochMs(NOW.0 - days_ago * DAY_MS),
            kind: EventKind::Flaky {
                batch: BatchId(batch.into()),
                step: step.into(),
                tests: tests.iter().map(|t| (*t).to_owned()).collect(),
                tree: Sha::try_from("a".repeat(40)).unwrap(),
            },
        }
    }

    fn probed(days_ago: u64, batch: &str, tests: &[&str], evidence: ProbeEvidence) -> Event {
        Event {
            at_ms: EpochMs(NOW.0 - days_ago * DAY_MS),
            kind: EventKind::FlakyProbe {
                batch: BatchId(batch.into()),
                step: "test".into(),
                tests: tests.iter().map(|t| (*t).to_owned()).collect(),
                evidence,
                runs: 3,
                failures: 1,
            },
        }
    }

    fn key(step: &str, test: Option<&str>) -> Key {
        Key {
            step: step.into(),
            test: test.map(str::to_owned),
        }
    }

    #[test]
    fn a_flake_counts_once_per_test_with_its_last_time_and_batch() {
        let events = [
            flaky(5, "b1", "test", &["a", "b"]),
            flaky(2, "b2", "test", &["a"]),
        ];

        let entries = ledger(&events, NOW, &policy(), 30);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key, key("test", Some("a")));
        assert_eq!(entries[0].count, 2);
        assert_eq!(entries[0].last_ms, EpochMs(NOW.0 - 2 * DAY_MS));
        assert_eq!(entries[0].last_batch, BatchId("b2".into()));
        assert_eq!(entries[1].key, key("test", Some("b")));
        assert_eq!(entries[1].count, 1);
    }

    #[test]
    fn only_flakes_inside_the_days_window_count() {
        let events = [
            flaky(40, "b1", "test", &["a"]),
            flaky(10, "b2", "test", &["a"]),
            flaky(1, "b3", "test", &["a"]),
        ];

        assert_eq!(ledger(&events, NOW, &policy(), 30)[0].count, 2);
        assert_eq!(ledger(&events, NOW, &policy(), 5)[0].count, 1);
        assert_eq!(ledger(&events, NOW, &policy(), 60)[0].count, 3);
        assert!(ledger(&events, NOW, &policy(), 0).is_empty());
    }

    #[test]
    fn a_flake_without_test_names_is_counted_under_its_step() {
        let events = [
            flaky(3, "b1", "lint", &[]),
            flaky(2, "b2", "lint", &[]),
            flaky(1, "b3", "test", &["lint"]),
        ];

        let entries = ledger(&events, NOW, &policy(), 30);

        let keys: Vec<(&Key, u32)> = entries.iter().map(|e| (&e.key, e.count)).collect();
        assert_eq!(
            keys,
            [(&key("lint", None), 2), (&key("test", Some("lint")), 1),]
        );
    }

    #[test]
    fn a_key_is_chronic_at_the_threshold_and_not_below_it() {
        let two = [
            flaky(3, "b1", "test", &["a"]),
            flaky(2, "b2", "test", &["a"]),
        ];
        assert!(!ledger(&two, NOW, &policy(), 30)[0].chronic);

        let three = [
            flaky(3, "b1", "test", &["a"]),
            flaky(2, "b2", "test", &["a"]),
            flaky(1, "b3", "test", &["a"]),
        ];
        assert!(ledger(&three, NOW, &policy(), 30)[0].chronic);
    }

    #[test]
    fn chronic_uses_the_policy_window_even_when_the_ledger_looks_back_less() {
        let events = [
            flaky(20, "b1", "test", &["a"]),
            flaky(10, "b2", "test", &["a"]),
            flaky(1, "b3", "test", &["a"]),
        ];

        let entries = ledger(&events, NOW, &policy(), 2);

        assert_eq!(entries[0].count, 1);
        assert!(entries[0].chronic);
        let narrow = FlakyPolicy {
            chronic_days: 15,
            ..policy()
        };
        assert!(!ledger(&events, NOW, &narrow, 2)[0].chronic);
    }

    #[test]
    fn evidence_comes_from_the_probe_of_the_latest_flake() {
        let events = [
            flaky(5, "b1", "test", &["a"]),
            probed(5, "b1", &["a"], ProbeEvidence::Base),
            flaky(2, "b2", "test", &["a", "b"]),
            probed(2, "b2", &["a", "b"], ProbeEvidence::Made),
            flaky(1, "b3", "test", &["c"]),
        ];

        let entries = ledger(&events, NOW, &policy(), 30);

        let evidence = |test: &str| {
            entries
                .iter()
                .find(|e| e.key.test.as_deref() == Some(test))
                .unwrap()
                .evidence
        };
        assert_eq!(evidence("a"), Some(ProbeEvidence::Made));
        assert_eq!(evidence("b"), Some(ProbeEvidence::Made));
        assert_eq!(evidence("c"), None);
    }

    #[test]
    fn the_most_frequent_comes_first_and_ties_go_to_the_latest() {
        let events = [
            flaky(3, "b1", "test", &["x"]),
            flaky(2, "b2", "test", &["y"]),
            flaky(1, "b3", "test", &["z"]),
            flaky(4, "b0", "test", &["z"]),
        ];

        let order: Vec<String> = ledger(&events, NOW, &policy(), 30)
            .into_iter()
            .map(|e| e.key.test.unwrap())
            .collect();

        assert_eq!(order, ["z", "y", "x"]);
    }

    fn warned(store: &RepoStore, events: &[Event], touched: &[Key]) -> Vec<Key> {
        refresh_chronic(store, events, NOW, &policy(), touched).unwrap()
    }

    fn three(test: &str) -> Vec<Event> {
        (1..=3)
            .map(|n| flaky(n, &format!("b{n}"), "test", &[test]))
            .collect()
    }

    fn store() -> (tempfile::TempDir, RepoStore) {
        let dir = tempfile::tempdir().unwrap();
        let home = crate::store::Home::at(dir.path());
        let store = home.repo(&crate::model::Slug::new("acme", "widget").unwrap());
        (dir, store)
    }

    fn messages(store: &RepoStore) -> Vec<String> {
        store
            .load()
            .unwrap()
            .warnings
            .into_iter()
            .map(|w| w.message)
            .collect()
    }

    #[test]
    fn chronic_warnings_of_tests_whose_names_share_a_prefix_stay_apart() {
        let (_dir, store) = store();
        let colliding = key("test", Some("a: b"));
        let plain = key("test", Some("a"));
        let both: Vec<Event> = three("a: b").into_iter().chain(three("a")).collect();

        assert_eq!(
            warned(&store, &three("a: b"), std::slice::from_ref(&colliding)),
            std::slice::from_ref(&colliding)
        );
        assert_eq!(
            warned(&store, &both, std::slice::from_ref(&plain)),
            std::slice::from_ref(&plain)
        );
        assert_eq!(messages(&store).len(), 2, "{:?}", messages(&store));

        let only_plain = three("a");
        assert!(warned(&store, &only_plain, &[]).is_empty());
        assert_eq!(messages(&store).len(), 1, "{:?}", messages(&store));
        assert!(messages(&store)[0].starts_with("단계 \"test\"의 시험 \"a\": 30일"));
    }

    #[test]
    fn a_step_named_like_a_test_label_does_not_share_a_warning_with_that_test() {
        let (_dir, store) = store();
        let named = key("x", Some("y"));
        let odd_step = key("x의 시험 y", None);
        let events: Vec<Event> = (1..=3)
            .flat_map(|n| {
                [
                    flaky(n, &format!("b{n}"), "x", &["y"]),
                    flaky(n, &format!("c{n}"), "x의 시험 y", &[]),
                ]
            })
            .collect();

        let due = warned(&store, &events, &[named.clone(), odd_step.clone()]);

        assert_eq!(due.len(), 2, "{due:?}");
        assert_eq!(messages(&store).len(), 2, "{:?}", messages(&store));
    }

    #[test]
    fn a_chronic_key_is_announced_once_a_day() {
        let (_dir, store) = store();
        let a = key("test", Some("a"));

        assert_eq!(
            warned(&store, &three("a"), std::slice::from_ref(&a)),
            std::slice::from_ref(&a)
        );
        assert!(warned(&store, &three("a"), std::slice::from_ref(&a)).is_empty());
        assert_eq!(messages(&store).len(), 1);
    }

    #[test]
    fn a_chronic_key_found_without_a_notice_is_still_announced_at_its_next_flake() {
        let (_dir, store) = store();
        let a = key("test", Some("a"));

        assert!(warned(&store, &three("a"), &[]).is_empty());
        assert_eq!(messages(&store).len(), 1);
        assert_eq!(warned(&store, &three("a"), std::slice::from_ref(&a)), [a]);
    }

    #[test]
    fn tests_are_quoted_for_the_shell() {
        let tests = ["a::b".to_owned(), "it's $HOME".to_owned()];

        assert_eq!(
            quote_within(&tests, 1000).unwrap().0,
            r"'a::b' 'it'\''s $HOME'"
        );
    }

    #[test]
    fn too_many_tests_are_cut_to_the_names_that_fit() {
        let tests: Vec<String> = (0..10).map(|n| format!("t{n}")).collect();

        assert_eq!(quote_within(&tests, 8).unwrap().0, "'t0'");
        assert_eq!(quote_within(&tests, 9).unwrap().0, "'t0' 't1'");
        assert_eq!(quote_within(&tests, 13).unwrap().0, "'t0' 't1'");
        let (text, taken) = quote_within(&tests, 14).unwrap();
        assert_eq!(text, "'t0' 't1' 't2'");
        assert_eq!(taken, ["t0", "t1", "t2"]);
        assert!(quote_within(&tests, 3).is_none());
        let mixed = ["x".repeat(100), "short".to_owned()];
        assert_eq!(quote_within(&mixed, 50).unwrap().1, ["short"]);
        assert!(quote_within(&[], 100).is_none());
    }
}
