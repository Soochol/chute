mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Output;

use support::Sandbox;

const AGENTS_LINE: &str = chute::cmd::init::AGENTS_LINE;

fn repo_file(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

fn install(sb: &Sandbox) -> Output {
    sb.command(env!("CARGO_BIN_EXE_chute"))
        .arg("install")
        .current_dir(sb.root())
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn tree(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let shown = path.strip_prefix(root).unwrap().display().to_string();
            found.insert(shown);
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    found
}

fn herdr_home(sb: &Sandbox) -> std::path::PathBuf {
    sb.home().join(".config/herdr-app")
}

fn prepare_full_home(sb: &Sandbox) {
    for dir in [".claude", ".codex", ".config/herdr-app"] {
        fs::create_dir_all(sb.home().join(dir)).unwrap();
    }
}

#[test]
fn installs_both_skills_and_the_mod_with_repository_content() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    stdout(&install(&sb));

    let skill = repo_file("skill/SKILL.md");
    for tool in [".claude", ".codex"] {
        let path = sb.home().join(tool).join("skills/chute/SKILL.md");
        assert_eq!(fs::read_to_string(path).unwrap(), skill);
    }
    let mod_dir = herdr_home(&sb).join("mods/chute");
    for name in ["main.js", "board.js", "mod.json"] {
        let got = fs::read_to_string(mod_dir.join(name)).unwrap();
        assert_eq!(got, repo_file(&format!("mod/chute/{name}")), "{name}");
    }
}

#[test]
fn second_run_changes_nothing_and_says_so() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    let first = stdout(&install(&sb));
    assert!(first.contains("새로 썼다"), "{first}");
    let before = tree(&sb.home());
    let second = stdout(&install(&sb));
    assert_eq!(tree(&sb.home()), before);
    assert!(second.contains("바뀐 것이 없다"), "{second}");
    assert!(!second.contains("새로 썼다"), "{second}");
    assert!(second.contains("그대로"), "{second}");
}

#[test]
fn skips_codex_when_it_is_not_installed() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    fs::remove_dir(sb.home().join(".codex")).unwrap();
    let text = stdout(&install(&sb));
    assert!(!sb.home().join(".codex").exists());
    assert!(sb.home().join(".claude/skills/chute/SKILL.md").exists());
    assert!(herdr_home(&sb).join("mods/chute/main.js").exists());
    assert!(text.contains("건너뛰었다"), "{text}");
    assert!(text.contains(".codex"), "{text}");
}

#[test]
fn skips_the_mod_when_herdr_has_no_home() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    fs::remove_dir(herdr_home(&sb)).unwrap();
    let text = stdout(&install(&sb));
    assert!(!herdr_home(&sb).exists());
    assert!(sb.home().join(".claude/skills/chute/SKILL.md").exists());
    assert!(sb.home().join(".codex/skills/chute/SKILL.md").exists());
    assert!(text.contains("Herdr 홈"), "{text}");
}

#[test]
fn herdr_app_home_chooses_where_the_mod_goes() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    let elsewhere = sb.root().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let out = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .arg("install")
        .env("HERDR_APP_HOME", &elsewhere)
        .current_dir(sb.root())
        .output()
        .unwrap();
    stdout(&out);
    assert!(elsewhere.join("mods/chute/mod.json").exists());
    assert!(!herdr_home(&sb).join("mods").exists());
}

#[test]
fn leaves_templates_and_agents_md_alone() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    fs::write(sb.root().join("AGENTS.md"), "그대로\n").unwrap();
    stdout(&install(&sb));
    assert!(!sb.home().join(".config/herdr-core").exists());
    assert_eq!(
        fs::read_to_string(sb.root().join("AGENTS.md")).unwrap(),
        "그대로\n"
    );
    let expected: BTreeSet<String> = [
        ".claude",
        ".claude/skills",
        ".claude/skills/chute",
        ".claude/skills/chute/SKILL.md",
        ".codex",
        ".codex/skills",
        ".codex/skills/chute",
        ".codex/skills/chute/SKILL.md",
        ".config",
        ".config/herdr-app",
        ".config/herdr-app/mods",
        ".config/herdr-app/mods/chute",
        ".config/herdr-app/mods/chute/board.js",
        ".config/herdr-app/mods/chute/main.js",
        ".config/herdr-app/mods/chute/mod.json",
    ]
    .map(str::to_owned)
    .into();
    assert_eq!(tree(&sb.home()), expected);
}

#[test]
fn install_and_init_print_the_same_agents_line() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    let installed = stdout(&install(&sb));
    assert!(installed.lines().any(|l| l == AGENTS_LINE), "{installed}");

    let repo = support::TestRepo::new(&sb, "acme/widget");
    let out = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .arg("init")
        .current_dir(repo.path())
        .output()
        .unwrap();
    let inited = stdout(&out);
    assert!(inited.lines().any(|l| l == AGENTS_LINE), "{inited}");
}

#[test]
fn the_mod_installed_in_a_temporary_home_passes_herdr_app_check() {
    let Ok(probe) = std::process::Command::new("herdr-app")
        .arg("--version")
        .output()
    else {
        eprintln!("herdr-app이 없어 건너뛴다");
        return;
    };
    if !probe.status.success() {
        return;
    }
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    stdout(&install(&sb));
    let out = sb
        .command("herdr-app")
        .arg("check")
        .arg(herdr_home(&sb).join("mods/chute"))
        .env("HERDR_APP_HOME", herdr_home(&sb))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn concurrent_installs_both_succeed() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    let skill = sb.home().join(".claude/skills/chute/SKILL.md");
    for round in 0..30 {
        let _ = fs::remove_file(&skill);
        let spawn = || {
            sb.command(env!("CARGO_BIN_EXE_chute"))
                .arg("install")
                .current_dir(sb.root())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        };
        let (a, b) = (spawn(), spawn());
        for out in [a.wait_with_output().unwrap(), b.wait_with_output().unwrap()] {
            assert!(
                out.status.success(),
                "round {round}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert_eq!(
            fs::read_to_string(&skill).unwrap(),
            repo_file("skill/SKILL.md")
        );
    }
}

#[test]
fn a_blocked_target_exits_2_and_installs_the_rest() {
    let sb = Sandbox::new();
    prepare_full_home(&sb);
    fs::create_dir_all(sb.home().join(".claude/skills/chute/SKILL.md")).unwrap();
    let out = install(&sb);
    assert_eq!(out.status.code(), Some(2));
    assert!(sb.home().join(".codex/skills/chute/SKILL.md").is_file());
    assert!(herdr_home(&sb).join("mods/chute/mod.json").is_file());
    assert!(String::from_utf8_lossy(&out.stdout).contains("실패했다"));
}
