mod support;

use std::fs;
use std::process::Output;

use chute::model::{MachineId, Slug, StatusView};
use chute::store::Home;
use serde_json::Value;
use support::{Net, Node, TestRepo};

const BIN: &str = env!("CARGO_BIN_EXE_chute");
const SLUG: &str = "acme/widget";

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "종료 코드 {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn slug(text: &str) -> Slug {
    Slug::try_from(text.to_owned()).unwrap()
}

fn id(node: &Node) -> MachineId {
    MachineId::try_from(node.id.clone()).unwrap()
}

fn point_at(from: &Node, to: &Node) {
    Home::at(from.sb.chute_home())
        .repo(&slug(SLUG))
        .set_hub(id(to))
        .unwrap();
}

fn item(pr: u64, machine: Option<&str>) -> Value {
    serde_json::json!({
        "pr": pr, "title": format!("t{pr}"), "branch": format!("feat/{pr}"), "head": "abc1234",
        "state": "queued", "state_reason": null,
        "agent": {"terminal": "t7", "created_ms": 5, "name": "impl", "workdir": "/w",
                  "alive": true, "machine": machine},
        "attempts": 0, "submitted_ms": pr, "touches_config": false, "last_failure": null
    })
}

fn write_queue(node: &Node, slug_dir: &str, items: Vec<Value>) {
    let state = serde_json::json!({
        "queue": items, "batch": null, "blocked": null,
        "verified_bases": [], "preflights": {}, "warnings": []
    });
    fs::write(
        node.sb
            .chute_home()
            .join("repos")
            .join(slug_dir)
            .join("state.json"),
        state.to_string(),
    )
    .unwrap();
}

fn net() -> Net {
    let net = Net::new(SLUG, &["hub", "pc", "lap", "desk"], BIN);
    for node in &net.nodes {
        ok(&node.chute(BIN, &["init"]));
    }
    net.connect("hub", "pc", "pc");
    net.connect("hub", "lap", "lap");
    net.connect("hub", "desk", "desk");
    net.connect("pc", "hub", "main-hub");
    net.connect("pc", "lap", "lappy");
    let (hub, pc, lap, desk) = (
        net.node("hub"),
        net.node("pc"),
        net.node("lap"),
        net.node("desk"),
    );
    point_at(pc, hub);
    write_queue(
        hub,
        "acme__widget",
        vec![
            item(1, None),
            item(2, Some(&pc.id)),
            item(3, Some(&lap.id)),
            item(4, Some(&desk.id)),
        ],
    );
    write_queue(pc, "acme__widget", vec![item(99, None)]);
    net
}

fn status(node: &Node, args: &[&str]) -> Value {
    let mut all = vec!["status", "--json"];
    all.extend_from_slice(args);
    json(&ok(&node.chute(BIN, &all)))
}

fn field<'a>(v: &'a Value, queue: usize, key: &str) -> &'a Value {
    &v["queue"][queue]["agent"][key]
}

#[test]
fn a_pc_that_is_not_the_hub_sees_the_hubs_queue_in_its_own_terms() {
    let net = net();
    let (hub, pc) = (net.node("hub"), net.node("pc"));
    let at_hub = status(hub, &[]);
    let at_pc = status(pc, &[]);

    for key in ["pr", "state", "head", "title"] {
        let pick = |v: &Value| -> Vec<Value> {
            v["queue"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i[key].clone())
                .collect()
        };
        assert_eq!(pick(&at_hub), pick(&at_pc), "{key}");
    }
    assert_eq!(at_pc["queue"].as_array().unwrap().len(), 4);

    let canon = |n: &Node| n.repo.path().canonicalize().unwrap();
    assert_eq!(at_pc["repo"]["path"], canon(pc).to_str().unwrap());
    assert_eq!(at_hub["repo"]["path"], canon(hub).to_str().unwrap());
    assert!(
        at_pc["repo"]["state_dir"]
            .as_str()
            .unwrap()
            .starts_with(pc.sb.chute_home().to_str().unwrap())
    );
    let hub_view = &at_pc["hub"];
    assert_eq!(hub_view["local"], false);
    assert_eq!(hub_view["reachable"], true);
    assert_eq!(hub_view["error"], Value::Null);
    assert_eq!(hub_view["repo_path"], canon(hub).to_str().unwrap());
    assert_eq!(hub_view["machine_id"], hub.id.as_str());
    assert_eq!(hub_view["machine"], "main-hub");
    assert_eq!(at_hub["hub"]["local"], true);
}

#[test]
fn agents_are_addressed_from_the_viewing_pc() {
    let net = net();
    let (hub, pc, lap, desk) = (
        net.node("hub"),
        net.node("pc"),
        net.node("lap"),
        net.node("desk"),
    );
    let at_hub = status(hub, &[]);
    assert_eq!(
        [0, 1, 2, 3].map(|n| field(&at_hub, n, "terminal").as_str().unwrap().to_owned()),
        ["t7", "pc/t7", "lap/t7", "desk/t7"]
    );

    let at_pc = status(pc, &[]);
    let seen = |n| {
        serde_json::json!([
            field(&at_pc, n, "terminal"),
            field(&at_pc, n, "machine"),
            field(&at_pc, n, "machine_id")
        ])
    };
    let expect = |terminal: Value, machine: Value, node: &Node| {
        serde_json::json!([terminal, machine, node.id])
    };
    assert_eq!(
        seen(0),
        expect("main-hub/t7".into(), "main-hub".into(), hub)
    );
    assert_eq!(
        seen(1),
        expect("t7".into(), Value::Null, pc),
        "an agent on the viewing PC"
    );
    assert_eq!(seen(2), expect("lappy/t7".into(), "lappy".into(), lap));
    assert_eq!(
        seen(3),
        expect("desk/t7".into(), Value::Null, desk),
        "a machine this PC does not know keeps the hub's address"
    );
}

#[test]
fn the_submitter_and_queue_options_are_shown_and_addressed_like_the_agent() {
    let net = net();
    let (hub, pc, lap) = (net.node("hub"), net.node("pc"), net.node("lap"));
    let mut orchestrated = item(2, Some(&pc.id));
    orchestrated["submitter"] = serde_json::json!({
        "terminal": "t9", "created_ms": 6, "name": "orch", "workdir": "/o",
        "alive": true, "machine": lap.id
    });
    orchestrated["priority"] = true.into();
    orchestrated["after"] = serde_json::json!([1]);
    orchestrated["repair"] = true.into();
    write_queue(hub, "acme__widget", vec![item(1, None), orchestrated]);

    let at_hub = status(hub, &[]);
    let at_pc = status(pc, &[]);
    for view in [&at_hub, &at_pc] {
        let plain = &view["queue"][0];
        assert_eq!(plain["submitter"], Value::Null);
        assert_eq!(plain["priority"], false);
        assert_eq!(plain["after"], serde_json::json!([]));
        assert_eq!(plain["repair"], false);
        let held = &view["queue"][1];
        assert_eq!(held["priority"], true);
        assert_eq!(held["after"], serde_json::json!([1]));
        assert_eq!(held["repair"], true);
        assert_eq!(held["submitter"]["name"], "orch");
        assert_eq!(held["submitter"]["machine_id"], lap.id.as_str());
    }
    assert_eq!(at_hub["queue"][1]["submitter"]["terminal"], "lap/t9");
    assert_eq!(at_pc["queue"][1]["submitter"]["terminal"], "lappy/t9");
    assert_eq!(at_pc["queue"][1]["submitter"]["machine"], "lappy");
    assert_eq!(at_pc["queue"][1]["agent"]["terminal"], "t7");
}

#[test]
fn batch_steps_and_recent_merged_reach_a_pc_that_is_not_the_hub() {
    let net = net();
    let (hub, pc) = (net.node("hub"), net.node("pc"));
    let dir = hub.sb.chute_home().join("repos").join("acme__widget");
    let state = serde_json::json!({
        "queue": [],
        "batch": {"id": "b1", "base": "abc1234", "prs": [3], "phase": "verifying",
                  "step": "test", "steps": ["fmt", "test"], "round": 1, "started_ms": 5},
        "blocked": null, "verified_bases": [], "preflights": {}, "warnings": []
    });
    fs::write(dir.join("state.json"), state.to_string()).unwrap();
    let merged = |pr: u64, at: u64, title: &str| {
        let title = if title.is_empty() {
            String::new()
        } else {
            format!(r#","title":"{title}""#)
        };
        format!(
            r#"{{"at_ms":{at},"event":"merged","pr":{pr},"batch":"b0","commit":"abc1234","submitted_ms":1{title}}}"#
        )
    };
    let bounced =
        r#"{"at_ms":15,"event":"bounced","pr":2,"batch":null,"reason":"conflict","attempts":1}"#;
    let lines = [
        merged(1, 10, ""),
        bounced.to_owned(),
        merged(2, 20, "fix: two"),
    ];
    fs::write(dir.join("history.jsonl"), lines.join("\n") + "\n").unwrap();

    let at_hub = status(hub, &[]);
    let at_pc = status(pc, &[]);

    assert_eq!(at_pc["batch"]["steps"], serde_json::json!(["fmt", "test"]));
    assert_eq!(at_pc["recent_merged"], at_hub["recent_merged"]);
    let shown = at_pc["recent_merged"].as_array().unwrap();
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[0]["pr"], 2);
    assert_eq!(shown[0]["title"], "fix: two");
    assert_eq!(shown[0]["bounces"], 1);
    assert_eq!(shown[1]["title"], Value::Null);
}

#[test]
fn what_the_pc_prints_reads_as_the_status_type() {
    let net = net();
    let text = ok(&net.node("pc").chute(BIN, &["status", "--json"]));
    let view: StatusView = serde_json::from_str(&text).unwrap();
    assert!(view.hub.reachable && !view.hub.local);
}

#[test]
fn a_hub_that_cannot_be_reached_gives_an_empty_unreachable_status_and_exit_0() {
    let net = net();
    let pc = net.node("pc");
    let cases: [(&str, &dyn Fn()); 2] = [
        ("machine down", &|| {
            net.set_state("pc", "main-hub", "unreachable")
        }),
        ("no chute there", &|| {
            net.set_state("pc", "main-hub", "connected");
            fs::remove_file(net.node("hub").sb.root().join("net/chute")).unwrap();
        }),
    ];
    for (case, set_up) in cases {
        set_up();
        let out = pc.chute(BIN, &["status", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{case}");
        let v = json(&String::from_utf8(out.stdout).unwrap());
        assert_eq!(v["hub"]["reachable"], false, "{case}");
        assert_eq!(v["hub"]["local"], false, "{case}");
        assert_eq!(v["hub"]["repo_path"], Value::Null, "{case}");
        assert_eq!(v["queue"], serde_json::json!([]), "{case}");
        assert_eq!(v["daemon"]["running"], false, "{case}");
        let error = v["hub"]["error"].as_str().unwrap();
        assert!(error.contains("main-hub"), "{case}: {error}");
        assert_eq!(v["warnings"][0]["kind"], "hub", "{case}");
        assert_eq!(v["warnings"][0]["message"], error, "{case}");
        assert!(!v.to_string().contains("feat/99"), "{case}");
        let _: StatusView = serde_json::from_value(v).unwrap();
    }
}

#[test]
fn the_human_status_names_the_hub_and_says_when_it_is_down() {
    let net = net();
    let pc = net.node("pc");
    let up = ok(&pc.chute(BIN, &["status"]));
    assert!(up.contains("허브: main-hub"), "{up}");
    assert!(up.contains("#3") && !up.contains("#99"), "{up}");
    net.set_state("pc", "main-hub", "unreachable");
    let down = ok(&pc.chute(BIN, &["status"]));
    assert!(
        down.contains("허브: main-hub") && down.contains("닿지 않음"),
        "{down}"
    );
    assert!(!down.contains("#99") && !down.contains("#1 "), "{down}");
}

#[test]
fn all_draws_a_dead_hub_and_a_local_hub_together() {
    let net = net();
    let pc = net.node("pc");
    let other = TestRepo::new(&pc.sb, "acme/other");
    ok(&pc
        .sb
        .command(BIN)
        .current_dir(other.path())
        .arg("init")
        .output()
        .unwrap());
    write_queue(pc, "acme__other", vec![item(5, None)]);
    net.set_state("pc", "main-hub", "unreachable");

    let all = json(&ok(&pc.chute(BIN, &["status", "--json", "--all"])));
    let by_slug = |s: &str| {
        all.as_array()
            .unwrap()
            .iter()
            .find(|v| v["repo"]["slug"] == s)
            .unwrap_or_else(|| panic!("{s}가 없다: {all}"))
    };
    let widget = by_slug(SLUG);
    assert_eq!(widget["hub"]["reachable"], false);
    assert_eq!(widget["queue"], serde_json::json!([]));
    let other = by_slug("acme/other");
    assert_eq!(other["hub"]["local"], true);
    assert_eq!(other["queue"][0]["pr"], 5);

    net.set_state("pc", "main-hub", "connected");
    let all = json(&ok(&pc.chute(BIN, &["status", "--json", "--all"])));
    let widget = all
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["repo"]["slug"] == SLUG)
        .unwrap();
    assert_eq!(widget["queue"].as_array().unwrap().len(), 4);
}

#[test]
fn all_asks_the_hubs_of_different_repos_at_the_same_time() {
    let net = net();
    let (hub, pc) = (net.node("hub"), net.node("pc"));
    for (name, repo) in [("acme/other", "other"), ("acme/third", "third")] {
        let on_hub = TestRepo::new(&hub.sb, name);
        let on_pc = TestRepo::clone_of(&pc.sb, &on_hub);
        for (node, dir) in [(hub, &on_hub), (pc, &on_pc)] {
            ok(&node
                .sb
                .command(BIN)
                .current_dir(dir.path())
                .arg("init")
                .output()
                .unwrap());
        }
        Home::at(pc.sb.chute_home())
            .repo(&slug(name))
            .set_hub(id(hub))
            .unwrap();
        write_queue(hub, &format!("acme__{repo}"), vec![item(7, None)]);
    }
    let launcher = hub.sb.root().join("net/chute");
    let body = fs::read_to_string(&launcher).unwrap();
    fs::write(&launcher, body.replacen('\n', "\nsleep 3\n", 1)).unwrap();

    let started = std::time::Instant::now();
    let all = json(&ok(&pc.chute(BIN, &["status", "--json", "--all"])));
    let reachable = all
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["hub"]["reachable"] == true)
        .count();
    assert_eq!(reachable, 3, "{all}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_hub_followed_after_it_moved_is_named_as_it_is_now_even_when_it_fails() {
    let net = net();
    let (hub, pc, lap) = (net.node("hub"), net.node("pc"), net.node("lap"));
    point_at(hub, lap);
    fs::write(lap.sb.root().join("net/chute"), "#!/bin/sh\nexit 1\n").unwrap();

    let v = status(pc, &[]);
    assert_eq!(v["hub"]["reachable"], false);
    assert_eq!(v["hub"]["machine_id"], lap.id.as_str());
    assert_eq!(v["hub"]["machine"], "lappy");
}

#[test]
fn history_lines_of_an_older_chute_and_removed_events_do_not_break_status_and_reset_the_bounce_count()
 {
    let net = net();
    let hub = net.node("hub");
    let dir = hub.sb.chute_home().join("repos").join("acme__widget");
    let state = serde_json::json!({
        "queue": [], "batch": null, "blocked": null,
        "verified_bases": [], "preflights": {}, "warnings": []
    });
    fs::write(dir.join("state.json"), state.to_string()).unwrap();
    let lines = [
        r#"{"at_ms":1,"event":"needs_human","pr":5,"reason":"budget","detail":null}"#,
        r#"{"at_ms":2,"event":"gave_up","pr":6,"reason":"그만둔다"}"#,
        r#"{"at_ms":3,"event":"notified","kind":"human","pr":5,"to":"t1orch","delivered":"sent"}"#,
        r#"{"at_ms":4,"event":"bounced","pr":7,"batch":null,"reason":"verify","attempts":1}"#,
        r#"{"at_ms":5,"event":"bounced","pr":7,"batch":null,"reason":"verify","attempts":2,"unreachable":"작업자 터미널이 없다"}"#,
        r#"{"at_ms":6,"event":"removed","pr":7,"reason":"budget","detail":"반송 한도 2번을 넘었다","report":"/r/7.md"}"#,
        r#"{"at_ms":7,"event":"bounced","pr":7,"batch":null,"reason":"verify","attempts":1}"#,
        r#"{"at_ms":8,"event":"merged","pr":7,"batch":"b0","commit":"abc1234","submitted_ms":6}"#,
    ];
    fs::write(dir.join("history.jsonl"), lines.join("\n") + "\n").unwrap();

    let view = status(hub, &[]);

    let shown = view["recent_merged"].as_array().unwrap();
    assert_eq!(shown.len(), 1, "{view}");
    assert_eq!(shown[0]["pr"], 7);
    assert_eq!(shown[0]["bounces"], 1);
}
