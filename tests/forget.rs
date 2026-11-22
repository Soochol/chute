mod queue_kit;
mod support;

use std::fs;

use chute::model::ItemState;
use queue_kit::{World, item, stderr};
use serde_json::Value;

fn listed(w: &World) -> Vec<String> {
    let all: Value =
        serde_json::from_str(&w.ok(w.sb.root(), &["status", "--json", "--all"])).unwrap();
    all.as_array()
        .unwrap()
        .iter()
        .map(|view| view["repo"]["slug"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn forget_takes_the_repo_off_the_list_and_init_brings_it_back_with_its_history() {
    let w = World::new();
    let history = w.store().dir().join("history.jsonl");
    fs::write(&history, "{\"kept\":true}\n").unwrap();
    assert_eq!(listed(&w), ["acme/widget"]);

    let stdout = w.ok(w.repo.path(), &["forget"]);

    assert!(stdout.contains("등록을 풀었다: acme/widget"), "{stdout}");
    assert!(stdout.contains("chute init"), "{stdout}");
    assert!(listed(&w).is_empty());
    assert!(w.store().info().unwrap().is_none());
    assert_eq!(fs::read_to_string(&history).unwrap(), "{\"kept\":true}\n");

    let again = w.chute(w.repo.path(), &["status"]);
    assert_eq!(again.status.code(), Some(2), "{}", stderr(&again));
    assert!(
        stderr(&again).contains("등록되지 않은 저장소"),
        "{}",
        stderr(&again)
    );

    w.ok(w.repo.path(), &["init"]);
    assert_eq!(listed(&w), ["acme/widget"]);
    assert_eq!(fs::read_to_string(&history).unwrap(), "{\"kept\":true}\n");
}

#[test]
fn forget_by_slug_works_from_any_folder() {
    let w = World::new();

    let stdout = w.ok(w.sb.root(), &["forget", "--slug", "acme/widget"]);

    assert!(stdout.contains("등록을 풀었다: acme/widget"), "{stdout}");
    assert!(listed(&w).is_empty());
}

#[test]
fn forget_refuses_while_prs_are_in_the_queue_and_keeps_the_registration() {
    let w = World::new();
    w.seed(vec![
        item(12, ItemState::Queued(None)),
        item(13, ItemState::AwaitingApproval),
    ]);

    let out = w.chute(w.repo.path(), &["forget"]);

    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("큐에 PR이 남아 있다: #12, #13"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("chute drop"), "{}", stderr(&out));
    assert_eq!(listed(&w), ["acme/widget"]);
}

#[test]
fn forget_refuses_while_the_daemon_runs() {
    let w = World::new();
    let _daemon = w.store().try_lock_daemon().unwrap().unwrap();

    let out = w.chute(w.repo.path(), &["forget"]);

    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("chute down"), "{}", stderr(&out));
    assert!(w.store().info().unwrap().is_some());
}

#[test]
fn forget_on_an_unregistered_repo_cannot_run() {
    let w = World::new();
    w.ok(w.repo.path(), &["forget"]);

    let out = w.chute(w.repo.path(), &["forget"]);

    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("등록되지 않은 저장소"),
        "{}",
        stderr(&out)
    );
}
