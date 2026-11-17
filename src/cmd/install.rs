use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::init::AGENTS_LINE;
use super::{CANNOT_RUN, Ctx};
use crate::ext::Ext;

pub(super) mod replica;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Herdr 탭 막대의 chute 버튼과 chute 패널 레이아웃도 설치하고 그 레이아웃으로 바꾼다
    #[arg(long)]
    pub layout: bool,
}

const SKILL_MD: &str = include_str!("../../skills/chute/SKILL.md");

const MOD_FILES: [(&str, &str); 3] = [
    ("main.js", include_str!("../../mod/chute/main.js")),
    ("board.js", include_str!("../../mod/chute/board.js")),
    ("mod.json", include_str!("../../mod/chute/mod.json")),
];

const TABS_CHUTE_FILES: [(&str, &str); 2] = [
    ("chute.js", include_str!("../../mod/chute-tabs/chute.js")),
    ("mod.json", include_str!("../../mod/chute-tabs/mod.json")),
];

pub(super) const HERDR_MOD_FILES: [&str; 3] = ["gpui-kit.d.ts", "herdr.d.ts", "jsconfig.json"];

/// Herdr reads a new layout file within a second; switching earlier can hit a name it does not know yet.
const HERDR_READS_FILES: Duration = Duration::from_millis(1200);

pub struct Layout {
    pub(super) home: PathBuf,
    pub(super) herdr: PathBuf,
}

impl Layout {
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Layout> {
        let set = |key: &str| var(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        let Some(home) = set("HOME") else {
            bail!("HOME이 없어 설치할 곳을 정하지 못했다");
        };
        let herdr = set("HERDR_APP_HOME").unwrap_or_else(|| home.join(".config/herdr-app"));
        Ok(Layout { home, herdr })
    }

    fn codex_root(&self) -> PathBuf {
        self.home.join(".codex")
    }

    fn legacy_claude_skill(&self) -> PathBuf {
        self.home.join(".claude/skills/chute/SKILL.md")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Wrote,
    Updated,
    Unchanged,
    Skipped(String),
    Failed(String),
}

#[derive(Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub outcome: Outcome,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let layout = Layout::from_vars(|key| std::env::var_os(key))?;
    let mut entries = install(&layout);
    let mut notes = String::new();
    if args.layout {
        let replica = install_layout(&layout);
        let installed = replica.iter().all(|e| {
            matches!(
                e.outcome,
                Outcome::Wrote | Outcome::Updated | Outcome::Unchanged
            )
        });
        let wrote = replica
            .iter()
            .any(|e| matches!(e.outcome, Outcome::Wrote | Outcome::Updated));
        entries.extend(replica);
        if installed {
            if wrote {
                std::thread::sleep(HERDR_READS_FILES);
            }
            notes = layout_notes(&switch_layout(&ctx.ext, replica::LAYOUT_NAME));
        }
    } else {
        notes = stale_warnings(&layout);
    }
    print!("{}", render_with(&entries, &notes));
    let failed = entries
        .iter()
        .any(|e| matches!(e.outcome, Outcome::Failed(_)));
    Ok(if failed {
        ExitCode::from(CANNOT_RUN)
    } else {
        ExitCode::SUCCESS
    })
}

pub fn install(layout: &Layout) -> Vec<Entry> {
    let mut entries = Vec::new();
    let root = layout.codex_root();
    let file = root.join("skills/chute/SKILL.md");
    if root.is_dir() {
        entries.push(sync(&file, SKILL_MD));
    } else {
        entries.push(Entry {
            outcome: Outcome::Skipped(format!("{}가 없다", root.display())),
            path: file,
        });
    }
    let legacy = layout.legacy_claude_skill();
    if legacy.exists() {
        entries.push(Entry {
            outcome: Outcome::Skipped(
                "Claude Code는 plugin이 스킬을 준다. 이 파일은 이전 `chute install`이 쓴 것이라 plugin과 겹치니 직접 지운다"
                    .to_owned(),
            ),
            path: legacy,
        });
    }
    let mod_dir = layout.herdr.join("mods/chute");
    if layout.herdr.is_dir() {
        for (name, text) in MOD_FILES {
            entries.push(sync(&mod_dir.join(name), text));
        }
    } else {
        entries.push(Entry {
            outcome: Outcome::Skipped(format!("Herdr 홈 {}이 없다", layout.herdr.display())),
            path: mod_dir,
        });
    }
    entries
}

pub(super) fn switch_layout(ext: &Ext, name: &str) -> Result<(), String> {
    let arg = serde_json::json!({ "name": name }).to_string();
    let out = ext
        .herdr_app(["do", "layout.switch", arg.as_str()])
        .map_err(|e| e.to_string())?;
    out.ok().map(drop).map_err(|e| e.to_string())
}

fn layout_notes(switched: &Result<(), String>) -> String {
    let mut out = String::new();
    match switched {
        Ok(()) => out.push_str("chute 레이아웃으로 바꿨다.\n"),
        Err(why) => out.push_str(&format!(
            "Herdr의 레이아웃을 바꾸지 못했다({why}). 설치는 끝났다. Herdr가 떠 있을 때 `herdr-app do layout.switch '{{\"name\":\"chute\"}}'`를 실행하거나 ctrl-shift-l로 chute를 고른다.\n"
        )),
    }
    out.push_str(
        "단축키를 쓰려면 Herdr의 keys.json에 다음 구역을 직접 더한다. chute는 keys.json을 읽지도 쓰지도 않는다.\n",
    );
    out.push_str(
        "  \"layout:chute\": { \"ctrl-shift-g\": \"layout.toggle:panel\", \"ctrl-shift-u\": \"layout.toggle:chute\" }\n",
    );
    out.push_str("이 레이아웃과 탭 막대 mod는 chute가 만드는 파일이니 고치지 않는다. 되돌리려면 `chute uninstall --layout`을 실행한다.\n");
    out
}

struct Generated {
    layout: Result<String, String>,
    main_js: Result<String, String>,
}

fn generate(herdr: &Path) -> Generated {
    let read = |rel: &str| {
        fs::read_to_string(herdr.join(rel))
            .map_err(|e| format!("Herdr 기본 파일 {rel}을 읽지 못했다: {e}"))
    };
    Generated {
        layout: read("layouts/main.json")
            .and_then(|text| replica::build_layout(&text).map_err(|e| format!("{e:#}"))),
        main_js: read("mods/tabs/main.js")
            .and_then(|text| replica::build_tabs_main(&text).map_err(|e| format!("{e:#}"))),
    }
}

pub(super) fn layout_file(herdr: &Path) -> PathBuf {
    herdr
        .join("layouts")
        .join(format!("{}.json", replica::LAYOUT_NAME))
}

pub(super) fn tabs_dir(herdr: &Path) -> PathBuf {
    herdr.join("mods").join(replica::TABS_MOD)
}

const NOT_OURS: &str = "chute의 것이 아니라 덮어쓰지 않는다. 지우거나 이름을 바꾼 뒤 다시 실행한다";

fn foreign_layout(path: &Path) -> Option<String> {
    let text = fs::read(path).ok()?;
    (!replica::layout_is_ours(&String::from_utf8_lossy(&text))).then(|| NOT_OURS.to_owned())
}

pub(super) fn tabs_dir_is_ours(dir: &Path) -> Option<bool> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => return Some(false),
    };
    if !meta.is_dir() {
        return Some(false);
    }
    match fs::read(dir.join("main.js")) {
        Ok(bytes) => return Some(replica::tabs_main_is_ours(&String::from_utf8_lossy(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Some(false),
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Some(false);
    };
    Some(entries.flatten().all(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        match TABS_CHUTE_FILES.iter().find(|(own, _)| *own == name) {
            Some((_, text)) => fs::read_to_string(entry.path()).is_ok_and(|got| got == *text),
            None => HERDR_MOD_FILES.contains(&name.as_str()),
        }
    }))
}

pub fn install_layout(layout: &Layout) -> Vec<Entry> {
    let herdr = &layout.herdr;
    if !herdr.is_dir() {
        return vec![Entry {
            path: layout_file(herdr),
            outcome: Outcome::Skipped(format!("Herdr 홈 {}이 없다", herdr.display())),
        }];
    }
    let generated = generate(herdr);
    let dir = tabs_dir(herdr);
    let layout_path = layout_file(herdr);
    let main_path = dir.join("main.js");
    let mut plan: Vec<(PathBuf, Result<String, String>)> = vec![
        (
            layout_path.clone(),
            match foreign_layout(&layout_path) {
                Some(why) => Err(why),
                None => generated.layout,
            },
        ),
        (
            main_path.clone(),
            match tabs_dir_is_ours(&dir) {
                Some(false) => Err(NOT_OURS.to_owned()),
                _ => generated.main_js,
            },
        ),
    ];
    plan.extend(
        TABS_CHUTE_FILES
            .iter()
            .map(|(name, text)| (dir.join(name), Ok((*text).to_owned()))),
    );
    if plan.iter().all(|(_, text)| text.is_ok()) {
        return plan
            .into_iter()
            .map(|(path, text)| sync(&path, &text.unwrap_or_default()))
            .collect();
    }
    plan.into_iter()
        .map(|(path, text)| Entry {
            outcome: match text {
                Ok(_) => Outcome::Skipped("함께 쓰는 파일이 막혀 쓰지 않았다".to_owned()),
                Err(why) => Outcome::Failed(why),
            },
            path,
        })
        .collect()
}

pub fn stale_warnings(layout: &Layout) -> String {
    let herdr = &layout.herdr;
    let dir = tabs_dir(herdr);
    let layout_path = layout_file(herdr);
    let tabs_ours = tabs_dir_is_ours(&dir) == Some(true);
    let installed_layout = fs::read_to_string(&layout_path)
        .ok()
        .filter(|text| replica::layout_is_ours(text));
    if !tabs_ours && installed_layout.is_none() {
        return String::new();
    }
    let generated = generate(herdr);
    let mut found = Vec::new();
    if tabs_ours {
        match fs::read_to_string(dir.join("main.js")) {
            Err(_) => found.push("설치된 mods/chute-tabs/main.js가 없다".to_owned()),
            Ok(installed) => match &generated.main_js {
                Err(why) => found.push(format!(
                    "Herdr의 기본 탭 막대가 바뀌어 chute 버튼을 다시 끼우지 못한다({why})"
                )),
                Ok(text) if *text != installed => {
                    let current =
                        fs::read_to_string(herdr.join("mods/tabs/main.js")).unwrap_or_default();
                    if replica::recorded_fingerprint(&installed)
                        != Some(replica::fingerprint_hex(&current).as_str())
                    {
                        found.push("Herdr가 기본 탭 막대(mods/tabs/main.js)를 바꿨다".to_owned());
                    } else {
                        found.push("설치된 mods/chute-tabs/main.js가 지금 chute가 만드는 것과 다르다(chute를 올렸거나 파일을 고쳤다)".to_owned());
                    }
                }
                Ok(_) => {}
            },
        }
        for (name, text) in TABS_CHUTE_FILES {
            if fs::read_to_string(dir.join(name)).ok().as_deref() != Some(text) {
                found.push(format!(
                    "설치된 mods/chute-tabs/{name}가 없거나 이 chute의 것과 다르다"
                ));
            }
        }
        if installed_layout.is_none() {
            found.push("layouts/chute.json이 없다".to_owned());
        }
    } else if !dir.exists() {
        found.push("mods/chute-tabs가 없어 chute 레이아웃의 탭 막대를 찾지 못한다".to_owned());
    }
    if let Some(installed) = installed_layout {
        match &generated.layout {
            Err(why) => found.push(format!(
                "Herdr의 기본 main 레이아웃으로 chute 레이아웃을 다시 만들지 못한다({why})"
            )),
            Ok(text) if *text != installed => {
                found.push("Herdr의 기본 main이 바뀌었거나 chute 레이아웃을 고쳤다".to_owned())
            }
            Ok(_) => {}
        }
    }
    if found.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for line in found {
        out.push_str(&format!("경고: {line}\n"));
    }
    out.push_str("`chute install --layout`을 다시 실행해 지금 Herdr 기본 파일로 다시 만든다. 이번에는 파일을 쓰지 않았다.\n");
    out
}

fn sync(path: &Path, text: &str) -> Entry {
    let outcome = match place(path, text) {
        Ok(outcome) => outcome,
        Err(e) => Outcome::Failed(format!("{e:#}")),
    };
    Entry {
        path: path.to_owned(),
        outcome,
    }
}

fn place(path: &Path, text: &str) -> Result<Outcome> {
    let outcome = match fs::read(path) {
        Ok(current) if current == text.as_bytes() => return Ok(Outcome::Unchanged),
        Ok(_) => Outcome::Updated,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Outcome::Wrote,
        Err(e) => return Err(e).with_context(|| format!("읽지 못했다: {}", path.display())),
    };
    write_atomic(path, text)?;
    Ok(outcome)
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("상위 폴더가 없는 경로다")?;
    fs::create_dir_all(dir).with_context(|| format!("만들지 못했다: {}", dir.display()))?;
    let name = path.file_name().context("파일 이름이 없는 경로다")?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".{}.chute-tmp", std::process::id()));
    let temp = dir.join(temp_name);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .with_context(|| format!("쓰지 못했다: {}", temp.display()));
    let renamed = written.and_then(|()| {
        fs::rename(&temp, path).with_context(|| format!("바꾸지 못했다: {}", path.display()))
    });
    if renamed.is_err() {
        let _ = fs::remove_file(&temp);
    }
    renamed
}

pub fn render(entries: &[Entry]) -> String {
    render_with(entries, "")
}

pub fn render_with(entries: &[Entry], notes: &str) -> String {
    let mut out = String::new();
    for entry in entries {
        let line = match &entry.outcome {
            Outcome::Wrote => format!("새로 썼다: {}", entry.path.display()),
            Outcome::Updated => format!("바꿔 썼다: {}", entry.path.display()),
            Outcome::Unchanged => format!("그대로: {}", entry.path.display()),
            Outcome::Skipped(why) => format!("건너뛰었다: {} ({why})", entry.path.display()),
            Outcome::Failed(why) => format!("실패했다: {} ({why})", entry.path.display()),
        };
        out.push_str(&line);
        out.push('\n');
    }
    let changed = entries
        .iter()
        .filter(|e| matches!(e.outcome, Outcome::Wrote | Outcome::Updated))
        .count();
    let failed = entries
        .iter()
        .filter(|e| matches!(e.outcome, Outcome::Failed(_)))
        .count();
    if failed > 0 {
        out.push_str(&format!(
            "{failed}개를 설치하지 못했다. 위의 이유를 고친 뒤 다시 실행한다.\n"
        ));
    } else if changed == 0 {
        out.push_str("바뀐 것이 없다.\n");
    } else {
        out.push_str(&format!("파일 {changed}개를 썼다.\n"));
    }
    if entries
        .iter()
        .any(|e| matches!(e.outcome, Outcome::Skipped(_)) && e.path.ends_with("mods/chute"))
    {
        out.push_str(
            "Herdr를 한 번 실행한 뒤 `chute install`을 다시 실행하면 현황판을 설치한다.\n",
        );
    }
    out.push_str(notes);
    out.push('\n');
    out.push_str(
        "대상 저장소의 AGENTS.md에 다음 한 줄을 넣는다. chute는 AGENTS.md와 herdr 템플릿을 고치지 않는다.\n",
    );
    out.push_str(AGENTS_LINE);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use super::*;

    fn layout(home: &Path) -> Layout {
        Layout::from_vars(|key| (key == "HOME").then(|| home.as_os_str().to_owned())).unwrap()
    }

    fn files_under(root: &Path) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut pending = vec![root.to_owned()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.insert(path.strip_prefix(root).unwrap().display().to_string());
                }
            }
        }
        found
    }

    fn full_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        for dir in [".claude", ".codex", ".config/herdr-app"] {
            fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        home
    }

    #[test]
    fn installs_the_codex_skill_and_mod_with_repository_content() {
        let home = full_home();
        install(&layout(home.path()));
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let got = fs::read_to_string(home.path().join(".codex/skills/chute/SKILL.md"));
        assert_eq!(
            got.unwrap(),
            fs::read_to_string(repo.join("skills/chute/SKILL.md")).unwrap()
        );
        let mod_dir = home.path().join(".config/herdr-app/mods/chute");
        for (name, _) in MOD_FILES {
            let got = fs::read_to_string(mod_dir.join(name)).unwrap();
            assert_eq!(
                got,
                fs::read_to_string(repo.join("mod/chute").join(name)).unwrap()
            );
        }
    }

    #[test]
    fn second_run_changes_nothing() {
        let home = full_home();
        let layout = layout(home.path());
        install(&layout);
        let before = files_under(home.path());
        let second = install(&layout);
        assert_eq!(files_under(home.path()), before);
        assert!(second.iter().all(|e| e.outcome == Outcome::Unchanged));
        assert!(render(&second).contains("바뀐 것이 없다"));
    }

    #[test]
    fn a_changed_file_is_replaced_and_reported() {
        let home = full_home();
        let layout = layout(home.path());
        install(&layout);
        let skill = home.path().join(".codex/skills/chute/SKILL.md");
        fs::write(&skill, "낡은 내용").unwrap();
        let entries = install(&layout);
        assert_eq!(fs::read_to_string(&skill).unwrap(), SKILL_MD);
        let entry = entries.iter().find(|e| e.path == skill).unwrap();
        assert_eq!(entry.outcome, Outcome::Updated);
    }

    #[test]
    fn skips_tools_that_are_not_installed() {
        let home = full_home();
        fs::remove_dir(home.path().join(".codex")).unwrap();
        let entries = install(&layout(home.path()));
        assert!(!home.path().join(".codex").exists());
        assert!(
            home.path()
                .join(".config/herdr-app/mods/chute/main.js")
                .exists()
        );
        assert!(render(&entries).contains("건너뛰었다"));
    }

    #[test]
    fn skips_the_mod_without_a_herdr_home() {
        let home = full_home();
        fs::remove_dir(home.path().join(".config/herdr-app")).unwrap();
        let entries = install(&layout(home.path()));
        assert!(!home.path().join(".config/herdr-app").exists());
        assert!(home.path().join(".codex/skills/chute/SKILL.md").exists());
        let text = render(&entries);
        assert!(text.contains("Herdr 홈"), "{text}");
    }

    #[test]
    fn herdr_app_home_moves_the_mod() {
        let home = full_home();
        let elsewhere = tempfile::tempdir().unwrap();
        let layout = Layout::from_vars(|key| match key {
            "HOME" => Some(home.path().as_os_str().to_owned()),
            "HERDR_APP_HOME" => Some(elsewhere.path().as_os_str().to_owned()),
            _ => None,
        })
        .unwrap();
        install(&layout);
        assert!(elsewhere.path().join("mods/chute/mod.json").exists());
        assert!(!home.path().join(".config/herdr-app/mods").exists());
    }

    #[test]
    fn touches_only_the_documented_paths() {
        let home = full_home();
        install(&layout(home.path()));
        let expected: BTreeSet<String> = [
            ".codex/skills/chute/SKILL.md",
            ".config/herdr-app/mods/chute/main.js",
            ".config/herdr-app/mods/chute/board.js",
            ".config/herdr-app/mods/chute/mod.json",
        ]
        .map(str::to_owned)
        .into();
        assert_eq!(files_under(home.path()), expected);
    }

    #[test]
    fn leaves_claude_to_the_plugin_and_points_out_an_old_skill_file() {
        let home = full_home();
        let entries = install(&layout(home.path()));
        assert!(!home.path().join(".claude/skills").exists());
        assert!(
            entries
                .iter()
                .all(|e| !e.path.starts_with(home.path().join(".claude")))
        );

        let old = home.path().join(".claude/skills/chute/SKILL.md");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, "이전 설치").unwrap();
        let entries = install(&layout(home.path()));
        assert_eq!(fs::read_to_string(&old).unwrap(), "이전 설치");
        let entry = entries.iter().find(|e| e.path == old).unwrap();
        assert!(matches!(entry.outcome, Outcome::Skipped(_)));
        assert!(render(&entries).contains("plugin"));
    }

    #[test]
    fn prints_the_same_agents_line_as_init() {
        assert!(render(&[]).contains(AGENTS_LINE));
    }

    #[test]
    fn needs_a_home() {
        assert!(Layout::from_vars(|_| None).is_err());
    }

    #[test]
    fn a_blocked_target_does_not_stop_the_others() {
        let home = full_home();
        fs::create_dir_all(home.path().join(".codex/skills/chute/SKILL.md")).unwrap();
        let entries = install(&layout(home.path()));
        assert!(
            home.path()
                .join(".config/herdr-app/mods/chute/main.js")
                .is_file()
        );
        let failed: Vec<_> = entries
            .iter()
            .filter(|e| matches!(e.outcome, Outcome::Failed(_)))
            .collect();
        assert_eq!(failed.len(), 1);
        let text = render(&entries);
        assert!(text.contains("실패했다"), "{text}");
        assert!(!text.contains("바뀐 것이 없다"), "{text}");
    }

    #[test]
    fn a_file_that_is_not_utf8_is_replaced() {
        let home = full_home();
        let skill = home.path().join(".codex/skills/chute/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, [0xff, 0xfe]).unwrap();
        let entries = install(&layout(home.path()));
        assert_eq!(fs::read_to_string(&skill).unwrap(), SKILL_MD);
        let entry = entries.iter().find(|e| e.path == skill).unwrap();
        assert_eq!(entry.outcome, Outcome::Updated);
    }

    #[test]
    fn never_writes_through_a_link_left_at_the_temporary_name() {
        let home = full_home();
        let dir = home.path().join(".codex/skills/chute");
        fs::create_dir_all(&dir).unwrap();
        let victim = home.path().join("victim");
        fs::write(&victim, "precious").unwrap();
        let temp = format!(".SKILL.md.{}.chute-tmp", std::process::id());
        std::os::unix::fs::symlink(&victim, dir.join(temp)).unwrap();
        let entries = install(&layout(home.path()));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.outcome, Outcome::Failed(_)))
        );
    }

    fn herdr(home: &Path) -> std::path::PathBuf {
        home.join(".config/herdr-app")
    }

    fn with_originals(home: &Path) {
        let base = herdr(home);
        fs::create_dir_all(base.join("layouts")).unwrap();
        fs::create_dir_all(base.join("mods/tabs")).unwrap();
        fs::write(base.join("layouts/main.json"), replica::fake::main_layout()).unwrap();
        fs::write(base.join("mods/tabs/main.js"), replica::fake::tabs_main()).unwrap();
    }

    fn all_installed(entries: &[Entry], outcome: Outcome) -> bool {
        entries.iter().all(|e| e.outcome == outcome)
    }

    #[test]
    fn layout_writes_four_files_and_a_second_run_changes_nothing() {
        let home = full_home();
        with_originals(home.path());
        let layout = layout(home.path());
        let first = install_layout(&layout);
        assert!(all_installed(&first, Outcome::Wrote), "{first:?}");
        assert_eq!(first.len(), 4);
        let base = herdr(home.path());
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(base.join("layouts/chute.json")).unwrap())
                .unwrap();
        let text = json.to_string();
        assert!(text.contains("\"mod\":\"chute-tabs\""), "{text}");
        let panel = text.find("\"id\":\"panel-body\"").unwrap();
        assert!(text[panel..].contains("\"id\":\"chute\""), "{text}");
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        for name in ["chute.js", "mod.json"] {
            assert_eq!(
                fs::read_to_string(base.join("mods/chute-tabs").join(name)).unwrap(),
                fs::read_to_string(repo.join("mod/chute-tabs").join(name)).unwrap()
            );
        }
        let before = files_under(home.path());
        let second = install_layout(&layout);
        assert!(all_installed(&second, Outcome::Unchanged), "{second:?}");
        assert_eq!(files_under(home.path()), before);
    }

    #[test]
    fn layout_never_touches_the_herdr_defaults() {
        let home = full_home();
        with_originals(home.path());
        let base = herdr(home.path());
        let before = (
            fs::read(base.join("layouts/main.json")).unwrap(),
            fs::read(base.join("mods/tabs/main.js")).unwrap(),
        );
        install_layout(&layout(home.path()));
        assert_eq!(fs::read(base.join("layouts/main.json")).unwrap(), before.0);
        assert_eq!(fs::read(base.join("mods/tabs/main.js")).unwrap(), before.1);
    }

    #[test]
    fn a_missing_edit_line_fails_and_leaves_installed_files_alone() {
        let home = full_home();
        with_originals(home.path());
        let layout = layout(home.path());
        install_layout(&layout);
        let before_files = files_under(home.path());
        let installed = fs::read(herdr(home.path()).join("mods/chute-tabs/main.js")).unwrap();
        let original = herdr(home.path()).join("mods/tabs/main.js");
        let changed = fs::read_to_string(&original)
            .unwrap()
            .lines()
            .filter(|l| !l.contains("const on = panel"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&original, changed).unwrap();
        let entries = install_layout(&layout);
        let main = entries
            .iter()
            .find(|e| e.path.ends_with("main.js"))
            .unwrap();
        assert!(
            matches!(&main.outcome, Outcome::Failed(why) if why.contains("바꿔 끼울 줄이 없다")),
            "{main:?}"
        );
        assert_eq!(
            fs::read(herdr(home.path()).join("mods/chute-tabs/main.js")).unwrap(),
            installed
        );
        assert_eq!(files_under(home.path()), before_files);
        assert!(
            entries
                .iter()
                .all(|e| !matches!(e.outcome, Outcome::Wrote | Outcome::Updated))
        );
    }

    #[test]
    fn a_fresh_install_with_a_broken_original_writes_nothing() {
        let home = full_home();
        with_originals(home.path());
        fs::write(
            herdr(home.path()).join("mods/tabs/main.js"),
            "nothing useful\n",
        )
        .unwrap();
        let entries = install_layout(&layout(home.path()));
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.outcome, Outcome::Failed(_)))
        );
        assert!(!herdr(home.path()).join("layouts/chute.json").exists());
        assert!(!herdr(home.path()).join("mods/chute-tabs").exists());
    }

    #[test]
    fn a_layout_that_is_not_chutes_is_not_overwritten() {
        let home = full_home();
        with_originals(home.path());
        let theirs = herdr(home.path()).join("layouts/chute.json");
        fs::write(&theirs, r#"{"id":"tabs","content":{"mod":"mine"}}"#).unwrap();
        let entries = install_layout(&layout(home.path()));
        assert_eq!(
            fs::read_to_string(&theirs).unwrap(),
            r#"{"id":"tabs","content":{"mod":"mine"}}"#
        );
        let entry = entries.iter().find(|e| e.path == theirs).unwrap();
        assert!(
            matches!(&entry.outcome, Outcome::Failed(why) if why.contains("chute의 것이 아니")),
            "{entry:?}"
        );
        assert!(!herdr(home.path()).join("mods/chute-tabs").exists());
    }

    #[test]
    fn a_main_js_that_is_not_chutes_blocks_the_whole_mod() {
        let home = full_home();
        with_originals(home.path());
        let dir = herdr(home.path()).join("mods/chute-tabs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("main.js"), "// mine\n").unwrap();
        let entries = install_layout(&layout(home.path()));
        assert_eq!(
            fs::read_to_string(dir.join("main.js")).unwrap(),
            "// mine\n"
        );
        assert!(!dir.join("chute.js").exists());
        assert!(!herdr(home.path()).join("layouts/chute.json").exists());
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.outcome, Outcome::Failed(_)))
        );
    }

    #[test]
    fn without_a_herdr_home_layout_is_skipped() {
        let home = full_home();
        fs::remove_dir(herdr(home.path())).unwrap();
        let entries = install_layout(&layout(home.path()));
        assert!(matches!(entries[0].outcome, Outcome::Skipped(_)));
    }

    #[test]
    fn plain_install_warns_when_herdr_changed_the_originals_and_writes_nothing() {
        let home = full_home();
        with_originals(home.path());
        let layout = layout(home.path());
        assert_eq!(stale_warnings(&layout), "");
        install_layout(&layout);
        assert_eq!(stale_warnings(&layout), "");

        let base = herdr(home.path());
        let main = base.join("mods/tabs/main.js");
        let mut text = fs::read_to_string(&main).unwrap();
        text.push_str("// a new Herdr release\n");
        fs::write(&main, text).unwrap();
        let before = files_under(home.path());
        let warning = stale_warnings(&layout);
        assert!(warning.contains("Herdr가 기본 탭 막대"), "{warning}");
        assert!(warning.contains("chute install --layout"), "{warning}");
        assert_eq!(files_under(home.path()), before);

        install_layout(&layout);
        assert_eq!(stale_warnings(&layout), "");
        fs::write(
            base.join("layouts/main.json"),
            r#"{"id":"root","children":[]}"#,
        )
        .unwrap();
        let warning = stale_warnings(&layout);
        assert!(warning.contains("레이아웃"), "{warning}");
    }

    #[test]
    fn plain_install_run_does_not_write_the_layout_files() {
        let home = full_home();
        with_originals(home.path());
        install(&layout(home.path()));
        let base = herdr(home.path());
        assert!(!base.join("layouts/chute.json").exists());
        assert!(!base.join("mods/chute-tabs").exists());
    }

    #[test]
    fn notes_tell_how_to_switch_when_herdr_cannot() {
        let ok = layout_notes(&Ok(()));
        assert!(ok.contains("chute 레이아웃으로 바꿨다"));
        assert!(ok.contains("layout:chute") && ok.contains("ctrl-shift-u"));
        let failed = layout_notes(&Err("꺼져 있다".to_owned()));
        assert!(
            failed.contains("layout.switch") && failed.contains("ctrl-shift-l"),
            "{failed}"
        );
        assert!(failed.contains("설치는 끝났다"));
    }

    #[test]
    fn a_file_where_the_mod_folder_belongs_blocks_every_layout_file() {
        let home = full_home();
        with_originals(home.path());
        let blocker = herdr(home.path()).join("mods/chute-tabs");
        fs::write(&blocker, "user notes").unwrap();
        let entries = install_layout(&layout(home.path()));
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.outcome, Outcome::Failed(_)))
        );
        assert!(!herdr(home.path()).join("layouts/chute.json").exists());
        assert_eq!(fs::read_to_string(&blocker).unwrap(), "user notes");
    }

    #[test]
    fn a_folder_without_main_js_is_chutes_only_when_it_holds_nothing_else_of_theirs() {
        let home = full_home();
        with_originals(home.path());
        let dir = herdr(home.path()).join("mods/chute-tabs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mod.json"), r#"{"name":"mine"}"#).unwrap();
        fs::write(dir.join("chute.js"), "// my code\n").unwrap();
        let entries = install_layout(&layout(home.path()));
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.outcome, Outcome::Failed(_)))
        );
        assert_eq!(
            fs::read_to_string(dir.join("chute.js")).unwrap(),
            "// my code\n"
        );
        assert!(!dir.join("main.js").exists());
    }

    #[test]
    fn plain_install_warns_about_missing_installed_files() {
        let home = full_home();
        with_originals(home.path());
        let layout = layout(home.path());
        install_layout(&layout);
        let dir = herdr(home.path()).join("mods/chute-tabs");
        fs::remove_file(dir.join("main.js")).unwrap();
        assert!(
            stale_warnings(&layout).contains("main.js"),
            "{}",
            stale_warnings(&layout)
        );

        install_layout(&layout);
        fs::remove_dir_all(&dir).unwrap();
        let warning = stale_warnings(&layout);
        assert!(warning.contains("mods/chute-tabs"), "{warning}");
        assert!(warning.contains("chute install --layout"));
    }
}
