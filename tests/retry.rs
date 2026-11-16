mod forward_kit;
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

#[test]
fn retry_on_another_pc_resets_the_hub_item_and_leaves_this_pcs_state_alone() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    let mut stuck = item(12, ItemState::NeedsHuman(HumanReason::Budget));
    stuck.attempts = 3;
    forward_kit::seed(b, vec![stuck]);
    let a_before = forward_kit::state_file(a);

    let out = a.chute(forward_kit::BIN, &["retry", "12"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        forward_kit::out(&out),
        "다시 큐에 넣었다: #12. 반송 횟수를 0으로 돌렸다\n"
    );
    let queued = &forward_kit::state(b).queue[0];
    assert_eq!(
        (&queued.state, queued.attempts),
        (&ItemState::Queued(None), 0)
    );
    assert_eq!(forward_kit::state_file(a), a_before);
}

#[test]
fn a_refused_retry_on_another_pc_has_the_hubs_exit_code_and_message() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    forward_kit::seed(b, vec![item(12, ItemState::Queued(None))]);

    let remote = a.chute(forward_kit::BIN, &["retry", "12"]);
    let direct = b.chute(forward_kit::BIN, &["retry", "12"]);

    assert_eq!(remote.status.code(), Some(1), "{}", stderr(&remote));
    assert_eq!(remote.status.code(), direct.status.code());
    assert_eq!(remote.stderr, direct.stderr);
}

#[test]
fn retry_on_another_pc_says_so_when_the_hub_is_down_or_this_is_not_the_hub() {
    let net = forward_kit::net();
    forward_kit::assert_unreachable_hub(&net, &["retry", "12"]);
    forward_kit::assert_not_hub_answer(&net, &["retry", "12"]);
}
