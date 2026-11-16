use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::ext::{Ext, Tool};
use crate::model::{EnvRecord, EpochMs, State, TreeKey, VerifiedTree};

const MAX_KEPT: usize = 64;
const HASH_TIMEOUT: Duration = Duration::from_secs(30);
const HOUR_MS: u64 = 60 * 60 * 1000;

/// Names what `verify::run` would execute: each step's name, command, timeout and `paths`. What
/// only shapes a report (`failed`, `probe`) is left out, so editing it keeps earlier passes valid.
pub fn fingerprint(ext: &Ext, repo: &Path, config: &Config) -> Result<String> {
    let steps: Vec<_> = config
        .verify
        .iter()
        .map(|step| {
            (
                &step.name,
                &step.run,
                step.timeout.as_secs(),
                step.paths
                    .as_ref()
                    .map(|paths| paths.iter().collect::<Vec<_>>()),
            )
        })
        .collect();
    let text = serde_json::to_string(&steps)?;
    let out = ext
        .call_with_input(
            Tool::Git,
            Some(repo),
            ["hash-object", "--stdin"],
            HASH_TIMEOUT,
            Some(text.as_bytes()),
        )?
        .ok()
        .context("검증 설정의 지문을 만들지 못했다")?;
    Ok(out.trim().to_owned())
}

/// Keeps `verified` for the next lookup. A record of the same key is replaced, records older than
/// `keep_hours` go, and only the newest `MAX_KEPT` stay.
pub fn remember(state: &mut State, verified: VerifiedTree, keep_hours: u32, now: EpochMs) {
    let oldest = now.0.saturating_sub(u64::from(keep_hours) * HOUR_MS);
    state
        .verified_trees
        .retain(|old| old.key != verified.key && old.at_ms.0 >= oldest);
    state.verified_trees.push(verified);
    let extra = state.verified_trees.len().saturating_sub(MAX_KEPT);
    state.verified_trees.drain(..extra);
}

pub struct Lookup<'a> {
    pub config: &'a Config,
    pub now: EpochMs,
    /// The machine that would verify. A tree recorded on another machine is not reused when this
    /// is unknown.
    pub here: Option<&'a EnvRecord>,
    /// Steps on the flaky ledger. A pass over any of them proves less than it seems to.
    pub flaky_steps: &'a [String],
}

impl Lookup<'_> {
    pub fn find<'r>(&self, records: &'r [VerifiedTree], key: &TreeKey) -> Option<&'r VerifiedTree> {
        let reuse = self.config.reuse;
        if !reuse.enabled || self.ran_flaky_step(key) {
            return None;
        }
        let window = u64::from(reuse.hours) * HOUR_MS;
        records
            .iter()
            .filter(|record| record.key == *key)
            .filter(|record| record.at_ms <= self.now && self.now.0 - record.at_ms.0 <= window)
            .filter(|record| self.trusts_machine_of(record))
            .max_by_key(|record| record.at_ms)
    }

    fn ran_flaky_step(&self, key: &TreeKey) -> bool {
        self.config
            .verify
            .iter()
            .filter(|step| !key.skipped.contains(&step.name))
            .any(|step| self.flaky_steps.contains(&step.name))
    }

    fn trusts_machine_of(&self, record: &VerifiedTree) -> bool {
        let (Some(there), Some(here)) = (&record.env, self.here) else {
            return false;
        };
        if same_machine(there, here) {
            return true;
        }
        self.config.reuse.other_machines
            && !self.config.env_check.is_empty()
            && there.checks == here.checks
            && here.checks.iter().all(|check| check.ok)
    }
}

fn same_machine(a: &EnvRecord, b: &EnvRecord) -> bool {
    a.host == b.host && a.machine_id.is_some() && a.machine_id == b.machine_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EnvCheck, MachineId, Sha, VerifiedBy};

    const TOML: &str = "[[verify]]\nname = \"build\"\nrun = \"make\"\n[[verify]]\nname = \"lint\"\nrun = \"lint\"\npaths = [\"src/\"]\n";

    fn sha(c: char) -> Sha {
        Sha::try_from(c.to_string().repeat(40)).unwrap()
    }

    fn key() -> TreeKey {
        TreeKey {
            tree: sha('a'),
            config: "cfg".to_owned(),
            skipped: Vec::new(),
        }
    }

    fn env(machine: &str, output: &str) -> EnvRecord {
        EnvRecord {
            machine_id: Some(MachineId::try_from(machine.to_owned()).unwrap()),
            host: machine.to_owned(),
            checks: vec![EnvCheck {
                run: "tool --version".to_owned(),
                output: output.to_owned(),
                ok: true,
            }],
        }
    }

    fn record(key: TreeKey, at: u64, env: EnvRecord) -> VerifiedTree {
        VerifiedTree {
            key,
            at_ms: EpochMs(at),
            by: VerifiedBy::Preflight,
            env: Some(env),
        }
    }

    fn lookup<'a>(
        config: &'a Config,
        here: Option<&'a EnvRecord>,
        flaky: &'a [String],
    ) -> Lookup<'a> {
        Lookup {
            config,
            now: EpochMs(100 * HOUR_MS),
            here,
            flaky_steps: flaky,
        }
    }

    #[test]
    fn a_fresh_record_of_the_same_key_on_the_same_machine_is_found() {
        let config = Config::parse(TOML).unwrap();
        let me = env("m1", "1");
        let records = [record(key(), 99 * HOUR_MS, me.clone())];
        assert!(
            lookup(&config, Some(&me), &[])
                .find(&records, &key())
                .is_some()
        );
    }

    #[test]
    fn any_part_of_the_key_that_differs_finds_nothing() {
        let config = Config::parse(TOML).unwrap();
        let me = env("m1", "1");
        let records = [record(key(), 99 * HOUR_MS, me.clone())];
        let look = lookup(&config, Some(&me), &[]);
        let other_tree = TreeKey {
            tree: sha('b'),
            ..key()
        };
        let other_config = TreeKey {
            config: "x".to_owned(),
            ..key()
        };
        let other_skips = TreeKey {
            skipped: vec!["lint".to_owned()],
            ..key()
        };
        for different in [other_tree, other_config, other_skips] {
            assert!(look.find(&records, &different).is_none());
        }
    }

    #[test]
    fn a_record_older_than_the_window_is_not_found() {
        let config = Config::parse(TOML).unwrap();
        let me = env("m1", "1");
        let hours = u64::from(config.reuse.hours);
        let stale = [record(key(), (100 - hours - 1) * HOUR_MS, me.clone())];
        let edge = [record(key(), (100 - hours) * HOUR_MS, me.clone())];
        assert!(
            lookup(&config, Some(&me), &[])
                .find(&stale, &key())
                .is_none()
        );
        assert!(
            lookup(&config, Some(&me), &[])
                .find(&edge, &key())
                .is_some()
        );
    }

    #[test]
    fn a_record_from_the_future_is_not_found() {
        let config = Config::parse(TOML).unwrap();
        let me = env("m1", "1");
        let records = [record(key(), 101 * HOUR_MS, me.clone())];
        assert!(
            lookup(&config, Some(&me), &[])
                .find(&records, &key())
                .is_none()
        );
    }

    #[test]
    fn turning_reuse_off_finds_nothing() {
        let config = Config::parse(&format!("reuse_verified = false\n{TOML}")).unwrap();
        let me = env("m1", "1");
        let records = [record(key(), 99 * HOUR_MS, me.clone())];
        assert!(
            lookup(&config, Some(&me), &[])
                .find(&records, &key())
                .is_none()
        );
    }

    #[test]
    fn another_machine_counts_only_when_allowed_and_the_env_checks_agree() {
        let there = env("m2", "1");
        let records = [record(key(), 99 * HOUR_MS, there)];
        let me = env("m1", "1");
        let by_default =
            Config::parse(&format!("env_check = [\"tool --version\"]\n{TOML}")).unwrap();
        assert!(
            lookup(&by_default, Some(&me), &[])
                .find(&records, &key())
                .is_none()
        );

        let allowed = Config::parse(&format!(
            "reuse_other_machines = true\nenv_check = [\"tool --version\"]\n{TOML}"
        ))
        .unwrap();
        assert!(
            lookup(&allowed, Some(&me), &[])
                .find(&records, &key())
                .is_some()
        );
        let drifted = env("m1", "2");
        assert!(
            lookup(&allowed, Some(&drifted), &[])
                .find(&records, &key())
                .is_none()
        );

        let no_checks = Config::parse(&format!("reuse_other_machines = true\n{TOML}")).unwrap();
        assert!(
            lookup(&no_checks, Some(&me), &[])
                .find(&records, &key())
                .is_none()
        );
        assert!(lookup(&allowed, None, &[]).find(&records, &key()).is_none());
    }

    #[test]
    fn a_machine_without_an_id_is_never_the_same_machine() {
        let config = Config::parse(TOML).unwrap();
        let mut me = env("m1", "1");
        me.machine_id = None;
        let records = [record(key(), 99 * HOUR_MS, me.clone())];
        assert!(
            lookup(&config, Some(&me), &[])
                .find(&records, &key())
                .is_none()
        );
    }

    #[test]
    fn a_step_on_the_flaky_ledger_blocks_reuse_unless_the_pass_left_it_out() {
        let config = Config::parse(TOML).unwrap();
        let me = env("m1", "1");
        let flaky = ["lint".to_owned()];
        let records = [record(key(), 99 * HOUR_MS, me.clone())];
        assert!(
            lookup(&config, Some(&me), &flaky)
                .find(&records, &key())
                .is_none()
        );
        let without_lint = TreeKey {
            skipped: vec!["lint".to_owned()],
            ..key()
        };
        let records = [record(without_lint.clone(), 99 * HOUR_MS, me.clone())];
        assert!(
            lookup(&config, Some(&me), &flaky)
                .find(&records, &without_lint)
                .is_some()
        );
    }

    #[test]
    fn remembering_replaces_the_same_key_drops_the_old_and_caps_the_count() {
        let me = env("m1", "1");
        let now = EpochMs(100 * HOUR_MS);
        let mut state = State::default();
        remember(&mut state, record(key(), 10 * HOUR_MS, me.clone()), 24, now);
        remember(&mut state, record(key(), 99 * HOUR_MS, me.clone()), 24, now);
        assert_eq!(state.verified_trees.len(), 1);
        assert_eq!(state.verified_trees[0].at_ms, EpochMs(99 * HOUR_MS));
        let old = TreeKey {
            tree: sha('c'),
            ..key()
        };
        state
            .verified_trees
            .push(record(old, 10 * HOUR_MS, me.clone()));
        remember(
            &mut state,
            record(
                TreeKey {
                    tree: sha('d'),
                    ..key()
                },
                99 * HOUR_MS,
                me.clone(),
            ),
            24,
            now,
        );
        assert_eq!(state.verified_trees.len(), 2);
        for n in 0..(MAX_KEPT + 5) {
            let tree = Sha::try_from(format!("{n:040x}")).unwrap();
            remember(
                &mut state,
                record(TreeKey { tree, ..key() }, 99 * HOUR_MS, me.clone()),
                24,
                now,
            );
        }
        assert_eq!(state.verified_trees.len(), MAX_KEPT);
    }
}
