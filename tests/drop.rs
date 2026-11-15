mod queue_kit;
mod support;

use chute::model::{BounceReason, HumanReason, ItemState};
use queue_kit::{World, item, stderr};

#[test]
fn drop_takes_a_pr_out_of_the_queue_wherever_it_waits() {
    let w = World::new();
    w.seed(vec![
        item(1, ItemState::AwaitingApproval),
        item(2, ItemState::Queued(None)),
        item(3, ItemState::Bounced(BounceReason::Conflict)),
        item(4, ItemState::NeedsHuman(HumanReason::Unreachable)),
        item(5, ItemState::Queued(None)),
    ]);

    for pr in ["1", "2", "3", "4"] {
        let stdout = w.ok(w.repo.path(), &["drop", pr]);
        assert!(stdout.contains(&format!("큐에서 뺐다: #{pr}")), "{stdout}");
    }

    let queue = w.status(w.repo.path())["queue"].clone();
    let left: Vec<u64> = queue
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["pr"].as_u64().unwrap())
        .collect();
    assert_eq!(left, [5]);
}

#[test]
fn drop_works_from_outside_the_repository_with_the_repo_flag() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::NeedsHuman(HumanReason::Budget))]);
    let repo = w.repo.path().to_string_lossy().into_owned();

    w.ok(w.sb.root(), &["drop", "12", "--repo", &repo]);

    assert!(w.entry(12).is_none());
}

#[test]
fn drop_refuses_a_pr_in_a_batch_and_a_pr_that_is_not_there() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::InBatch)]);

    let out = w.chute(w.repo.path(), &["drop", "12"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("batch가 끝난 뒤"), "{}", stderr(&out));
    assert_eq!(w.entry(12).unwrap()["state"], "in_batch");

    let out = w.chute(w.repo.path(), &["drop", "99"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn dropping_many_prs_at_once_loses_none() {
    let w = World::new();
    w.seed((1..=11).map(|n| item(n, ItemState::Queued(None))).collect());
    let commands: Vec<Vec<String>> = (1..=10)
        .map(|n| vec!["drop".to_owned(), n.to_string()])
        .collect();

    for out in w.run_parallel(&commands) {
        assert!(out.status.success(), "{}", stderr(&out));
    }

    let left: Vec<u64> = w.state().queue.iter().map(|i| i.pr.0).collect();
    assert_eq!(left, [11]);
}

#[test]
fn a_corrupt_state_file_stops_drop_with_exit_code_2_and_is_left_alone() {
    let w = World::new();
    let garbage = w.corrupt_state();
    let out = w.chute(w.repo.path(), &["drop", "12"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(w.state_file()).unwrap(), garbage);
}
