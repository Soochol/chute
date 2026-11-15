mod queue_kit;
mod support;

use chute::model::{BounceReason, HumanReason, ItemState};
use queue_kit::{World, item, stderr};

#[test]
fn retry_puts_a_humans_pr_back_in_line_with_a_fresh_budget() {
    let w = World::new();
    let mut stuck = item(12, ItemState::NeedsHuman(HumanReason::Budget));
    stuck.attempts = 3;
    w.seed(vec![stuck]);
    let before = w.entry(12).unwrap();
    assert_eq!(
        (&before["state"], &before["attempts"]),
        (&"needs_human".into(), &3.into())
    );

    let stdout = w.ok(w.repo.path(), &["retry", "12"]);

    assert!(stdout.contains("다시 큐에 넣었다: #12"), "{stdout}");
    let after = w.entry(12).unwrap();
    assert_eq!(after["state"], "queued");
    assert_eq!(after["attempts"], 0);
}

#[test]
fn retry_works_from_outside_the_repository_with_the_repo_flag() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::NeedsHuman(HumanReason::GaveUp))]);
    let repo = w.repo.path().to_string_lossy().into_owned();

    w.ok(w.sb.root(), &["--repo", &repo, "retry", "12"]);

    assert_eq!(w.entry(12).unwrap()["state"], "queued");
}

#[test]
fn retry_refuses_a_pr_that_is_not_on_a_humans_turn() {
    let w = World::new();
    w.seed(vec![
        item(12, ItemState::Bounced(BounceReason::Verify)),
        item(13, ItemState::InBatch),
    ]);

    let out = w.chute(w.repo.path(), &["retry", "12"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("다시 submit해야 한다"),
        "{}",
        stderr(&out)
    );

    let out = w.chute(w.repo.path(), &["retry", "13"]);
    assert_eq!(out.status.code(), Some(1));

    assert_eq!(w.entry(12).unwrap()["state"], "bounced");
    assert_eq!(w.entry(13).unwrap()["state"], "in_batch");
}

#[test]
fn retrying_many_prs_at_once_loses_none() {
    let w = World::new();
    w.seed(
        (1..=10)
            .map(|n| item(n, ItemState::NeedsHuman(HumanReason::Budget)))
            .collect(),
    );
    let commands: Vec<Vec<String>> = (1..=10)
        .map(|n| vec!["retry".to_owned(), n.to_string()])
        .collect();

    for out in w.run_parallel(&commands) {
        assert!(out.status.success(), "{}", stderr(&out));
    }

    assert!(
        w.state()
            .queue
            .iter()
            .all(|i| i.state == ItemState::Queued(None))
    );
}

#[test]
fn a_corrupt_state_file_stops_retry_with_exit_code_2_and_is_left_alone() {
    let w = World::new();
    let garbage = w.corrupt_state();
    let out = w.chute(w.repo.path(), &["retry", "12"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(w.state_file()).unwrap(), garbage);
}
