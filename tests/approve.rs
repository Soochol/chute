mod queue_kit;
mod support;

use chute::model::ItemState;
use queue_kit::{World, item, stderr};

#[test]
fn approve_puts_a_waiting_pr_in_the_queue_and_status_shows_it() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::AwaitingApproval)]);
    assert_eq!(w.entry(12).unwrap()["state"], "awaiting_approval");

    let stdout = w.ok(w.repo.path(), &["approve", "12"]);

    assert!(stdout.contains("승인했다: #12"), "{stdout}");
    let entry = w.entry(12).unwrap();
    assert_eq!(entry["state"], "queued");
    assert_eq!(entry["state_reason"], serde_json::Value::Null);
}

#[test]
fn approve_works_from_outside_the_repository_with_the_repo_flag() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::AwaitingApproval)]);
    let repo = w.repo.path().to_string_lossy().into_owned();

    w.ok(w.sb.root(), &["approve", "12", "--repo", &repo]);

    assert_eq!(w.entry(12).unwrap()["state"], "queued");
}

#[test]
fn approve_refuses_with_exit_code_1_and_changes_nothing() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::Queued(None))]);

    let out = w.chute(w.repo.path(), &["approve", "12"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("승인을 기다리는 PR만"),
        "{}",
        stderr(&out)
    );

    let out = w.chute(w.repo.path(), &["approve", "99"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("#99: 큐에 없다"), "{}", stderr(&out));

    assert_eq!(w.entry(12).unwrap()["state"], "queued");
}

#[test]
fn approve_outside_any_repository_cannot_run() {
    let w = World::new();
    let out = w.chute(w.sb.root(), &["approve", "12"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn approving_many_prs_at_once_loses_none() {
    let w = World::new();
    w.seed(
        (1..=10)
            .map(|n| item(n, ItemState::AwaitingApproval))
            .collect(),
    );
    let commands: Vec<Vec<String>> = (1..=10)
        .map(|n| vec!["approve".to_owned(), n.to_string()])
        .collect();

    for out in w.run_parallel(&commands) {
        assert!(out.status.success(), "{}", stderr(&out));
    }

    let waiting = w
        .state()
        .queue
        .iter()
        .filter(|i| i.state != ItemState::Queued(None))
        .count();
    assert_eq!(waiting, 0);
}

#[test]
fn a_corrupt_state_file_stops_approve_with_exit_code_2_and_is_left_alone() {
    let w = World::new();
    let garbage = w.corrupt_state();
    let out = w.chute(w.repo.path(), &["approve", "12"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(w.state_file()).unwrap(), garbage);
}

#[test]
fn approve_with_a_head_that_is_not_the_current_one_refuses_and_changes_nothing() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::AwaitingApproval)]);

    let out = w.chute(w.repo.path(), &["approve", "12", "--head", "def5678"]);

    assert_eq!(out.status.code(), Some(1));
    let said = stderr(&out);
    assert!(said.contains("HEAD가 바뀌어"), "{said}");
    assert!(
        said.contains("승인하려던 HEAD def5678, 지금 HEAD abc1234"),
        "{said}"
    );
    assert_eq!(w.entry(12).unwrap()["state"], "awaiting_approval");
}

#[test]
fn approve_with_a_head_that_is_not_a_sha_says_so_instead_of_claiming_the_head_moved() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::AwaitingApproval)]);

    for given in ["a", "", "zzzz"] {
        let out = w.chute(
            w.repo.path(),
            &["approve", "12", &format!("--head={given}")],
        );
        assert_eq!(out.status.code(), Some(1), "{given:?}");
        let said = stderr(&out);
        assert!(said.contains("4자 이상의 16진수"), "{given:?}: {said}");
        assert!(!said.contains("HEAD가 바뀌어"), "{given:?}: {said}");
    }
    assert_eq!(w.entry(12).unwrap()["state"], "awaiting_approval");

    w.ok(w.repo.path(), &["approve", "12", "--head= ABC1234 "]);
    assert_eq!(w.entry(12).unwrap()["state"], "queued");
}

#[test]
fn approve_with_the_current_head_or_a_prefix_of_it_approves() {
    let w = World::new();
    w.seed(vec![
        item(12, ItemState::AwaitingApproval),
        item(13, ItemState::AwaitingApproval),
    ]);

    w.ok(w.repo.path(), &["approve", "12", "--head", "abc1234"]);
    w.ok(w.repo.path(), &["approve", "13", "--head=abc1"]);

    assert_eq!(w.entry(12).unwrap()["state"], "queued");
    assert_eq!(w.entry(13).unwrap()["state"], "queued");
}
