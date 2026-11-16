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

fn warning_prefix(key: &Key) -> String {
    format!("{}: ", key.label())
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
        state.warnings.retain(|w| {
            w.kind != WarningKind::FlakyChronic
                || chronic
                    .keys()
                    .any(|key| w.message.starts_with(&warning_prefix(key)))
        });
        let mut due = Vec::new();
        for (key, count) in &chronic {
            let message = format!(
                "{}{}일 안에 {count}번 흔들렸다",
                warning_prefix(key),
                policy.chronic_days
            );
            let held = state.warnings.iter_mut().find(|w| {
                w.kind == WarningKind::FlakyChronic && w.message.starts_with(&warning_prefix(key))
            });
            match held {
                Some(warning) => {
                    warning.message = message;
                    if touched.contains(key) && now.0.saturating_sub(warning.at_ms.0) >= DAY_MS {
                        warning.at_ms = now;
                        due.push(key.clone());
                    }
                }
                None => {
                    state.warnings.push(Warning {
                        kind: WarningKind::FlakyChronic,
                        message,
                        pr: None,
                        at_ms: now,
                    });
                    if touched.contains(key) {
                        due.push(key.clone());
                    }
                }
            }
        }
        Ok(due)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub evidence: ProbeEvidence,
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
    let command = job
        .command
        .replace(PROBE_PLACEHOLDER, &quote_all(job.tests));
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
        match verify::run_command(
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

fn quote_all(tests: &[String]) -> String {
    tests
        .iter()
        .map(|test| format!("'{}'", test.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
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

    #[test]
    fn tests_are_quoted_for_the_shell() {
        let tests = ["a::b".to_owned(), "it's $HOME".to_owned()];

        assert_eq!(quote_all(&tests), r"'a::b' 'it'\''s $HOME'");
    }
}
