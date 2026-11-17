mod forward_kit;
mod queue_kit;
mod support;

use std::fs;

use chute::model::{Approval, EpochMs, EventKind, ItemState, PrNumber, RemovedReason, Sha};
use chute::queue::{self, Change, Submission};
use queue_kit::{World, item};
use serde_json::{Value, json};

fn old_item(pr: u64, reason: &str, submitter: Option<&str>) -> Value {
    json!({
        "pr": pr, "title": format!("PR {pr}"), "branch": format!("feat/{pr}"),
        "head": "abc1234", "state": "needs_human", "state_reason": reason,
        "agent": null, "attempts": 3, "submitted_ms": pr, "touches_config": false,
        "last_failure": null,
        "submitter": submitter.map(|t| json!({
            "terminal": t, "created_ms": 1, "name": null, "workdir": "/w", "alive": true
        })),
    })
}

fn write_old_state(w: &World) {
    let queue: Vec<Value> = [
        old_item(1, "budget", Some("t1orch")),
        old_item(2, "unreachable", Some("t1orch")),
        old_item(3, "gave_up", None),
    ]
    .into_iter()
    .chain([serde_json::to_value(item(4, ItemState::Queued(None))).unwrap()])
    .collect();
    let state = json!({
        "queue": queue, "batch": null, "blocked": null,
        "verified_bases": [], "preflights": {}, "warnings": []
    });
    fs::write(w.state_file(), state.to_string()).unwrap();
}

fn removed_events(w: &World) -> Vec<(PrNumber, RemovedReason)> {
    w.store()
        .history()
        .unwrap()
        .events
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Removed { pr, reason, .. } => Some((pr, reason)),
            _ => None,
        })
        .collect()
}

fn sends(w: &World) -> usize {
    w.sb.fake("hc")
        .calls()
        .iter()
        .filter(|c| c.first().is_some_and(|v| v == "send"))
        .count()
}

#[test]
fn an_old_state_file_with_human_turn_items_is_read_without_failing() {
    let w = World::new();
    write_old_state(&w);

    let status = w.status(w.repo.path());

    let prs: Vec<u64> = status["queue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["pr"].as_u64().unwrap())
        .collect();
    assert_eq!(prs, [4]);
    assert!(
        fs::read_to_string(w.state_file())
            .unwrap()
            .contains("needs_human"),
        "읽기만 해서는 파일을 고치지 않는다"
    );
    assert!(removed_events(&w).is_empty());
}

#[test]
fn the_first_change_to_the_queue_drops_the_old_items_and_records_each_once_without_telling_anyone()
{
    let w = World::new();
    write_old_state(&w);

    w.ok(w.repo.path(), &["drop", "4"]);

    let text = fs::read_to_string(w.state_file()).unwrap();
    assert!(!text.contains("needs_human"), "{text}");
    assert!(w.state().queue.is_empty());
    assert_eq!(
        removed_events(&w),
        [
            (PrNumber(1), RemovedReason::Upgrade),
            (PrNumber(2), RemovedReason::Upgrade),
            (PrNumber(3), RemovedReason::Upgrade),
        ]
    );
    let details: Vec<String> = w
        .store()
        .history()
        .unwrap()
        .events
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Removed {
                reason: RemovedReason::Upgrade,
                detail,
                report,
                ..
            } => {
                assert_eq!(report, None);
                detail
            }
            _ => None,
        })
        .collect();
    assert!(details[0].contains("budget"), "{details:?}");
    assert!(details[1].contains("unreachable"), "{details:?}");
    assert!(details[2].contains("gave_up"), "{details:?}");
    assert_eq!(sends(&w), 0);

    w.seed(vec![item(5, ItemState::Queued(None))]);
    assert_eq!(
        removed_events(&w).len(),
        3,
        "두 번째 쓰기는 다시 남기지 않는다"
    );
}

#[test]
fn a_write_cut_short_after_the_events_does_not_record_them_twice() {
    let w = World::new();
    write_old_state(&w);
    let before = fs::read_to_string(w.state_file()).unwrap();
    w.ok(w.repo.path(), &["drop", "4"]);
    assert_eq!(removed_events(&w).len(), 3);

    fs::write(w.state_file(), before).unwrap();
    w.ok(w.repo.path(), &["drop", "4"]);

    assert_eq!(removed_events(&w).len(), 3);
    assert!(w.state().queue.is_empty());
}

#[test]
fn an_old_item_submitted_again_is_a_new_item_with_no_bounces_counted() {
    let w = World::new();
    write_old_state(&w);
    let again = Submission {
        pr: PrNumber(1),
        title: "PR 1".into(),
        branch: "feat/1".into(),
        head: Sha::try_from("abc1234".to_owned()).unwrap(),
        agent: None,
        submitter: None,
        touches_config: false,
        config_change: None,
        approval: Approval::Agent,
        priority: false,
        after: Vec::new(),
        repair: false,
        now: EpochMs(9),
    };

    let done = w
        .store()
        .update(|state| Ok(queue::submit(state, again)?))
        .unwrap();

    assert_eq!(
        (done.state, done.change),
        (ItemState::Queued(None), Change::Added)
    );
    let state = w.state();
    let added = state.queue.iter().find(|i| i.pr == PrNumber(1)).unwrap();
    assert_eq!(added.attempts, 0);
    assert!(added.last_failure.is_none());
    assert_eq!(removed_events(&w).len(), 3);
}
