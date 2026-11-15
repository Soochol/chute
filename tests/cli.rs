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
fn unimplemented_commands_exit_2() {
    let sb = Sandbox::new();
    for args in [
        &["preflight"][..],
        &["submit", "12"],
        &["giveup", "--reason", "x"],
        &["approve", "12"],
        &["retry", "#12"],
        &["drop", "12"],
        &["up"],
        &["down", "--now"],
        &["daemon"],
        &["batches"],
        &["revert", "b1", "--no-verify"],
        &["install"],
        &["--skill"],
    ] {
        let out = chute(&sb, sb.root(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("아직 구현되지 않았다"),
            "{args:?}"
        );
    }
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
    repo.write("chute.toml", &draft);

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
        "remote get-url origin",
        support::Reply::ok("https://github.com/acme/widget.git\n"),
    );
    let out = chute(&sb, sb.root(), &["status", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(git.calls(), [["remote", "get-url", "origin"]]);
}
