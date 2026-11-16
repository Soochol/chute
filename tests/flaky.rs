mod forward_kit;
mod support;

use chute::model::{BatchId, EpochMs, Event, EventKind, ProbeEvidence, Sha};
use forward_kit::{BIN, err, out, store};
use serde_json::Value;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn flaky(days_ago: u64, batch: &str, tests: &[&str]) -> Event {
    Event {
        at_ms: EpochMs(EpochMs::now().0 - days_ago * DAY_MS),
        kind: EventKind::Flaky {
            batch: BatchId(batch.into()),
            step: "test".into(),
            tests: tests.iter().map(|t| (*t).to_owned()).collect(),
            tree: Sha::try_from("a".repeat(40)).unwrap(),
        },
    }
}

fn probed(days_ago: u64, batch: &str, tests: &[&str]) -> Event {
    Event {
        at_ms: EpochMs(EpochMs::now().0 - days_ago * DAY_MS),
        kind: EventKind::FlakyProbe {
            batch: BatchId(batch.into()),
            step: "test".into(),
            tests: tests.iter().map(|t| (*t).to_owned()).collect(),
            evidence: ProbeEvidence::Made,
            runs: 5,
            failures: 1,
        },
    }
}

fn seed(node: &support::Node, events: &[Event]) {
    for event in events {
        store(node).append_history(event).unwrap();
    }
}

fn ledger() -> Vec<Event> {
    vec![
        flaky(20, "b1", &["a"]),
        flaky(10, "b2", &["a", "b"]),
        probed(10, "b2", &["a", "b"]),
        flaky(1, "b3", &["a"]),
        flaky(1, "b3", &[]),
    ]
}

fn json(output: &std::process::Output) -> Value {
    serde_json::from_str(&out(output)).unwrap()
}

#[test]
fn flaky_json_gives_the_ledger_with_counts_evidence_and_chronic() {
    let net = forward_kit::net();
    let hub = net.node("b");
    seed(hub, &ledger());

    let output = hub.chute(BIN, &["flaky", "--json"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    let report = json(&output);
    assert_eq!(report["days"], 30);
    assert_eq!(report["chronic"], 3);
    assert_eq!(report["chronic_days"], 30);
    let rows: Vec<(String, Option<String>, u64, bool)> = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["step"].as_str().unwrap().to_owned(),
                e["test"].as_str().map(str::to_owned),
                e["count"].as_u64().unwrap(),
                e["chronic"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("test".to_owned(), Some("a".to_owned()), 3, true),
            ("test".to_owned(), None, 1, false),
            ("test".to_owned(), Some("b".to_owned()), 1, false),
        ]
    );
    let a = &report["entries"][0];
    assert_eq!(a["last_batch"], "b3");
    assert!(a["last_ms"].as_u64().unwrap() > 0);
}

#[test]
fn flaky_days_narrows_the_window_and_a_plain_run_is_korean_text() {
    let net = forward_kit::net();
    let hub = net.node("b");
    seed(hub, &ledger());

    let output = hub.chute(BIN, &["flaky", "--json", "--days", "5"]);
    let report = json(&output);

    assert_eq!(report["days"], 5);
    assert_eq!(report["entries"][0]["count"], 1);
    let plain = out(&hub.chute(BIN, &["flaky"]));
    assert!(plain.starts_with("최근 30일에 흔들린 시험"), "{plain}");
    assert!(plain.contains("단계 test의 시험 a · 3번"), "{plain}");
    assert!(plain.contains("만성"), "{plain}");
    assert!(plain.contains("흔들림을 재현함"), "{plain}");
}

#[test]
fn flaky_on_another_pc_shows_the_hubs_ledger_and_not_this_pcs() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    seed(b, &ledger());
    seed(a, &[flaky(1, "other", &["zzz"])]);

    let remote = a.chute(BIN, &["flaky", "--json", "--days", "40"]);
    let direct = b.chute(BIN, &["flaky", "--json", "--days", "40"]);

    assert_eq!(remote.status.code(), Some(0), "{}", err(&remote));
    assert_eq!(remote.stdout, direct.stdout);
    assert!(!out(&remote).contains("zzz"));
    assert_eq!(json(&remote)["days"], 40);
    assert_eq!(json(&remote)["entries"][0]["count"], 3);
}

#[test]
fn flaky_with_nothing_flaky_says_so() {
    let net = forward_kit::net();
    let hub = net.node("b");

    let output = hub.chute(BIN, &["flaky"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(out(&output), "최근 30일에 흔들린 시험이 없다\n");
    assert_eq!(
        json(&hub.chute(BIN, &["flaky", "--json"]))["entries"],
        serde_json::json!([])
    );
}

#[test]
fn flaky_asked_of_a_pc_that_is_not_the_hub_answers_not_hub_and_an_unreachable_hub_fails() {
    let net = forward_kit::net();

    forward_kit::assert_not_hub_answer(&net, &["flaky", "--json"]);
    net.set_state("a", "b", "unreachable");
    let output = net.node("a").chute(BIN, &["flaky"]);
    net.set_state("a", "b", "connected");

    assert_eq!(output.status.code(), Some(2), "{}", err(&output));
}
