mod forward_kit;
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

#[test]
fn drop_on_another_pc_removes_the_item_from_the_hub_queue_only() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    forward_kit::seed(
        b,
        vec![
            item(12, ItemState::Queued(None)),
            item(13, ItemState::Queued(None)),
        ],
    );
    let a_before = forward_kit::state_file(a);

    let out = a.chute(forward_kit::BIN, &["drop", "12"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(forward_kit::out(&out), "큐에서 뺐다: #12 (PR 12)\n");
    let left: Vec<u64> = forward_kit::state(b).queue.iter().map(|i| i.pr.0).collect();
    assert_eq!(left, [13]);
    assert_eq!(forward_kit::state_file(a), a_before);
}

#[test]
fn a_refused_drop_on_another_pc_has_the_hubs_exit_code_and_message() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));

    let remote = a.chute(forward_kit::BIN, &["drop", "99"]);
    let direct = b.chute(forward_kit::BIN, &["drop", "99"]);

    assert_eq!(remote.status.code(), Some(1), "{}", stderr(&remote));
    assert_eq!(remote.status.code(), direct.status.code());
    assert_eq!(remote.stderr, direct.stderr);
}

#[test]
fn drop_on_another_pc_says_so_when_the_hub_is_down_or_this_is_not_the_hub() {
    let net = forward_kit::net();
    forward_kit::assert_unreachable_hub(&net, &["drop", "12"]);
    forward_kit::assert_not_hub_answer(&net, &["drop", "12"]);
}
