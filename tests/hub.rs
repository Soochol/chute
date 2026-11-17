mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{ExitCode, Output};
use std::time::Duration;

use chute::cmd::{Ctx, Refusal, forward_to_hub, run_on_hub};
use chute::ext::Ext;
use chute::hub::{self, ForwardError, ForwardErrorKind, Route};
use chute::model::{MachineId, Slug};
use chute::store::Home;
use serde_json::Value;
use support::{Net, Node};

const BIN: &str = env!("CARGO_BIN_EXE_chute");
const SLUG: &str = "acme/widget";
const WAIT: Duration = Duration::from_secs(30);

fn net(names: &[&str]) -> Net {
    let net = Net::new(SLUG, names, BIN);
    for node in &net.nodes {
        ok(&node.chute(BIN, &["init"]));
    }
    net
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "종료 코드 {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn home_of(node: &Node) -> Home {
    Home::at(node.sb.chute_home())
}

fn ext_of(node: &Node) -> Ext {
    Ext::from_vars(|k| node.sb.var(k))
}

fn slug() -> Slug {
    Slug::try_from(SLUG.to_owned()).unwrap()
}

fn id(node: &Node) -> MachineId {
    MachineId::try_from(node.id.clone()).unwrap()
}

fn hub_json(node: &Node) -> PathBuf {
    node.sb.chute_home().join("repos/acme__widget/hub.json")
}

fn point_at(from: &Node, to: &Node) {
    home_of(from).repo(&slug()).set_hub(id(to)).unwrap();
}

fn hub_of(node: &Node) -> Option<MachineId> {
    home_of(node).repo(&slug()).hub().unwrap()
}

fn remote(to: &Node) -> Route {
    Route::Remote {
        id: id(to),
        name: to.name.clone(),
    }
}

fn forward(from: &Node, to: &Route, args: &[&str]) -> anyhow::Result<hub::Forwarded> {
    hub::forward(&home_of(from), &ext_of(from), &slug(), to, args, WAIT)
}

fn ctx(node: &Node) -> Ctx {
    Ctx::new(
        home_of(node),
        ext_of(node),
        Some(node.repo.path().to_owned()),
    )
}

fn kind(err: &anyhow::Error) -> ForwardErrorKind {
    err.downcast_ref::<ForwardError>()
        .unwrap_or_else(|| panic!("ForwardError가 아니다: {err:#}"))
        .kind
}

#[test]
fn without_hub_json_this_pc_is_the_hub() {
    let net = net(&["a", "b"]);
    let a = net.node("a");
    let hub = json(&ok(&a.chute(BIN, &["hub", "--json"])));
    assert_eq!(hub["local"], true);
    assert_eq!(hub["machine_id"], a.id);
    assert_eq!(hub["host"], "a");
    assert_eq!(hub["machine"], Value::Null);
    assert_eq!(hub["reachable"], true);
    assert!(!hub_json(a).exists());
    assert!(ok(&a.chute(BIN, &["hub"])).contains("이 PC"));
}

#[test]
fn hub_set_writes_hub_json_after_the_checks_and_local_undoes_it() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));

    ok(&a.chute(BIN, &["hub", "set", "b"]));
    assert_eq!(
        json(&fs::read_to_string(hub_json(a)).unwrap()),
        json(&format!(r#"{{"machine_id":"{}"}}"#, b.id))
    );
    let shown = json(&ok(&a.chute(BIN, &["hub", "--json"])));
    assert_eq!(
        (
            shown["local"].clone(),
            shown["machine"].clone(),
            shown["machine_id"].clone()
        ),
        (
            Value::Bool(false),
            Value::from("b"),
            Value::from(b.id.clone())
        )
    );
    assert_eq!(shown["reachable"], true);
    assert!(ok(&a.chute(BIN, &["hub"])).contains("허브: b"));

    ok(&a.chute(BIN, &["hub", "set", "--local"]));
    assert!(!hub_json(a).exists());
    ok(&a.chute(BIN, &["hub", "set", "--local"]));
    assert_eq!(json(&ok(&a.chute(BIN, &["hub", "--json"])))["local"], true);
}

const BATCH_ONLY: &str = r#"{
  "queue": [],
  "batch": {"id": "b1", "base": "abc1234", "prs": [7], "phase": "verifying", "step": null,
            "round": 1, "started_ms": 1},
  "blocked": null, "verified_bases": [], "preflights": {}, "warnings": []
}"#;

const QUEUED: &str = r#"{
  "queue": [{
    "pr": 7, "title": "t", "branch": "feat/x", "head": "abc1234",
    "state": "queued", "state_reason": null, "agent": null, "attempts": 0,
    "submitted_ms": 10, "touches_config": false, "last_failure": null
  }],
  "batch": null, "blocked": null, "verified_bases": [], "preflights": {}, "warnings": []
}"#;

fn refused(case: &str, machine: &str, setup: impl Fn(&Net), reason: &str) {
    let net = Net::new(SLUG, &["a", "b", "c"], BIN);
    net.connect_all();
    ok(&net.node("a").chute(BIN, &["init"]));
    ok(&net.node("b").chute(BIN, &["init"]));
    ok(&net.node("c").chute(BIN, &["init"]));
    setup(&net);
    let a = net.node("a");
    let out = a.chute(BIN, &["hub", "set", machine]);
    assert_eq!(out.status.code(), Some(1), "{case}: {}", stderr(&out));
    assert!(stderr(&out).contains(reason), "{case}: {}", stderr(&out));
    assert!(!hub_json(a).exists(), "{case}");
}

#[test]
fn hub_set_refuses_an_unknown_machine() {
    refused(
        "모르는 머신",
        "nosuch",
        |_| {},
        "hc가 머신 `nosuch`을 모른다",
    );
}

#[test]
fn hub_set_refuses_a_machine_that_is_not_connected() {
    for state in ["unreachable", "needs_login", "connecting"] {
        refused(
            state,
            "b",
            |net| net.set_state("a", "b", state),
            &format!("연결돼 있지 않다(상태: {state})"),
        );
    }
}

#[test]
fn hub_set_refuses_a_machine_where_the_repository_is_not_registered() {
    refused(
        "등록 안 됨",
        "b",
        |net| {
            fs::remove_dir_all(net.node("b").sb.chute_home().join("repos")).unwrap();
        },
        "chute init",
    );
}

#[test]
fn hub_set_refuses_a_machine_that_is_not_the_hub_either() {
    refused(
        "허브 아님",
        "b",
        |net| point_at(net.node("b"), net.node("c")),
        "`b`도 허브가 아니다",
    );
}

#[test]
fn hub_set_refuses_a_machine_that_points_back_at_this_pc() {
    refused(
        "되가리킴",
        "b",
        |net| point_at(net.node("b"), net.node("a")),
        "`b`도 허브가 아니다",
    );
}

#[test]
fn hub_set_refuses_while_this_pcs_queue_holds_items() {
    refused(
        "큐가 비지 않음",
        "b",
        |net| {
            fs::write(
                net.node("a")
                    .sb
                    .chute_home()
                    .join("repos/acme__widget/state.json"),
                QUEUED,
            )
            .unwrap();
        },
        "큐에 항목이 1개 남아 있다",
    );
}

#[test]
fn hub_set_refuses_while_a_batch_runs_and_says_so() {
    refused(
        "batch만 남음",
        "b",
        |net| {
            fs::write(
                net.node("a")
                    .sb
                    .chute_home()
                    .join("repos/acme__widget/state.json"),
                BATCH_ONLY,
            )
            .unwrap();
        },
        "진행 중인 batch가 남아 있다",
    );
}

#[test]
fn hub_set_refuses_a_machine_running_another_chute_version() {
    refused(
        "버전",
        "b",
        |net| {
            let old = net.node("b").sb.script("old-chute", "echo 'chute 0.0.0'\n");
            net.node("b").set_chute(&old.to_string_lossy());
        },
        "chute 버전이 이 PC와 다르다",
    );
}

#[test]
fn forward_returns_what_the_hub_wrote_and_its_exit_code() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    let to_b = remote(b);

    let zero = forward(a, &to_b, &["hub", "--json"]).unwrap();
    assert_eq!(zero.code, 0);
    let seen = json(&String::from_utf8(zero.stdout).unwrap());
    assert_eq!(
        (seen["local"].clone(), seen["machine_id"].clone()),
        (Value::Bool(true), Value::from(b.id.clone()))
    );
    assert!(zero.stderr.is_empty());

    let one = forward(a, &to_b, &["approve", "99"]).unwrap();
    assert_eq!(one.code, 1);
    assert!(one.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&one.stderr).contains("99"),
        "{:?}",
        String::from_utf8_lossy(&one.stderr)
    );

    let unregistered = Slug::try_from("zed/none".to_owned()).unwrap();
    let two = hub::forward(
        &home_of(a),
        &ext_of(a),
        &unregistered,
        &to_b,
        &["status"],
        WAIT,
    )
    .unwrap();
    assert_eq!(two.code, 2);
    assert!(
        String::from_utf8_lossy(&two.stderr).contains("chute에 등록되지 않은 저장소다: zed/none")
    );
}

#[test]
fn a_forwarded_call_to_a_pc_that_is_not_the_hub_answers_exit_3_and_names_the_hub() {
    let net = net(&["a", "b"]);
    let (a, b) = (net.node("a"), net.node("b"));
    point_at(b, a);
    let out =
        b.sb.command(BIN)
            .env("CHUTE_FORWARDED", "1")
            .args(["--slug", SLUG, "approve", "1"])
            .output()
            .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert_eq!(
        json(&String::from_utf8(out.stdout).unwrap()),
        json(&format!(r#"{{"not_hub":{{"hub":"{}"}}}}"#, a.id))
    );
    assert!(out.stderr.is_empty());

    let on_hub =
        a.sb.command(BIN)
            .env("CHUTE_FORWARDED", "1")
            .args(["--slug", SLUG, "approve", "1"])
            .output()
            .unwrap();
    assert_eq!(on_hub.status.code(), Some(1), "{}", stderr(&on_hub));
}

#[test]
fn a_hub_that_moved_is_followed_once_and_hub_json_is_corrected() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b, c) = (net.node("a"), net.node("b"), net.node("c"));
    point_at(a, b);
    point_at(b, c);

    let answer = forward(a, &remote(b), &["hub", "--json"]).unwrap();
    assert_eq!(answer.code, 0);
    let seen = json(&String::from_utf8(answer.stdout).unwrap());
    assert_eq!(seen["machine_id"], c.id, "답한 쪽은 c여야 한다");
    assert_eq!(hub_of(a), Some(id(c)));
}

#[test]
fn a_moved_hub_that_this_pc_does_not_know_is_not_followed() {
    let net = net(&["a", "b", "c"]);
    net.connect("a", "b", "b");
    let (a, b, c) = (net.node("a"), net.node("b"), net.node("c"));
    net.connect("b", "c", "c");
    point_at(a, b);
    point_at(b, c);

    let err = forward(a, &remote(b), &["status"]).unwrap_err();
    assert_eq!(kind(&err), ForwardErrorKind::UnknownMachine);
    assert!(err.to_string().contains("hc machine add"), "{err}");
    assert_eq!(hub_of(a), Some(id(b)));
}

#[test]
fn two_machines_pointing_at_each_other_end_instead_of_looping() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    point_at(a, b);
    point_at(b, a);

    let err = forward(a, &remote(b), &["status"]).unwrap_err();
    assert_eq!(kind(&err), ForwardErrorKind::Misconfigured);
    assert!(err.to_string().contains("서로를 가리킨다"), "{err}");
    assert_eq!(hub_of(a), Some(id(b)));
    assert_eq!(hub_of(b), Some(id(a)));
}

#[test]
fn a_ring_of_three_is_followed_once_then_ends_without_rewriting_hub_json() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b, c) = (net.node("a"), net.node("b"), net.node("c"));
    point_at(a, b);
    point_at(b, c);
    point_at(c, a);

    let err = forward(a, &remote(b), &["status"]).unwrap_err();
    assert_eq!(kind(&err), ForwardErrorKind::Misconfigured);
    assert_eq!(hub_of(a), Some(id(b)));
}

#[test]
fn forwarding_to_a_disconnected_machine_fails_in_korean_and_is_not_a_refusal() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    point_at(a, b);
    net.set_state("a", "b", "unreachable");

    let err = run_on_hub(&ctx(a), &["approve", "1"]).unwrap_err();
    assert_eq!(kind(&err), ForwardErrorKind::Unreachable);
    let text = format!("{err:#}");
    for part in ["허브 `b`에 닿지 않는다", "기록되지 않았다", "hc machine ls"] {
        assert!(text.contains(part), "{text}");
    }
    assert!(err.downcast_ref::<Refusal>().is_none());

    net.set_state("a", "b", "needs_login");
    let err = run_on_hub(&ctx(a), &["approve", "1"]).unwrap_err();
    assert!(format!("{err:#}").contains("hc machine login b"), "{err:#}");
}

#[test]
fn each_hc_failure_is_told_apart() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    point_at(a, b);

    fs::remove_file(b.sb.root().join("net/chute")).unwrap();
    let err = run_on_hub(&ctx(a), &["status"]).unwrap_err();
    assert_eq!(kind(&err), ForwardErrorKind::NoChute);
    assert!(format!("{err:#}").contains("PATH"), "{err:#}");

    let slow = b.sb.script("slow-chute", "sleep 20\n");
    b.set_chute(&slow.to_string_lossy());
    let err = forward_to_hub(&ctx(a), &["status"], Duration::from_secs(1)).unwrap_err();
    assert_eq!(kind(&err), ForwardErrorKind::TimedOut);
    assert!(format!("{err:#}").contains("1초"), "{err:#}");

    let nobody = MachineId::try_from("f".repeat(32)).unwrap();
    home_of(a).repo(&slug()).set_hub(nobody).unwrap();
    let err = run_on_hub(&ctx(a), &["status"]).unwrap_err();
    assert_eq!(kind(&err), ForwardErrorKind::UnknownMachine);
}

#[test]
fn a_relayed_answer_that_was_cut_short_ends_as_cannot_run_and_says_so() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    point_at(a, b);

    let whole = run_on_hub(&ctx(a), &["status"]).unwrap().unwrap();
    assert_eq!(format!("{whole:?}"), format!("{:?}", ExitCode::SUCCESS));

    a.truncate_exec_output();
    let cut = run_on_hub(&ctx(a), &["status"]).unwrap().unwrap();
    assert_eq!(format!("{cut:?}"), format!("{:?}", ExitCode::from(2)));
}

#[test]
fn the_hub_runs_its_own_commands_and_a_forwarded_call_is_never_sent_on() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));

    assert!(run_on_hub(&ctx(a), &["status"]).unwrap().is_none());
    point_at(a, a);
    assert!(run_on_hub(&ctx(a), &["status"]).unwrap().is_none());
    point_at(a, b);
    assert!(
        run_on_hub(&ctx(a).with_forwarded(true), &["status"])
            .unwrap()
            .is_none()
    );
    assert!(
        forward_to_hub(&ctx(a), &["status"], WAIT)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_machine_is_named_by_each_pc_in_its_own_words() {
    let net = net(&["a", "b"]);
    net.connect("a", "b", "desk");
    net.connect("b", "a", "home");
    let (a, b) = (net.node("a"), net.node("b"));

    ok(&a.chute(BIN, &["hub", "set", "desk"]));
    assert_eq!(hub_of(a), Some(id(b)));
    let shown = json(&ok(&a.chute(BIN, &["hub", "--json"])));
    assert_eq!(shown["machine"], "desk");
    let answer = forward(
        a,
        &Route::Remote {
            id: id(b),
            name: "desk".into(),
        },
        &["hub", "--json"],
    )
    .unwrap();
    assert_eq!(answer.code, 0);
}

#[test]
fn status_shows_the_hub_and_where_each_agent_lives() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    let item = |pr: u64, machine: Value| {
        serde_json::json!({
            "pr": pr, "title": "t", "branch": format!("feat/{pr}"), "head": "abc1234",
            "state": "queued", "state_reason": null,
            "agent": {"terminal": "t1rs", "created_ms": 5, "name": "impl", "workdir": "/w",
                      "alive": true, "machine": machine},
            "attempts": 0, "submitted_ms": pr, "touches_config": false, "last_failure": null
        })
    };
    let state = serde_json::json!({
        "queue": [item(1, Value::Null), item(2, Value::from(b.id.clone())), item(3, Value::from("f".repeat(32)))],
        "batch": null, "blocked": null, "verified_bases": [], "preflights": {}, "warnings": []
    });
    fs::write(
        a.sb.chute_home().join("repos/acme__widget/state.json"),
        state.to_string(),
    )
    .unwrap();

    let status = json(&ok(&a.chute(BIN, &["status", "--json"])));
    assert_eq!(
        status["hub"],
        serde_json::json!({
            "local": true, "machine_id": a.id, "host": "a", "machine": null,
            "repo_path": a.repo.path().canonicalize().unwrap(), "reachable": true, "error": null
        })
    );
    let agent = |n: usize| {
        let a = &status["queue"][n]["agent"];
        [
            a["machine_id"].clone(),
            a["host"].clone(),
            a["machine"].clone(),
        ]
    };
    assert_eq!(
        agent(0),
        [Value::from(a.id.clone()), "a".into(), Value::Null]
    );
    assert_eq!(
        agent(1),
        [Value::from(b.id.clone()), "b".into(), "b".into()]
    );
    assert_eq!(
        agent(2),
        [Value::from("f".repeat(32)), Value::Null, Value::Null]
    );
}

#[test]
fn status_still_works_when_hc_cannot_say_who_this_pc_is() {
    let net = net(&["a"]);
    let a = net.node("a");
    let out =
        a.sb.command(BIN)
            .env("CHUTE_HC", "/nonexistent/hc")
            .current_dir(a.repo.path())
            .args(["status", "--json"])
            .output()
            .unwrap();
    let status = json(&ok(&out));
    assert_eq!(status["hub"]["local"], true);
    assert_eq!(status["hub"]["machine_id"], Value::Null);
    let warning = status["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["kind"] == "hub")
        .unwrap_or_else(|| panic!("hub 경고가 없다: {status}"));
    assert!(
        warning["message"]
            .as_str()
            .unwrap()
            .contains("머신 id를 읽지 못했다")
    );
}

fn item_json(pr: u32, state: &str, reason: Value, agent: Value) -> Value {
    serde_json::json!({
        "pr": pr, "title": format!("pr {pr}"), "branch": format!("feat/{pr}"),
        "head": "abc1234", "state": state, "state_reason": reason, "agent": agent,
        "attempts": 0, "submitted_ms": 10 + u64::from(pr), "touches_config": false,
        "last_failure": null
    })
}

fn agent_json(machine: Option<&str>) -> Value {
    let mut agent = serde_json::json!({
        "terminal": "t7", "created_ms": 5, "name": "w", "workdir": "/w", "alive": true
    });
    if let Some(id) = machine {
        agent["machine"] = Value::from(id);
    }
    agent
}

fn seed(node: &Node, state: Value) {
    let state = serde_json::from_value(state).unwrap();
    home_of(node)
        .repo(&slug())
        .update(|s| {
            *s = state;
            Ok(())
        })
        .unwrap();
}

fn state_of(node: &Node) -> Value {
    serde_json::to_value(home_of(node).repo(&slug()).load().unwrap()).unwrap()
}

fn history_of(node: &Node) -> Vec<chute::model::Event> {
    home_of(node).repo(&slug()).history().unwrap().events
}

fn events(n: u64) -> Vec<chute::model::Event> {
    use chute::model::{BatchId, EpochMs, Event, EventKind, PrNumber, Sha};
    (1..=n)
        .map(|i| Event {
            at_ms: EpochMs(i),
            kind: EventKind::Merged {
                pr: PrNumber(i),
                batch: BatchId("b0".to_owned()),
                commit: Sha::try_from("abc1234".to_owned()).unwrap(),
                submitted_ms: EpochMs(1),
                title: None,
            },
        })
        .collect()
}

fn three_items(c: &Node) -> Value {
    let mut first = item_json(1, "queued", Value::Null, agent_json(Some(&c.id)));
    first["submitter"] = agent_json(None);
    serde_json::json!({
        "queue": [
            first,
            item_json(2, "bounced", Value::from("conflict"), agent_json(Some(&c.id))),
            item_json(3, "queued", Value::Null, agent_json(None)),
        ],
        "batch": null, "blocked": null, "verified_bases": [], "preflights": {}, "warnings": []
    })
}

fn old_hub_with_items(net: &Net) -> Value {
    let (a, c) = (net.node("a"), net.node("c"));
    seed(a, three_items(c));
    let state = state_of(a);
    let store = home_of(a).repo(&slug());
    for event in events(2) {
        store.append_history(&event).unwrap();
    }
    state
}

fn take(b: &Node, from: &str) -> Output {
    b.chute(BIN, &["hub", "take", from])
}

fn with_a_filled_in(mut state: Value, a: &Node) -> Value {
    for item in state["queue"].as_array_mut().unwrap() {
        if item["agent"]["machine"].is_null() {
            item["agent"]["machine"] = Value::from(a.id.clone());
        }
        if item["submitter"]["terminal"].is_string() && item["submitter"]["machine"].is_null() {
            item["submitter"]["machine"] = Value::from(a.id.clone());
        }
    }
    state
}

#[test]
fn take_moves_the_queue_and_the_history_and_makes_this_pc_the_hub() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    let state = old_hub_with_items(&net);

    let out = take(b, "a");
    let said = ok(&out);
    assert!(
        said.contains("보고서와 검증 로그는 옮기지 않는다"),
        "{said}"
    );

    assert_eq!(state_of(b), with_a_filled_in(state.clone(), a));
    assert_eq!(state_of(b)["queue"][2]["agent"]["machine"], a.id);
    assert_eq!(state_of(a), state, "얼어 있는 옛 허브의 큐는 그대로다");
    assert_eq!(hub_of(a), Some(id(b)));
    assert!(!hub_json(b).exists());
    assert_eq!(history_of(b), events(2));
    assert_eq!(json(&ok(&b.chute(BIN, &["hub", "--json"])))["local"], true);
}

#[test]
fn take_refuses_while_the_old_hubs_daemon_runs_and_changes_nothing() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    old_hub_with_items(&net);
    let _daemon = home_of(a)
        .repo(&slug())
        .try_lock_daemon()
        .unwrap()
        .expect("데몬 잠금");

    let out = take(b, "a");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("chute down"), "{}", stderr(&out));
    assert!(!hub_json(a).exists());
    assert!(!hub_json(b).exists());
    assert_eq!(state_of(b)["queue"], serde_json::json!([]));
    assert!(history_of(b).is_empty());
}

#[test]
fn take_refuses_while_this_pcs_own_queue_holds_other_items() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    old_hub_with_items(&net);
    let mine = serde_json::json!({
        "queue": [item_json(9, "queued", Value::Null, Value::Null)],
        "batch": null, "blocked": null, "verified_bases": [], "preflights": {}, "warnings": []
    });
    seed(b, mine.clone());

    let out = take(b, "a");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("다른 항목"), "{}", stderr(&out));
    let as_state = |v: Value| serde_json::from_value::<chute::model::State>(v).unwrap();
    assert_eq!(as_state(state_of(b)), as_state(mine));
    assert!(history_of(b).is_empty());
    assert_eq!(hub_of(a), Some(id(b)), "옛 허브는 이미 얼어 있다");
}

#[test]
fn take_refuses_a_machine_that_is_unknown_or_not_connected() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let b = net.node("b");
    let out = take(b, "nosuch");
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("hc가 머신 `nosuch`을 모른다"));
    net.set_state("b", "a", "unreachable");
    let out = take(b, "a");
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("연결돼 있지 않다(상태: unreachable)"));
}

#[test]
fn hidden_release_and_export_guard_themselves() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    old_hub_with_items(&net);

    let out = a.chute(BIN, &["hub", "export"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("아직 허브다"), "{}", stderr(&out));

    ok(&a.chute(BIN, &["hub", "release", "--to", &b.id]));
    ok(&a.chute(BIN, &["hub", "release", "--to", &b.id]));
    let other = format!("{:032x}", 0xff);
    let out = a.chute(BIN, &["hub", "release", "--to", &other]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("허브가 아니다"), "{}", stderr(&out));
    assert_eq!(hub_of(a), Some(id(b)));
    a.chute(BIN, &["hub", "set", "--local"]);
    let me = a.chute(BIN, &["hub", "release", "--to", &a.id]);
    assert_eq!(me.status.code(), Some(1), "{}", stderr(&me));
    let ghost = a.chute(BIN, &["hub", "release", "--to", &other]);
    assert_eq!(ghost.status.code(), Some(1), "{}", stderr(&ghost));
    assert!(stderr(&ghost).contains("hc가 모른다"), "{}", stderr(&ghost));
    assert!(!hub_json(a).exists());
    ok(&a.chute(BIN, &["hub", "release", "--to", &b.id]));
    let exported = json(&ok(&a.chute(BIN, &["hub", "export"])));
    assert_eq!(exported["state"]["queue"].as_array().unwrap().len(), 3);
}

fn end_state(b: &Node, a: &Node) -> (Value, Vec<chute::model::Event>, Option<MachineId>, bool) {
    (state_of(b), history_of(b), hub_of(a), hub_json(b).exists())
}

#[test]
fn take_run_again_after_any_step_ends_in_the_same_state() {
    let reference = {
        let net = net(&["a", "b", "c"]);
        net.connect_all();
        old_hub_with_items(&net);
        ok(&take(net.node("b"), "a"));
        end_state(net.node("b"), net.node("a"))
    };
    assert_eq!(reference.0["queue"].as_array().unwrap().len(), 3);

    type Stop = fn(&Net);
    let stops: [(&str, Stop); 4] = [
        ("1단계 뒤", |net| {
            let (a, b) = (net.node("a"), net.node("b"));
            ok(&a.chute(BIN, &["hub", "release", "--to", &b.id]));
        }),
        ("2단계 뒤", |net| {
            let (a, b) = (net.node("a"), net.node("b"));
            ok(&a.chute(BIN, &["hub", "release", "--to", &b.id]));
            ok(&a.chute(BIN, &["hub", "export"]));
        }),
        (
            "3단계 도중(상태만 쓰고 기록은 한 줄만)",
            |net| {
                let (a, b) = (net.node("a"), net.node("b"));
                ok(&a.chute(BIN, &["hub", "release", "--to", &b.id]));
                let store = home_of(b).repo(&slug());
                let exported = json(&ok(&a.chute(BIN, &["hub", "export"])));
                seed(b, exported["state"].clone());
                store.append_history(&events(2)[0]).unwrap();
                point_at(b, a);
            },
        ),
        ("4단계 앞(모두 쓰고 hub.json만 남음)", |net| {
            let (a, b) = (net.node("a"), net.node("b"));
            ok(&take(b, "a"));
            point_at(b, a);
        }),
    ];
    for (when, stop) in stops {
        let net = net(&["a", "b", "c"]);
        net.connect_all();
        old_hub_with_items(&net);
        stop(&net);
        ok(&take(net.node("b"), "a"));
        assert_eq!(end_state(net.node("b"), net.node("a")), reference, "{when}");
        ok(&take(net.node("b"), "a"));
        assert_eq!(
            end_state(net.node("b"), net.node("a")),
            reference,
            "{when}, 한 번 더"
        );
    }
}

#[test]
fn a_pc_that_knew_the_old_hub_follows_it_to_the_new_one() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b, c) = (net.node("a"), net.node("b"), net.node("c"));
    seed(
        a,
        serde_json::json!({
            "queue": [item_json(4, "awaiting_approval", Value::Null, Value::Null)],
            "batch": null, "blocked": null, "verified_bases": [], "preflights": {}, "warnings": []
        }),
    );
    point_at(c, a);
    ok(&take(b, "a"));

    let answer = forward(c, &remote(a), &["approve", "4"]).unwrap();
    assert_eq!(
        answer.code,
        0,
        "{}",
        String::from_utf8_lossy(&answer.stderr)
    );

    assert_eq!(state_of(b)["queue"][0]["state"], "queued");
    assert_eq!(state_of(a)["queue"][0]["state"], "awaiting_approval");
    assert_eq!(hub_of(c), Some(id(b)));
}

#[test]
fn an_in_batch_item_taken_over_goes_back_to_the_queue_when_the_new_hubs_daemon_recovers() {
    let net = net(&["a", "b"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    seed(
        a,
        serde_json::json!({
        "queue": [item_json(5, "in_batch", Value::Null, agent_json(Some(&b.id)))],
        "batch": {"id": "b1", "base": "abc1234", "prs": [5], "phase": "verifying",
                  "step": null, "round": 1, "started_ms": 1},
        "blocked": null, "verified_bases": [], "preflights": {}, "warnings": []
        }),
    );
    let mut state = state_of(a);
    ok(&take(b, "a"));
    assert_eq!(state_of(b), state, "가져온 그대로다");

    let store = home_of(b).repo(&slug());
    let info = store.info().unwrap().unwrap();
    let worktrees = b.sb.root().join("worktrees");
    chute::engine::Engine::new(ext_of(b), home_of(b), info, worktrees)
        .recover()
        .unwrap();

    state["queue"][0]["state"] = Value::from("queued");
    state["batch"] = Value::Null;
    let after = state_of(b);
    assert_eq!(after["queue"][0]["state"], "queued", "{after}");
    assert_eq!(after["batch"], Value::Null, "{after}");
    assert_eq!(after["queue"][0]["pr"], 5);
}

#[test]
fn take_run_again_after_the_new_hub_has_worked_changes_nothing_of_its_own() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let b = net.node("b");
    old_hub_with_items(&net);
    ok(&take(b, "a"));

    let store = home_of(b).repo(&slug());
    let extra = events(3).pop().unwrap();
    store.append_history(&extra).unwrap();
    store
        .update(|s| {
            s.verified_bases
                .push(chute::model::Sha::try_from("def5678".to_owned()).unwrap());
            Ok(())
        })
        .unwrap();
    let (state, history) = (state_of(b), history_of(b));

    ok(&take(b, "a"));

    assert_eq!(state_of(b), state);
    assert_eq!(history_of(b), history);
    assert_eq!(history.len(), 3);
}

#[test]
fn take_run_again_after_the_new_hubs_queue_drained_does_not_bring_it_back() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    old_hub_with_items(&net);
    ok(&take(b, "a"));
    seed(
        b,
        serde_json::json!({
            "queue": [], "batch": null, "blocked": null,
            "verified_bases": [], "preflights": {}, "warnings": []
        }),
    );

    ok(&take(b, "a"));

    assert_eq!(state_of(b)["queue"], serde_json::json!([]));
    assert_eq!(history_of(b), events(2));
    assert_eq!(hub_of(a), Some(id(b)));
}

#[test]
fn take_run_again_with_an_empty_history_does_not_bring_back_drained_items() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    seed(a, three_items(net.node("c")));
    ok(&take(b, "a"));
    seed(
        b,
        serde_json::json!({
            "queue": [], "batch": null, "blocked": null,
            "verified_bases": [], "preflights": {}, "warnings": []
        }),
    );

    ok(&take(b, "a"));

    assert_eq!(state_of(b)["queue"], serde_json::json!([]));
}

#[test]
fn taking_the_hub_back_refuses_instead_of_overwriting_the_work_done_meanwhile() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    old_hub_with_items(&net);
    ok(&take(b, "a"));
    home_of(b)
        .repo(&slug())
        .update(|s| {
            let mut extra = s.queue[0].clone();
            extra.pr = chute::model::PrNumber(7);
            s.queue.push(extra);
            Ok(())
        })
        .unwrap();
    let (frozen_a, worked_b) = (state_of(a), state_of(b));

    let out = take(a, "b");

    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("다른 항목"), "{}", stderr(&out));
    assert_eq!(state_of(a), frozen_a);
    assert_eq!(state_of(b), worked_b);
}

#[test]
fn take_run_again_refuses_when_the_old_hub_has_become_a_hub_again() {
    let net = net(&["a", "b", "c"]);
    net.connect_all();
    let (a, b) = (net.node("a"), net.node("b"));
    old_hub_with_items(&net);
    ok(&take(b, "a"));
    ok(&a.chute(BIN, &["hub", "set", "--local"]));

    let out = take(b, "a");

    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("다시 스스로를 허브로 안다"),
        "{}",
        stderr(&out)
    );
    assert_eq!(hub_of(a), None);
}
