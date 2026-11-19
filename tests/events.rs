mod forward_kit;
mod support;

use chute::model::{BatchId, EpochMs, Event, EventKind, PrNumber};
use forward_kit::{BIN, err, out, store};
use serde_json::Value;

fn closed(at: u64, pr: u64) -> Event {
    Event {
        at_ms: EpochMs(at),
        kind: EventKind::Closed { pr: PrNumber(pr) },
    }
}

fn finished(at: u64, merged: &[u64]) -> Event {
    Event {
        at_ms: EpochMs(at),
        kind: EventKind::BatchFinished {
            batch: BatchId("b1".into()),
            merged: merged.iter().copied().map(PrNumber).collect(),
            bounced: vec![],
            seconds: 1,
            verify_s: 1,
            ran: vec![],
            verifications: 0,
            step_times: vec![],
        },
    }
}

fn seed(node: &support::Node, events: &[Event]) {
    for event in events {
        store(node).append_history(event).unwrap();
    }
}

fn json_lines(output: &std::process::Output) -> Vec<Value> {
    out(output)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn ats(lines: &[Value]) -> Vec<u64> {
    lines.iter().map(|l| l["at_ms"].as_u64().unwrap()).collect()
}

fn hub() -> (support::Net, &'static str) {
    (forward_kit::net(), "b")
}

#[test]
fn events_json_output_is_the_history_lines_one_per_line() {
    let (net, hub) = hub();
    let node = net.node(hub);
    let events = [closed(10, 1), finished(20, &[1, 2])];
    seed(node, &events);

    let output = node.chute(BIN, &["events", "--json"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    let expected: String = events
        .iter()
        .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
        .collect();
    assert_eq!(out(&output), expected);
    let raw = std::fs::read_to_string(store(node).dir().join("history.jsonl")).unwrap();
    assert_eq!(out(&output), raw);
}

#[test]
fn events_since_takes_epoch_millis_and_includes_the_same_instant() {
    let (net, hub) = hub();
    let node = net.node(hub);
    seed(
        node,
        &[
            closed(100, 1),
            closed(200, 2),
            closed(200, 3),
            closed(300, 4),
        ],
    );

    let output = node.chute(BIN, &["events", "--json", "--since", "200"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(ats(&json_lines(&output)), [200, 200, 300]);
}

#[test]
fn events_since_takes_rfc3339() {
    let (net, hub) = hub();
    let node = net.node(hub);
    seed(
        node,
        &[
            closed(1_700_000_000_000 - 1, 1),
            closed(1_700_000_000_000, 2),
        ],
    );

    let output = node.chute(
        BIN,
        &["events", "--json", "--since", "2023-11-15T07:13:20+09:00"],
    );

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(ats(&json_lines(&output)), [1_700_000_000_000]);
}

#[test]
fn events_since_takes_relative_times() {
    let (net, hub) = hub();
    let node = net.node(hub);
    let now = EpochMs::now().0;
    let hour = 3_600_000;
    seed(
        node,
        &[
            closed(now - 3 * hour, 1),
            closed(now - 90 * 60_000, 2),
            closed(now - 10 * 60_000, 3),
        ],
    );

    let by = |since: &str| {
        let output = node.chute(BIN, &["events", "--json", "--since", since]);
        assert_eq!(output.status.code(), Some(0), "{since}: {}", err(&output));
        json_lines(&output).len()
    };

    assert_eq!(by("30m"), 1);
    assert_eq!(by("2h"), 2);
    assert_eq!(by("1d"), 3);
}

#[test]
fn pr_keeps_only_events_naming_that_pr() {
    let (net, hub) = hub();
    let node = net.node(hub);
    seed(node, &[closed(1, 7), closed(2, 8), finished(3, &[8, 9])]);

    let output = node.chute(BIN, &["events", "--json", "--pr", "8"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(ats(&json_lines(&output)), [2, 3]);
}

#[test]
fn events_limit_keeps_the_most_recent_in_time_order_and_warns_on_stderr() {
    let (net, hub) = hub();
    let node = net.node(hub);
    seed(
        node,
        &[closed(1, 1), closed(2, 2), closed(3, 3), closed(4, 4)],
    );

    let output = node.chute(BIN, &["events", "--json", "--limit", "2"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(ats(&json_lines(&output)), [3, 4]);
    let said = err(&output);
    assert!(said.contains("가장 최근 2개만"), "{said}");
    assert!(said.contains("4개"), "{said}");

    let all = node.chute(BIN, &["events", "--json", "--limit", "4"]);
    assert_eq!(err(&all), "");
}

#[test]
fn events_the_default_limit_is_500() {
    let (net, hub) = hub();
    let node = net.node(hub);
    let events: Vec<Event> = (1..=502).map(|i| closed(i, i)).collect();
    seed(node, &events);

    let output = node.chute(BIN, &["events", "--json"]);

    let lines = json_lines(&output);
    assert_eq!(lines.len(), 500);
    assert_eq!(ats(&lines)[0], 3);
    assert!(
        err(&output).contains("가장 최근 500개만"),
        "{}",
        err(&output)
    );
}

#[test]
fn events_unreadable_history_lines_are_skipped_and_counted_on_stderr() {
    let (net, hub) = hub();
    let node = net.node(hub);
    seed(node, &[closed(1, 1)]);
    let path = store(node).dir().join("history.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("{\"at_ms\":2,\"event\":\"from_the_future\"}\nnot json\n");
    std::fs::write(&path, text).unwrap();
    seed(node, &[closed(3, 3)]);

    let output = node.chute(BIN, &["events", "--json"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(ats(&json_lines(&output)), [1, 3]);
    assert!(
        err(&output).contains("읽지 못한 줄이 2개"),
        "{}",
        err(&output)
    );
}

#[test]
fn events_a_bad_since_exits_2_and_says_why() {
    let (net, hub) = hub();
    let node = net.node(hub);

    for bad in ["yesterday", "5w", "2026-13-40", ""] {
        let output = node.chute(BIN, &["events", "--since", bad]);
        assert_eq!(output.status.code(), Some(2), "{bad:?}");
        assert!(
            err(&output).contains("--since"),
            "{bad:?}: {}",
            err(&output)
        );
        assert_eq!(out(&output), "", "{bad:?}");
    }
}

#[test]
fn events_a_bad_since_is_refused_before_anything_goes_to_the_hub() {
    let net = forward_kit::net();
    net.set_state("a", "b", "unreachable");

    let output = net
        .node("a")
        .chute(BIN, &["events", "--since", "yesterday"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(err(&output).contains("--since"), "{}", err(&output));
    assert!(
        !err(&output).contains("기록되지 않았다"),
        "{}",
        err(&output)
    );
}

#[test]
fn events_a_human_readable_line_per_event_says_when_and_what() {
    let (net, hub) = hub();
    let node = net.node(hub);
    seed(node, &[closed(1_700_000_000_000, 5)]);

    let output = node.chute(BIN, &["events"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    let text = out(&output);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("#5 닫힘"), "{text}");
}

#[test]
fn events_on_another_pc_show_the_hubs_history_and_not_this_pcs() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    seed(b, &[closed(10, 1), closed(20, 2)]);
    seed(a, &[closed(30, 99)]);

    let remote = a.chute(BIN, &["events", "--json"]);
    let direct = b.chute(BIN, &["events", "--json"]);

    assert_eq!(remote.status.code(), Some(0), "{}", err(&remote));
    assert_eq!(remote.stdout, direct.stdout);
    assert_eq!(ats(&json_lines(&remote)), [10, 20]);
    let plain = a.chute(BIN, &["events"]);
    assert_eq!(plain.stdout, b.chute(BIN, &["events"]).stdout);
}

#[test]
fn events_on_another_pc_pass_the_filters_and_the_warnings_on() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    seed(
        b,
        &[closed(10, 1), closed(20, 2), closed(30, 2), closed(40, 2)],
    );

    let output = a.chute(
        BIN,
        &[
            "events", "--json", "--pr", "2", "--since", "15", "--limit", "2",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(ats(&json_lines(&output)), [30, 40]);
    assert!(err(&output).contains("가장 최근 2개만"), "{}", err(&output));
}

#[test]
fn events_on_another_pc_turn_a_relative_since_into_one_instant_on_this_pc() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    let now = EpochMs::now().0;
    seed(b, &[closed(now - 3_600_000, 1), closed(now - 60_000, 2)]);

    let output = a.chute(BIN, &["events", "--json", "--since", "30m"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(json_lines(&output).len(), 1);
}

#[test]
fn events_on_another_pc_say_so_when_the_hub_is_down_or_this_is_not_the_hub() {
    let net = forward_kit::net();
    forward_kit::assert_unreachable_hub(&net, &["events"]);
    forward_kit::assert_not_hub_answer(&net, &["events"]);
}

const OLD_AND_NEW_LINES: [&str; 8] = [
    r#"{"at_ms":1,"event":"needs_human","pr":5,"reason":"budget","detail":"재시도 예산 3번을 모두 썼다"}"#,
    r#"{"at_ms":2,"event":"gave_up","pr":6,"reason":"충돌을 풀 수 없다"}"#,
    r#"{"at_ms":3,"event":"notified","kind":"human","pr":5,"to":"t1orch","delivered":"sent"}"#,
    r#"{"at_ms":4,"event":"removed","pr":7,"reason":"budget","detail":"반송 한도 3번을 넘었다","report":"/state/reports/pr-7/3.md"}"#,
    r#"{"at_ms":5,"event":"removed","pr":8,"reason":"gave_up","detail":"그만둔다","report":null}"#,
    r#"{"at_ms":6,"event":"removed","pr":9,"reason":"upgrade","detail":"옛 chute가 사람 차례(budget)로 둔 항목","report":null}"#,
    r#"{"at_ms":7,"event":"bounced","pr":10,"batch":null,"reason":"verify","attempts":2,"unreachable":"작업자 터미널이 없다"}"#,
    r#"{"at_ms":8,"event":"notified","kind":"removed","pr":7,"to":"t1orch","delivered":"sent"}"#,
];

#[test]
fn events_show_the_lines_an_older_chute_wrote_next_to_the_removed_and_unreachable_ones() {
    let (net, hub) = hub();
    let node = net.node(hub);
    let path = store(node).dir().join("history.jsonl");
    std::fs::write(&path, OLD_AND_NEW_LINES.join("\n") + "\n").unwrap();

    let json = node.chute(BIN, &["events", "--json"]);
    let text = node.chute(BIN, &["events"]);

    assert_eq!(json.status.code(), Some(0), "{}", err(&json));
    assert_eq!(ats(&json_lines(&json)), [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(text.status.code(), Some(0), "{}", err(&text));
    assert!(!err(&text).contains("읽지 못한 줄"), "{}", err(&text));
    let shown = out(&text);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), 8, "{shown}");
    for (line, needed) in lines.iter().zip([
        &["#5", "사람 차례"][..],
        &["#6", "포기됨", "충돌을 풀 수 없다"],
        &["사람 차례"],
        &[
            "#7",
            "큐에서 빠짐",
            "반송 한도 초과",
            "/state/reports/pr-7/3.md",
        ],
        &["#8", "큐에서 빠짐", "작업자가 포기", "그만둔다"],
        &["#9", "큐에서 빠짐", "옛 사람 차례 항목 정리"],
        &["#10", "반송됨", "닿지 않아", "작업자 터미널이 없다"],
        &["큐에서 빠짐"],
    ]) {
        for part in needed {
            assert!(line.contains(part), "{part}: {line}");
        }
    }
}

#[test]
fn events_pr_finds_a_removed_event_by_its_pr() {
    let (net, hub) = hub();
    let node = net.node(hub);
    let path = store(node).dir().join("history.jsonl");
    std::fs::write(&path, OLD_AND_NEW_LINES.join("\n") + "\n").unwrap();

    let output = node.chute(BIN, &["events", "--json", "--pr", "7"]);

    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    assert_eq!(ats(&json_lines(&output)), [4, 8]);
}
