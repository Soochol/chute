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

#[test]
fn a_passing_pr_behind_more_pending_prs_than_max_batch_is_still_examined_and_merged() {
    let lab = Lab::build("", "require_ci = true\nmax_batch = 2", 60);
    for n in 1..=3 {
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
        lab.gh_checks(n, &[("build", if n < 3 { "pending" } else { "pass" })]);
    }
    for n in 1..=3 {
        lab.store
            .update(|s| Ok(chute::queue::set_ci_pending(s, PrNumber(n))?))
            .unwrap();
    }

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[3]));
    assert_eq!(lab.item_state(1), Some(CI_PENDING));
    assert_eq!(lab.item_state(2), Some(CI_PENDING));
}

#[test]
fn passing_prs_beyond_max_batch_go_back_to_the_queue_and_merge_next_time() {
    let lab = Lab::build("", "require_ci = true\nmax_batch = 1", 60);
    for n in 1..=2 {
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
        lab.gh_checks(n, &[("build", "pass")]);
        lab.store
            .update(|s| Ok(chute::queue::set_ci_pending(s, PrNumber(n))?))
            .unwrap();
    }

    let first = done(lab.engine().run_once().unwrap());
    assert_eq!(first.merged, prs(&[1]));
    assert_eq!(lab.item_state(2), Some(CI_PENDING));
    assert_eq!(lab.gh_check_reads(), 1);

    let second = done(lab.engine().run_once().unwrap());
    assert_eq!(second.merged, prs(&[2]));
}

#[test]
fn a_leftover_ci_wait_mark_with_the_option_off_is_merged_on_local_verification_without_asking_gh() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.store
        .update(|s| Ok(chute::queue::set_ci_pending(s, PrNumber(1))?))
        .unwrap();
    lab.gh_checks(1, &[("build", "fail")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(lab.gh_check_reads(), 0);
}

#[test]
fn a_pr_held_back_for_ci_is_not_a_member_of_the_running_batch_or_its_record() {
    let lab = Lab::build("@STATUS@", REQUIRE_CI, 60);
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.gh_checks(1, &[("build", "pending")]);
    lab.gh_checks(2, &[("build", "pass")]);

    let summary = done(lab.engine().run_once().unwrap());

    let during = std::fs::read_to_string(lab.sb.root().join("status-during.json")).unwrap();
    let during: serde_json::Value = serde_json::from_str(&during).unwrap();
    assert_eq!(during["batch"]["prs"], serde_json::json!([2]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.prs, prs(&[2]));
}

#[test]
fn a_resubmitted_pr_gets_ten_quiet_minutes_before_the_no_checks_warning() {
    let lab = lab_requiring_ci();
    let head = lab.open_pr(1, &[("a.txt", "a\n")]);
    make_submitted_minutes_ago(&lab, 1, 30);
    lab.gh_checks(1, &[("build", "fail")]);
    done(lab.engine().run_once().unwrap());
    lab.queue_pr(1, &head, Some(lab.agent()), Approval::Agent);
    make_submitted_minutes_ago(&lab, 1, 30);
    lab.gh_checks(1, &[]);

    lab.engine().run_once().unwrap();

    assert_eq!(lab.item_state(1), Some(CI_PENDING));
    assert!(
        lab.state().warnings.is_empty(),
        "{:?}",
        lab.state().warnings
    );
}

#[test]
fn the_no_checks_warning_goes_away_when_the_pr_leaves_the_queue() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    make_submitted_minutes_ago(&lab, 1, 11);
    lab.engine().run_once().unwrap();
    assert_eq!(lab.state().warnings.len(), 1);

    lab.gh_says_closed(1);
    lab.engine().run_once().unwrap();
    assert!(lab.state().queue.is_empty());
    lab.engine().run_once().unwrap();

    assert!(
        lab.state().warnings.is_empty(),
        "{:?}",
        lab.state().warnings
    );
}

#[test]
fn the_no_checks_warning_goes_away_once_the_option_is_turned_off() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    make_submitted_minutes_ago(&lab, 1, 11);
    lab.engine().run_once().unwrap();
    assert_eq!(lab.state().warnings.len(), 1);
    let config = std::fs::read_to_string(lab.repo.path().join("chute.toml"))
        .unwrap()
        .replace("require_ci = true", "require_ci = false");
    lab.main_commit(&[("chute.toml", &config)], "chore: require_ci 끔");

    let summary = done(lab.engine().run_once().unwrap());
    lab.engine().run_once().unwrap();

    assert_eq!(summary.merged, prs(&[1]));
    assert!(
        lab.state().warnings.is_empty(),
        "{:?}",
        lab.state().warnings
    );
}

#[test]
fn eight_prs_without_checks_do_not_keep_a_ninth_green_pr_from_merging() {
    let lab = lab_requiring_ci();
    for n in 1..=9 {
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
    }
    lab.gh_checks(9, &[("build", "pending")]);
    lab.engine().run_once().unwrap();
    lab.gh_checks(9, &[("build", "pass")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[9]));
}

fn mark_ci_pending(lab: &Lab, prs: &[u64]) {
    for n in prs {
        lab.store
            .update(|s| Ok(chute::queue::set_ci_pending(s, PrNumber(*n))?))
            .unwrap();
    }
}

#[test]
fn a_ci_waiter_that_did_not_fit_is_no_batch_member_and_keeps_its_wait_mark() {
    let lab = Lab::build("@STATUS@", "require_ci = true\nmax_batch = 1", 60);
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.gh_checks(1, &[("build", "pass")]);
    lab.gh_checks(2, &[("build", "pending")]);
    mark_ci_pending(&lab, &[1, 2]);

    let summary = done(lab.engine().run_once().unwrap());

    let during = std::fs::read_to_string(lab.sb.root().join("status-during.json")).unwrap();
    let during: serde_json::Value = serde_json::from_str(&during).unwrap();
    assert_eq!(during["batch"]["prs"], serde_json::json!([1]));
    assert_eq!(during["queue"][1]["state"], "queued");
    assert_eq!(during["queue"][1]["state_reason"], "ci_pending");
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.prs, prs(&[1]));
    assert_eq!(lab.item_state(2), Some(CI_PENDING));
}

#[test]
fn ci_waiters_are_read_once_per_interval_even_while_other_prs_keep_arriving() {
    let lab = Lab::build("", "require_ci = true\nmax_batch = 1", 60);
    for n in 1..=6 {
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
        lab.gh_checks(n, &[("build", "pending")]);
    }
    let engine = lab.engine_rechecking_ci_every(Duration::from_secs(3600));

    for _ in 0..6 {
        engine.run_once().unwrap();
    }

    assert_eq!(lab.gh_check_reads(), 6);
}

#[test]
fn a_no_checks_warning_is_written_once_even_when_its_pr_is_pushed_out_of_a_batch() {
    let lab = Lab::build("", "require_ci = true\nmax_batch = 1", 60);
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.gh_checks(1, &[("build", "pending")]);
    mark_ci_pending(&lab, &[1, 2]);
    make_submitted_minutes_ago(&lab, 2, 11);
    lab.engine().run_once().unwrap();
    let first = lab.state().warnings;
    assert_eq!(first.len(), 1, "{first:?}");

    lab.gh_checks(1, &[("build", "pass")]);
    done(lab.engine().run_once().unwrap());
    lab.engine().run_once().unwrap();

    assert_eq!(lab.state().warnings, first);
    assert_eq!(lab.item_state(2), Some(CI_PENDING));
}

#[test]
fn reading_new_prs_does_not_push_back_the_next_look_at_the_ones_already_waiting() {
    let lab = Lab::build("", REQUIRE_CI, 60);
    lab.open_pr(1, &[("f1.txt", "x\n")]);
    lab.gh_checks(1, &[("build", "pending")]);
    let engine = lab.engine_rechecking_ci_every(Duration::from_millis(400));
    engine.run_once().unwrap();
    lab.gh_checks(1, &[("build", "pass")]);

    for n in 2..=3 {
        std::thread::sleep(Duration::from_millis(250));
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
        lab.gh_checks(n, &[("build", "pending")]);
        engine.run_once().unwrap();
    }

    assert_eq!(lab.merged_prs(), [1]);
}

#[test]
fn a_green_ci_waiter_is_not_starved_by_ordinary_prs_that_fill_every_batch() {
    let lab = Lab::build("", "require_ci = true\nmax_batch = 1", 60);
    lab.open_pr(1, &[("f1.txt", "x\n")]);
    lab.gh_checks(1, &[("build", "pass")]);
    mark_ci_pending(&lab, &[1]);

    for n in 2..=4 {
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
        lab.gh_checks(n, &[("build", "pass")]);
        lab.engine().run_once().unwrap();
    }

    assert_eq!(lab.merged_prs()[0], 1);
}

#[test]
fn a_failed_gh_read_keeps_the_wait_marks_and_the_no_checks_warnings_as_they_were() {
    let lab = lab_requiring_ci();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    make_submitted_minutes_ago(&lab, 1, 11);
    make_submitted_minutes_ago(&lab, 2, 11);
    lab.engine().run_once().unwrap();
    let before = lab.state().warnings;
    assert_eq!(before.len(), 2, "{before:?}");

    lab.gh_checks_error(1, 2);
    assert!(lab.engine().run_once().is_err());
    assert_eq!(lab.item_state(1), Some(CI_PENDING));
    assert_eq!(lab.item_state(2), Some(CI_PENDING));
    lab.gh_checks(1, &[]);
    lab.engine().run_once().unwrap();

    assert_eq!(lab.state().warnings, before);
}

#[test]
fn turning_the_option_off_clears_every_no_checks_warning_and_wait_mark() {
    let lab = Lab::build("", "require_ci = true\nmax_batch = 1", 60);
    for n in 1..=3 {
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
        make_submitted_minutes_ago(&lab, n, 11);
    }
    mark_ci_pending(&lab, &[1, 2, 3]);
    lab.engine().run_once().unwrap();
    assert_eq!(lab.state().warnings.len(), 3);
    let config = std::fs::read_to_string(lab.repo.path().join("chute.toml"))
        .unwrap()
        .replace("require_ci = true", "require_ci = false");
    lab.main_commit(&[("chute.toml", &config)], "chore: require_ci 끔");

    lab.engine().run_once().unwrap();

    assert!(
        lab.state().warnings.is_empty(),
        "{:?}",
        lab.state().warnings
    );
    let marked = lab
        .state()
        .queue
        .iter()
        .filter(|i| i.state == CI_PENDING)
        .count();
    assert_eq!(marked, 0);
}
