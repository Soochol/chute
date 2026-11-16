mod engine_kit;
mod support;

use std::fs;
use std::path::{Path, PathBuf};

use chute::config::Config;
use chute::engine::Pass;
use chute::ext::Ext;
use chute::feedback::{Delivery, Feedback, Notice, Subject, Unreachable};
use chute::model::{
    Agent, Approval, EpochMs, EventKind, Failure, HumanReason, ItemState, MachineId, PrNumber,
    QueueItem, Sha, StepFailure, TerminalId, VerifyMode,
};
use chute::repo;
use chute::store::Home;
use engine_kit::Lab;
use support::{Net, Node, Reply};

const BIN: &str = env!("CARGO_BIN_EXE_chute");
const SLUG: &str = "acme/widget";
const FAILED_REF: &str = "refs/chute/failed/b1.pr_7";
const RECORDED_MS: u64 = 1_794_722_483_775;
const NEW_MS: u64 = 1_794_800_000_000;
const CONFIG: &str = r#"
respawn = ["claude"]
[[verify]]
name = "test"
run = "cargo test --quiet"
timeout = 600
failed = 'test (\S+) \.\.\. FAILED'
"#;

struct World {
    net: Net,
    head: String,
    tree: String,
}

impl World {
    fn hub(&self) -> &Node {
        self.net.node("hub")
    }

    fn desk(&self) -> &Node {
        self.net.node("desk")
    }

    fn agent_on_desk(&self) -> Agent {
        Agent {
            terminal: TerminalId("t7".into()),
            created_ms: EpochMs(RECORDED_MS),
            name: Some("impl-a".into()),
            workdir: self.desk().sb.root().join("work/agent"),
            alive: true,
            machine: Some(machine_id(self.desk())),
        }
    }

    fn item(&self, agent: Agent) -> QueueItem {
        QueueItem {
            pr: PrNumber(7),
            title: "feat: a".into(),
            branch: "feat/a".into(),
            head: Sha::try_from(self.head.clone()).unwrap(),
            state: ItemState::Queued(None),
            agent: Some(agent),
            attempts: 0,
            submitted_ms: EpochMs(1),
            touches_config: false,
            last_failure: None,
        }
    }

    fn failure(&self) -> Failure {
        let log = self.hub().sb.root().join("verify.log");
        fs::write(&log, "running 1 test\ntest a::b ... FAILED\nlast line\n").unwrap();
        Failure::Verify(StepFailure {
            step: "test".into(),
            mode: VerifyMode::Solo,
            failed_tests: Vec::new(),
            log,
            tree: Sha::try_from(self.tree.clone()).unwrap(),
        })
    }

    fn bounce(&self, item: &QueueItem) -> (Delivery, PathBuf) {
        let hub = self.hub();
        let ext = Ext::from_vars(|k| hub.sb.var(k));
        let info = repo::discover(&ext, hub.repo.path()).unwrap();
        let store = Home::at(hub.sb.chute_home()).repo(&info.slug);
        let config = Config::parse(CONFIG).unwrap();
        let worktrees = hub.sb.root().join("hub-worktrees");
        let fb = Feedback {
            ext: &ext,
            repo: &info,
            store: &store,
            config: &config,
            worktrees_root: &worktrees,
        };
        let written = fb
            .write_report(&Subject::from(item), &self.failure(), Some(FAILED_REF))
            .unwrap();
        (fb.deliver(item, &Notice::Bounced(&written)), written.md)
    }
}

fn machine_id(node: &Node) -> MachineId {
    MachineId::try_from(node.id.clone()).unwrap()
}

fn terminal(address: &str, state: &str, created_ms: u64) -> Reply {
    Reply::ok(format!(
        r#"{{"address":"{address}","state":"{state}","created_ms":{created_ms},"labels":{{}}}}"#
    ))
}

fn world(register_desk: bool) -> World {
    let net = Net::new(SLUG, &["hub", "desk"], BIN);
    net.connect_all();
    let hub = net.node("hub");
    let desk = net.node("desk");
    hub.repo
        .commit_file("chute.toml", CONFIG, "chore: chute 설정");
    hub.repo.git(&["push", "--quiet", "origin", "main"]);
    desk.repo.git(&["pull", "--quiet", "origin", "main"]);
    let nodes = if register_desk {
        vec![hub, desk]
    } else {
        vec![hub]
    };
    for node in nodes {
        let out = node.chute(BIN, &["init"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let head = hub.repo.pr_branch("feat/a", "a.txt", "a\n");
    hub.repo
        .git(&["checkout", "--quiet", "-b", "tmp-tree", "main"]);
    hub.repo.git(&[
        "merge",
        "--quiet",
        "--no-ff",
        "-m",
        "Merge pull request #7 from acme/feat/a",
        "feat/a",
    ]);
    let tree = hub.repo.head();
    hub.repo.git(&["update-ref", FAILED_REF, &tree]);
    hub.repo.git(&["checkout", "--quiet", "main"]);
    World { net, head, tree }
}

fn calls(node: &Node, verb: &str) -> Vec<Vec<String>> {
    node.sb
        .fake("hc")
        .calls()
        .into_iter()
        .filter(|c| c.first().is_some_and(|v| v == verb))
        .collect()
}

fn report_path_in(call: &[String]) -> PathBuf {
    let line = call.last().unwrap();
    let md = line
        .split_whitespace()
        .map(|word| word.trim_end_matches('.'))
        .find(|word| word.ends_with(".md"))
        .unwrap_or_else(|| panic!("보고서 경로가 없다: {line}"));
    PathBuf::from(md)
}

fn rev(repo: &support::TestRepo, name: &str) -> String {
    repo.git(&["rev-parse", name]).trim().to_owned()
}

fn unreachable_text(delivery: &Delivery) -> String {
    match delivery {
        Delivery::Unreachable(why) => why.to_string(),
        other => panic!("전달하지 못해야 한다: {other:?}"),
    }
}

#[test]
fn a_live_agent_on_another_machine_gets_the_report_from_that_machine() {
    let w = world(true);
    let desk_hc = w.desk().sb.fake("hc");
    desk_hc
        .respond("get t7*", terminal("t7", "live", RECORDED_MS))
        .respond("send t7 *", Reply::ok("{}"));

    let (delivery, hub_md) = w.bounce(&w.item(w.agent_on_desk()));

    assert_eq!(
        delivery,
        Delivery::Sent {
            terminal: TerminalId("t7".into())
        }
    );
    assert!(
        calls(w.hub(), "send").is_empty(),
        "허브의 hc는 send를 받지 않는다"
    );
    assert!(calls(w.hub(), "get").is_empty());
    let sends = calls(w.desk(), "send");
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(sends[0][1], "t7");

    let desk_md = report_path_in(&sends[0]);
    assert!(
        desk_md.starts_with(w.desk().sb.chute_home()),
        "{}",
        desk_md.display()
    );
    assert_ne!(desk_md, hub_md);
    let text = fs::read_to_string(&desk_md).unwrap();
    assert!(text.contains("log-"), "{text}");
    assert!(text.contains("이 보고서와 같은 폴더"), "{text}");
    assert!(
        text.contains(&w.desk().repo.path().display().to_string()),
        "재현 명령은 그 머신의 저장소를 가리킨다: {text}"
    );
    assert!(
        !text.contains(&w.hub().sb.root().display().to_string()),
        "허브의 경로가 남았다: {text}"
    );
    let folder = desk_md.parent().unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(desk_md.with_extension("json")).unwrap()).unwrap();
    let log = json["failure"]["log"].as_str().unwrap();
    assert!(!Path::new(log).is_absolute(), "{log}");
    assert!(
        fs::read_to_string(folder.join(log))
            .unwrap()
            .contains("last line")
    );
}

#[test]
fn the_failed_tree_ref_is_made_on_the_agents_machine_at_the_same_commit() {
    let w = world(true);
    w.desk()
        .sb
        .fake("hc")
        .respond("get t7*", terminal("t7", "live", RECORDED_MS))
        .respond("send t7 *", Reply::ok("{}"));

    w.bounce(&w.item(w.agent_on_desk()));

    assert_eq!(rev(&w.hub().repo, FAILED_REF), w.tree);
    assert_eq!(rev(&w.desk().repo, FAILED_REF), w.tree);
    let report = fs::read_to_string(report_path_in(&calls(w.desk(), "send")[0])).unwrap();
    assert!(report.contains(FAILED_REF), "{report}");
    assert!(report.contains("worktree add --detach"), "{report}");
}

#[test]
fn a_finished_agent_is_replaced_on_its_own_machine() {
    let w = world(true);
    w.desk()
        .sb
        .fake("hc")
        .respond("get t7*", terminal("t7", "finished", RECORDED_MS))
        .respond(
            "run *",
            Reply::ok(r#"{"address":"t9","first_message":{"sent":true}}"#),
        )
        .respond("get t9*", terminal("t9", "live", NEW_MS));

    let (delivery, _) = w.bounce(&w.item(w.agent_on_desk()));

    let Delivery::Spawned(agent) = delivery else {
        panic!("새 에이전트가 떠야 한다: {delivery:?}");
    };
    assert_eq!(agent.terminal, TerminalId("t9".into()));
    assert_eq!(agent.machine, Some(machine_id(w.desk())));
    assert_eq!(agent.created_ms, EpochMs(NEW_MS));
    assert!(
        agent.workdir.starts_with(w.desk().sb.home()),
        "worktree는 그 머신에 있다: {}",
        agent.workdir.display()
    );
    assert!(agent.workdir.join("a.txt").exists());

    let runs = calls(w.desk(), "run");
    assert_eq!(runs.len(), 1, "{runs:?}");
    for flag in ["--bg", "--no-parent"] {
        assert!(runs[0].iter().any(|a| a == flag), "{flag}: {:?}", runs[0]);
    }
    assert!(
        calls(w.hub(), "run").is_empty(),
        "허브는 대신 띄우지 않는다"
    );
    assert!(!w.hub().sb.home().join(".herdr/worktrees").exists());
}

#[test]
fn a_machine_that_is_down_sends_the_pr_to_a_human_and_names_the_machine() {
    let w = world(true);
    w.net.set_state("hub", "desk", "disconnected");

    let (delivery, _) = w.bounce(&w.item(w.agent_on_desk()));

    let why = unreachable_text(&delivery);
    assert!(why.contains("desk"), "{why}");
    assert!(calls(w.hub(), "run").is_empty());
    assert!(calls(w.desk(), "send").is_empty());
}

#[test]
fn a_machine_without_the_repository_registered_is_unreachable() {
    let w = world(false);
    w.desk()
        .sb
        .fake("hc")
        .respond("get t7*", terminal("t7", "live", RECORDED_MS))
        .respond("send t7 *", Reply::ok("{}"));

    let (delivery, _) = w.bounce(&w.item(w.agent_on_desk()));

    let why = unreachable_text(&delivery);
    assert!(why.contains("desk"), "{why}");
    assert!(why.contains("등록되지 않은"), "{why}");
    assert!(calls(w.desk(), "send").is_empty());
}

#[test]
fn a_machine_without_chute_is_unreachable() {
    let w = world(true);
    fs::remove_file(w.desk().sb.root().join("net/chute")).unwrap();

    let (delivery, _) = w.bounce(&w.item(w.agent_on_desk()));

    let why = unreachable_text(&delivery);
    assert!(why.contains("desk") && why.contains("chute"), "{why}");
}

#[test]
fn an_agent_on_the_hub_itself_is_delivered_by_the_v1_path() {
    let w = world(true);
    w.hub()
        .sb
        .fake("hc")
        .respond("get t7*", terminal("t7", "live", RECORDED_MS))
        .respond("send t7 *", Reply::ok("{}"));
    let mut agent = w.agent_on_desk();
    agent.machine = Some(machine_id(w.hub()));

    let (delivery, _) = w.bounce(&w.item(agent));

    assert_eq!(
        delivery,
        Delivery::Sent {
            terminal: TerminalId("t7".into())
        }
    );
    assert_eq!(calls(w.hub(), "send").len(), 1);
    assert!(calls(w.desk(), "send").is_empty());
}

#[test]
fn a_machine_hc_does_not_know_is_unreachable_without_asking_anyone() {
    let w = world(true);
    let mut agent = w.agent_on_desk();
    agent.machine = Some(MachineId::try_from("ffff".to_owned()).unwrap());

    let (delivery, _) = w.bounce(&w.item(agent));

    assert!(matches!(
        delivery,
        Delivery::Unreachable(Unreachable::Failed(_))
    ));
    assert!(unreachable_text(&delivery).contains("ffff"));
    assert!(calls(w.desk(), "send").is_empty());
}

#[test]
fn the_engine_hands_the_pr_to_a_human_when_the_agents_machine_is_down() {
    let lab = Lab::new("[ -e BAD ] && fail \"FAILED bad_test\"");
    let hc = lab.sb.fake("hc");
    hc.clear_rules()
        .respond(
            "server status --json",
            Reply::ok(r#"{"core":{"id":"aaaa","name":"hub"}}"#),
        )
        .respond(
            "machine ls --json",
            Reply::ok(
                r#"{"machines":[{"id":"bbbb","name":"desk","host":"desk-host","state":"disconnected"}]}"#,
            ),
        )
        .respond(
            "exec *",
            Reply::ok("")
                .code(5)
                .stderr(r#"{"error":{"kind":"unreachable","message":"machine desk is disconnected","hint":""}}"#),
        );
    let mut agent = lab.agent();
    agent.machine = Some(MachineId::try_from("bbbb".to_owned()).unwrap());
    lab.open_pr_for(1, &[("BAD", "")], Some(agent), Approval::Agent);

    let pass = lab.engine().run_once().unwrap();

    assert!(
        matches!(pass, Pass::Done(_)),
        "{pass:?} {:?}",
        lab.state().blocked
    );
    assert_eq!(
        lab.item_state(1),
        Some(ItemState::NeedsHuman(HumanReason::Unreachable))
    );
    let detail = lab
        .events()
        .into_iter()
        .find_map(|e| match e {
            EventKind::NeedsHuman {
                reason: HumanReason::Unreachable,
                detail,
                ..
            } => detail,
            _ => None,
        })
        .expect("사람 차례 사건");
    assert!(detail.contains("desk"), "{detail}");
}

#[test]
fn a_head_change_notice_goes_to_the_agents_machine_without_a_report() {
    let w = world(true);
    w.desk()
        .sb
        .fake("hc")
        .respond("get t7*", terminal("t7", "live", RECORDED_MS))
        .respond("send t7 *", Reply::ok("{}"));
    let item = w.item(w.agent_on_desk());
    let hub = w.hub();
    let ext = Ext::from_vars(|k| hub.sb.var(k));
    let info = repo::discover(&ext, hub.repo.path()).unwrap();
    let store = Home::at(hub.sb.chute_home()).repo(&info.slug);
    let config = Config::parse(CONFIG).unwrap();
    let worktrees = hub.sb.root().join("hub-worktrees");
    let fb = Feedback {
        ext: &ext,
        repo: &info,
        store: &store,
        config: &config,
        worktrees_root: &worktrees,
    };
    let current = Sha::try_from(w.tree.clone()).unwrap();

    let delivery = fb.deliver(
        &item,
        &Notice::HeadChanged {
            registered: &item.head,
            current: &current,
        },
    );

    assert_eq!(
        delivery,
        Delivery::Sent {
            terminal: TerminalId("t7".into())
        }
    );
    let sends = calls(w.desk(), "send");
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert!(sends[0][2].contains("HEAD가 바뀌었다"), "{:?}", sends[0]);
    assert!(calls(w.hub(), "send").is_empty());
}

#[test]
fn a_banner_before_the_answer_does_not_hide_a_delivery_that_happened() {
    let w = world(true);
    let launcher = w.desk().sb.root().join("net/chute");
    let body = fs::read_to_string(&launcher).unwrap();
    fs::write(
        &launcher,
        body.replacen("#!/bin/sh\n", "#!/bin/sh\necho 'Welcome to desk'\n", 1),
    )
    .unwrap();
    w.desk()
        .sb
        .fake("hc")
        .respond("get t7*", terminal("t7", "live", RECORDED_MS))
        .respond("send t7 *", Reply::ok("{}"));

    let (delivery, _) = w.bounce(&w.item(w.agent_on_desk()));

    assert_eq!(
        delivery,
        Delivery::Sent {
            terminal: TerminalId("t7".into())
        }
    );
}
