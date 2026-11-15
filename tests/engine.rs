mod engine_kit;
mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chute::engine::{Pass, Summary};
use chute::model::{
    Approval, BlockReason, BounceReason, EventKind, Failure, HumanReason, ItemState, PrNumber,
    StatusView, VerifyMode,
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
fn after_the_budget_is_spent_the_next_failure_goes_to_a_human_and_nothing_is_sent() {
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
    assert_eq!(
        lab.item_state(1),
        Some(ItemState::NeedsHuman(HumanReason::Budget))
    );
    assert_eq!(lab.hc_calls("send").len(), 3);
    assert!(lab.events().iter().any(|e| matches!(
        e,
        EventKind::NeedsHuman {
            reason: HumanReason::Budget,
            ..
        }
    )));
    assert!(lab.item(1).unwrap().last_failure.is_some());
}

#[test]
fn a_finished_agent_is_replaced_and_the_new_address_is_recorded() {
    let lab = Lab::new(BAD);
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
fn without_an_agent_or_a_respawn_command_the_pr_waits_for_a_human() {
    let lab = Lab::build(BAD, "respawn = []", 60);
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
        Some(ItemState::NeedsHuman(HumanReason::Unreachable))
    );
    assert!(lab.hc_calls("run").is_empty());
    assert!(lab.events().iter().any(|e| matches!(
        e,
        EventKind::NeedsHuman {
            reason: HumanReason::Unreachable,
            detail: Some(_),
            ..
        }
    )));
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
