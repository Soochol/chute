mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use support::{Sandbox, TestRepo};

fn repo_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn json(path: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(repo_path(path)).unwrap()).unwrap()
}

#[test]
fn the_marketplace_lists_this_repository_as_the_chute_plugin() {
    let plugin = json(".claude-plugin/plugin.json");
    let marketplace = json(".claude-plugin/marketplace.json");
    assert_eq!(marketplace["name"], "chute");
    let listed = &marketplace["plugins"][0];
    assert_eq!(listed["name"], plugin["name"]);
    assert_eq!(listed["name"], "chute");
    assert_eq!(listed["version"], plugin["version"]);
    assert_eq!(listed["source"], "./");
    assert_eq!(marketplace["plugins"].as_array().unwrap().len(), 1);
}

#[test]
fn the_plugin_version_follows_the_crate_version() {
    assert_eq!(
        json(".claude-plugin/plugin.json")["version"],
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn the_plugin_skill_is_the_file_chute_install_writes_for_codex() {
    let skill = fs::read_to_string(repo_path("skills/chute/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: chute\n"), "{skill}");
    assert!(skill.contains("chute --skill"));

    let sb = Sandbox::new();
    fs::create_dir_all(sb.home().join(".codex")).unwrap();
    let out = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .arg("install")
        .current_dir(sb.root())
        .output()
        .unwrap();
    assert!(out.status.success());
    let written = fs::read_to_string(sb.home().join(".codex/skills/chute/SKILL.md")).unwrap();
    assert_eq!(written, skill);
    assert!(!repo_path("skill/SKILL.md").exists());
}

#[test]
fn the_hook_config_runs_the_executable_guard_script_on_bash_calls() {
    let config = json("hooks/hooks.json");
    let entries = config["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["matcher"], "Bash");
    let hook = &entries[0]["hooks"][0];
    assert_eq!(hook["type"], "command");
    assert_eq!(hook["command"], "\"${CLAUDE_PLUGIN_ROOT}/hooks/guard.sh\"");
    let mode = fs::metadata(repo_path("hooks/guard.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "hooks/guard.sh는 실행 가능해야 한다");
}

fn path_with(dirs: &[&Path]) -> std::ffi::OsString {
    std::env::join_paths(dirs).unwrap()
}

fn run_guard(sb: &Sandbox, path: &std::ffi::OsStr, cwd: &Path, stdin: &str) -> Output {
    let mut child = sb
        .command("/bin/sh")
        .arg(repo_path("hooks/guard.sh"))
        .env("PATH", path)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn git_dir(sb: &Sandbox) -> PathBuf {
    let bin = sb.root().join("only-git");
    fs::create_dir_all(&bin).unwrap();
    let git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let _ = std::os::unix::fs::symlink(git.trim(), bin.join("git"));
    bin
}

fn bash_call(command: &str, cwd: &Path) -> String {
    serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": cwd,
    })
    .to_string()
}

fn chute_repo(sb: &Sandbox) -> TestRepo {
    let repo = TestRepo::new(sb, "acme/widget");
    repo.commit_file("chute.toml", "approval = \"agent\"\n", "chore: chute 설정");
    repo
}

#[test]
fn the_hook_blocks_through_the_installed_binary_with_exit_code_2() {
    let sb = Sandbox::new();
    let repo = chute_repo(&sb);
    let chute = Path::new(env!("CARGO_BIN_EXE_chute")).parent().unwrap();
    let path = path_with(&[chute, &git_dir(&sb)]);

    for command in ["git push origin main", "gh pr merge 3 --squash"] {
        let out = run_guard(&sb, &path, repo.path(), &bash_call(command, repo.path()));
        assert_eq!(out.status.code(), Some(2), "{command}");
        let why = String::from_utf8_lossy(&out.stderr);
        assert!(why.contains("chute submit"), "{why}");
    }
    for command in ["git push origin feat/a", "chute submit 3", "ls"] {
        let out = run_guard(&sb, &path, repo.path(), &bash_call(command, repo.path()));
        assert_eq!(out.status.code(), Some(0), "{command}");
        assert!(out.stderr.is_empty());
    }
}

#[test]
fn the_hook_blocks_nothing_in_a_repository_without_chute_toml() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/plain");
    let chute = Path::new(env!("CARGO_BIN_EXE_chute")).parent().unwrap();
    let path = path_with(&[chute, &git_dir(&sb)]);
    for command in ["git push origin main", "gh pr merge 3"] {
        let out = run_guard(&sb, &path, repo.path(), &bash_call(command, repo.path()));
        assert_eq!(out.status.code(), Some(0), "{command}");
    }
}

#[test]
fn a_hook_input_the_binary_cannot_read_does_not_block() {
    let sb = Sandbox::new();
    let repo = chute_repo(&sb);
    let chute = Path::new(env!("CARGO_BIN_EXE_chute")).parent().unwrap();
    let path = path_with(&[chute, &git_dir(&sb)]);
    let out = run_guard(&sb, &path, repo.path(), "not json");
    assert_eq!(out.status.code(), Some(1));
    assert!(!out.stderr.is_empty());
}

#[test]
fn without_the_binary_the_hook_lets_calls_through_and_says_so_only_in_chute_repositories() {
    let sb = Sandbox::new();
    let path = path_with(&[&git_dir(&sb)]);

    let chute = chute_repo(&sb);
    let call = bash_call("git push origin main", chute.path());
    let out = run_guard(&sb, &path, chute.path(), &call);
    assert_eq!(out.status.code(), Some(1));
    let why = String::from_utf8_lossy(&out.stderr);
    assert!(why.contains("cargo install"), "{why}");

    let plain = TestRepo::new(&sb, "acme/plain");
    let out = run_guard(&sb, &path, plain.path(), &call);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());

    let outside = sb.root().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let out = run_guard(&sb, &path, &outside, &call);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn claude_plugin_validate_accepts_the_plugin_and_the_marketplace() {
    let sb = Sandbox::new();
    let probe = sb.command("claude").arg("plugin").arg("--help").output();
    if !probe.is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("validate")) {
        eprintln!("claude plugin validate가 없어 건너뛴다");
        return;
    }
    for target in [".", ".claude-plugin/marketplace.json"] {
        let out = sb
            .command("claude")
            .env("CLAUDE_CONFIG_DIR", sb.root().join("claude-config"))
            .args(["plugin", "validate"])
            .arg(repo_path(target))
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        assert!(text.contains("Validation passed"), "{text}");
    }
}
