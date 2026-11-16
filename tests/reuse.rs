mod engine_kit;
mod support;

use std::fs;
use std::time::Duration;

use chute::config::Config;
use chute::engine::Pass;
use chute::ext::Ext;
use chute::model::{
    BatchId, EnvCheck, EnvRecord, EpochMs, Event, EventKind, MachineId, Sha, TreeKey, VerifiedBy,
    VerifiedTree,
};
use chute::verify::Cancel;
use engine_kit::Lab;

const HOUR: Duration = Duration::from_secs(3600);

fn merged_all(pass: Pass) -> chute::engine::Summary {
    match pass {
        Pass::Done(summary) => summary,
        other => panic!("batch가 끝나야 한다: {other:?}"),
    }
}

fn hours_ago(hours: u64) -> EpochMs {
    EpochMs(EpochMs::now().0 - hours * HOUR.as_millis() as u64)
}

fn config_of(lab: &Lab) -> Config {
    Config::parse(&fs::read_to_string(lab.repo.path().join("chute.toml")).unwrap()).unwrap()
}

/// The tree a batch builds from `main` and `heads` merged in order.
fn merged_tree(lab: &Lab, heads: &[&Sha]) -> Sha {
    let probe = lab.sb.root().join("tree-probe");
    let _ = fs::remove_dir_all(&probe);
    lab.origin_git(&["worktree", "prune"]);
    lab.repo.git(&["fetch", "--quiet", "origin"]);
    lab.repo.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        &probe.to_string_lossy(),
        "origin/main",
    ]);
    let git = |args: &[&str]| {
        let out = lab
            .sb
            .command("git")
            .current_dir(&probe)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    for head in heads {
        git(&["merge", "--quiet", "--no-ff", "-m", "probe", head.as_str()]);
    }
    let tree = git(&["rev-parse", "HEAD^{tree}"]);
    lab.repo
        .git(&["worktree", "remove", "--force", &probe.to_string_lossy()]);
    Sha::try_from(tree).unwrap()
}

fn here(lab: &Lab, config: &Config) -> EnvRecord {
    let ext = Ext::from_vars(|k| lab.sb.var(k));
    chute::env::record(
        &ext,
        lab.repo.path(),
        &lab.sb.root().join("env-probe"),
        &config.env_check,
        &Cancel::new(),
    )
}

fn key_of(lab: &Lab, config: &Config, tree: Sha) -> TreeKey {
    let ext = Ext::from_vars(|k| lab.sb.var(k));
    TreeKey {
        tree,
        config: chute::reuse::fingerprint(&ext, lab.repo.path(), config).unwrap(),
        skipped: Vec::new(),
    }
}

fn remember(lab: &Lab, key: TreeKey, at_ms: EpochMs, env: EnvRecord) {
    lab.store
        .update(|s| {
            s.verified_trees.push(VerifiedTree {
                key,
                at_ms,
                by: VerifiedBy::Preflight,
                env: Some(env),
            });
            Ok(())
        })
        .unwrap();
}

fn reused_events(lab: &Lab) -> Vec<(VerifiedBy, Sha)> {
    lab.events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Reused { by, tree, .. } => Some((by, tree)),
            _ => None,
        })
        .collect()
}

fn ran_the_steps(lab: &Lab) -> usize {
    lab.verify_runs()
}

#[test]
fn a_tree_that_passed_before_is_not_verified_again_and_the_batch_says_whose_pass_it_took() {
    let lab = Lab::new("");
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    remember(
        &lab,
        key_of(&lab, &config, tree.clone()),
        hours_ago(1),
        here(&lab, &config),
    );

    let summary = merged_all(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, [chute::model::PrNumber(1)]);
    assert_eq!(ran_the_steps(&lab), 0);
    assert_eq!(reused_events(&lab), [(VerifiedBy::Preflight, tree.clone())]);
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.verifications, 0);
    assert_eq!(record.reused.len(), 1);
    assert_eq!(record.reused[0].tree, tree);
    assert_eq!(record.reused[0].by, VerifiedBy::Preflight);
    assert_eq!(lab.merged_prs(), [1]);
}

#[test]
fn a_batch_that_verified_a_tree_leaves_the_pass_for_the_next_batch_that_builds_it_again() {
    let lab = Lab::new("");
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let tree = merged_tree(&lab, &[&head]);
    let main_before = lab.origin_main();
    merged_all(lab.engine().run_once().unwrap());
    assert_eq!(ran_the_steps(&lab), 1);
    let kept = lab.state().verified_trees;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].key.tree, tree);
    assert_eq!(kept[0].by, VerifiedBy::Batch);

    lab.origin_git(&["update-ref", "refs/heads/main", &main_before]);
    lab.queue_pr(1, &head, Some(lab.agent()), chute::model::Approval::Agent);
    let again = merged_all(lab.engine().run_once().unwrap());

    assert_eq!(ran_the_steps(&lab), 1, "같은 트리라 다시 돌지 않았다");
    assert_eq!(reused_events(&lab), [(VerifiedBy::Batch, tree)]);
    let record = lab.store.read_batch(&again.batch).unwrap().unwrap();
    assert_eq!(record.reused.len(), 1);
}

fn assert_verified_again(lab: &Lab) {
    let summary = merged_all(lab.engine().run_once().unwrap());
    assert!(!summary.merged.is_empty(), "{summary:?}");
    assert!(lab.verify_runs() >= 1, "다시 검증해야 한다");
    assert!(reused_events(lab).is_empty(), "{:?}", reused_events(lab));
}

#[test]
fn a_different_main_means_verifying_again() {
    let lab = Lab::new("");
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    remember(
        &lab,
        key_of(&lab, &config, tree),
        hours_ago(1),
        here(&lab, &config),
    );
    lab.main_commit(&[("elsewhere.txt", "y\n")], "chore: main이 움직였다");
    lab.repo.git(&["push", "--quiet", "origin", "main"]);

    assert_verified_again(&lab);
}

#[test]
fn a_different_pr_head_means_verifying_again() {
    let lab = Lab::new("");
    let old = lab.push_pr_branch(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&old]);
    remember(
        &lab,
        key_of(&lab, &config, tree),
        hours_ago(1),
        here(&lab, &config),
    );
    lab.open_pr(1, &[("f1.txt", "changed\n")]);

    assert_verified_again(&lab);
}

#[test]
fn a_different_queue_means_verifying_again() {
    let lab = Lab::new("");
    let one = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&one]);
    remember(
        &lab,
        key_of(&lab, &config, tree),
        hours_ago(1),
        here(&lab, &config),
    );
    lab.open_pr(2, &[("f2.txt", "y\n")]);

    assert_verified_again(&lab);
}

#[test]
fn a_different_verify_definition_means_verifying_again() {
    let lab = Lab::new("");
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    let other = Config::parse("[[verify]]\nname = \"check\"\nrun = \"true\"\n").unwrap();
    remember(
        &lab,
        key_of(&lab, &other, tree),
        hours_ago(1),
        here(&lab, &config),
    );

    assert_verified_again(&lab);
}

#[test]
fn a_step_left_out_by_paths_is_part_of_the_key() {
    let lab = Lab::build_with_step("", "", 60, "paths = [\"docs/\"]");
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    remember(
        &lab,
        key_of(&lab, &config, tree.clone()),
        hours_ago(1),
        here(&lab, &config),
    );

    merged_all(lab.engine().run_once().unwrap());

    assert!(reused_events(&lab).is_empty());
    let kept = lab.state().verified_trees;
    let own = kept.iter().find(|v| v.by == VerifiedBy::Batch).unwrap();
    assert_eq!(own.key.tree, tree);
    assert_eq!(own.key.skipped, ["check"]);
}

#[test]
fn a_failed_verification_leaves_no_record_and_the_next_batch_verifies_again() {
    let lab = Lab::new("[ -e BAD ] && fail \"FAILED bad_test\"");
    let bad = lab.open_pr(1, &[("BAD", "")]);
    let failing = merged_tree(&lab, &[&bad]);
    let summary = merged_all(lab.engine().run_once().unwrap());
    assert!(summary.merged.is_empty());
    let kept = |lab: &Lab| {
        lab.state()
            .verified_trees
            .iter()
            .any(|v| v.key.tree == failing)
    };
    assert!(!kept(&lab));
    let runs = lab.verify_runs();
    assert!(runs >= 1);

    lab.open_pr(1, &[("BAD", "")]);
    merged_all(lab.engine().run_once().unwrap());

    assert!(lab.verify_runs() > runs);
    assert!(reused_events(&lab).is_empty());
    assert!(!kept(&lab));
}

#[test]
fn a_record_older_than_the_window_is_not_reused_and_one_inside_it_is() {
    let lab = Lab::build("", "reuse_hours = 2", 60);
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    remember(
        &lab,
        key_of(&lab, &config, tree),
        hours_ago(3),
        here(&lab, &config),
    );
    assert_verified_again(&lab);

    let lab = Lab::build("", "reuse_hours = 2", 60);
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    remember(
        &lab,
        key_of(&lab, &config, tree.clone()),
        hours_ago(1),
        here(&lab, &config),
    );
    merged_all(lab.engine().run_once().unwrap());
    assert_eq!(lab.verify_runs(), 0);
    assert_eq!(reused_events(&lab).len(), 1);
}

fn another_machine(env: &EnvRecord) -> EnvRecord {
    EnvRecord {
        machine_id: Some(
            MachineId::try_from("0000000000000000000000000000000b".to_owned()).unwrap(),
        ),
        host: "another-pc".to_owned(),
        checks: env.checks.clone(),
    }
}

#[test]
fn a_pass_from_another_machine_is_not_reused_by_default() {
    let lab = Lab::build("", "env_check = [\"echo v1\"]", 60);
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    let there = another_machine(&here(&lab, &config));
    remember(&lab, key_of(&lab, &config, tree), hours_ago(1), there);

    assert_verified_again(&lab);
}

#[test]
fn a_pass_from_another_machine_is_reused_when_allowed_and_the_environments_match() {
    let top = "env_check = [\"echo v1\"]\nreuse_other_machines = true";
    let lab = Lab::build("", top, 60);
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    let there = another_machine(&here(&lab, &config));
    remember(
        &lab,
        key_of(&lab, &config, tree.clone()),
        hours_ago(1),
        there,
    );
    merged_all(lab.engine().run_once().unwrap());
    assert_eq!(lab.verify_runs(), 0);
    assert_eq!(reused_events(&lab), [(VerifiedBy::Preflight, tree)]);

    let lab = Lab::build("", top, 60);
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    let mut there = another_machine(&here(&lab, &config));
    there.checks = vec![EnvCheck {
        run: "echo v1".to_owned(),
        output: "v0".to_owned(),
        ok: true,
    }];
    remember(&lab, key_of(&lab, &config, tree), hours_ago(1), there);
    assert_verified_again(&lab);
}

#[test]
fn a_step_on_the_flaky_ledger_is_verified_again() {
    let lab = Lab::new("");
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    remember(
        &lab,
        key_of(&lab, &config, tree.clone()),
        hours_ago(1),
        here(&lab, &config),
    );
    lab.store
        .append_history(&Event {
            at_ms: hours_ago(2),
            kind: EventKind::Flaky {
                batch: BatchId("b-old".to_owned()),
                step: "check".to_owned(),
                tests: vec!["some_test".to_owned()],
                tree: Sha::try_from("1".repeat(40)).unwrap(),
            },
        })
        .unwrap();

    assert_verified_again(&lab);
}

#[test]
fn turning_reuse_off_verifies_every_time_and_keeps_no_record() {
    let lab = Lab::build("", "reuse_verified = false", 60);
    let head = lab.open_pr(1, &[("f1.txt", "x\n")]);
    let config = config_of(&lab);
    let tree = merged_tree(&lab, &[&head]);
    remember(
        &lab,
        key_of(&lab, &config, tree),
        hours_ago(1),
        here(&lab, &config),
    );
    let before = lab.state().verified_trees.len();

    assert_verified_again(&lab);

    assert_eq!(lab.state().verified_trees.len(), before);
}

fn bisected_runs(seed_the_good_half: bool) -> (usize, usize) {
    let lab = Lab::new("[ -e BAD ] && fail \"FAILED bad_test\"");
    let good = lab.open_pr(1, &[("f1.txt", "x\n")]);
    lab.open_pr(2, &[("BAD", "")]);
    if seed_the_good_half {
        let config = config_of(&lab);
        let tree = merged_tree(&lab, &[&good]);
        remember(
            &lab,
            key_of(&lab, &config, tree),
            hours_ago(1),
            here(&lab, &config),
        );
    }
    let summary = merged_all(lab.engine().run_once().unwrap());
    assert_eq!(summary.merged, [chute::model::PrNumber(1)]);
    (lab.verify_runs(), reused_events(&lab).len())
}

#[test]
fn bisecting_a_failed_batch_verifies_each_half_without_taking_over_a_pass() {
    let (plain, _) = bisected_runs(false);
    let (seeded, reused) = bisected_runs(true);

    assert_eq!(reused, 0);
    assert_eq!(seeded, plain);
}
