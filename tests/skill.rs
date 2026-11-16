mod support;

use std::fs;
use std::path::Path;

use support::{Sandbox, TestRepo};

#[test]
fn skill_flag_prints_the_manual_unchanged() {
    let sb = Sandbox::new();
    let out = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .arg("--skill")
        .current_dir(sb.root())
        .output()
        .unwrap();
    assert!(out.status.success());
    let manual = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("skill/manual.md")).unwrap();
    assert_eq!(out.stdout, manual);
}

#[test]
fn skill_flag_prints_the_manual_when_no_state_folder_can_be_named() {
    let sb = Sandbox::new();
    let out = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .arg("--skill")
        .env_remove("HOME")
        .env_remove("CHUTE_HOME")
        .env_remove("XDG_STATE_HOME")
        .current_dir(sb.root())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manual = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("skill/manual.md")).unwrap();
    assert_eq!(out.stdout, manual);
}

#[test]
fn skill_flag_rejects_a_subcommand() {
    let sb = Sandbox::new();
    let out = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .args(["--skill", "status"])
        .current_dir(sb.root())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

const STEP: &str = "[[verify]]\nname = \"t\"\nrun = \"true\"\n";
const NOTICE: &str = "이 저장소에서는 submit하지 말고 PR과 HEAD를 보고한다";

fn skill_output(sb: &Sandbox, dir: &Path) -> String {
    let out = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .arg("--skill")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

fn registered_with(chute_toml: &str) -> (Sandbox, TestRepo) {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    repo.commit_file("chute.toml", chute_toml, "chore: 설정");
    repo.git(&["push", "--quiet", "origin", "main"]);
    let init = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .arg("init")
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    (sb, repo)
}

#[test]
fn skill_flag_starts_with_the_report_notice_when_the_repo_requires_head() {
    let (sb, repo) = registered_with(&format!("require_head = true\n{STEP}"));
    let text = skill_output(&sb, repo.path());
    assert!(text.lines().next().unwrap().starts_with(NOTICE), "{text}");
    let manual =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("skill/manual.md")).unwrap();
    assert!(text.ends_with(&manual));
}

#[test]
fn skill_flag_prints_only_the_manual_for_other_repos_and_unregistered_folders() {
    let manual =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("skill/manual.md")).unwrap();
    let (sb, repo) = registered_with(STEP);
    assert_eq!(skill_output(&sb, repo.path()), manual);
    let lone = Sandbox::new();
    let unregistered = TestRepo::new(&lone, "acme/other");
    assert_eq!(skill_output(&lone, unregistered.path()), manual);
}
