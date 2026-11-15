mod support;

use std::fs;
use std::process::Output;

use chute::config::Config;
use serde_json::Value;
use support::{Sandbox, TestRepo};

fn chute(sb: &Sandbox, cwd: &std::path::Path, args: &[&str]) -> Output {
    sb.command(env!("CARGO_BIN_EXE_chute"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn ok(out: Output) -> String {
    assert!(
        out.status.success(),
        "종료 코드 {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn init_writes_a_readable_draft_and_registers() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    let stdout = ok(chute(&sb, repo.path(), &["init"]));
    assert!(stdout.contains("chute --skill"), "{stdout}");

    let draft = fs::read_to_string(repo.path().join("chute.toml")).unwrap();
    let config = Config::parse(&draft).unwrap();
    assert_eq!(config.respawn, ["claude", "--dangerously-skip-permissions"]);
    let lines: Vec<&str> = draft.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("respawn")).unwrap();
    assert!(lines[at - 1].contains("권한 확인 없이"), "{draft}");

    let repo_json = sb.chute_home().join("repos/acme__widget/repo.json");
    let info: Value = serde_json::from_str(&fs::read_to_string(repo_json).unwrap()).unwrap();
    assert_eq!(info["slug"], "acme/widget");
    assert_eq!(info["default_branch"], "main");
    assert!(!repo.path().join("AGENTS.md").exists());
}

#[test]
fn init_twice_gives_the_same_files() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    let config = repo.path().join("chute.toml");
    let registered = sb.chute_home().join("repos/acme__widget/repo.json");

    ok(chute(&sb, repo.path(), &["init"]));
    let first = (fs::read(&config).unwrap(), fs::read(&registered).unwrap());
    ok(chute(&sb, repo.path(), &["init"]));
    let second = (fs::read(&config).unwrap(), fs::read(&registered).unwrap());
    assert_eq!(first, second);
}

#[test]
fn init_keeps_an_existing_config() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    repo.write("chute.toml", "# mine\n");
    ok(chute(&sb, repo.path(), &["init"]));
    assert_eq!(
        fs::read_to_string(repo.path().join("chute.toml")).unwrap(),
        "# mine\n"
    );
}

#[test]
fn status_after_init_shows_an_empty_queue() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));

    let status: Value =
        serde_json::from_str(&ok(chute(&sb, repo.path(), &["status", "--json"]))).unwrap();
    assert_eq!(status["schema"], 1);
    assert_eq!(status["queue"], Value::Array(vec![]));
    assert_eq!(status["repo"]["slug"], "acme/widget");
    assert_eq!(status["daemon"]["running"], false);
    assert_eq!(status["blocked"], Value::Null);
    assert_eq!(status["today"]["merged"], 0);
}

#[test]
fn status_reads_approval_from_the_default_branch() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    let draft = fs::read_to_string(repo.path().join("chute.toml")).unwrap();
    repo.commit_file(
        "chute.toml",
        &draft.replace("approval = \"agent\"", "approval = \"human\""),
        "chore: chute 설정",
    );
    repo.git(&["push", "--quiet", "origin", "main"]);
    repo.commit_file("chute.toml", &draft, "local only");

    let status: Value =
        serde_json::from_str(&ok(chute(&sb, repo.path(), &["status", "--json"]))).unwrap();
    assert_eq!(status["approval"], "human");
    assert_eq!(status["warnings"], Value::Array(vec![]));
}

#[test]
fn status_all_lists_every_registered_repository() {
    let sb = Sandbox::new();
    let a = TestRepo::new(&sb, "acme/widget");
    let b = TestRepo::new(&sb, "zed/gadget");
    ok(chute(&sb, a.path(), &["init"]));
    ok(chute(&sb, b.path(), &["init"]));

    let all: Value =
        serde_json::from_str(&ok(chute(&sb, sb.root(), &["status", "--json", "--all"]))).unwrap();
    let slugs: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["repo"]["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["acme/widget", "zed/gadget"]);

    let human = ok(chute(&sb, sb.root(), &["status", "--all"]));
    assert!(
        human.contains("acme/widget") && human.contains("zed/gadget"),
        "{human}"
    );
}

#[test]
fn status_with_repo_flag_from_outside() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    let path = repo.path().to_string_lossy().into_owned();
    let status: Value = serde_json::from_str(&ok(chute(
        &sb,
        sb.root(),
        &["--repo", &path, "status", "--json"],
    )))
    .unwrap();
    assert_eq!(status["repo"]["slug"], "acme/widget");
}

#[test]
fn status_of_an_unregistered_repository_fails() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    let out = chute(&sb, repo.path(), &["status", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("chute init"));
}

#[test]
fn chute_git_selects_the_git_executable() {
    let sb = Sandbox::new();
    let git = sb.fake("git");
    git.respond(
        "worktree list --porcelain",
        support::Reply::ok("worktree /w/widget\nHEAD abc1234\nbranch refs/heads/main\n"),
    )
    .respond(
        "remote get-url origin",
        support::Reply::ok("https://github.com/acme/widget.git\n"),
    );
    let out = chute(&sb, sb.root(), &["status", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        git.calls(),
        [
            vec!["worktree", "list", "--porcelain"],
            vec!["remote", "get-url", "origin"]
        ]
    );
}

fn status_json(sb: &Sandbox, repo: &TestRepo) -> Value {
    serde_json::from_str(&ok(chute(sb, repo.path(), &["status", "--json"]))).unwrap()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[test]
fn status_shows_the_daemon_that_holds_the_lock() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    let dir = sb.chute_home().join("repos/acme__widget");
    fs::write(
        dir.join("daemon.json"),
        r#"{"pid": 4242, "terminal": "t1zz", "doing": null, "started_ms": 0}"#,
    )
    .unwrap();
    let lock = fs::File::create(dir.join("daemon.lock")).unwrap();

    let idle = status_json(&sb, &repo);
    assert_eq!(
        idle["daemon"],
        serde_json::json!({"running": false, "terminal": null, "pid": null})
    );

    lock.lock().unwrap();
    let running = status_json(&sb, &repo);
    assert_eq!(
        running["daemon"],
        serde_json::json!({"running": true, "terminal": "t1zz", "pid": 4242})
    );
}

#[test]
fn status_counts_today_from_history() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    let now = now_ms();
    let history = format!(
        "{{\"at_ms\":{now},\"event\":\"merged\",\"pr\":3,\"batch\":\"b1\",\"commit\":\"abc1234\",\"submitted_ms\":{}}}\n\
         {{\"at_ms\":{now},\"event\":\"batch_finished\",\"batch\":\"b1\",\"merged\":[3],\"bounced\":[],\"seconds\":40,\"verify_s\":30}}\n\
         {{\"at_ms\":{now},\"event\":\"bou",
        now - 90_000
    );
    fs::write(
        sb.chute_home().join("repos/acme__widget/history.jsonl"),
        history,
    )
    .unwrap();

    let status = status_json(&sb, &repo);
    assert_eq!(status["today"]["merged"], 1);
    assert_eq!(status["today"]["batches"], 1);
    assert_eq!(status["today"]["median_lead_s"], 90);
    assert_eq!(status["recent"][0]["batch"], "b1");
    let kinds: Vec<&str> = status["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"history"), "{kinds:?}");
}

#[test]
fn status_without_json_prints_a_summary() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    let text = ok(chute(&sb, repo.path(), &["status"]));
    assert!(serde_json::from_str::<Value>(&text).is_err(), "{text}");
    assert!(text.starts_with("acme/widget (main)"), "{text}");
    assert!(text.contains("큐가 비어 있다"), "{text}");
}

#[test]
fn status_all_skips_stray_files_in_the_state_folder() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    fs::write(sb.chute_home().join("repos/.DS_Store"), "").unwrap();
    let all: Value =
        serde_json::from_str(&ok(chute(&sb, sb.root(), &["status", "--json", "--all"]))).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);
}

#[test]
fn init_leaves_agents_md_alone() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    repo.write("AGENTS.md", "# rules\n");
    ok(chute(&sb, repo.path(), &["init"]));
    assert_eq!(
        fs::read_to_string(repo.path().join("AGENTS.md")).unwrap(),
        "# rules\n"
    );
}

#[test]
fn init_does_not_redraft_a_config_already_on_the_default_branch() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    repo.commit_file(
        "chute.toml",
        "[[verify]]\nname = \"t\"\nrun = \"true\"\n",
        "chore: chute 설정",
    );
    repo.git(&["push", "--quiet", "origin", "main"]);
    repo.git(&["checkout", "--quiet", "-b", "old", "HEAD~1"]);

    ok(chute(&sb, repo.path(), &["init"]));
    assert!(!repo.path().join("chute.toml").exists());
}

#[test]
fn a_missing_repo_folder_is_reported_as_missing() {
    let sb = Sandbox::new();
    let out = chute(&sb, sb.root(), &["--repo", "nope", "status"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("폴더가 없다"), "{err}");
}

#[test]
fn status_keeps_every_null_key_of_the_contract() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    let status = status_json(&sb, &repo);
    let obj = status.as_object().unwrap();
    for key in ["blocked", "batch"] {
        assert_eq!(obj.get(key), Some(&Value::Null), "{key}");
    }
    assert_eq!(
        status["today"],
        serde_json::json!({"merged": 0, "bounced": 0, "flaky": 0, "batches": 0,
                           "median_lead_s": null, "verify_s": 0})
    );
}

#[test]
fn status_shows_queue_items_from_the_state_folder() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    let draft = fs::read_to_string(repo.path().join("chute.toml")).unwrap();
    repo.commit_file(
        "chute.toml",
        &draft.replace("budget = 3", "budget = 5"),
        "chore: chute 설정",
    );
    repo.git(&["push", "--quiet", "origin", "main"]);
    let state = serde_json::json!({
        "queue": [{
            "pr": 7, "title": "설정 바꾸기", "branch": "feat/cfg", "head": "abc1234",
            "state": "awaiting_approval", "state_reason": null,
            "agent": {"terminal": "t1rs", "created_ms": 5, "name": "impl-a",
                      "workdir": "/w/cfg", "alive": false},
            "attempts": 1, "submitted_ms": 10, "touches_config": true,
            "last_failure": {"kind": "verify", "step": "test", "mode": "solo",
                             "failed_tests": ["a::b"], "log": "/l", "tree": "def5678",
                             "summary": "test 실패", "report": "/r/1.md", "at_ms": 9}
        }],
        "batch": null, "blocked": null, "verified_bases": [],
        "preflights": {"feat/cfg": {"head": "abc1234", "result": "pass", "report": null, "at_ms": 8}},
        "warnings": []
    });
    fs::write(
        sb.chute_home().join("repos/acme__widget/state.json"),
        state.to_string(),
    )
    .unwrap();

    let item = status_json(&sb, &repo)["queue"][0].clone();
    assert_eq!(
        item,
        serde_json::json!({
            "pr": 7, "title": "설정 바꾸기", "branch": "feat/cfg", "head": "abc1234",
            "state": "awaiting_approval", "state_reason": null,
            "agent": {"terminal": "t1rs", "name": "impl-a", "alive": false},
            "attempts": 1, "budget": 5, "submitted_ms": 10, "touches_config": true,
            "preflight": {"result": "pass", "head": "abc1234"},
            "last_failure": {"kind": "verify", "mode": "solo", "summary": "test 실패", "report": "/r/1.md"}
        })
    );
}

#[test]
fn status_refuses_a_corrupt_state_file() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    ok(chute(&sb, repo.path(), &["init"]));
    let path = sb.chute_home().join("repos/acme__widget/state.json");
    fs::write(&path, "{\"queue\": [").unwrap();
    let out = chute(&sb, repo.path(), &["status", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"queue\": [");
}
