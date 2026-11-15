mod support;

use std::fs;
use std::path::Path;

use support::Sandbox;

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
