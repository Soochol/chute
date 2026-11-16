#![allow(dead_code)]

use std::fs;
use std::process::Output;

use chute::model::{QueueItem, Slug, State};
use chute::store::{Home, RepoStore};

use crate::support::{Net, Node};

pub const BIN: &str = env!("CARGO_BIN_EXE_chute");
const SLUG: &str = "acme/widget";

/// Machine `a` has `b` as its hub. Both have the repository registered and know each other.
pub fn net() -> Net {
    let net = Net::new(SLUG, &["a", "b"], BIN);
    net.connect_all();
    for node in &net.nodes {
        assert!(node.chute(BIN, &["init"]).status.success());
    }
    let (a, b) = (net.node("a"), net.node("b"));
    store(a).set_hub(b.id.clone().try_into().unwrap()).unwrap();
    net
}

pub fn store(node: &Node) -> RepoStore {
    Home::at(node.sb.chute_home()).repo(&Slug::new("acme", "widget").unwrap())
}

pub fn seed(node: &Node, items: Vec<QueueItem>) {
    store(node)
        .update(|state| {
            state.queue.extend(items);
            Ok(())
        })
        .unwrap();
}

pub fn state(node: &Node) -> State {
    store(node).load().unwrap()
}

/// What `state.json` holds right now, or `None` when it was never written.
pub fn state_file(node: &Node) -> Option<String> {
    fs::read_to_string(store(node).dir().join("state.json")).ok()
}

pub fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Runs `args` on `a` while the hub `b` cannot be reached: the command ends with exit code 2 and
/// says it was not recorded.
pub fn assert_unreachable_hub(net: &Net, args: &[&str]) {
    net.set_state("a", "b", "unreachable");
    let out = net.node("a").chute(BIN, args);
    net.set_state("a", "b", "connected");
    assert_eq!(out.status.code(), Some(2), "{args:?}: {}", err(&out));
    let said = err(&out);
    assert!(said.contains("기록되지 않았다"), "{args:?}: {said}");
    assert!(said.contains("허브 `b`"), "{args:?}: {said}");
}

/// A forwarded call that reaches a PC that is not the hub answers exit code 3 and names the hub.
pub fn assert_not_hub_answer(net: &Net, args: &[&str]) {
    let a = net.node("a");
    let before = state_file(a);
    let out =
        a.sb.command(BIN)
            .current_dir(a.repo.path())
            .env("CHUTE_FORWARDED", "1")
            .args(args)
            .output()
            .unwrap();
    assert_eq!(out.status.code(), Some(3), "{args:?}: {}", err(&out));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
        serde_json::json!({"not_hub": {"hub": net.node("b").id}}),
        "{args:?}"
    );
    assert_eq!(state_file(a), before, "{args:?}");
}
