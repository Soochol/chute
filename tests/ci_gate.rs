mod engine_kit;
mod support;

use std::time::Duration;

use chute::engine::{Pass, Summary};
use chute::model::{
    Approval, BounceReason, EpochMs, Failure, HumanReason, ItemState, PrNumber, QueuedReason,
    WarningKind,
};
use engine_kit::Lab;

const CI_PENDING: ItemState = ItemState::Queued(Some(QueuedReason::CiPending));
const REQUIRE_CI: &str = "require_ci = true";
const BAD: &str = "[ -e BAD ] && fail \"FAILED bad_test\"";

fn done(pass: Pass) -> Summary {
    match pass {
        Pass::Done(summary) => summary,
        other => panic!("batch가 끝나야 한다: {other:?}"),
    }
}

fn prs(list: &[u64]) -> Vec<PrNumber> {
    list.iter().copied().map(PrNumber).collect()
}

fn lab_requiring_ci() -> Lab {
    Lab::build("", REQUIRE_CI, 60)
}

fn make_submitted_minutes_ago(lab: &Lab, pr: u64, minutes: u64) {
    lab.store
        .update(|s| {
            let item = s.queue.iter_mut().find(|i| i.pr == PrNumber(pr)).unwrap();
            item.submitted_ms = EpochMs(EpochMs::now().0 - minutes * 60_000);
            Ok(())
        })
        .unwrap();
}

#[test]
fn with_the_option_off_a_failing_check_does_not_stop_a_locally_green_pr_and_gh_is_never_asked() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.gh_checks(1, &[("build", "fail")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert!(summary.bounced.is_empty());
    assert_eq!(lab.gh_check_reads(), 0);
    assert!(lab.state().queue.is_empty());
}

#[test]
fn a_pr_whose_checks_passed_is_batched_and_merged() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.gh_checks(1, &[("build", "pass"), ("docs", "skipping")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(lab.gh_check_reads(), 1);
    assert_eq!(lab.merge_commits_on_main().len(), 1);
}

#[test]
fn a_pr_with_running_checks_waits_as_ci_pending_while_the_rest_of_the_queue_merges() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.gh_checks(1, &[("build", "pending"), ("lint", "pass")]);
    lab.gh_checks(2, &[("build", "pass")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[2]));
    assert_eq!(lab.item_state(1), Some(CI_PENDING));
    assert_eq!(lab.item(1).unwrap().attempts, 0);
    assert_eq!(lab.merged_prs(), [2]);
    assert!(lab.state().batch.is_none());

    lab.gh_checks(1, &[("build", "pass"), ("lint", "pass")]);
    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert!(lab.state().queue.is_empty());
    assert_eq!(lab.merged_prs(), [2, 1]);
}

#[test]
fn a_ci_pending_pr_does_not_crowd_a_ready_pr_out_of_a_full_batch() {
    let lab = Lab::build("", "require_ci = true\nmax_batch = 1", 60);
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.gh_checks(1, &[("build", "pending")]);
    lab.gh_checks(2, &[("build", "pass")]);
    lab.store
        .update(|s| Ok(chute::queue::set_ci_pending(s, PrNumber(1))?))
        .unwrap();

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[2]));
    assert_eq!(lab.item_state(1), Some(CI_PENDING));
}

#[test]
fn a_queue_holding_only_ci_waiters_pushes_nothing_and_leaves_no_batch_behind() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.gh_checks(1, &[("build", "pending")]);
    let main = lab.origin_main();

    let pass = lab.engine().run_once().unwrap();
    let again = lab.engine().run_once().unwrap();

    assert_eq!(pass, Pass::Idle);
    assert_eq!(again, Pass::Idle);
    assert_eq!(lab.origin_main(), main);
    assert_eq!(lab.item_state(1), Some(CI_PENDING));
    assert!(lab.batch_ids().is_empty());
    assert_eq!(lab.verify_runs(), 0);
    assert!(lab.events().is_empty());
}

#[test]
fn ci_waiters_are_looked_at_again_only_after_the_recheck_interval() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.gh_checks(1, &[("build", "pending")]);
    let engine = lab.engine_rechecking_ci_every(Duration::from_secs(3600));

    assert_eq!(engine.run_once().unwrap(), Pass::Idle);
    assert_eq!(engine.run_once().unwrap(), Pass::Idle);
    assert_eq!(lab.gh_check_reads(), 1);

    lab.gh_checks(1, &[("build", "pass")]);
    assert_eq!(engine.run_once().unwrap(), Pass::Idle);
    assert_eq!(lab.gh_check_reads(), 1, "the interval has not passed");

    let summary = done(lab.engine().run_once().unwrap());
    assert_eq!(summary.merged, prs(&[1]));
}

#[test]
fn a_pr_with_no_checks_waits_and_warns_once_after_ten_minutes() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);

    lab.engine().run_once().unwrap();
    assert_eq!(lab.item_state(1), Some(CI_PENDING));
    assert!(lab.state().warnings.is_empty());

    make_submitted_minutes_ago(&lab, 1, 11);
    lab.engine().run_once().unwrap();
    lab.engine().run_once().unwrap();

    let warnings = lab.state().warnings;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].kind, WarningKind::NoChecks);
    assert_eq!(warnings[0].pr, Some(PrNumber(1)));
    assert_eq!(lab.item_state(1), Some(CI_PENDING));
    let status = lab
        .sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .current_dir(lab.repo.path())
        .args(["status", "--json"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(json["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(json["warnings"][0]["kind"], "no_checks");

    lab.gh_checks(1, &[("build", "pass")]);
    let summary = done(lab.engine().run_once().unwrap());
    assert_eq!(summary.merged, prs(&[1]));
    assert!(lab.state().warnings.is_empty());
}

#[test]
fn a_failed_check_bounces_the_pr_with_a_report_one_message_and_no_local_verification() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.gh_checks(1, &[("build", "fail"), ("lint", "pass")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    assert!(summary.merged.is_empty());
    assert_eq!(
        lab.item_state(1),
        Some(ItemState::Bounced(BounceReason::Ci))
    );
    let item = lab.item(1).unwrap();
    assert_eq!(item.attempts, 1);
    let record = item.last_failure.unwrap();
    assert!(matches!(record.failure, Failure::Ci { .. }));
    let report = std::fs::read_to_string(&record.report).unwrap();
    assert!(report.contains("build"), "{report}");
    assert!(report.contains("https://ci.example/1/build"), "{report}");
    assert!(!report.contains("lint"), "{report}");
    let sends = lab.hc_calls("send");
    assert_eq!(sends.len(), 1);
    assert!(sends[0][2].contains("원격 CI 실패"), "{:?}", sends[0]);
    assert_eq!(lab.verify_runs(), 0);
    assert_eq!(lab.bounced_events(), [(1, BounceReason::Ci)]);
}

#[test]
fn a_cancelled_check_and_a_failure_beside_a_running_one_both_bounce() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.gh_checks(1, &[("build", "cancel")]);
    lab.gh_checks(2, &[("build", "fail"), ("slow", "pending")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1, 2]));
    assert_eq!(lab.verify_runs(), 0);
}

#[test]
fn ci_failures_spend_the_retry_budget_and_then_go_to_a_human() {
    let lab = lab_requiring_ci();
    let head = lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.gh_checks(1, &[("build", "fail")]);

    for round in 1..=3 {
        done(lab.engine().run_once().unwrap());
        assert_eq!(lab.item(1).unwrap().attempts, round);
        lab.queue_pr(1, &head, Some(lab.agent()), Approval::Agent);
    }
    assert_eq!(lab.hc_calls("send").len(), 3);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    assert_eq!(
        lab.item_state(1),
        Some(ItemState::NeedsHuman(HumanReason::Budget))
    );
    assert_eq!(lab.hc_calls("send").len(), 3);
}

#[test]
fn passing_checks_do_not_override_a_failing_local_verification() {
    let lab = Lab::build(BAD, REQUIRE_CI, 60);
    lab.open_pr(1, &[("BAD", "")]);
    lab.gh_checks(1, &[("build", "pass")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    assert!(summary.merged.is_empty());
    assert_eq!(
        lab.item_state(1),
        Some(ItemState::Bounced(BounceReason::Verify))
    );
    assert!(lab.merge_commits_on_main().is_empty());
}
