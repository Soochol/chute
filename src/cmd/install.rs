use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use super::init::AGENTS_LINE;
use super::{CANNOT_RUN, Ctx};

#[derive(clap::Args, Debug)]
pub struct Args {}

const SKILL_MD: &str = include_str!("../../skills/chute/SKILL.md");

const MOD_FILES: [(&str, &str); 3] = [
    ("main.js", include_str!("../../mod/chute/main.js")),
    ("board.js", include_str!("../../mod/chute/board.js")),
    ("mod.json", include_str!("../../mod/chute/mod.json")),
];

pub struct Layout {
    home: PathBuf,
    herdr: PathBuf,
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

pub fn run(_args: Args, _ctx: &Ctx) -> Result<ExitCode> {
    let layout = Layout::from_vars(|key| std::env::var_os(key))?;
    let entries = install(&layout);
    print!("{}", render(&entries));
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
}
