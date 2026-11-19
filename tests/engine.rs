mod engine_kit;
mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chute::engine::{Pass, Summary};
use chute::model::{
    Approval, BatchId, BlockReason, BounceReason, EpochMs, Event, EventKind, Failure, ItemState,
    PrNumber, ProbeEvidence, RemovedReason, Rerun, RerunSignal, SkippedStep, StatusView,
    VerifyMode, WarningKind,
};
use chute::queue;
use engine_kit::Lab;
use support::Reply;

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

fn chute_status(lab: &Lab) -> serde_json::Value {
    let out = lab
        .sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .current_dir(lab.repo.path())
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn three_passing_prs_become_three_merge_commits_after_one_verification() {
    let lab = Lab::new("");
    for n in 1..=3 {
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
    }

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2, 3]));
    assert!(summary.bounced.is_empty());
    assert!(lab.state().queue.is_empty());
    assert!(lab.state().batch.is_none());
    assert_eq!(lab.merge_commits_on_main().len(), 3);
    let subjects = lab.origin_git(&["log", "--first-parent", "--merges", "--format=%s", "main"]);
    assert_eq!(
        subjects.lines().collect::<Vec<_>>(),
        [
            "Merge pull request #3 from acme/pr-3",
            "Merge pull request #2 from acme/pr-2",
            "Merge pull request #1 from acme/pr-1"
        ]
    );
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.verifications, 1);
    assert_eq!(record.pushed.len(), 1);
    assert_eq!(record.merges.len(), 3);
    assert!(record.finished_ms.is_some());
    assert_eq!(lab.assert_every_pushed_commit_was_verified(), 1);
    assert!(lab.state().verified_bases.contains(&record.pushed[0]));
    assert!(lab.hc_calls("send").is_empty());
    assert_eq!(lab.merged_prs(), [1, 2, 3]);
}

fn main_of(lab: &Lab, git_dir: &std::path::Path) -> String {
    let mut git = lab.sb.command("git");
    let out = git
        .arg("--git-dir")
        .arg(git_dir)
        .args(["rev-parse", "main"]);
    String::from_utf8(out.output().unwrap().stdout).unwrap()
}

#[test]
fn a_batch_merges_into_the_remote_named_upstream_and_leaves_the_fork_alone() {
    let mut lab = Lab::new("");
    for n in 1..=2 {
        lab.open_pr(n, &[(&format!("f{n}.txt"), "x\n")]);
    }
    let fork = lab.sb.root().join("remotes/me/widget.git");
    let fork_path = fork.to_string_lossy().into_owned();
    let upstream = lab.repo.origin().to_string_lossy().into_owned();
    lab.repo
        .git(&["clone", "--quiet", "--bare", &upstream, &fork_path]);
    let fork_before = main_of(&lab, &fork);
    lab.repo.git(&["remote", "rename", "origin", "upstream"]);
    lab.repo.git(&["remote", "add", "origin", &fork_path]);
    lab.repo.git(&["fetch", "--quiet", "origin"]);
    lab.repo.git(&["checkout", "--quiet", "main"]);
    let config = fs::read_to_string(lab.repo.path().join("chute.toml")).unwrap();
    lab.repo.commit_file(
        "chute.toml",
        &format!("remote = \"upstream\"\n{config}"),
        "chore: remote 키",
    );
    lab.repo.git(&["push", "--quiet", "upstream", "main"]);
    lab.info = chute::repo::discover(
        &chute::ext::Ext::from_vars(|k| lab.sb.var(k)),
        lab.repo.path(),
        &|| Ok(Vec::new()),
    )
    .unwrap();
    assert_eq!(lab.info.remote_name, "upstream");
    lab.store.register(&lab.info).unwrap();

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2]));
    assert_eq!(lab.merge_commits_on_main().len(), 2);
    assert_eq!(main_of(&lab, &fork), fork_before, "fork의 main은 그대로다");
}

#[test]
fn status_has_the_planned_shape_during_and_after_a_batch() {
    let lab = Lab::new("@STATUS@");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    let during = fs::read_to_string(lab.sb.root().join("status-during.json")).unwrap();
    let view: StatusView = serde_json::from_str(&during).unwrap();
    let batch = view.batch.expect("검증 도중에는 batch가 있다");
    assert_eq!(batch.id, summary.batch);
    assert_eq!(batch.prs, prs(&[1, 2]));
    assert_eq!(batch.step.as_deref(), Some("check"));
    assert_eq!(batch.round, 1);
    let json: serde_json::Value = serde_json::from_str(&during).unwrap();
    assert_eq!(json["batch"]["phase"], "verifying");
    assert_eq!(json["queue"][0]["state"], "in_batch");
    assert_eq!(json["queue"][1]["state"], "in_batch");

    let after = chute_status(&lab);
    assert_eq!(after["batch"], serde_json::Value::Null);
    assert_eq!(after["queue"], serde_json::json!([]));
    assert_eq!(after["today"]["merged"], 2);
    assert_eq!(after["today"]["batches"], 1);
    assert_eq!(after["recent"][0]["merged"], serde_json::json!([1, 2]));
    let _: StatusView = serde_json::from_value(after).unwrap();
}

#[test]
fn batch_steps_in_the_status_during_verification_name_the_rounds_steps() {
    let lab = Lab::new("@STATUS@");
    lab.open_pr(1, &[("a.txt", "a\n")]);

    done(lab.engine().run_once().unwrap());

    let during = fs::read_to_string(lab.sb.root().join("status-during.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&during).unwrap();
    assert_eq!(json["batch"]["steps"], serde_json::json!(["check"]));
}

#[test]
fn merged_title_is_the_queue_items_title_in_history_and_in_recent_merged() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);

    done(lab.engine().run_once().unwrap());

    let titles: Vec<Option<String>> = lab
        .events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Merged { title, .. } => Some(title),
            _ => None,
        })
        .collect();
    assert_eq!(
        titles,
        [
            Some("feat: change 1".to_owned()),
            Some("feat: change 2".to_owned())
        ]
    );
    let status = chute_status(&lab);
    assert_eq!(status["recent_merged"][0]["pr"], 2);
    assert_eq!(status["recent_merged"][0]["title"], "feat: change 2");
    assert_eq!(status["recent_merged"][1]["title"], "feat: change 1");
}

#[test]
fn a_pr_that_conflicts_with_main_is_bounced_with_a_report_and_one_message() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("shared.txt", "from pr\n")]);
    lab.open_pr(3, &[("c.txt", "c\n")]);
    lab.main_commit(&[("shared.txt", "from main\n")], "feat: shared");

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 3]));
    assert_eq!(summary.bounced, prs(&[2]));
    let item = lab.item(2).unwrap();
    assert_eq!(item.state, ItemState::Bounced(BounceReason::Conflict));
    assert_eq!(item.attempts, 1);
    let failure = item.last_failure.unwrap();
    assert!(matches!(failure.failure, Failure::Conflict { .. }));
    let report = fs::read_to_string(&failure.report).unwrap();
    assert!(report.contains("shared.txt"), "{report}");
    assert_eq!(lab.hc_calls("send").len(), 1);
    assert_eq!(lab.merge_commits_on_main().len(), 2);
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_conflict_with_a_pr_that_was_not_merged_goes_back_to_the_queue() {
    let lab = Lab::new(BAD);
    lab.open_pr(1, &[("x.txt", "one\n"), ("BAD", "")]);
    lab.open_pr(2, &[("x.txt", "two\n")]);

    let first = done(lab.engine().run_once().unwrap());

    assert!(first.merged.is_empty());
    assert_eq!(first.bounced, prs(&[1]));
    assert_eq!(
        lab.item_state(1),
        Some(ItemState::Bounced(BounceReason::Verify))
    );
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
    assert_eq!(lab.item(2).unwrap().attempts, 0);
    assert_eq!(lab.hc_calls("send").len(), 1);

    let second = done(lab.engine().run_once().unwrap());
    assert_eq!(second.merged, prs(&[2]));
    assert_eq!(lab.origin_git(&["show", "main:x.txt"]), "two");
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn one_failing_pr_among_four_is_found_alone_and_the_rest_merge() {
    let lab = Lab::new(BAD);
    lab.open_pr(1, &[("f1", "x")]);
    lab.open_pr(2, &[("f2", "x")]);
    lab.open_pr(3, &[("f3", "x"), ("BAD", "")]);
    lab.open_pr(4, &[("f4", "x")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2, 4]));
    assert_eq!(summary.bounced, prs(&[3]));
    assert_eq!(
        lab.item_state(3),
        Some(ItemState::Bounced(BounceReason::Verify))
    );
    let Failure::Verify(step) = lab.item(3).unwrap().last_failure.unwrap().failure else {
        panic!("검증 실패여야 한다");
    };
    assert_eq!(step.mode, VerifyMode::Solo);
    assert_eq!(step.step, "check");
    assert_eq!(step.failed_tests, ["bad_test"]);
    assert_eq!(lab.merge_commits_on_main().len(), 3);
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(
        record.verifications <= 8,
        "검증이 {}번 돌았다",
        record.verifications
    );
    assert_eq!(lab.bounced_events(), [(3, BounceReason::Verify)]);
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn two_prs_that_only_fail_together_bounce_the_later_one_as_a_combination() {
    let lab = Lab::new("[ -e a.txt ] && [ -e b.txt ] && fail \"a와 b가 함께 있다\"");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(summary.bounced, prs(&[2]));
    let record = lab.item(2).unwrap().last_failure.unwrap();
    let Failure::Verify(step) = &record.failure else {
        panic!("검증 실패여야 한다");
    };
    let VerifyMode::Combo { partners } = &step.mode else {
        panic!("조합 실패여야 한다: {:?}", step.mode);
    };
    assert_eq!(partners.len(), 1);
    assert_eq!(partners[0].pr, PrNumber(1));
    assert!(partners[0].files.contains(&"a.txt".to_owned()));
    let report = fs::read_to_string(&record.report).unwrap();
    assert!(report.contains("#1"), "{report}");
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_batch_waits_for_a_pc_slot_shows_it_in_the_state_and_then_merges() {
    let lab = Lab::new("");
    std::fs::write(lab.sb.chute_home().join("machine.toml"), "max_verify = 1\n").unwrap();
    let held = chute::pc_limit::acquire(&lab.sb.chute_home(), &chute::verify::Cancel::new(), None)
        .unwrap();
    lab.open_pr(1, &[("a.txt", "a\n")]);

    let watcher = {
        let store = lab.store.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut seen = false;
            while !seen && Instant::now() < deadline {
                seen = store
                    .load()
                    .unwrap()
                    .batch
                    .is_some_and(|batch| batch.waiting_machine);
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(held);
            seen
        })
    };
    let summary = done(lab.engine().run_once().unwrap());

    assert!(
        watcher.join().unwrap(),
        "batch가 PC 자리를 기다리는 중으로 보이지 않았다"
    );
    assert_eq!(summary.merged, prs(&[1]));
    assert!(lab.state().batch.is_none());
}

#[test]
fn a_step_that_fails_once_and_then_passes_is_flaky_and_merges() {
    let lab = Lab::new(
        "m='@ROOT@/flaky-marker'\nif [ ! -e \"$m\" ]; then touch \"$m\"; fail \"FAILED flaky_test\"; fi",
    );
    lab.open_pr(1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert!(summary.bounced.is_empty());
    let flaky: Vec<_> = lab
        .events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Flaky { step, tests, .. } => Some((step, tests)),
            _ => None,
        })
        .collect();
    assert_eq!(flaky, [("check".to_owned(), vec!["flaky_test".to_owned()])]);
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.flaky.len(), 1);
    assert_eq!(chute_status(&lab)["today"]["flaky"], 1);
    assert!(lab.hc_calls("send").is_empty());
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_pr_that_runs_past_the_time_limit_is_bounced_as_a_timeout() {
    let lab = Lab::build("[ -e SLOW ] && sleep 30", "", 1);
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("SLOW", "")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(
        lab.item_state(2),
        Some(ItemState::Bounced(BounceReason::Timeout))
    );
    let Failure::Timeout(step) = lab.item(2).unwrap().last_failure.unwrap().failure else {
        panic!("시간 초과여야 한다");
    };
    assert_eq!(step.mode, VerifyMode::Solo);
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn after_the_budget_is_spent_the_next_failure_takes_the_pr_out_and_sends_nothing_to_the_worker() {
    let lab = Lab::new(BAD);
    let head = lab.open_pr(1, &[("BAD", "")]);

    for round in 1..=3 {
        let summary = done(lab.engine().run_once().unwrap());
        assert_eq!(summary.bounced, prs(&[1]), "{round}번째");
        assert_eq!(
            lab.item_state(1),
            Some(ItemState::Bounced(BounceReason::Verify))
        );
        assert_eq!(lab.item(1).unwrap().attempts, round);
        lab.queue_pr(1, &head, Some(lab.agent()), Approval::Agent);
    }
    assert_eq!(lab.hc_calls("send").len(), 3);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    assert_eq!(lab.item_state(1), None);
    assert_eq!(lab.hc_calls("send").len(), 3);
    let removed: Vec<EventKind> = lab
        .events()
        .into_iter()
        .filter(|e| matches!(e, EventKind::Removed { .. }))
        .collect();
    let [
        EventKind::Removed {
            pr,
            reason: RemovedReason::Budget,
            report: Some(report),
            ..
        },
    ] = removed.as_slice()
    else {
        panic!("removed 이벤트가 하나여야 한다: {removed:?}");
    };
    assert_eq!(*pr, PrNumber(1));
    assert!(report.is_file(), "{}", report.display());
    assert!(
        !lab.events()
            .iter()
            .any(|e| matches!(e, EventKind::NeedsHuman { .. } | EventKind::GaveUp { .. }))
    );
}

#[test]
fn a_finished_agent_is_replaced_and_the_new_address_is_recorded() {
    let lab = Lab::build(BAD, "respawn = [\"claude\"]", 60);
    let hc = lab.sb.fake("hc");
    hc.respond(
        "get t1gone --json",
        Reply::ok(r#"{"address":"t1gone","state":"finished","created_ms":1763188200000}"#),
    );
    hc.respond(
        "run *",
        Reply::ok(
            r#"{"address":"t1new","state":"live","created_ms":1763199999000,"first_message":{"sent":true}}"#,
        ),
    );
    let mut gone = lab.agent();
    gone.terminal = chute::model::TerminalId("t1gone".into());
    lab.open_pr_for(1, &[("BAD", "")], Some(gone), Approval::Agent);

    done(lab.engine().run_once().unwrap());

    let runs = lab.hc_calls("run");
    assert_eq!(runs.len(), 1, "{runs:?}");
    for flag in ["--bg", "--no-parent"] {
        assert!(runs[0].iter().any(|a| a == flag), "{flag}: {:?}", runs[0]);
    }
    assert_eq!(
        lab.item_state(1),
        Some(ItemState::Bounced(BounceReason::Verify))
    );
    let agent = lab.item(1).unwrap().agent.unwrap();
    assert_eq!(agent.terminal.0, "t1new");
    assert_eq!(agent.created_ms.0, 1763199999000);
}

#[test]
fn without_an_agent_or_a_respawn_command_the_pr_stays_bounced_and_the_event_says_why() {
    let lab = Lab::new(BAD);
    let hc = lab.sb.fake("hc");
    hc.respond(
        "get t1gone --json",
        Reply::ok(r#"{"address":"t1gone","state":"finished","created_ms":1763188200000}"#),
    );
    let mut gone = lab.agent();
    gone.terminal = chute::model::TerminalId("t1gone".into());
    lab.open_pr_for(1, &[("BAD", "")], Some(gone), Approval::Agent);

    done(lab.engine().run_once().unwrap());

    assert_eq!(
        lab.item_state(1),
        Some(ItemState::Bounced(BounceReason::Verify))
    );
    assert_eq!(lab.item(1).unwrap().attempts, 1);
    assert!(lab.hc_calls("run").is_empty());
    assert!(lab.events().iter().any(|e| matches!(
        e,
        EventKind::Bounced {
            reason: BounceReason::Verify,
            unreachable: Some(_),
            ..
        }
    )));
    assert!(
        !lab.events()
            .iter()
            .any(|e| matches!(e, EventKind::NeedsHuman { .. }))
    );
    assert_eq!(
        lab.notified(),
        [(
            chute::model::NotifyKind::Bounced,
            chute::model::Delivered::NoTerminal
        )]
    );
}

#[test]
fn a_pr_whose_head_moved_is_not_batched_and_keeps_its_attempts() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.gh_says_head(1, &"ab".repeat(20));

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[2]));
    assert_eq!(summary.bounced, prs(&[1]));
    let item = lab.item(1).unwrap();
    assert_eq!(item.state, ItemState::Bounced(BounceReason::HeadChanged));
    assert_eq!(item.attempts, 0);
    assert_eq!(lab.hc_calls("send").len(), 1);
    assert_eq!(lab.merge_commits_on_main().len(), 1);
}

#[test]
fn a_closed_pr_leaves_the_queue_and_a_pr_merged_elsewhere_counts_as_merged() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.open_pr(3, &[("c.txt", "c\n")]);
    lab.gh_says_closed(1);
    let head2 = lab.item(2).unwrap().head;
    lab.repo.git(&["fetch", "--quiet", "origin"]);
    lab.repo
        .git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
    lab.repo.git(&[
        "merge",
        "--no-ff",
        "--quiet",
        "-m",
        "by hand",
        head2.as_str(),
    ]);
    lab.repo.git(&["push", "--quiet", "origin", "main"]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[2, 3]));
    assert!(lab.state().queue.is_empty());
    assert!(
        lab.events()
            .iter()
            .any(|e| matches!(e, EventKind::Closed { pr } if pr.0 == 1))
    );
    assert_eq!(lab.merged_prs(), [2, 3]);
    assert_eq!(lab.merge_commits_on_main().len(), 2);
}

fn changed_during_verification(change: &str) -> Lab {
    let lab = Lab::new(change);
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab
}

fn assert_only_pr_1_landed_after_a_second_verification(lab: &Lab, summary: &Summary) {
    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(lab.merged_prs(), [1]);
    assert_eq!(lab.verify_runs(), 2);
    assert_eq!(lab.assert_every_pushed_commit_was_verified(), 1);
    let files = lab.origin_git(&["ls-tree", "--name-only", "main"]);
    assert!(files.lines().any(|f| f == "a.txt"), "{files}");
    assert!(!files.lines().any(|f| f == "b.txt"), "{files}");
}

#[test]
fn a_pr_closed_while_the_batch_verifies_is_not_merged_and_the_rest_is_verified_again() {
    let lab = changed_during_verification("touch @ROOT@/gh-state/2.closed");

    let summary = done(lab.engine().run_once().unwrap());

    assert_only_pr_1_landed_after_a_second_verification(&lab, &summary);
    assert!(lab.item(2).is_none());
    assert!(
        lab.events()
            .iter()
            .any(|e| matches!(e, EventKind::Closed { pr } if pr.0 == 2))
    );
    assert!(lab.state().batch.is_none());
}

#[test]
fn a_pr_turned_into_a_draft_while_the_batch_verifies_is_not_merged_and_leaves_the_queue() {
    let lab = changed_during_verification("touch @ROOT@/gh-state/2.draft");

    let summary = done(lab.engine().run_once().unwrap());

    assert_only_pr_1_landed_after_a_second_verification(&lab, &summary);
    assert!(lab.item(2).is_none());
    assert!(
        lab.events()
            .iter()
            .any(|e| matches!(e, EventKind::Dropped { pr } if pr.0 == 2))
    );
    assert!(lab.bounced_events().is_empty());
}

#[test]
fn a_pr_retargeted_while_the_batch_verifies_is_not_merged_and_leaves_the_queue() {
    let lab = changed_during_verification("echo release > @ROOT@/gh-state/2.base");

    let summary = done(lab.engine().run_once().unwrap());

    assert_only_pr_1_landed_after_a_second_verification(&lab, &summary);
    assert!(lab.item(2).is_none());
    assert!(
        lab.events()
            .iter()
            .any(|e| matches!(e, EventKind::Dropped { pr } if pr.0 == 2))
    );
}

#[test]
fn a_draft_pr_at_the_front_leaves_the_queue_once_and_does_not_block_the_ready_pr_behind_it() {
    let lab = Lab::build("", "max_batch = 1\n", 60);
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    fs::write(lab.sb.root().join("gh-state/1.draft"), "").unwrap();
    let engine = lab.engine();

    for _ in 0..5 {
        engine.run_once().unwrap();
    }

    assert_eq!(lab.merged_prs(), [2]);
    assert!(lab.item(1).is_none());
    let calls = fs::read_to_string(lab.sb.root().join("gh-state/calls")).unwrap();
    let reads_of_1 = calls
        .lines()
        .filter(|l| l.starts_with("pr view 1 "))
        .count();
    assert_eq!(reads_of_1, 1, "{calls}");
}

#[test]
fn a_pr_whose_head_moves_while_the_batch_verifies_is_sent_back_and_not_merged() {
    let moved = "ab".repeat(20);
    let lab = changed_during_verification(&format!("echo {moved} > @ROOT@/gh-state/2.head"));

    let summary = done(lab.engine().run_once().unwrap());

    assert_only_pr_1_landed_after_a_second_verification(&lab, &summary);
    assert_eq!(summary.bounced, prs(&[2]));
    assert_eq!(
        lab.item_state(2),
        Some(ItemState::Bounced(BounceReason::HeadChanged))
    );
    assert_eq!(lab.bounced_events(), [(2, BounceReason::HeadChanged)]);
    assert_eq!(lab.hc_calls("send").len(), 1);
}

#[test]
fn when_every_pr_is_closed_while_the_batch_verifies_nothing_is_pushed() {
    let lab =
        changed_during_verification("touch @ROOT@/gh-state/1.closed @ROOT@/gh-state/2.closed");
    let before = lab.origin_main();

    done(lab.engine().run_once().unwrap());

    assert_eq!(lab.origin_main(), before);
    assert!(lab.merged_prs().is_empty());
    assert_eq!(lab.verify_runs(), 1);
    assert!(lab.state().queue.is_empty());
}

#[test]
fn when_github_cannot_be_read_before_the_push_nothing_is_pushed_and_the_batch_ends_in_an_error() {
    let lab = changed_during_verification("touch @ROOT@/gh-state/2.unreadable");
    let before = lab.origin_main();

    let err = lab.engine().run_once().unwrap_err();

    assert!(format!("{err:#}").contains("push하지 않았다"), "{err:#}");
    assert_eq!(lab.origin_main(), before);
    assert!(lab.merged_prs().is_empty());
    assert!(lab.state().batch.is_none());
    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
}

#[test]
fn when_the_base_itself_fails_nobody_is_bounced_and_the_queue_stops_until_main_changes() {
    let lab = Lab::new("[ -e BASEBAD ] && fail \"기준이 깨졌다\"");
    lab.main_commit(&[("BASEBAD", "")], "feat: broken");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);

    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);

    let blocked = lab.state().blocked.expect("저장소가 멈춘다");
    assert_eq!(blocked.reason, BlockReason::BaseBroken);
    assert_eq!(blocked.base.as_str(), lab.origin_main());
    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
    assert!(lab.bounced_events().is_empty());
    assert!(lab.hc_calls("send").is_empty());
    assert!(lab.state().batch.is_none());
    assert_eq!(chute_status(&lab)["blocked"]["reason"], "base_broken");
    assert!(lab.events().iter().any(|e| matches!(
        e,
        EventKind::Blocked {
            reason: BlockReason::BaseBroken,
            ..
        }
    )));

    let runs = lab.verify_runs();
    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);
    assert_eq!(lab.verify_runs(), runs, "멈춘 동안은 검증하지 않는다");

    lab.remove_from_main("BASEBAD", "fix: 기준을 고친다");
    let summary = done(lab.engine().run_once().unwrap());
    assert_eq!(summary.merged, prs(&[1, 2]));
    assert!(lab.state().blocked.is_none());
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn when_main_moves_during_verification_the_batch_is_rebuilt_and_verified_again() {
    let lab = Lab::new(
        "m='@ROOT@/moved'\nif [ ! -e \"$m\" ]; then touch \"$m\"; (git clone -q '@ORIGIN@' '@ROOT@/mover' && cd '@ROOT@/mover' && echo x > moved.txt && git add moved.txt && git commit -qm 'chore: other push' && git push -q origin HEAD:main) || fail 'mover failed'; fi",
    );
    lab.open_pr(1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.verifications, 2);
    assert_eq!(record.pushed.len(), 1);
    assert!(lab.origin_git(&["show", "main:moved.txt"]).starts_with('x'));
    assert!(lab.origin_git(&["show", "main:a.txt"]).starts_with('a'));
    assert_eq!(lab.verify_runs(), 2);
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_pr_waiting_for_approval_is_left_alone_until_it_is_approved() {
    let lab = Lab::new("");
    lab.open_pr_for(1, &[("a.txt", "a\n")], Some(lab.agent()), Approval::Human);
    assert_eq!(lab.item_state(1), Some(ItemState::AwaitingApproval));

    assert_eq!(lab.engine().run_once().unwrap(), Pass::Idle);
    assert_eq!(lab.item_state(1), Some(ItemState::AwaitingApproval));
    assert_eq!(lab.verify_runs(), 0);

    lab.store
        .update(|s| Ok(queue::approve(s, PrNumber(1), None)?))
        .unwrap();
    let summary = done(lab.engine().run_once().unwrap());
    assert_eq!(summary.merged, prs(&[1]));
}

#[test]
fn more_waiting_prs_than_max_batch_take_several_batches() {
    let lab = Lab::build("", "max_batch = 2", 60);
    for n in 1..=3 {
        lab.open_pr(n, &[(&format!("f{n}"), "x")]);
    }

    assert_eq!(done(lab.engine().run_once().unwrap()).merged, prs(&[1, 2]));
    assert_eq!(done(lab.engine().run_once().unwrap()).merged, prs(&[3]));
    assert_eq!(lab.engine().run_once().unwrap(), Pass::Idle);
    assert_eq!(lab.batch_ids().len(), 2);
}

#[test]
fn a_failed_tree_ref_lives_while_the_pr_is_in_the_queue_and_goes_when_it_leaves() {
    let lab = Lab::new(BAD);
    lab.open_pr(1, &[("BAD", "")]);
    done(lab.engine().run_once().unwrap());
    let refs = lab
        .repo
        .git(&["for-each-ref", "--format=%(refname)", "refs/chute/failed/"]);
    assert!(refs.contains("refs/chute/failed/pr1-"), "{refs}");

    let head = lab.repo.head();
    lab.repo
        .git(&["update-ref", "refs/chute/failed/preflight-1-abc", &head]);
    lab.repo
        .git(&["update-ref", "refs/chute/failed/commit-abc", &head]);
    lab.store
        .update(|s| Ok(queue::drop_item(s, PrNumber(1))?))
        .unwrap();
    lab.engine().recover().unwrap();

    let refs = lab
        .repo
        .git(&["for-each-ref", "--format=%(refname)", "refs/chute/failed/"]);
    assert_eq!(
        refs.lines().collect::<Vec<_>>(),
        [
            "refs/chute/failed/commit-abc",
            "refs/chute/failed/preflight-1-abc"
        ],
        "preflight와 커밋 참조는 engine이 지우지 않는다"
    );
}

#[test]
fn recovery_drops_an_item_an_older_chute_left_on_a_humans_turn_and_tells_no_one() {
    let lab = Lab::new("");
    lab.open_pr(2, &[("b.txt", "b\n")]);
    let old = serde_json::json!({
        "pr": 1, "title": "PR 1", "branch": "feat/1", "head": "abc1234",
        "state": "needs_human", "state_reason": "budget", "agent": null, "attempts": 3,
        "submitted_ms": 1, "touches_config": false, "last_failure": null,
        "submitter": {"terminal": "t1orch", "created_ms": 1, "name": null,
                      "workdir": "/w", "alive": true}
    });
    let path = lab.store.dir().join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    state["queue"].as_array_mut().unwrap().push(old);
    fs::write(&path, state.to_string()).unwrap();

    lab.engine().recover().unwrap();
    lab.engine().recover().unwrap();

    assert_eq!(lab.item_state(1), None);
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
    let upgrades: Vec<PrNumber> = lab
        .events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Removed {
                pr,
                reason: RemovedReason::Upgrade,
                ..
            } => Some(pr),
            _ => None,
        })
        .collect();
    assert_eq!(upgrades, prs(&[1]));
    assert!(lab.hc_calls("send").is_empty());
}

#[test]
fn a_daemon_that_died_mid_batch_leaves_items_that_are_processed_again() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.strand_in_batch();
    assert_eq!(lab.item_state(1), Some(ItemState::InBatch));

    lab.engine().recover().unwrap();

    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert!(lab.state().batch.is_none());
    let summary = done(lab.engine().run_once().unwrap());
    assert_eq!(summary.merged, prs(&[1]));
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_daemon_that_died_right_after_the_push_finds_the_pr_merged() {
    let lab = Lab::new("");
    let head = lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.strand_in_batch();
    lab.repo.git(&["fetch", "--quiet", "origin"]);
    lab.repo
        .git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
    lab.repo.git(&[
        "merge",
        "--no-ff",
        "--quiet",
        "-m",
        "Merge pull request #1 from acme/pr-1\n\nfeat: change 1",
        head.as_str(),
    ]);
    lab.repo.git(&["push", "--quiet", "origin", "main"]);
    let merge_commit = lab.origin_main();
    lab.store
        .write_batch(&chute::model::BatchRecord {
            id: chute::model::BatchId("b-crashed".into()),
            base: engine_kit::sha(&lab.origin_git(&["rev-parse", "main~1"])),
            prs: prs(&[1]),
            started_ms: chute::model::EpochMs::now(),
            finished_ms: None,
            merges: Vec::new(),
            bounced: Vec::new(),
            flaky: Vec::new(),
            verify_s: 0,
            pushed: Vec::new(),
            verifications: 0,
            skipped: Vec::new(),
            reused: Vec::new(),
            reruns: Vec::new(),
            step_times: Vec::new(),
            ran: Vec::new(),
            env: None,
        })
        .unwrap();

    lab.engine().recover().unwrap();

    let record = lab
        .store
        .read_batch(&chute::model::BatchId("b-crashed".into()))
        .unwrap()
        .unwrap();
    assert!(record.finished_ms.is_some());
    assert_eq!(record.merges.len(), 1);
    assert_eq!(record.merges[0].pr, PrNumber(1));
    assert_eq!(record.merges[0].commit.as_str(), merge_commit);
    assert!(lab.events().iter().any(|e| matches!(
        e,
        EventKind::BatchFinished { merged, .. } if *merged == prs(&[1])
    )));

    assert!(lab.state().queue.is_empty());
    assert!(lab.state().batch.is_none());
    assert_eq!(lab.verify_runs(), 0);
    let merged: Vec<_> = lab
        .events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Merged { pr, commit, .. } => Some((pr.0, commit.to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(merged, [(1, merge_commit)]);
    lab.engine().recover().unwrap();
    assert_eq!(lab.merged_prs(), [1], "다시 정리해도 같은 결과다");
}

#[test]
fn recovery_ends_verification_processes_a_killed_daemon_left_in_the_batch_slot() {
    let lab = Lab::new("");
    let slot = lab.sb.chute_home().join("slots/acme__widget/batch");
    fs::create_dir_all(&slot).unwrap();
    let spawn = |run_id: bool| {
        let mut cmd = Command::new("sleep");
        cmd.arg("300")
            .current_dir(&slot)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if run_id {
            cmd.env("CHUTE_RUN_ID", "orphan");
        } else {
            cmd.env_remove("CHUTE_RUN_ID");
        }
        cmd.spawn().unwrap()
    };
    let mut orphan = spawn(true);
    let mut bystander = spawn(false);

    lab.engine().recover().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = orphan.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "고아 프로세스가 끝나지 않았다");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success());
    assert!(
        bystander.try_wait().unwrap().is_none(),
        "run id가 없는 프로세스는 건드리지 않는다"
    );
    bystander.kill().unwrap();
    bystander.wait().unwrap();
}

#[test]
fn a_pushed_pr_that_github_does_not_show_as_merged_leaves_a_warning() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.gh_never_shows_merged(1);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2]));
    assert!(lab.state().queue.is_empty());
    let warnings = lab.state().warnings;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].kind, chute::model::WarningKind::NotMerged);
    assert_eq!(warnings[0].pr, Some(PrNumber(1)));
    assert_eq!(chute_status(&lab)["warnings"][0]["kind"], "not_merged");
}

#[test]
fn an_error_in_the_middle_of_a_batch_puts_every_pr_back_in_the_queue() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    lab.origin_git(&["update-ref", "-d", "refs/pull/2/head"]);

    let error = lab.engine().run_once().unwrap_err();

    assert!(format!("{error:#}").contains("PR"), "{error:#}");
    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
    assert!(lab.state().batch.is_none());
    assert_eq!(lab.verify_runs(), 0);
}

fn mover(script: &str) -> String {
    format!(
        "m='@ROOT@/moved'\nif [ ! -e \"$m\" ]; then touch \"$m\"; (git clone -q '@ORIGIN@' '@ROOT@/mover' && cd '@ROOT@/mover' && {script} && git push -q origin HEAD:main) || fail 'mover failed'; fi"
    )
}

#[test]
fn a_conflict_that_goes_away_while_the_push_is_retried_merges_the_pr_once() {
    let lab = Lab::new(&mover(
        "git rm -q shared.txt && git commit -qm 'chore: drop shared'",
    ));
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("shared.txt", "from pr\n")]);
    lab.main_commit(&[("shared.txt", "from main\n")], "feat: shared");

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2]));
    assert!(lab.state().queue.is_empty());
    assert_eq!(lab.merged_prs(), [1, 2]);
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_pr_that_conflicted_in_round_one_and_failed_in_round_two_is_bounced_for_failing_only() {
    let lab = Lab::new(&format!(
        "{}\n[ -e shared.txt ] && [ -e BAD ] && fail 'BAD'",
        mover("git rm -q shared.txt && git commit -qm 'chore: drop shared'")
    ));
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("shared.txt", "from pr\n"), ("BAD", "")]);
    lab.main_commit(&[("shared.txt", "from main\n")], "feat: shared");

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(summary.bounced, prs(&[2]));
    assert_eq!(lab.bounced_events(), [(2, BounceReason::Verify)]);
    let Failure::Verify(step) = lab.item(2).unwrap().last_failure.unwrap().failure else {
        panic!("검증 실패여야 한다");
    };
    assert_eq!(
        step.mode,
        VerifyMode::Solo,
        "시작 main에 얹을 수 없는 PR을 앞 PR의 조합 탓으로 돌리지 않는다"
    );
}

#[test]
fn a_pr_sent_back_for_its_head_is_not_sent_back_again_when_the_push_is_retried() {
    let lab = Lab::new(&mover(
        "echo x > moved.txt && git add moved.txt && git commit -qm 'chore: other push'",
    ));
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("b.txt", "b\n")]);
    let other = lab.origin_git(&["rev-parse", "refs/pull/1/head"]);
    lab.origin_git(&["update-ref", "refs/pull/2/head", &other]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(
        lab.item_state(2),
        Some(ItemState::Bounced(BounceReason::HeadChanged))
    );
    assert_eq!(lab.bounced_events(), [(2, BounceReason::HeadChanged)]);
    assert_eq!(lab.hc_calls("send").len(), 1);
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_main_that_a_pr_of_the_batch_repaired_does_not_stop_the_queue() {
    let lab = Lab::new("[ -e BASEBAD ] && fail 'base'\n[ -e BAD ] && fail 'bad'");
    lab.main_commit(&[("BASEBAD", "")], "feat: broken");
    lab.open_pr_removing(1, "BASEBAD");
    lab.open_pr(2, &[("BAD", "")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(
        lab.item_state(2),
        Some(ItemState::Bounced(BounceReason::Verify))
    );
    assert!(lab.state().blocked.is_none());
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_main_broken_by_someone_else_mid_batch_stops_the_queue_instead_of_blaming_a_pr() {
    let lab = Lab::new("[ -e EXTBAD ] && fail 'ext'\n[ -e BAD ] && fail 'bad'");
    lab.open_pr(1, &[("f1", "x")]);
    lab.open_pr(2, &[("BAD", "")]);
    lab.open_pr(3, &[("f3", "x")]);
    let root = lab.sb.root().to_string_lossy().into_owned();
    let origin = lab.repo.origin().to_string_lossy().into_owned();
    let hook = format!(
        "#!/bin/sh\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_QUARANTINE_PATH GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES\nexport GIT_CONFIG_GLOBAL='{root}/gitconfig' GIT_CONFIG_NOSYSTEM=1\nm='{root}/extbad'\nwhile read old new ref; do\n  [ \"$ref\" = refs/heads/main ] || continue\n  [ -e \"$m\" ] && continue\n  git --git-dir='{origin}' cat-file -e \"$new:f1\" 2>/dev/null || continue\n  touch \"$m\"\n  (git clone -q '{origin}' '{root}/ext' && cd '{root}/ext' && echo x > EXTBAD && git add EXTBAD && git commit -qm 'chore: direct push' && git push -q origin HEAD:main) >'{root}/hook.log' 2>&1\ndone\n"
    );
    let path = lab.repo.origin().join("hooks/post-receive");
    fs::write(&path, hook).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);

    assert!(
        lab.origin_git(&["ls-tree", "--name-only", "main"])
            .contains("EXTBAD")
    );
    assert_eq!(lab.item_state(3), Some(ItemState::Queued(None)));
    assert_eq!(lab.bounced_events(), [(2, BounceReason::Verify)]);
    assert_eq!(lab.state().blocked.unwrap().reason, BlockReason::BaseBroken);
}

#[test]
fn the_time_verification_took_is_recorded() {
    let lab = Lab::new("sleep 1.2");
    lab.open_pr(1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(record.verify_s >= 1, "{}", record.verify_s);
    assert!(chute_status(&lab)["today"]["verify_s"].as_u64().unwrap() >= 1);
}

#[test]
fn status_shows_the_bisecting_phase_while_the_batch_is_split() {
    let lab = Lab::new(
        "@STATUS@\ncp '@ROOT@/status-during.json' \"@ROOT@/st-$$\"\n[ -e BAD ] && fail \"FAILED bad_test\"",
    );
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("BAD", "")]);

    done(lab.engine().run_once().unwrap());

    let phases: Vec<String> = fs::read_dir(lab.sb.root())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("st-"))
        .map(|e| {
            let json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(e.path()).unwrap()).unwrap();
            json["batch"]["phase"].as_str().unwrap().to_owned()
        })
        .collect();
    assert!(phases.contains(&"verifying".to_owned()), "{phases:?}");
    assert!(phases.contains(&"bisecting".to_owned()), "{phases:?}");
}

#[test]
fn the_failed_ref_of_a_dropped_pr_is_pruned_when_the_next_batch_ends() {
    let lab = Lab::new(BAD);
    lab.open_pr(1, &[("BAD", "")]);
    done(lab.engine().run_once().unwrap());
    lab.store
        .update(|s| Ok(queue::drop_item(s, PrNumber(1))?))
        .unwrap();
    lab.open_pr(2, &[("a.txt", "a\n")]);

    done(lab.engine().run_once().unwrap());

    let refs = lab.repo.git(&["for-each-ref", "refs/chute/failed/"]);
    assert!(refs.trim().is_empty(), "{refs}");
}

#[test]
fn recovery_keeps_the_bounces_of_a_batch_and_reports_what_it_had_already_merged() {
    let lab = Lab::new("");
    lab.strand_in_batch();
    let landed = engine_kit::sha(&lab.origin_main());
    lab.store
        .write_batch(&chute::model::BatchRecord {
            id: chute::model::BatchId("b-crashed".into()),
            base: landed.clone(),
            prs: prs(&[1, 2, 3]),
            started_ms: chute::model::EpochMs::now(),
            finished_ms: None,
            merges: vec![chute::model::MergeRef {
                pr: PrNumber(2),
                commit: landed.clone(),
                submitter: None,
                agent: None,
            }],
            bounced: prs(&[1]),
            flaky: Vec::new(),
            verify_s: 7,
            pushed: vec![landed],
            verifications: 3,
            skipped: Vec::new(),
            reused: Vec::new(),
            reruns: Vec::new(),
            step_times: Vec::new(),
            ran: Vec::new(),
            env: None,
        })
        .unwrap();

    lab.engine().recover().unwrap();

    let record = lab
        .store
        .read_batch(&chute::model::BatchId("b-crashed".into()))
        .unwrap()
        .unwrap();
    assert!(record.finished_ms.is_some());
    assert_eq!(record.bounced, prs(&[1]));
    assert_eq!(record.merges.len(), 1);
    let finished: Vec<_> = lab
        .events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::BatchFinished {
                merged,
                bounced,
                verify_s,
                ..
            } => Some((merged, bounced, verify_s)),
            _ => None,
        })
        .collect();
    assert_eq!(finished, [(prs(&[2]), prs(&[1]), 7)]);
}

#[test]
fn a_recovery_cut_short_finishes_the_record_and_the_events_when_it_runs_again() {
    let lab = Lab::new("");
    let head = lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.strand_in_batch();
    lab.repo.git(&["fetch", "--quiet", "origin"]);
    lab.repo
        .git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
    lab.repo.git(&[
        "merge",
        "--no-ff",
        "--quiet",
        "-m",
        "Merge pull request #1 from acme/pr-1\n\nfeat: change 1",
        head.as_str(),
    ]);
    lab.repo.git(&["push", "--quiet", "origin", "main"]);
    let landed = engine_kit::sha(&lab.origin_main());
    lab.store
        .write_batch(&chute::model::BatchRecord {
            id: chute::model::BatchId("b-crashed".into()),
            base: landed.clone(),
            prs: prs(&[1]),
            started_ms: chute::model::EpochMs::now(),
            finished_ms: None,
            merges: Vec::new(),
            bounced: Vec::new(),
            flaky: Vec::new(),
            verify_s: 0,
            pushed: Vec::new(),
            verifications: 0,
            skipped: Vec::new(),
            reused: Vec::new(),
            reruns: Vec::new(),
            step_times: Vec::new(),
            ran: Vec::new(),
            env: None,
        })
        .unwrap();
    let history = lab.store.dir().join("history.jsonl");
    fs::create_dir_all(&history).unwrap();

    assert!(lab.engine().recover().is_err());
    fs::remove_dir(&history).unwrap();
    lab.engine().recover().unwrap();
    lab.engine().recover().unwrap();

    let record = lab
        .store
        .read_batch(&chute::model::BatchId("b-crashed".into()))
        .unwrap()
        .unwrap();
    assert!(record.finished_ms.is_some());
    assert_eq!(record.merges.len(), 1);
    assert_eq!(lab.merged_prs(), [1]);
    assert!(lab.state().queue.is_empty());
    assert!(lab.state().batch.is_none());
}

fn batch_record(
    id: &str,
    merges: Vec<chute::model::MergeRef>,
    bounced: &[u64],
) -> chute::model::BatchRecord {
    chute::model::BatchRecord {
        id: chute::model::BatchId(id.into()),
        base: engine_kit::sha("abc1234"),
        prs: prs(&[1, 2, 3]),
        started_ms: chute::model::EpochMs::now(),
        finished_ms: None,
        merges,
        bounced: prs(bounced),
        flaky: Vec::new(),
        verify_s: 0,
        pushed: Vec::new(),
        verifications: 0,
        skipped: Vec::new(),
        reused: Vec::new(),
        reruns: Vec::new(),
        step_times: Vec::new(),
        ran: Vec::new(),
        env: None,
    }
}

fn batch_finished(lab: &Lab) -> Vec<(Vec<PrNumber>, Vec<PrNumber>)> {
    lab.events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::BatchFinished {
                merged, bounced, ..
            } => Some((merged, bounced)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_batch_record_that_cannot_be_read_does_not_keep_the_queue_from_recovering() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.strand_in_batch();
    let dir = lab
        .store
        .batch_dir(&chute::model::BatchId("b-crashed".into()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("batch.json"), "{\"id\":").unwrap();

    lab.engine().recover().unwrap();

    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert!(lab.state().batch.is_none());
    assert_eq!(done(lab.engine().run_once().unwrap()).merged, prs(&[1]));
}

#[test]
fn recovery_cut_before_the_batch_finished_event_writes_it_once_after_the_rerun() {
    let lab = Lab::new("");
    lab.strand_in_batch();
    let landed = engine_kit::sha(&lab.origin_main());
    lab.store
        .write_batch(&batch_record(
            "b-crashed",
            vec![chute::model::MergeRef {
                pr: PrNumber(2),
                commit: landed,
                submitter: None,
                agent: None,
            }],
            &[1],
        ))
        .unwrap();
    let history = lab.store.dir().join("history.jsonl");
    fs::write(&history, "").unwrap();
    fs::set_permissions(
        &history,
        std::os::unix::fs::PermissionsExt::from_mode(0o444),
    )
    .unwrap();

    assert!(lab.engine().recover().is_err());
    fs::set_permissions(
        &history,
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    lab.engine().recover().unwrap();
    lab.engine().recover().unwrap();

    assert_eq!(batch_finished(&lab), [(prs(&[2]), prs(&[1]))]);
    let record = lab
        .store
        .read_batch(&chute::model::BatchId("b-crashed".into()))
        .unwrap()
        .unwrap();
    assert!(record.finished_ms.is_some());
}

#[test]
fn a_finished_event_already_written_is_not_written_again() {
    let lab = Lab::new("");
    lab.strand_in_batch();
    lab.store
        .write_batch(&batch_record("b-crashed", Vec::new(), &[1]))
        .unwrap();
    lab.engine().recover().unwrap();
    lab.store
        .update(|s| {
            s.batch = Some(chute::model::Batch {
                id: chute::model::BatchId("b-crashed".into()),
                base: engine_kit::sha("abc1234"),
                prs: Vec::new(),
                phase: chute::model::Phase::Verifying,
                step: None,
                steps: Vec::new(),
                round: 1,
                started_ms: chute::model::EpochMs::now(),
                waiting_machine: false,
                reused: None,
                step_started_ms: None,
                done: Vec::new(),
                skipped: Vec::new(),
                reruns: Vec::new(),
            });
            Ok(())
        })
        .unwrap();
    let mut record = lab
        .store
        .read_batch(&chute::model::BatchId("b-crashed".into()))
        .unwrap()
        .unwrap();
    record.finished_ms = None;
    lab.store.write_batch(&record).unwrap();

    lab.engine().recover().unwrap();

    assert_eq!(batch_finished(&lab).len(), 1);
}

#[test]
fn a_revert_merge_that_quotes_the_title_is_not_taken_for_the_pr_merge_commit() {
    let lab = Lab::new("");
    let head = lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.strand_in_batch();
    lab.repo.git(&["fetch", "--quiet", "origin"]);
    lab.repo
        .git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
    lab.repo.git(&[
        "merge",
        "--no-ff",
        "--quiet",
        "-m",
        "Merge pull request #1 from acme/pr-1\n\nfeat: change 1",
        head.as_str(),
    ]);
    let real = lab.repo.head();
    lab.repo
        .git(&["checkout", "--quiet", "-B", "revert-1", "main~1"]);
    lab.repo.write("r.txt", "r\n");
    lab.repo.git(&["add", "r.txt"]);
    lab.repo
        .git(&["commit", "--quiet", "-m", "Revert change 1"]);
    lab.repo.git(&["checkout", "--quiet", "main"]);
    lab.repo.git(&[
        "merge",
        "--no-ff",
        "--quiet",
        "-m",
        "Merge pull request #2 from acme/revert-1\n\nRevert \"Merge pull request #1 from acme/pr-1\"",
        "revert-1",
    ]);
    lab.repo.git(&["push", "--quiet", "origin", "main"]);

    lab.engine().recover().unwrap();

    let commits: Vec<String> = lab
        .events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Merged { commit, .. } => Some(commit.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(commits, [real]);
}

#[test]
fn a_run_cut_short_inside_the_landing_still_ends_with_one_merged_event() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    let history = lab.store.dir().join("history.jsonl");
    fs::create_dir_all(&history).unwrap();

    assert!(lab.engine().run_once().is_err());
    fs::remove_dir(&history).unwrap();
    lab.engine().recover().unwrap();
    done(lab.engine().run_once().unwrap());

    assert_eq!(lab.merged_prs(), [1]);
    assert!(lab.state().queue.is_empty());
    assert_eq!(lab.merge_commits_on_main().len(), 1);
}

#[test]
fn a_bounce_is_in_the_batch_record_before_the_next_verification_runs() {
    let lab = Lab::new(&format!(
        "cp \"@ROOT@\"/chute-home/repos/acme__widget/batches/*/batch.json \"@ROOT@/copy-$$\" 2>/dev/null\n{BAD}"
    ));
    lab.open_pr(1, &[("BAD", "")]);
    lab.open_pr(2, &[("a.txt", "a\n")]);

    done(lab.engine().run_once().unwrap());

    let during: Vec<chute::model::BatchRecord> = fs::read_dir(lab.sb.root())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("copy-"))
        .map(|e| serde_json::from_str(&fs::read_to_string(e.path()).unwrap()).unwrap())
        .collect();
    assert!(
        during.iter().any(|r| r.bounced == prs(&[1])),
        "{:?}",
        during.iter().map(|r| &r.bounced).collect::<Vec<_>>()
    );
}

#[test]
fn a_pr_whose_merge_was_already_recorded_is_not_recorded_again_when_it_is_processed_again() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    let item = lab.item(1).unwrap();
    done(lab.engine().run_once().unwrap());
    assert_eq!(lab.merged_prs(), [1]);

    lab.store
        .update(|s| {
            s.queue.push(item);
            Ok(())
        })
        .unwrap();
    done(lab.engine().run_once().unwrap());

    assert_eq!(lab.merged_prs(), [1]);
    assert!(lab.state().queue.is_empty());
    assert_eq!(chute_status(&lab)["today"]["merged"], 1);
}

#[test]
fn recovery_writes_the_finished_event_a_batch_with_a_finished_record_never_got() {
    let lab = Lab::new("");
    lab.strand_in_batch();
    let landed = engine_kit::sha(&lab.origin_main());
    let mut record = batch_record(
        "b-crashed",
        vec![chute::model::MergeRef {
            pr: PrNumber(2),
            commit: landed,
            submitter: None,
            agent: None,
        }],
        &[1],
    );
    record.finished_ms = Some(chute::model::EpochMs::now());
    lab.store.write_batch(&record).unwrap();

    lab.engine().recover().unwrap();
    lab.engine().recover().unwrap();

    assert_eq!(batch_finished(&lab), [(prs(&[2]), prs(&[1]))]);
}

#[test]
fn recovery_tells_history_about_a_bounce_the_record_has_and_the_history_lacks() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.strand_in_batch();
    lab.store
        .update(|s| {
            s.queue[0].state = ItemState::Bounced(BounceReason::Verify);
            s.queue[0].attempts = 1;
            Ok(())
        })
        .unwrap();
    lab.store
        .write_batch(&batch_record("b-crashed", Vec::new(), &[1]))
        .unwrap();

    lab.engine().recover().unwrap();
    lab.engine().recover().unwrap();

    assert_eq!(lab.bounced_events(), [(1, BounceReason::Verify)]);
    assert_eq!(chute_status(&lab)["today"]["bounced"], 1);
}

mod env {
    use super::*;
    use chute::model::{EnvCheck, EnvRecord, EpochMs, PreflightRecord, PreflightResult};

    fn preflight_env(version: &str) -> EnvRecord {
        EnvRecord {
            machine_id: None,
            host: "laptop".into(),
            checks: vec![EnvCheck {
                run: "echo hub-tool".into(),
                output: version.into(),
                ok: true,
            }],
        }
    }

    fn preflight_of(head: &chute::model::Sha, env: EnvRecord) -> PreflightRecord {
        PreflightRecord {
            head: head.clone(),
            result: PreflightResult::Pass,
            report: None,
            at_ms: EpochMs(1),
            base: None,
            with: Vec::new(),
            env: Some(env),
        }
    }

    #[test]
    fn a_batch_records_its_env_once_before_the_first_verification() {
        let marks = tempfile::tempdir().unwrap();
        let marker = marks.path().join("env-count");
        let first = format!("echo run >> {}; echo hub-tool 1", marker.display());
        let lab = Lab::build("", &format!("env_check = [{first:?}, \"exit 3\"]"), 60);
        lab.open_pr(1, &[("f1", "x")]);
        lab.open_pr(2, &[("f2", "x")]);

        let summary = done(lab.engine().run_once().unwrap());

        assert_eq!(summary.merged, prs(&[1, 2]));
        let env = lab
            .store
            .read_batch(&summary.batch)
            .unwrap()
            .unwrap()
            .env
            .expect("batch 기록에 환경이 있다");
        assert_eq!(
            env.machine_id.as_ref().map(|id| id.as_str()),
            Some(support::SANDBOX_MACHINE_ID)
        );
        let outputs: Vec<(&str, &str, bool)> = env
            .checks
            .iter()
            .map(|c| (c.run.as_str(), c.output.as_str(), c.ok))
            .collect();
        assert_eq!(
            outputs,
            [(first.as_str(), "hub-tool 1", true), ("exit 3", "", false)]
        );
        assert_eq!(lab.verify_runs(), 1, "실패한 환경 명령이 검증을 막았다");
        assert_eq!(fs::read_to_string(&marker).unwrap().lines().count(), 1);
    }

    #[test]
    fn a_bounce_report_shows_the_hub_env_beside_the_preflight_of_the_same_head() {
        let lab = Lab::build(BAD, "env_check = [\"echo hub-tool 2\"]", 60);
        lab.open_pr(1, &[("f1", "x")]);
        let head = lab.open_pr(2, &[("f2", "x"), ("BAD", "")]);
        lab.store
            .update(|s| {
                s.preflights.insert(
                    "pr-2".into(),
                    preflight_of(&head, preflight_env("hub-tool 1")),
                );
                Ok(())
            })
            .unwrap();

        done(lab.engine().run_once().unwrap());

        let failure = lab.item(2).unwrap().last_failure.unwrap();
        let report = fs::read_to_string(&failure.report).unwrap();
        assert!(report.contains("## 검사 환경"), "{report}");
        assert!(
            report.contains("`echo hub-tool 2`: `hub-tool 2`"),
            "{report}"
        );
        assert!(
            report.contains("| 항목 | preflight를 돌린 PC | 허브 |"),
            "{report}"
        );
        assert!(report.contains("`laptop`"), "{report}");
        let json = fs::read_to_string(failure.report.with_extension("json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(doc["env"]["preflight"]["host"], "laptop");
        assert_eq!(doc["env"]["env"]["checks"][0]["output"], "hub-tool 2");
    }

    #[test]
    fn a_bounce_report_says_the_environment_is_the_same_when_the_preflight_matches() {
        let lab = Lab::build(BAD, "env_check = [\"echo hub-tool 1\"]", 60);
        let head = lab.open_pr(1, &[("f1", "x"), ("BAD", "")]);
        let mut same = preflight_env("hub-tool 1");
        same.checks[0].run = "echo hub-tool 1".into();
        let machine = chute::env::record(
            &chute::ext::Ext::from_vars(|k| lab.sb.var(k)),
            lab.repo.path(),
            &lab.sb.root().join("env-logs"),
            &[],
            &chute::verify::Cancel::new(),
        );
        same.host = machine.host.clone();
        same.machine_id = machine.machine_id.clone();
        lab.store
            .update(|s| {
                s.preflights
                    .insert("pr-1".into(), preflight_of(&head, same));
                Ok(())
            })
            .unwrap();

        done(lab.engine().run_once().unwrap());

        let failure = lab.item(1).unwrap().last_failure.unwrap();
        let report = fs::read_to_string(&failure.report).unwrap();
        assert!(report.contains("preflight와 같은 환경이다."), "{report}");
    }

    #[test]
    fn a_preflight_of_another_head_is_not_compared() {
        let lab = Lab::build(BAD, "env_check = [\"echo hub-tool 2\"]", 60);
        lab.open_pr(1, &[("f1", "x"), ("BAD", "")]);
        let other = chute::model::Sha::try_from("a".repeat(40)).unwrap();
        lab.store
            .update(|s| {
                s.preflights
                    .insert("pr-1".into(), preflight_of(&other, preflight_env("old")));
                Ok(())
            })
            .unwrap();

        done(lab.engine().run_once().unwrap());

        let failure = lab.item(1).unwrap().last_failure.unwrap();
        let report = fs::read_to_string(&failure.report).unwrap();
        assert!(report.contains("## 검사 환경"), "{report}");
        assert!(!report.contains("preflight를 돌린 PC"), "{report}");
        assert!(!report.contains("같은 환경"), "{report}");
    }
}

mod env_ci {
    use super::*;

    #[test]
    fn the_batch_env_is_recorded_in_a_slot_set_to_the_base() {
        let lab = Lab::build("", "env_check = [\"git rev-parse HEAD\"]", 60);
        lab.open_pr(1, &[("f1", "x")]);
        done(lab.engine().run_once().unwrap());
        lab.main_commit(&[("moved.txt", "x\n")], "feat: main moved");
        lab.open_pr(2, &[("f2", "x")]);
        let base = lab.origin_main();

        let summary = done(lab.engine().run_once().unwrap());

        let env = lab
            .store
            .read_batch(&summary.batch)
            .unwrap()
            .unwrap()
            .env
            .unwrap();
        assert_eq!(env.checks[0].output, base.trim());
    }

    #[test]
    fn a_ci_failure_report_carries_the_batch_env_too() {
        let lab = Lab::build(
            "",
            "require_ci = true\nenv_check = [\"echo hub-tool 1\"]",
            60,
        );
        lab.open_pr(1, &[("f1", "x")]);
        lab.gh_checks(1, &[("build", "fail")]);

        done(lab.engine().run_once().unwrap());

        let failure = lab.item(1).unwrap().last_failure.unwrap();
        let report = fs::read_to_string(&failure.report).unwrap();
        assert!(
            report.contains("`echo hub-tool 1`: `hub-tool 1`"),
            "{report}"
        );
    }
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

fn has_key(line: &str, pair: &str) -> bool {
    words(line).iter().any(|w| w == pair)
}

#[test]
fn notify_the_orchestrator_hears_a_merge_and_the_worker_does_not() {
    let lab = Lab::new("");
    let head = lab.push_pr_branch(1, &[("a.txt", "a\n")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    let heard = lab.lines_sent_to("t1orch");
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(
        heard[0].starts_with("chute 결과 merged slug=acme/widget at="),
        "{}",
        heard[0]
    );
    assert!(has_key(&heard[0], "pr=1"), "{}", heard[0]);
    assert!(has_key(
        &heard[0],
        &format!("head={}", &head.as_str()[..12])
    ));
    assert!(lab.lines_sent_to("t1rs").is_empty());
    assert_eq!(
        lab.notified(),
        [(
            chute::model::NotifyKind::Merged,
            chute::model::Delivered::Sent
        )]
    );
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.merges[0].submitter, Some(lab.orchestrator()));
    assert_eq!(record.merges[0].agent, Some(lab.agent()));
}

#[test]
fn notify_a_merge_without_a_submitter_is_only_recorded() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);

    done(lab.engine().run_once().unwrap());

    assert!(lab.hc_calls("send").is_empty());
    assert_eq!(
        lab.notified(),
        [(
            chute::model::NotifyKind::Merged,
            chute::model::Delivered::NoTerminal
        )]
    );
}

#[test]
fn notify_a_bounce_goes_to_the_worker_and_a_summary_to_the_orchestrator() {
    let lab = Lab::new(BAD);
    let head = lab.push_pr_branch(1, &[("BAD", "")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );

    done(lab.engine().run_once().unwrap());

    let worker = lab.lines_sent_to("t1rs");
    assert_eq!(worker.len(), 1, "{worker:?}");
    assert!(worker[0].starts_with("chute 반송:"), "{}", worker[0]);
    let orch = lab.lines_sent_to("t1orch");
    assert_eq!(orch.len(), 1, "{orch:?}");
    assert!(orch[0].starts_with("chute 결과 bounced slug=acme/widget at="));
    for pair in [
        "pr=1",
        "reason=verify",
        "mode=solo",
        "attempts=1",
        "budget=3",
        "to=t1rs",
        "delivered=sent",
    ] {
        assert!(has_key(&orch[0], pair), "{pair}: {}", orch[0]);
    }
    let report = lab.item(1).unwrap().last_failure.unwrap().report;
    assert!(
        has_key(&orch[0], &format!("report={}", report.display())),
        "{}",
        orch[0]
    );
}

#[test]
fn notify_a_worker_that_submitted_for_itself_gets_the_bounce_line_only() {
    let lab = Lab::new(BAD);
    let head = lab.push_pr_branch(1, &[("BAD", "")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(lab.agent()),
        Some(lab.agent()),
        Approval::Agent,
    );

    done(lab.engine().run_once().unwrap());

    assert_eq!(lab.hc_calls("send").len(), 1);
    assert!(lab.lines_sent_to("t1rs")[0].starts_with("chute 반송:"));
    assert_eq!(
        lab.notified(),
        [(
            chute::model::NotifyKind::Bounced,
            chute::model::Delivered::SameAsAgent
        )]
    );
}

#[test]
fn notify_a_spent_budget_is_a_removed_line_with_the_last_report_and_not_a_bounce_line() {
    let lab = Lab::new(BAD);
    let head = lab.push_pr_branch(1, &[("BAD", "")]);
    let submit = |lab: &Lab| {
        lab.queue_pr_by(
            1,
            &head,
            Some(lab.agent()),
            Some(lab.orchestrator()),
            Approval::Agent,
        )
    };
    for _ in 0..3 {
        submit(&lab);
        done(lab.engine().run_once().unwrap());
    }
    assert_eq!(lab.lines_sent_to("t1orch").len(), 3);

    submit(&lab);
    done(lab.engine().run_once().unwrap());

    assert_eq!(lab.item_state(1), None);
    let orch = lab.lines_sent_to("t1orch");
    assert_eq!(orch.len(), 4, "{orch:?}");
    assert!(orch[3].starts_with("chute 결과 removed slug=acme/widget at="));
    for pair in ["pr=1", "reason=budget", "attempts=3", "budget=3"] {
        assert!(has_key(&orch[3], pair), "{pair}: {}", orch[3]);
    }
    assert!(
        orch[3].contains(" head=") && orch[3].contains(" report="),
        "{}",
        orch[3]
    );
    assert_eq!(lab.lines_sent_to("t1rs").len(), 3);
    assert_eq!(
        lab.notified().last(),
        Some(&(
            chute::model::NotifyKind::Removed,
            chute::model::Delivered::Sent
        ))
    );
}

#[test]
fn notify_a_bounce_that_cannot_be_delivered_goes_to_the_orchestrator_as_an_unreachable_bounce() {
    let lab = Lab::new(BAD);
    let hc = lab.sb.fake("hc");
    hc.respond(
        "get t1gone --json",
        Reply::ok(r#"{"address":"t1gone","state":"finished","created_ms":1763188200000}"#),
    );
    let mut gone = lab.agent();
    gone.terminal = chute::model::TerminalId("t1gone".into());
    let head = lab.push_pr_branch(1, &[("BAD", "")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(gone),
        Some(lab.orchestrator()),
        Approval::Agent,
    );

    done(lab.engine().run_once().unwrap());

    assert_eq!(
        lab.item_state(1),
        Some(ItemState::Bounced(BounceReason::Verify))
    );
    let orch = lab.lines_sent_to("t1orch");
    assert_eq!(orch.len(), 1, "{orch:?}");
    assert!(orch[0].starts_with("chute 결과 bounced "), "{}", orch[0]);
    for pair in [
        "pr=1",
        "reason=verify",
        "delivered=unreachable",
        "to=t1orch",
    ] {
        assert!(has_key(&orch[0], pair), "{pair}: {}", orch[0]);
    }
    assert!(orch[0].contains(" report="), "{}", orch[0]);
    assert_eq!(
        lab.notified(),
        [(
            chute::model::NotifyKind::Bounced,
            chute::model::Delivered::Sent
        )]
    );

    let mut gone = lab.agent();
    gone.terminal = chute::model::TerminalId("t1gone".into());
    lab.queue_pr_by(
        1,
        &head,
        Some(gone),
        Some(lab.orchestrator()),
        Approval::Agent,
    );
    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert_eq!(lab.item(1).unwrap().attempts, 1);
}

#[test]
fn notify_an_unreachable_bounce_is_still_sent_when_the_worker_is_also_the_submitter() {
    let lab = Lab::new(BAD);
    lab.sb
        .fake("hc")
        .clear_rules()
        .respond("get t1rs --json", Reply::file("tests/fixtures/hc/get.json"))
        .respond("send t1rs *", Reply::ok("").code(9));
    let head = lab.push_pr_branch(1, &[("BAD", "")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(lab.agent()),
        Some(lab.agent()),
        Approval::Agent,
    );

    done(lab.engine().run_once().unwrap());

    assert_eq!(
        lab.item_state(1),
        Some(ItemState::Bounced(BounceReason::Verify))
    );
    let to_worker = lab.lines_sent_to("t1rs");
    assert_eq!(to_worker.len(), 2, "{to_worker:?}");
    assert!(to_worker[0].starts_with("chute 반송:"), "{}", to_worker[0]);
    assert!(
        to_worker[1].starts_with("chute 결과 bounced ")
            && has_key(&to_worker[1], "delivered=unreachable"),
        "{}",
        to_worker[1]
    );
    assert_eq!(
        lab.notified(),
        [(
            chute::model::NotifyKind::Bounced,
            chute::model::Delivered::Blocked
        )]
    );
}

#[test]
fn notify_a_halt_reaches_every_submitting_terminal_once() {
    let lab = Lab::new("[ -e BASEBAD ] && fail \"기준이 깨졌다\"");
    lab.main_commit(&[("BASEBAD", "")], "feat: broken");
    for n in 1..=3 {
        let head = lab.push_pr_branch(n, &[(&format!("f{n}.txt"), "x\n")]);
        let submitter = if n == 3 {
            lab.agent()
        } else {
            lab.orchestrator()
        };
        lab.queue_pr_by(
            n,
            &head,
            Some(lab.agent()),
            Some(submitter),
            Approval::Agent,
        );
    }
    let head = lab.push_pr_branch(4, &[("f4.txt", "x\n")]);
    lab.queue_pr(4, &head, None, Approval::Agent);

    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);
    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);

    let orch = lab.lines_sent_to("t1orch");
    assert_eq!(orch.len(), 1, "{orch:?}");
    assert!(orch[0].starts_with("chute 결과 halted slug=acme/widget at="));
    assert!(has_key(
        &orch[0],
        &format!("base={}", &lab.origin_main()[..12])
    ));
    assert!(has_key(&orch[0], "step=check"), "{}", orch[0]);
    assert!(has_key(&orch[0], "pushed_by_chute=no"), "{}", orch[0]);
    assert!(has_key(&orch[0], "rechecked=no"), "{}", orch[0]);
    assert!(orch[0].contains(" log="), "{}", orch[0]);
    assert!(!orch[0].contains("suspect="), "{}", orch[0]);
    assert_eq!(lab.lines_sent_to("t1rs").len(), 1);
    assert!(lab.bounced_events().is_empty());
}

fn hold_behind(lab: &Lab, pr: u64, after: u64) {
    lab.store
        .update(|s| {
            let item = s.queue.iter_mut().find(|i| i.pr == PrNumber(pr)).unwrap();
            item.after = vec![PrNumber(after)];
            Ok(())
        })
        .unwrap();
}

fn queue_bad_pr_and_a_waiter(lab: &Lab) {
    let head = lab.push_pr_branch(1, &[("BAD", "")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(lab.agent()),
        Some(lab.agent()),
        Approval::Agent,
    );
    let head = lab.push_pr_branch(2, &[("b.txt", "b\n")]);
    lab.queue_pr_by(
        2,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );
    hold_behind(lab, 2, 1);
}

fn waiting_lines(lab: &Lab) -> Vec<String> {
    lab.lines_sent_to("t1orch")
        .into_iter()
        .filter(|l| l.starts_with("chute 결과 waiting "))
        .collect()
}

#[test]
fn waiting_a_bounced_pr_tells_the_submitter_of_the_pr_behind_it_and_holds_that_pr() {
    let lab = Lab::new(BAD);
    queue_bad_pr_and_a_waiter(&lab);

    let summary = done(lab.engine().run_once().unwrap());

    assert!(summary.merged.is_empty());
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
    assert_eq!(lab.item(2).unwrap().after, prs(&[1]));
    let lines = waiting_lines(&lab);
    assert_eq!(lines.len(), 1, "{lines:?}");
    for pair in ["slug=acme/widget", "pr=2", "after=1", "why=bounced"] {
        assert!(has_key(&lines[0], pair), "{pair}: {}", lines[0]);
    }
    assert!(lab.notified().iter().any(|(kind, delivered)| {
        *kind == chute::model::NotifyKind::Waiting && *delivered == chute::model::Delivered::Sent
    }));
}

#[test]
fn waiting_a_pr_taken_out_by_a_spent_budget_says_removed() {
    let lab = Lab::new(BAD);
    queue_bad_pr_and_a_waiter(&lab);
    for _ in 0..4 {
        done(lab.engine().run_once().unwrap());
        let Some(item) = lab.item(1) else { break };
        let head = item.head;
        lab.queue_pr_by(
            1,
            &head,
            Some(lab.agent()),
            Some(lab.agent()),
            Approval::Agent,
        );
    }

    assert_eq!(lab.item_state(1), None);
    assert_eq!(lab.item(2).unwrap().after, prs(&[1]));
    let lines = waiting_lines(&lab);
    assert!(has_key(lines.last().unwrap(), "why=removed"), "{lines:?}");
    assert!(
        lines[..lines.len() - 1]
            .iter()
            .all(|l| has_key(l, "why=bounced"))
    );
    assert!(lab.merged_prs().is_empty());
}

#[test]
fn waiting_a_dropped_pr_tells_the_submitter_of_the_pr_behind_it() {
    let lab = Lab::new("");
    let head = lab.push_pr_branch(1, &[("a.txt", "a\n")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(lab.agent()),
        Some(lab.agent()),
        Approval::Agent,
    );
    let head = lab.push_pr_branch(2, &[("b.txt", "b\n")]);
    lab.queue_pr_by(
        2,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );
    hold_behind(&lab, 2, 1);

    let out = lab
        .sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .current_dir(lab.repo.path())
        .args(["drop", "1"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines = waiting_lines(&lab);
    assert_eq!(lines.len(), 1, "{lines:?}");
    for pair in ["pr=2", "after=1", "why=dropped"] {
        assert!(has_key(&lines[0], pair), "{pair}: {}", lines[0]);
    }
    assert!(
        lab.events()
            .contains(&EventKind::Dropped { pr: PrNumber(1) })
    );
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
    assert_eq!(lab.engine().run_once().unwrap(), Pass::Idle);
    assert!(lab.merged_prs().is_empty());
}

#[test]
fn waiting_a_pr_closed_on_github_tells_the_submitter_of_the_pr_behind_it() {
    let lab = Lab::new("");
    let head = lab.push_pr_branch(1, &[("a.txt", "a\n")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(lab.agent()),
        Some(lab.agent()),
        Approval::Agent,
    );
    let head = lab.push_pr_branch(2, &[("b.txt", "b\n")]);
    lab.queue_pr_by(
        2,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );
    hold_behind(&lab, 2, 1);
    lab.gh_says_closed(1);

    lab.engine().run_once().unwrap();

    let lines = waiting_lines(&lab);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(has_key(&lines[0], "why=closed"), "{}", lines[0]);
    assert!(lab.item(1).is_none());
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
}

#[test]
fn waiting_a_pr_behind_one_that_merges_joins_the_next_batch_and_hears_nothing() {
    let lab = Lab::new("");
    let head = lab.push_pr_branch(1, &[("a.txt", "a\n")]);
    lab.queue_pr_by(
        1,
        &head,
        Some(lab.agent()),
        Some(lab.agent()),
        Approval::Agent,
    );
    let head = lab.push_pr_branch(2, &[("b.txt", "b\n")]);
    lab.queue_pr_by(
        2,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );
    hold_behind(&lab, 2, 1);

    let first = done(lab.engine().run_once().unwrap());
    assert_eq!(first.merged, prs(&[1]));
    assert!(lab.item(2).unwrap().after.is_empty());
    let second = done(lab.engine().run_once().unwrap());

    assert_eq!(second.merged, prs(&[2]));
    assert!(waiting_lines(&lab).is_empty());
}

const FLAKE_ONCE: &str =
    "m='@ROOT@/flake-now'\nif [ -e \"$m\" ]; then rm \"$m\"; fail \"FAILED flaky_test\"; fi";

fn probe_lab(top: &str, probe: &str) -> Lab {
    Lab::build_with_step(FLAKE_ONCE, top, 60, probe)
}

fn probe_step(command: &str) -> String {
    format!("probe = \"{command}\"")
}

fn flake_next(lab: &Lab) {
    fs::write(lab.sb.root().join("flake-now"), "").unwrap();
}

fn probes(lab: &Lab) -> Vec<(String, Vec<String>, ProbeEvidence, u32, u32)> {
    lab.events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::FlakyProbe {
                step,
                tests,
                evidence,
                runs,
                failures,
                ..
            } => Some((step, tests, evidence, runs, failures)),
            _ => None,
        })
        .collect()
}

fn flaky_lines(lab: &Lab, terminal: &str) -> Vec<String> {
    lab.lines_sent_to(terminal)
        .into_iter()
        .filter(|l| l.starts_with("chute 결과 flaky "))
        .collect()
}

fn open_pr_by_orchestrator(lab: &Lab, n: u64, files: &[(&str, &str)]) {
    let head = lab.push_pr_branch(n, files);
    lab.queue_pr_by(
        n,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );
}

#[test]
fn a_probe_that_fails_only_on_the_flaked_tree_says_made_and_tells_the_submitter() {
    let lab = probe_lab(
        "[flaky]\nprobe_repeats = 3\n",
        &probe_step("echo {tests} >> '@ROOT@/probe-runs'; [ ! -e PROBE_FAILS ]"),
    );
    open_pr_by_orchestrator(&lab, 1, &[("PROBE_FAILS", "x\n")]);
    flake_next(&lab);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(
        probes(&lab),
        [(
            "check".to_owned(),
            vec!["flaky_test".to_owned()],
            ProbeEvidence::Made,
            4,
            1
        )]
    );
    let told = flaky_lines(&lab, "t1orch");
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(has_key(&told[0], "evidence=made"), "{}", told[0]);
    assert!(has_key(&told[0], "step=check"), "{}", told[0]);
    assert!(has_key(&told[0], "tests=flaky_test"), "{}", told[0]);
    assert!(!told[0].contains("chronic="), "{}", told[0]);
    assert!(flaky_lines(&lab, "t1rs").is_empty());
    let runs = fs::read_to_string(lab.sb.root().join("probe-runs")).unwrap();
    assert_eq!(runs.lines().collect::<Vec<_>>(), ["flaky_test"; 4]);
    assert!(lab.state().warnings.is_empty());
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_probe_that_fails_on_the_base_says_base_and_tells_nobody() {
    let lab = probe_lab("", &probe_step(": {tests}; [ ! -e PROBE_FAILS ]"));
    lab.main_commit(&[("PROBE_FAILS", "x\n")], "chore: broken on main");
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);
    flake_next(&lab);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(
        probes(&lab),
        [(
            "check".to_owned(),
            vec!["flaky_test".to_owned()],
            ProbeEvidence::Base,
            1,
            1
        )]
    );
    assert!(flaky_lines(&lab, "t1orch").is_empty());
    assert!(
        lab.hc_calls("send")
            .iter()
            .all(|c| !c[2].contains(" flaky "))
    );
}

#[test]
fn a_probe_that_never_fails_is_unresolved_after_all_its_repeats() {
    let lab = probe_lab("[flaky]\nprobe_repeats = 2\n", &probe_step(": {tests}"));
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);
    flake_next(&lab);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(
        probes(&lab),
        [(
            "check".to_owned(),
            vec!["flaky_test".to_owned()],
            ProbeEvidence::Unresolved,
            4,
            0
        )]
    );
    assert!(flaky_lines(&lab, "t1orch").is_empty());
}

#[test]
fn a_probe_running_past_probe_seconds_is_cut_and_unresolved() {
    let lab = probe_lab(
        "[flaky]\nprobe_seconds = 1\n",
        &probe_step(": {tests}; sleep 20"),
    );
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);
    flake_next(&lab);

    let started = Instant::now();
    let summary = done(lab.engine().run_once().unwrap());

    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(
        probes(&lab),
        [(
            "check".to_owned(),
            vec!["flaky_test".to_owned()],
            ProbeEvidence::Unresolved,
            0,
            0
        )]
    );
}

#[test]
fn a_cancel_during_a_probe_cuts_it_and_leaves_it_unresolved() {
    let lab = probe_lab(
        "",
        &probe_step(": {tests}; touch '@ROOT@/probing'; sleep 20"),
    );
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);
    flake_next(&lab);
    let cancel = chute::verify::Cancel::new();
    let watcher = {
        let (cancel, marker) = (cancel.clone(), lab.sb.root().join("probing"));
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !marker.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            cancel.cancel();
        })
    };

    let started = Instant::now();
    let summary = done(lab.engine().with_cancel(cancel).run_once().unwrap());
    watcher.join().unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(
        probes(&lab),
        [(
            "check".to_owned(),
            vec!["flaky_test".to_owned()],
            ProbeEvidence::Unresolved,
            0,
            0
        )]
    );
}

#[test]
fn a_step_without_a_probe_is_not_probed() {
    let lab = Lab::new(FLAKE_ONCE);
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);
    flake_next(&lab);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert!(probes(&lab).is_empty());
    assert!(flaky_lines(&lab, "t1orch").is_empty());
}

#[test]
fn a_flake_without_test_names_is_not_probed() {
    let lab = Lab::build_with_step(
        "m='@ROOT@/flake-now'\nif [ -e \"$m\" ]; then rm \"$m\"; fail \"boom\"; fi",
        "",
        60,
        &probe_step(": {tests}"),
    );
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);
    flake_next(&lab);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert!(probes(&lab).is_empty());
}

fn flake_one_batch(lab: &Lab, n: u64) {
    open_pr_by_orchestrator(lab, n, &[(&format!("f{n}.txt"), "x\n")]);
    flake_next(lab);
    assert_eq!(done(lab.engine().run_once().unwrap()).merged, prs(&[n]));
}

#[test]
fn a_chronic_test_raises_one_warning_and_one_notice_a_day() {
    let lab = Lab::build(FLAKE_ONCE, "[flaky]\nchronic = 3\n", 60);

    flake_one_batch(&lab, 1);
    flake_one_batch(&lab, 2);
    assert!(lab.state().warnings.is_empty());
    assert!(flaky_lines(&lab, "t1orch").is_empty());

    flake_one_batch(&lab, 3);
    let warnings = lab.state().warnings;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].kind, WarningKind::FlakyChronic);
    assert!(warnings[0].message.contains("flaky_test"), "{warnings:?}");
    let told = flaky_lines(&lab, "t1orch");
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(has_key(&told[0], "chronic=yes"), "{}", told[0]);
    assert!(!told[0].contains("evidence="), "{}", told[0]);
    assert!(has_key(&told[0], "prs=3"), "{}", told[0]);

    flake_one_batch(&lab, 4);
    assert_eq!(lab.state().warnings.len(), 1);
    assert_eq!(flaky_lines(&lab, "t1orch").len(), 1);
    assert!(lab.state().warnings[0].message.contains("4번"));

    lab.store
        .update(|s| {
            s.warnings[0].at_ms.0 -= 25 * 60 * 60 * 1000;
            Ok(())
        })
        .unwrap();
    flake_one_batch(&lab, 5);
    assert_eq!(lab.state().warnings.len(), 1);
    assert_eq!(flaky_lines(&lab, "t1orch").len(), 2);
}

#[test]
fn a_chronic_warning_goes_when_the_count_falls_under_the_threshold() {
    let lab = Lab::build(FLAKE_ONCE, "[flaky]\nchronic = 2\nchronic_days = 5\n", 60);
    flake_one_batch(&lab, 1);
    flake_one_batch(&lab, 2);
    assert_eq!(lab.state().warnings.len(), 1);
    let history = lab.store.history().unwrap().events;
    let later = EpochMs(EpochMs::now().0 + 10 * 24 * 60 * 60 * 1000);

    let due = chute::flaky::refresh_chronic(
        &lab.store,
        &history,
        later,
        &chute::config::FlakyPolicy {
            chronic: 2,
            chronic_days: 5,
            ..Default::default()
        },
        &[],
    )
    .unwrap();

    assert!(due.is_empty());
    assert!(lab.state().warnings.is_empty());
}

#[test]
fn a_flake_on_a_tip_that_was_never_pushed_is_not_probed() {
    let verify = format!(
        "m='@ROOT@/moved'\nif [ ! -e \"$m\" ]; then touch \"$m\"; (git clone -q '@ORIGIN@' '@ROOT@/mover' && cd '@ROOT@/mover' && echo x > moved.txt && git add moved.txt && git commit -qm 'chore: other push' && git push -q origin HEAD:main) || fail 'mover failed'; fi\n{FLAKE_ONCE}"
    );
    let lab = Lab::build_with_step(
        &verify,
        "",
        60,
        &probe_step("[ -e moved.txt ] || [ ! -e f1.txt ]; : {tests}"),
    );
    open_pr_by_orchestrator(&lab, 1, &[("f1.txt", "x\n")]);
    flake_next(&lab);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert!(
        lab.origin_git(&["ls-tree", "-r", "--name-only", "main"])
            .contains("moved.txt")
    );
    assert!(probes(&lab).is_empty(), "{:?}", probes(&lab));
    assert!(flaky_lines(&lab, "t1orch").is_empty());
}

#[test]
fn a_flake_found_by_verifying_again_is_probed_on_the_pushed_commit() {
    let verify = format!(
        "m='@ROOT@/moved'\nif [ ! -e \"$m\" ]; then touch \"$m\"; (git clone -q '@ORIGIN@' '@ROOT@/mover' && cd '@ROOT@/mover' && echo x > moved.txt && git add moved.txt && git commit -qm 'chore: other push' && git push -q origin HEAD:main) || fail 'mover failed'; fi\n{}",
        scripted_failures(&[&["test_a"], &["test_b"]])
    );
    let lab = Lab::build_with_step(
        &verify,
        "",
        60,
        &probe_step("git rev-parse HEAD >> '@ROOT@/probed-at'; : {tests}"),
    );
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.reruns.len(), 1);
    assert_eq!(probes(&lab).len(), 1, "{:?}", probes(&lab));
    assert_eq!(record.pushed.len(), 1);
    assert_eq!(record.flaky.len(), 1);
    let failed_tree = record.flaky[0].tree.clone();
    assert_ne!(failed_tree, record.pushed[0]);
    let probed_at = fs::read_to_string(lab.sb.root().join("probed-at")).unwrap();
    assert!(probed_at.contains(record.pushed[0].as_str()), "{probed_at}");
    assert!(!probed_at.contains(failed_tree.as_str()), "{probed_at}");
}

#[test]
fn thousands_of_flaked_tests_are_still_probed() {
    let body = "m='@ROOT@/flake-now'\nif [ -e \"$m\" ]; then rm \"$m\"; i=0; while [ $i -lt 5000 ]; do echo \"FAILED some_long_test_name_$i\"; i=$((i+1)); done; fail \"FAILED last\"; fi";
    let lab = Lab::build_with_step(body, "", 60, &probe_step(": {tests}"));
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);
    flake_next(&lab);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    let found = probes(&lab);
    assert_eq!(found.len(), 1);
    assert!(
        (1000..5001).contains(&found[0].1.len()),
        "the event names only the tests the probe ran with: {}",
        found[0].1.len()
    );
    assert_eq!(found[0].2, ProbeEvidence::Unresolved);
}

fn merge_by_hand(lab: &Lab, head: &chute::model::Sha) {
    lab.repo.git(&["fetch", "--quiet", "origin"]);
    lab.repo
        .git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
    lab.repo.git(&[
        "merge",
        "--no-ff",
        "--quiet",
        "-m",
        "by hand",
        head.as_str(),
    ]);
    lab.repo.git(&["push", "--quiet", "origin", "main"]);
}

#[test]
fn waiting_a_pr_behind_one_merged_on_github_outside_chute_is_released() {
    let lab = Lab::new("");
    let head1 = lab.push_pr_branch(1, &[("a.txt", "a\n")]);
    lab.queue_pr_by(1, &head1, Some(lab.agent()), None, Approval::Agent);
    let head2 = lab.push_pr_branch(2, &[("b.txt", "b\n")]);
    lab.queue_pr_by(2, &head2, Some(lab.agent()), None, Approval::Agent);
    hold_behind(&lab, 2, 1);
    lab.store
        .update(|s| Ok(queue::drop_item(s, PrNumber(1))?))
        .unwrap();

    assert_eq!(lab.engine().run_once().unwrap(), Pass::Idle);
    assert_eq!(lab.item(2).unwrap().after, prs(&[1]));

    merge_by_hand(&lab, &head1);
    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[2]));
    assert!(lab.item(2).is_none());
}

#[test]
fn waiting_a_pr_behind_one_whose_github_state_cannot_be_read_fails_the_pass_and_keeps_the_hold() {
    let lab = Lab::new("");
    let head2 = lab.push_pr_branch(2, &[("b.txt", "b\n")]);
    lab.queue_pr_by(2, &head2, Some(lab.agent()), None, Approval::Agent);
    lab.store
        .update(|s| {
            s.queue[0].after = prs(&[1]);
            Ok(())
        })
        .unwrap();
    lab.sb.script(
        "gh",
        "echo 'error connecting to api.github.com' >&2\nexit 1\n",
    );

    assert!(lab.engine().run_once().is_err());
    assert_eq!(lab.item(2).unwrap().after, prs(&[1]));
}

const BROKEN_BASE: &str =
    "[ -e BASEBAD ] && fail \"기준이 깨졌다\"\n[ -e BAD ] && fail \"FAILED bad_test\"";

fn queue_for_worker(lab: &Lab, n: u64) {
    let head = lab.push_pr_branch(n, &[(&format!("f{n}.txt"), "x\n")]);
    lab.queue_pr_by(
        n,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );
}

fn halted_lab() -> Lab {
    halted_lab_running(BROKEN_BASE)
}

fn halted_lab_running(verify_body: &str) -> Lab {
    let lab = Lab::new(verify_body);
    lab.main_commit(&[("BASEBAD", "")], "feat: broken");
    queue_for_worker(&lab, 1);
    let head = lab.push_pr_branch(2, &[("f2.txt", "x\n")]);
    lab.queue_pr_by(
        2,
        &head,
        Some(lab.agent()),
        Some(lab.agent()),
        Approval::Agent,
    );
    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);
    lab
}

#[test]
fn halt_the_state_read_when_the_halted_line_goes_out_is_blocked_with_the_items_queued() {
    let lab = Lab::new(BROKEN_BASE);
    lab.main_commit(&[("BASEBAD", "")], "feat: broken");
    queue_for_worker(&lab, 1);
    let head = lab.push_pr_branch(2, &[("f2.txt", "x\n")]);
    lab.queue_pr_by(
        2,
        &head,
        Some(lab.agent()),
        Some(lab.agent()),
        Approval::Agent,
    );
    let at_send = lab.sb.root().join("status-at-send.json");
    let hc = lab.sb.script(
        "hc",
        &format!(
            r#"case "$1 $2" in
  "get t1rs") cat '{rs}' ;;
  "get t1orch") echo '{{"address":"t1orch","name":"orch","state":"live","created_ms":{orch}}}' ;;
  "send "*) [ -e '{at_send}' ] || env CHUTE_HOME='{home}' HOME='{user}' CHUTE_GIT='{git}' '{chute}' status --json --repo '{repo}' > '{at_send}' ;;
  *) exit 97 ;;
esac
"#,
            rs = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hc/get.json"),
            orch = engine_kit::ORCH_CREATED_MS,
            at_send = at_send.display(),
            home = lab.sb.chute_home().display(),
            user = lab.sb.home().display(),
            git = lab.sb.root().join("bin/git").display(),
            chute = env!("CARGO_BIN_EXE_chute"),
            repo = lab.repo.path().display(),
        ),
    );
    assert!(hc.exists());

    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);

    let status: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&at_send).expect("알림이 나갔다")).unwrap();
    assert_eq!(status["blocked"]["reason"], "base_broken");
    let items = status["queue"].as_array().expect("queue 목록");
    assert_eq!(items.len(), 2, "{status}");
    for item in items {
        assert_eq!(item["state"], "queued", "{item}");
    }
    let members = status["batch"]["prs"].as_array().map_or(0, Vec::len);
    assert_eq!(members, 0, "{}", status["batch"]);
}

fn lines_with(lab: &Lab, terminal: &str, kind: &str) -> Vec<String> {
    lab.lines_sent_to(terminal)
        .into_iter()
        .filter(|l| l.starts_with(&format!("chute 결과 {kind} ")))
        .collect()
}

#[test]
fn repair_a_halt_names_the_step_the_log_and_whether_chute_pushed_the_commit() {
    let lab = halted_lab();

    let blocked = lab.state().blocked.expect("저장소가 멈춘다");
    assert_eq!(blocked.step.as_deref(), Some("check"));
    assert!(!blocked.pushed_by_chute);
    assert!(!blocked.rechecked);
    let log = fs::read_to_string(blocked.log.as_ref().expect("로그 경로가 있다")).unwrap();
    assert!(log.contains("기준이 깨졌다"), "{log}");
    let status = chute_status(&lab);
    assert_eq!(status["blocked"]["step"], "check");
    assert_eq!(status["blocked"]["pushed_by_chute"], false);
    assert_eq!(status["blocked"]["rechecked"], false);
    assert_eq!(
        status["blocked"]["log"].as_str().map(std::path::Path::new),
        blocked.log.as_deref()
    );
    let halted = lines_with(&lab, "t1orch", "halted");
    assert_eq!(halted.len(), 1, "{halted:?}");
    assert!(has_key(&halted[0], "step=check"), "{}", halted[0]);
    assert!(has_key(&halted[0], "pushed_by_chute=no"), "{}", halted[0]);
}

#[test]
fn repair_a_repair_pr_is_verified_alone_pushed_and_lifts_the_halt() {
    let lab = halted_lab();
    let head = lab.push_pr_branch_removing(3, "BASEBAD");
    lab.queue_repair_by(3, &head, Some(lab.agent()), Some(lab.orchestrator()));
    let runs = lab.verify_runs();

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[3]));
    assert_eq!(
        lab.verify_runs(),
        runs + 1,
        "복구 PR만 한 번 검증한다. 큐의 다른 PR과 묶지 않는다"
    );
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.prs, prs(&[3]));
    assert_eq!(record.merges.len(), 1);
    assert_eq!(lab.merged_prs(), [3]);
    assert!(lab.state().blocked.is_none());
    assert!(lab.state().verified_bases.contains(&record.pushed[0]));
    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert_eq!(lab.item_state(2), Some(ItemState::Queued(None)));
    lab.assert_every_pushed_commit_was_verified();

    let merged = lines_with(&lab, "t1orch", "merged");
    assert_eq!(merged.len(), 1, "{merged:?}");
    assert!(has_key(&merged[0], "pr=3"), "{}", merged[0]);
    assert!(has_key(&merged[0], "repair=yes"), "{}", merged[0]);
    for terminal in ["t1orch", "t1rs"] {
        let resumed = lines_with(&lab, terminal, "resumed");
        assert_eq!(resumed.len(), 1, "{terminal}: {resumed:?}");
        assert!(has_key(&resumed[0], "by=repair"), "{}", resumed[0]);
        assert!(
            has_key(
                &resumed[0],
                &format!("base={}", &record.pushed[0].as_str()[..12])
            ),
            "{}",
            resumed[0]
        );
    }
    assert!(has_key(&lines_with(&lab, "t1orch", "resumed")[0], "pr=3"));
    assert!(lab.events().iter().any(|e| matches!(
        e,
        EventKind::Resumed {
            by: chute::model::ResumedBy::Repair,
            pr: Some(PrNumber(3)),
            ..
        }
    )));

    let after = done(lab.engine().run_once().unwrap());
    assert_eq!(after.merged, prs(&[1, 2]));
    assert!(lab.state().queue.is_empty());
}

#[test]
fn repair_a_failing_repair_pr_is_bounced_and_the_queue_stays_halted() {
    let lab = halted_lab();
    let before = lab.state().blocked.unwrap();
    let head = lab.push_pr_branch(3, &[("still-broken.txt", "x\n")]);
    lab.queue_repair_by(3, &head, Some(lab.agent()), Some(lab.orchestrator()));

    let summary = done(lab.engine().run_once().unwrap());

    assert!(summary.merged.is_empty());
    assert_eq!(summary.bounced, prs(&[3]));
    let item = lab.item(3).unwrap();
    assert_eq!(item.state, ItemState::Bounced(BounceReason::Verify));
    assert_eq!(item.attempts, 1);
    assert!(item.repair);
    let report = fs::read_to_string(item.last_failure.unwrap().report).unwrap();
    assert!(report.contains("기준이 깨졌다"), "{report}");
    let to_worker = lab.lines_sent_to("t1rs");
    assert_eq!(to_worker.len(), 2, "멈춤 줄과 반송 줄: {to_worker:?}");
    assert!(
        to_worker[0].starts_with("chute 결과 halted "),
        "{to_worker:?}"
    );
    assert!(!to_worker[1].starts_with("chute 결과 "), "{to_worker:?}");
    assert_eq!(lab.state().blocked, Some(before), "멈춤은 그대로다");
    assert!(lab.merge_commits_on_main().is_empty());

    let runs = lab.verify_runs();
    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);
    assert_eq!(
        lab.verify_runs(),
        runs,
        "복구 항목이 없으면 검증하지 않는다"
    );

    let fixed = lab.push_pr_branch_removing(3, "BASEBAD");
    lab.queue_repair_by(3, &fixed, Some(lab.agent()), Some(lab.orchestrator()));
    let again = done(lab.engine().run_once().unwrap());
    assert_eq!(again.merged, prs(&[3]));
    assert!(lab.state().blocked.is_none());
}

#[test]
fn repair_a_halt_that_main_moved_off_says_main_moved_and_the_repair_item_goes_first() {
    let lab = halted_lab();
    let head = lab.push_pr_branch(3, &[("f3.txt", "x\n")]);
    lab.queue_repair_by(3, &head, Some(lab.agent()), Some(lab.orchestrator()));
    lab.remove_from_main("BASEBAD", "fix: 기준을 손으로 고친다");

    let summary = done(lab.engine().run_once().unwrap());

    assert!(lab.state().blocked.is_none());
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    let order: Vec<u64> = record.merges.iter().map(|m| m.pr.0).collect();
    assert_eq!(order, [3, 1, 2], "복구 항목, 나머지 순서다");
    for terminal in ["t1orch", "t1rs"] {
        let resumed = lines_with(&lab, terminal, "resumed");
        assert_eq!(resumed.len(), 1, "{terminal}: {resumed:?}");
        assert!(has_key(&resumed[0], "by=main_moved"), "{}", resumed[0]);
        assert!(!resumed[0].contains(" pr="), "{}", resumed[0]);
    }
    assert!(lab.events().iter().any(|e| matches!(
        e,
        EventKind::Resumed {
            by: chute::model::ResumedBy::MainMoved,
            pr: None,
            ..
        }
    )));
    let merged = lines_with(&lab, "t1orch", "merged");
    assert_eq!(merged.len(), 2, "{merged:?}");
    let marked: Vec<&String> = merged
        .iter()
        .filter(|l| l.contains(" repair=yes "))
        .collect();
    assert_eq!(marked.len(), 1, "복구 표시는 기록으로 남는다: {merged:?}");
    assert!(has_key(marked[0], "pr=3"), "{}", marked[0]);
}

const FLIPPING_ENV: &str =
    "[ -e '@ROOT@/envbroken' ] && fail 'environment broke'\n[ -e BAD ] && fail \"FAILED bad_test\"";

fn lab_on_a_pushed_base() -> Lab {
    let lab = Lab::new(FLIPPING_ENV);
    lab.open_pr(1, &[("f1.txt", "x\n")]);
    done(lab.engine().run_once().unwrap());
    assert!(
        lab.state()
            .verified_bases
            .iter()
            .any(|b| b.as_str() == lab.origin_main())
    );
    lab
}

fn queue_by_orchestrator(lab: &Lab, n: u64, files: &[(&str, &str)]) {
    let head = lab.push_pr_branch(n, files);
    lab.queue_pr_by(
        n,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );
}

#[test]
fn recheck_a_pushed_base_is_verified_again_before_the_second_failing_pr_is_bounced() {
    let lab = lab_on_a_pushed_base();
    fs::write(lab.sb.root().join("envbroken"), "").unwrap();
    queue_by_orchestrator(&lab, 2, &[("f2.txt", "x\n")]);
    queue_by_orchestrator(&lab, 3, &[("f3.txt", "x\n")]);

    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);

    assert_eq!(lab.bounced_events(), [(2, BounceReason::Verify)]);
    assert_eq!(lab.item_state(3), Some(ItemState::Queued(None)));
    let blocked = lab.state().blocked.expect("기준이 깨져 멈춘다");
    assert!(blocked.rechecked);
    assert!(blocked.pushed_by_chute);
    assert_eq!(blocked.step.as_deref(), Some("check"));
    assert_eq!(blocked.base.as_str(), lab.origin_main());
    let halted = lines_with(&lab, "t1orch", "halted");
    assert_eq!(halted.len(), 1, "{halted:?}");
    assert!(has_key(&halted[0], "rechecked=yes"), "{}", halted[0]);
    assert!(has_key(&halted[0], "pushed_by_chute=yes"), "{}", halted[0]);
    assert!(has_key(&halted[0], "suspect=2"), "{}", halted[0]);
    let status = chute_status(&lab);
    assert_eq!(status["blocked"]["rechecked"], true);
    assert_eq!(status["blocked"]["pushed_by_chute"], true);
}

#[test]
fn recheck_a_base_that_still_passes_lets_the_second_pr_be_bounced_and_resets_the_count() {
    let lab = lab_on_a_pushed_base();
    queue_by_orchestrator(&lab, 2, &[("BAD", "")]);
    queue_by_orchestrator(&lab, 3, &[("BAD", "")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[2, 3]));
    assert!(lab.state().blocked.is_none());
    assert_eq!(lab.state().base_doubt, None);
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(
        record.verifications, 4,
        "묶음, 2번, 3번, 그리고 3번을 반송하기 전의 기준 재검증"
    );
}

#[test]
fn recheck_a_pass_in_between_does_not_count() {
    let lab = lab_on_a_pushed_base();
    queue_by_orchestrator(&lab, 2, &[("BAD", "")]);
    queue_by_orchestrator(&lab, 4, &[("f4.txt", "x\n")]);
    queue_by_orchestrator(&lab, 3, &[("BAD", "")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[4]));
    assert_eq!(summary.bounced, prs(&[2, 3]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(
        record.verifications, 6,
        "묶음, 2번, 4와 3, 4, 3, 3번 단독. 기준 재검증은 없다"
    );
    let doubt = lab.state().base_doubt.expect("3번이 새로 센다");
    assert_eq!(doubt.prs, prs(&[3]));
}

#[test]
fn recheck_the_same_pr_failing_twice_is_not_two_prs() {
    let lab = lab_on_a_pushed_base();
    queue_by_orchestrator(&lab, 2, &[("BAD", "")]);
    let first = done(lab.engine().run_once().unwrap());
    assert_eq!(first.bounced, prs(&[2]));
    let head = lab.item(2).unwrap().head;
    lab.queue_pr_by(
        2,
        &head,
        Some(lab.agent()),
        Some(lab.orchestrator()),
        Approval::Agent,
    );

    let second = done(lab.engine().run_once().unwrap());

    assert_eq!(second.bounced, prs(&[2]));
    let record = lab.store.read_batch(&second.batch).unwrap().unwrap();
    assert_eq!(record.verifications, 1, "기준을 다시 검증하지 않는다");
    assert_eq!(lab.state().base_doubt.unwrap().prs, prs(&[2]));
}

const WEB_ONLY: &str = r#"paths = ["web/", "Cargo.toml"]"#;

fn skipped(round: u32) -> SkippedStep {
    SkippedStep {
        round,
        step: "check".to_owned(),
    }
}

#[test]
fn a_batch_runs_a_step_when_a_changed_file_matches_its_paths() {
    let lab = Lab::build_with_step("", "", 60, WEB_ONLY);
    lab.open_pr(1, &[("docs/a.md", "a\n")]);
    lab.open_pr(2, &[("web/b.js", "b\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2]));
    assert_eq!(lab.verify_runs(), 1);
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(record.skipped.is_empty());
}

#[test]
fn a_batch_skips_a_step_no_changed_file_matches_and_records_it() {
    let lab = Lab::build_with_step("fail 'must not run'", "", 60, WEB_ONLY);
    lab.open_pr(1, &[("docs/a.md", "a\n")]);
    lab.open_pr(2, &[("src/b.rs", "b\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2]));
    assert_eq!(lab.verify_runs(), 0);
    assert_eq!(lab.merge_commits_on_main().len(), 2);
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.verifications, 1);
    assert_eq!(record.skipped, [skipped(1)]);
    let json = serde_json::to_string(&record).unwrap();
    assert!(
        json.contains(r#""skipped":[{"round":1,"step":"check"}]"#),
        "{json}"
    );
}

#[test]
fn a_split_batch_measures_its_changes_from_the_base_the_batch_started_on() {
    let lab = Lab::build_with_step(BAD, "", 60, WEB_ONLY);
    lab.open_pr(1, &[("docs/1.md", "x")]);
    lab.open_pr(2, &[("docs/2.md", "x")]);
    lab.open_pr(3, &[("web/3.js", "x"), ("BAD", "")]);
    lab.open_pr(4, &[("docs/4.md", "x")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2, 4]));
    assert_eq!(summary.bounced, prs(&[3]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(!record.skipped.is_empty(), "문서만 고친 쪽은 건너뛴다");
    assert!(
        record.skipped.iter().all(|s| s.round > 1),
        "처음 검증은 web/을 고친 PR이 있어 돈다: {:?}",
        record.skipped
    );
}

#[test]
fn checking_a_base_runs_every_step_even_when_paths_would_skip_it() {
    let lab = Lab::build_with_step("[ -e BASEBAD ] && fail \"기준이 깨졌다\"", "", 60, WEB_ONLY);
    lab.main_commit(&[("BASEBAD", "")], "feat: broken");
    lab.open_pr(1, &[("web/a.js", "a\n")]);

    assert_eq!(lab.engine().run_once().unwrap(), Pass::Blocked);

    let blocked = lab.state().blocked.expect("저장소가 멈춘다");
    assert_eq!(blocked.reason, BlockReason::BaseBroken);
    assert!(lab.bounced_events().is_empty());
}

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn scripted_failures(runs: &[&[&str]]) -> String {
    let runs: Vec<Option<&[&str]>> = runs.iter().map(|tests| Some(*tests)).collect();
    scripted_outcomes(&runs)
}

fn scripted_outcomes(runs: &[Option<&[&str]>]) -> String {
    let mut script = String::from(
        "c='@ROOT@/runs'; n=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$c\"\ncase $n in\n",
    );
    for (n, outcome) in runs.iter().enumerate() {
        let Some(tests) = outcome else {
            script.push_str(&format!("{}) :;;\n", n + 1));
            continue;
        };
        let lines: Vec<String> = tests.iter().map(|t| format!("FAILED {t}")).collect();
        let text = if lines.is_empty() {
            "boom".to_owned()
        } else {
            lines.join("\\n")
        };
        script.push_str(&format!("{}) fail \"$(printf '{text}')\";;\n", n + 1));
    }
    script.push_str("esac");
    script
}

fn flaky_events(lab: &Lab) -> Vec<(String, Vec<String>)> {
    lab.events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Flaky { step, tests, .. } => Some((step, tests)),
            _ => None,
        })
        .collect()
}

fn recorded_as_flaky_by_another_batch(lab: &Lab, days_ago: u64, test: &str) {
    lab.store
        .append_history(&Event {
            at_ms: EpochMs(EpochMs::now().0 - days_ago * DAY_MS),
            kind: EventKind::Flaky {
                batch: BatchId("b-earlier".into()),
                step: "check".into(),
                tests: vec![test.to_owned()],
                tree: engine_kit::sha(&"a".repeat(40)),
            },
        })
        .unwrap();
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn a_solo_failure_whose_attempts_name_different_tests_is_verified_again_and_merges() {
    let lab = Lab::new(&scripted_failures(&[&["test_a"], &["test_b"]]));
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert!(summary.bounced.is_empty());
    assert_eq!(
        flaky_events(&lab),
        [("check".to_owned(), strings(&["test_a", "test_b"]))]
    );
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(
        record.reruns,
        [Rerun {
            pr: PrNumber(1),
            step: "check".into(),
            failed_round: 1,
            signal: RerunSignal::DifferentTests,
            tests: strings(&["test_a", "test_b"]),
        }]
    );
    assert_eq!(record.verifications, 2);
    assert_eq!(record.flaky.len(), 1);
    assert!(lab.bounced_events().is_empty());
    assert!(lab.lines_sent_to("t1rs").is_empty());
    lab.assert_every_pushed_commit_was_verified();
}

#[test]
fn a_solo_failure_whose_attempts_name_different_tests_again_is_bounced_after_one_more_verification()
{
    let lab = Lab::new(&scripted_failures(&[
        &["test_a"],
        &["test_b"],
        &["test_c"],
        &["test_d"],
    ]));
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    assert!(summary.merged.is_empty());
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(
        record.verifications, 3,
        "묶음, 다시 돈 검증, 반송 전의 기준 확인. 다시 돌린 것은 한 번뿐이다"
    );
    assert_eq!(record.reruns.len(), 1);
    assert!(record.flaky.is_empty());
    assert!(flaky_events(&lab).is_empty());
    let Failure::Verify(step) = lab.item(1).unwrap().last_failure.unwrap().failure else {
        panic!("검증 실패여야 한다");
    };
    assert_eq!(step.failed_tests, ["test_d"]);
}

#[test]
fn a_rerun_found_while_splitting_a_batch_records_the_round_that_failed() {
    let lab = Lab::new(&scripted_outcomes(&[
        Some(&["boom_1"]),
        Some(&["boom_1"]),
        None,
        Some(&["boom_2"]),
        Some(&["boom_2"]),
        Some(&["boom_3"]),
        Some(&["boom_3"]),
        Some(&["test_a"]),
        Some(&["test_b"]),
    ]));
    for n in 1..=3 {
        open_pr_by_orchestrator(&lab, n, &[(&format!("f{n}.txt"), "x\n")]);
    }

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1, 2, 3]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.reruns.len(), 1);
    assert_eq!(record.reruns[0].pr, PrNumber(2));
    let batch_dir = lab.store.batch_dir(&summary.batch);
    let rounds_naming = |test: &str| -> Vec<u32> {
        (1..=record.verifications)
            .filter(|round| {
                fs::read_dir(batch_dir.join(format!("verify-{round}")))
                    .into_iter()
                    .flatten()
                    .filter_map(|file| fs::read_to_string(file.unwrap().path()).ok())
                    .any(|log| log.contains(test))
            })
            .collect()
    };
    assert_eq!(
        rounds_naming("test_b"),
        [record.reruns[0].failed_round],
        "the round of the failure that showed the sign, not the base check after it"
    );
    assert!(record.reruns[0].failed_round < record.verifications);
}

fn bounce_report(lab: &Lab, pr: u64) -> (String, serde_json::Value) {
    let report = lab.item(pr).unwrap().last_failure.unwrap().report;
    let md = fs::read_to_string(&report).unwrap();
    let json = fs::read_to_string(report.with_extension("json")).unwrap();
    (md, serde_json::from_str(&json).unwrap())
}

#[test]
fn a_solo_bounce_report_says_the_base_was_verified_earlier_and_not_run_again() {
    let lab = lab_on_a_pushed_base();
    queue_by_orchestrator(&lab, 2, &[("BAD", "")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[2]));
    let (md, json) = bounce_report(&lab, 2);
    assert!(!md.contains("원인이다"), "{md}");
    assert!(
        md.contains("- 기준 브랜치: chute가 앞서 검증한 커밋이라 이 batch에서는 돌리지 않았다"),
        "{md}"
    );
    assert!(md.contains("(1번 돌려 1번 실패)"), "{md}");
    let evidence = &json["failure"]["evidence"];
    assert_eq!(evidence["base"], "verified_earlier", "{json}");
    assert_eq!(evidence["runs"], 1, "{json}");
    assert_eq!(
        evidence["attempts"],
        serde_json::json!([["bad_test"], ["bad_test"]]),
        "검증은 실패한 단계를 한 번 더 돌린다"
    );
}

#[test]
fn a_solo_bounce_after_one_more_verification_reports_two_failed_runs() {
    let lab = Lab::new(&scripted_failures(&[
        &["test_a"],
        &["test_b"],
        &["test_c"],
        &["test_d"],
    ]));
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let (md, json) = bounce_report(&lab, 1);
    assert!(!md.contains("원인이다"), "{md}");
    assert!(md.contains("(2번 돌려 2번 실패)"), "{md}");
    assert!(
        md.contains("- 기준 브랜치: 이 batch에서 돌려 통과했다"),
        "{md}"
    );
    assert!(md.contains("서로 달랐다"), "{md}");
    for test in ["test_a", "test_b", "test_c", "test_d"] {
        assert!(md.contains(test), "{test}: {md}");
    }
    let evidence = &json["failure"]["evidence"];
    assert_eq!(evidence["base"], "ran_in_this_batch", "{json}");
    assert_eq!(evidence["runs"], 2, "{json}");
    assert_eq!(
        evidence["attempts"],
        serde_json::json!([["test_a"], ["test_b"], ["test_c"], ["test_d"]])
    );
}

#[test]
fn a_solo_report_says_the_base_ran_in_this_batch_when_this_batch_pushed_it() {
    let lab = Lab::new(BAD);
    lab.main_commit(&[("x", "0\n")], "feat: x0");
    lab.repo
        .git(&["checkout", "--quiet", "-B", "pr-2", "origin/main"]);
    lab.repo.write("x", "3\n");
    lab.repo.write("BAD", "");
    lab.repo.git(&["add", "x", "BAD"]);
    lab.repo.git(&["commit", "--quiet", "-m", "feat: change 2"]);
    let head2 = lab.repo.head();
    lab.repo.git(&[
        "push",
        "--quiet",
        "--force",
        "origin",
        &format!("{head2}:refs/heads/pr-2"),
        &format!("{head2}:refs/pull/2/head"),
    ]);
    lab.repo.git(&["checkout", "--quiet", "main"]);
    lab.gh_says_head(2, &head2);
    lab.main_commit(&[("x", "1\n")], "feat: x1");
    lab.open_pr(1, &[("x", "0\n")]);
    lab.queue_pr(
        2,
        &engine_kit::sha(&head2),
        Some(lab.agent()),
        Approval::Agent,
    );

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[2]));
    let (md, json) = bounce_report(&lab, 2);
    assert!(
        md.contains("- 기준 브랜치: 이 batch에서 돌려 통과했다"),
        "{md}"
    );
    assert_eq!(json["failure"]["evidence"]["base"], "ran_in_this_batch");
}

#[test]
fn a_solo_report_counts_only_the_attempts_of_the_step_that_failed() {
    let lint = "\n[[verify]]\nname = \"lint\"\nrun = \"sh @ROOT@/lint.sh\"\ntimeout = 60\nfailed = 'FAILED (\\S+)'\n";
    let lab = Lab::build_with_step(
        &scripted_failures(&[&["test_a"], &["test_b"]]),
        "",
        60,
        lint,
    );
    fs::write(
        lab.sb.root().join("lint.sh"),
        "[ -e a.txt ] && { echo 'FAILED lint_x'; exit 1; }\nexit 0\n",
    )
    .unwrap();
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let (md, json) = bounce_report(&lab, 1);
    assert_eq!(json["failure"]["step"], "lint");
    assert!(!md.contains("test_a") && !md.contains("test_b"), "{md}");
    assert!(
        md.contains("(2번 돌려 1번 실패, 1번은 앞 단계가 실패해 이 단계를 돌리지 않았다)"),
        "{md}"
    );
    assert_eq!(
        json["failure"]["evidence"]["attempts"],
        serde_json::json!([["lint_x"], ["lint_x"]])
    );
}

fn scope_line(md: &str) -> &str {
    md.lines().find(|l| l.starts_with("- 범위:")).unwrap()
}

const SLOW_STEP_SECONDS: u64 = 5;

#[test]
fn a_solo_report_counts_a_failure_and_a_timeout_of_two_runs() {
    let script = "c='@ROOT@/runs'; n=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$c\"\n\
case $n in\n\
1) fail 'FAILED test_a';;\n\
2) fail 'FAILED test_b';;\n\
3) echo 'FAILED test_c'; sleep 600;;\n\
esac";
    let lab = Lab::build(script, "", SLOW_STEP_SECONDS);
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let (md, json) = bounce_report(&lab, 1);
    assert_eq!(
        scope_line(&md),
        "- 범위: 단독. 기준 브랜치에 이 PR만 얹은 트리에서 통과하지 못했다(2번 돌려 1번 실패, 1번 시간 초과)",
        "{md}"
    );
    assert!(
        md.contains("- 기준 브랜치: 이 batch에서 돌려 통과했다"),
        "{md}"
    );
    let evidence = &json["failure"]["evidence"];
    assert_eq!(evidence["runs"], 2, "{json}");
    assert_eq!(
        evidence["run_outcomes"],
        serde_json::json!(["failed", "timed_out"]),
        "{json}"
    );
}

#[test]
fn a_solo_report_counts_a_timeout_and_a_failure_of_two_runs() {
    let script = "c='@ROOT@/runs'; n=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$c\"\n\
case $n in\n\
1) echo 'FAILED test_k'; sleep 600;;\n\
2) fail 'FAILED test_k';;\n\
3) fail 'FAILED test_k';;\n\
esac";
    let lab = Lab::build(script, "", SLOW_STEP_SECONDS);
    recorded_as_flaky_by_another_batch(&lab, 3, "test_k");
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let (md, json) = bounce_report(&lab, 1);
    assert_eq!(
        scope_line(&md),
        "- 범위: 단독. 기준 브랜치에 이 PR만 얹은 트리에서 통과하지 못했다(2번 돌려 1번 실패, 1번 시간 초과)",
        "{md}"
    );
    let evidence = &json["failure"]["evidence"];
    assert_eq!(evidence["runs"], 2, "{json}");
    assert_eq!(
        evidence["run_outcomes"],
        serde_json::json!(["timed_out", "failed"]),
        "{json}"
    );
}

#[test]
fn a_solo_report_counts_the_run_where_the_failed_step_passed() {
    let lint = "\n[[verify]]\nname = \"lint\"\nrun = \"sh @ROOT@/lint.sh\"\ntimeout = 60\nfailed = 'FAILED (\\S+)'\n";
    let lab = Lab::build_with_step(
        &scripted_outcomes(&[None, Some(&["check_x"]), Some(&["check_x"])]),
        "",
        60,
        lint,
    );
    fs::write(
        lab.sb.root().join("lint.sh"),
        "c='@ROOT@/lint-runs'; n=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$c\"\n\
[ $n -eq 1 ] && { echo 'FAILED lint_a'; exit 1; }\n\
[ $n -eq 2 ] && { echo 'FAILED lint_b'; exit 1; }\n\
exit 0\n"
            .replace("@ROOT@", &lab.sb.root().display().to_string()),
    )
    .unwrap();
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let (md, json) = bounce_report(&lab, 1);
    assert_eq!(json["failure"]["step"], "check");
    assert_eq!(
        scope_line(&md),
        "- 범위: 단독. 기준 브랜치에 이 PR만 얹은 트리에서 실패했다(2번 돌려 1번 실패, 1번 통과)",
        "{md}"
    );
    let evidence = &json["failure"]["evidence"];
    assert_eq!(evidence["runs"], 2, "{json}");
    assert_eq!(
        evidence["run_outcomes"],
        serde_json::json!(["passed", "failed"]),
        "{json}"
    );
    assert_eq!(
        evidence["attempts"],
        serde_json::json!([["check_x"], ["check_x"]])
    );
}

#[test]
fn a_solo_report_says_the_pushed_base_skipped_the_step_by_paths() {
    let lab = Lab::build_with_step(BAD, "", 60, WEB_ONLY);
    lab.main_commit(&[("x", "0\n")], "feat: x0");
    lab.repo
        .git(&["checkout", "--quiet", "-B", "pr-2", "origin/main"]);
    lab.repo.write("x", "3\n");
    lab.repo.write("BAD", "");
    lab.repo.write("web/w.js", "w\n");
    lab.repo.git(&["add", "x", "BAD", "web/w.js"]);
    lab.repo.git(&["commit", "--quiet", "-m", "feat: change 2"]);
    let head2 = lab.repo.head();
    lab.repo.git(&[
        "push",
        "--quiet",
        "--force",
        "origin",
        &format!("{head2}:refs/heads/pr-2"),
        &format!("{head2}:refs/pull/2/head"),
    ]);
    lab.repo.git(&["checkout", "--quiet", "main"]);
    lab.gh_says_head(2, &head2);
    lab.main_commit(&[("x", "1\n")], "feat: x1");
    lab.open_pr(1, &[("x", "0\n")]);
    lab.queue_pr(
        2,
        &engine_kit::sha(&head2),
        Some(lab.agent()),
        Approval::Agent,
    );

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert_eq!(summary.bounced, prs(&[2]));
    let (md, json) = bounce_report(&lab, 2);
    assert!(
        md.contains("- 기준 브랜치: 이 batch가 push한 커밋이다. 그 검증은 변경 범위(`paths`)에 들지 않아 이 단계를 건너뛰었다\n"),
        "{md}"
    );
    assert_eq!(
        json["failure"]["evidence"]["base"], "pushed_skipped_by_paths",
        "{json}"
    );
}

#[test]
fn a_solo_report_looks_up_the_ledger_for_the_tests_of_every_attempt() {
    let lab = Lab::new(&scripted_failures(&[&["test_a", "test_x"], &["test_x"]]));
    recorded_as_flaky_by_another_batch(&lab, 3, "test_a");
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let (md, _) = bounce_report(&lab, 1);
    assert!(
        md.contains("흔들림 기록이 있는 실패 시험: `test_a`"),
        "{md}"
    );
}

#[test]
fn a_solo_report_counts_one_run_when_the_rebuilt_tree_failed_after_a_passing_rerun() {
    let script = "c='@ROOT@/runs'; n=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$c\"\n\
case $n in\n\
1) fail 'FAILED test_a';;\n\
2) fail 'FAILED test_b';;\n\
3) (git clone -q '@ORIGIN@' '@ROOT@/mover' && cd '@ROOT@/mover' && echo x > moved.txt && git add moved.txt && git commit -qm 'chore: other push' && git push -q origin HEAD:main) || fail 'mover failed';;\n\
4) fail 'FAILED test_c';;\n\
5) fail 'FAILED test_c';;\n\
esac";
    let lab = Lab::new(script);
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let (md, json) = bounce_report(&lab, 1);
    assert!(md.contains("(1번 돌려 1번 실패)"), "{md}");
    assert_eq!(
        json["failure"]["evidence"]["attempts"],
        serde_json::json!([["test_c"], ["test_c"]])
    );
}

#[test]
fn a_solo_failure_of_a_test_another_batch_recorded_as_flaky_is_verified_again() {
    let lab = Lab::build(
        &scripted_failures(&[&["known_test"], &["known_test"]]),
        "[flaky]\nchronic_days = 5\n",
        60,
    );
    recorded_as_flaky_by_another_batch(&lab, 1, "known_test");
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    assert!(summary.bounced.is_empty());
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.reruns.len(), 1);
    assert_eq!(record.reruns[0].signal, RerunSignal::KnownFlaky);
    assert_eq!(record.reruns[0].tests, ["known_test"]);
    assert_eq!(record.flaky.len(), 1);
    assert_eq!(record.verifications, 2);
}

#[test]
fn a_flaky_record_older_than_chronic_days_does_not_spare_the_pr() {
    let lab = Lab::build(
        &scripted_failures(&[&["known_test"], &["known_test"]]),
        "[flaky]\nchronic_days = 5\n",
        60,
    );
    recorded_as_flaky_by_another_batch(&lab, 6, "known_test");
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(record.reruns.is_empty());
    assert_eq!(record.verifications, 2, "묶음과 기준 확인");
}

#[test]
fn a_solo_failure_with_a_known_flaky_test_and_an_unknown_test_is_bounced() {
    let lab = Lab::build(
        &scripted_failures(&[
            &["known_test", "unknown_test"],
            &["known_test", "unknown_test"],
        ]),
        "[flaky]\nchronic_days = 5\n",
        60,
    );
    recorded_as_flaky_by_another_batch(&lab, 1, "known_test");
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(record.reruns.is_empty());
    assert_eq!(record.verifications, 2);
}

#[test]
fn a_solo_failure_of_the_same_test_in_both_attempts_is_bounced_without_another_verification() {
    let lab = Lab::new(&scripted_failures(&[&["bad_test"], &["bad_test"]]));
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(record.reruns.is_empty());
    assert_eq!(record.verifications, 2, "묶음과 기준 확인");
    assert!(flaky_events(&lab).is_empty());
}

#[test]
fn a_solo_failure_without_test_names_is_bounced_as_before() {
    let lab = Lab::new(&scripted_failures(&[&[], &[]]));
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.bounced, prs(&[1]));
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(record.reruns.is_empty());
    assert_eq!(record.verifications, 2, "묶음과 기준 확인");
}

#[test]
fn a_repair_pr_whose_attempts_name_different_tests_is_verified_again_and_lifts_the_halt() {
    let flaking = scripted_failures(&[&["test_a"], &["test_b"]]);
    let lab = halted_lab_running(&format!(
        "{BROKEN_BASE}\nif [ -e '@ROOT@/arm' ]; then\n{flaking}\nfi"
    ));
    fs::write(lab.sb.root().join("arm"), "").unwrap();
    let head = lab.push_pr_branch_removing(3, "BASEBAD");
    lab.queue_repair_by(3, &head, Some(lab.agent()), Some(lab.orchestrator()));

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[3]));
    assert!(summary.bounced.is_empty());
    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert_eq!(record.reruns.len(), 1);
    assert_eq!(record.reruns[0].pr, PrNumber(3));
    assert_eq!(record.verifications, 2);
    assert!(lab.state().blocked.is_none());
    assert_eq!(lab.merged_prs(), [3]);
}

const LINT_AND_E2E: &str = "\n[[verify]]\nname = \"lint\"\nrun = \"sh @ROOT@/lint.sh\"\ntimeout = 60\nfailed = 'FAILED (\\S+)'\n\n[[verify]]\nname = \"e2e\"\nrun = \"true\"\ntimeout = 60\npaths = [\"web/\"]\n";

fn status_into(lab: &Lab, file: &str) -> String {
    format!(
        "env CHUTE_HOME='{}' HOME='{}' CHUTE_GIT='{}' '{}' status --json --repo '{}' > '{}'",
        lab.sb.chute_home().display(),
        lab.sb.home().display(),
        lab.sb.root().join("bin/git").display(),
        env!("CARGO_BIN_EXE_chute"),
        lab.repo.path().display(),
        lab.sb.root().join(file).display(),
    )
}

#[test]
fn a_batch_records_how_long_each_step_took_in_each_round() {
    let lab = Lab::build_with_step("sleep 1", "", 60, LINT_AND_E2E);
    fs::write(lab.sb.root().join("lint.sh"), "exit 0\n").unwrap();
    lab.open_pr(1, &[("a.txt", "a\n")]);

    let summary = lab
        .engine_with_clock(Duration::ZERO, chute::clock::Clock::frozen())
        .run_once()
        .unwrap();

    let record = lab.store.read_batch(&done(summary).batch).unwrap().unwrap();
    let step_run = |round, step: &str, seconds, result| chute::model::StepRun {
        round,
        step: step.to_owned(),
        seconds,
        result,
    };
    assert_eq!(
        record.step_times,
        [
            step_run(1, "check", 0, chute::model::StepResult::Passed),
            step_run(1, "lint", 0, chute::model::StepResult::Passed),
        ],
        "a step that sleeps a real second took 0 on the engine's frozen clock; e2e was left out by paths"
    );

    let lab = Lab::build_with_step(BAD, "", 60, LINT_AND_E2E);
    fs::write(lab.sb.root().join("lint.sh"), "exit 0\n").unwrap();
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.open_pr(2, &[("BAD", "")]);

    let summary = done(lab.engine().run_once().unwrap());

    let record = lab.store.read_batch(&summary.batch).unwrap().unwrap();
    assert!(record.verifications > 1, "the batch was split");
    assert_eq!(
        record.ran,
        ["check", "lint"],
        "ran is the first round's step list even though that round stopped at check"
    );
    let first_round: Vec<_> = record.step_times.iter().filter(|r| r.round == 1).collect();
    assert_eq!(first_round.len(), 1);
    assert_eq!(first_round[0].step, "check");
    assert_eq!(first_round[0].result, chute::model::StepResult::Failed);
    assert!(
        record
            .step_times
            .iter()
            .any(|r| r.result == chute::model::StepResult::Passed),
        "a later round passed"
    );
}

#[test]
fn the_status_during_verification_shows_the_steps_done_and_skipped_this_round() {
    let lab = Lab::build_with_step("exit 0", "", 60, LINT_AND_E2E);
    let status = status_into(&lab, "status-during.json");
    fs::write(lab.sb.root().join("lint.sh"), format!("{status}\nexit 0\n")).unwrap();
    lab.open_pr(1, &[("a.txt", "a\n")]);

    done(lab.engine().run_once().unwrap());

    let during = fs::read_to_string(lab.sb.root().join("status-during.json")).unwrap();
    let view: StatusView = serde_json::from_str(&during).unwrap();
    let batch = view.batch.expect("there is a batch while lint runs");
    assert_eq!(batch.step.as_deref(), Some("lint"));
    assert_eq!(batch.steps, ["check", "lint"]);
    assert_eq!(
        batch
            .done
            .iter()
            .map(|d| d.step.as_str())
            .collect::<Vec<_>>(),
        ["check"]
    );
    assert!(batch.step_started_ms.is_some());
    assert_eq!(batch.skipped, ["e2e"]);
    assert!(batch.reruns.is_empty());
}

#[test]
fn a_finished_batch_tells_history_which_steps_it_ran_and_how_long_they_took() {
    let lab = Lab::build_with_step("exit 0", "", 60, LINT_AND_E2E);
    fs::write(lab.sb.root().join("lint.sh"), "exit 0\n").unwrap();
    lab.open_pr(1, &[("a.txt", "a\n")]);

    done(lab.engine().run_once().unwrap());

    let finished: Vec<_> = lab
        .events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::BatchFinished {
                ran,
                verifications,
                step_times,
                ..
            } => Some((ran, verifications, step_times)),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 1);
    let (ran, verifications, step_times) = &finished[0];
    assert_eq!(ran, &["check", "lint"]);
    assert_eq!(*verifications, 1);
    assert_eq!(
        step_times
            .iter()
            .map(|t| t.step.as_str())
            .collect::<Vec<_>>(),
        ["check", "lint"]
    );
}

#[test]
fn a_rerun_shows_in_the_status_batch_while_the_rerun_round_runs() {
    let body = "c='@ROOT@/runs'; n=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$c\"\ncase $n in\n1) fail \"$(printf 'FAILED test_a')\";;\n2) fail \"$(printf 'FAILED test_b')\";;\n3) @STATUS@;;\nesac";
    let lab = Lab::new(body);
    open_pr_by_orchestrator(&lab, 1, &[("a.txt", "a\n")]);

    let summary = done(lab.engine().run_once().unwrap());

    assert_eq!(summary.merged, prs(&[1]));
    let during = fs::read_to_string(lab.sb.root().join("status-during.json")).unwrap();
    let view: StatusView = serde_json::from_str(&during).unwrap();
    let batch = view
        .batch
        .expect("there is a batch while the rerun round runs");
    assert_eq!(batch.round, 2);
    assert_eq!(
        batch.reruns,
        [Rerun {
            pr: PrNumber(1),
            step: "check".into(),
            failed_round: 1,
            signal: RerunSignal::DifferentTests,
            tests: strings(&["test_a", "test_b"]),
        }]
    );
}
