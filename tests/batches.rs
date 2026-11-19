mod forward_kit;
mod support;

use chute::model::{BatchId, BatchRecord, EpochMs, PrNumber, Sha};
use forward_kit::{BIN, err, out, store};
use serde_json::Value;

fn record(id: &str, finished: u64) -> BatchRecord {
    BatchRecord {
        id: BatchId(id.to_owned()),
        base: Sha::try_from("abc1234".to_owned()).unwrap(),
        prs: vec![PrNumber(5)],
        started_ms: EpochMs(finished - 1),
        finished_ms: Some(EpochMs(finished)),
        merges: Vec::new(),
        bounced: vec![PrNumber(5)],
        flaky: Vec::new(),
        verify_s: 1,
        pushed: Vec::new(),
        verifications: 1,
        skipped: Vec::new(),
        reused: Vec::new(),
        reruns: Vec::new(),
        step_times: Vec::new(),
        env: None,
    }
}

#[test]
fn batches_on_another_pc_show_the_hubs_history_and_not_this_pcs() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    store(b).write_batch(&record("hub-batch", 2000)).unwrap();
    store(a).write_batch(&record("local-batch", 3000)).unwrap();

    let remote = a.chute(BIN, &["batches", "--json"]);
    let direct = b.chute(BIN, &["batches", "--json"]);

    assert_eq!(remote.status.code(), Some(0), "{}", err(&remote));
    assert_eq!(remote.stdout, direct.stdout);
    let listed: Value = serde_json::from_str(&out(&remote)).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], "hub-batch");

    let plain = a.chute(BIN, &["batches"]);
    assert_eq!(plain.stdout, b.chute(BIN, &["batches"]).stdout);
    assert!(out(&plain).contains("hub-batch"), "{}", out(&plain));
}

#[test]
fn batches_on_another_pc_pass_the_limit_on() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    store(b).write_batch(&record("older", 2000)).unwrap();
    store(b).write_batch(&record("newer", 3000)).unwrap();

    let out = a.chute(BIN, &["batches", "--json", "-n", "1"]);

    assert_eq!(out.status.code(), Some(0), "{}", err(&out));
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], "newer");
}

#[test]
fn batches_on_another_pc_says_so_when_the_hub_is_down_or_this_is_not_the_hub() {
    let net = forward_kit::net();
    forward_kit::assert_unreachable_hub(&net, &["batches"]);
    forward_kit::assert_not_hub_answer(&net, &["batches"]);
}
