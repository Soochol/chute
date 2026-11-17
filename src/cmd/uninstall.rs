use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use serde::Deserialize;

use super::install::replica::layout_is_ours;
use super::install::{
    HERDR_MOD_FILES, Layout, layout_file, switch_layout, tabs_dir, tabs_dir_is_ours,
};
use super::{CANNOT_RUN, Ctx, REFUSED};
use crate::ext::Ext;

const OURS_IN_TABS_DIR: [&str; 3] = ["main.js", "chute.js", "mod.json"];

#[derive(clap::Args, Debug)]
pub struct Args {
    /// `chute install --layout`이 만든 chute 레이아웃과 탭 막대 mod를 지운다
    #[arg(long)]
    pub layout: bool,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    if !args.layout {
        eprintln!("chute: 지금은 --layout만 되돌린다. `chute uninstall --layout`을 실행한다.");
        return Ok(ExitCode::from(REFUSED));
    }
    let layout = Layout::from_vars(|key| std::env::var_os(key))?;
    let report = uninstall_layout(&layout, &ctx.ext);
    print!("{}", report.text);
    Ok(if report.failed {
        ExitCode::from(CANNOT_RUN)
    } else {
        ExitCode::SUCCESS
    })
}

pub struct Report {
    pub text: String,
    pub failed: bool,
}

#[derive(Deserialize)]
struct Status {
    layout: Option<String>,
}

fn shown_layout(ext: &Ext) -> Result<Option<String>, String> {
    let out = ext
        .herdr_app(["status"])
        .map_err(|e| e.to_string())?
        .ok()
        .map_err(|e| e.to_string())?;
    let status: Status = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    Ok(status.layout)
}

pub fn uninstall_layout(layout: &Layout, ext: &Ext) -> Report {
    let mut report = Report {
        text: String::new(),
        failed: false,
    };
    let herdr = &layout.herdr;
    if !herdr.is_dir() {
        report.text = format!("Herdr 홈 {}이 없어 지울 것이 없다.\n", herdr.display());
        return report;
    }
    let layout_path = layout_file(herdr);
    let dir = tabs_dir(herdr);
    let layout_ours = match fs::read_to_string(&layout_path) {
        Ok(text) => Some(layout_is_ours(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => Some(false),
    };
    let tabs_ours = tabs_dir_is_ours(&dir);

    if layout_ours != Some(false) {
        switch_back(ext, &mut report);
    }
    if layout_ours == Some(true) {
        remove(&layout_path, &mut report);
    } else if layout_ours == Some(false) {
        keep(&layout_path, "chute의 것이 아니다", &mut report);
    } else {
        missing(&layout_path, &mut report);
    }
    match tabs_ours {
        Some(true) => remove_tabs_dir(&dir, &mut report),
        Some(false) => keep(&dir, "chute의 것이 아니다", &mut report),
        None => missing(&dir, &mut report),
    }
    report
}

fn switch_back(ext: &Ext, report: &mut Report) {
    let why = match shown_layout(ext) {
        Ok(Some(name)) if name == "chute" => match switch_layout(ext, "main") {
            Ok(()) => {
                report.text.push_str("main 레이아웃으로 돌아갔다.\n");
                return;
            }
            Err(why) => format!("main으로 바꾸지 못했다: {why}"),
        },
        Ok(_) => return,
        Err(why) => format!("보이는 레이아웃을 읽지 못했다: {why}"),
    };
    report.text.push_str(&format!(
        "Herdr의 레이아웃을 확인하지 못했다({why}). 파일은 지운다. 다음 시작에 chute 레이아웃이 없으면 ctrl-shift-l로 main을 고른다.\n"
    ));
}

fn remove_tabs_dir(dir: &Path, report: &mut Report) {
    let names = OURS_IN_TABS_DIR.iter().chain(HERDR_MOD_FILES.iter());
    for name in names {
        let path = dir.join(name);
        if path.exists() {
            remove(&path, report);
        }
    }
    match fs::remove_dir(dir) {
        Ok(()) => report
            .text
            .push_str(&format!("지웠다: {}\n", dir.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) if dir.is_dir() && fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()) => {
            let left: Vec<String> = fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            keep(
                dir,
                &format!("다른 파일이 남아 폴더를 두었다: {}", left.join(", ")),
                report,
            );
        }
        Err(e) => fail(dir, &e, report),
    }
}

fn remove(path: &PathBuf, report: &mut Report) {
    match fs::remove_file(path) {
        Ok(()) => report
            .text
            .push_str(&format!("지웠다: {}\n", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => missing(path, report),
        Err(e) => fail(path, &e, report),
    }
}

fn fail(path: &Path, e: &io::Error, report: &mut Report) {
    report.failed = true;
    report
        .text
        .push_str(&format!("실패했다: {} ({e})\n", path.display()));
}

fn keep(path: &Path, why: &str, report: &mut Report) {
    report
        .text
        .push_str(&format!("남겼다: {} ({why})\n", path.display()));
}

fn missing(path: &Path, report: &mut Report) {
    report.text.push_str(&format!("없다: {}\n", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::install::install_layout;
    use crate::cmd::install::replica::fake;
    use crate::testkit::{Reply, Sandbox};
    use std::collections::BTreeSet;

    struct Fixture {
        sb: Sandbox,
        layout: Layout,
    }

    impl Fixture {
        fn new() -> Fixture {
            let sb = Sandbox::new();
            let base = sb.home().join(".config/herdr-app");
            fs::create_dir_all(base.join("layouts")).unwrap();
            fs::create_dir_all(base.join("mods/tabs")).unwrap();
            fs::write(base.join("layouts/main.json"), fake::main_layout()).unwrap();
            fs::write(base.join("mods/tabs/main.js"), fake::tabs_main()).unwrap();
            let home = sb.home().to_owned();
            let layout = Layout::from_vars(|k| (k == "HOME").then(|| home.clone().into())).unwrap();
            Fixture { sb, layout }
        }

        fn base(&self) -> PathBuf {
            self.sb.home().join(".config/herdr-app")
        }

        fn installed(self) -> Fixture {
            install_layout(&self.layout);
            fs::write(self.base().join("mods/chute-tabs/herdr.d.ts"), "types").unwrap();
            self
        }

        fn ext(&self, shown: Option<&str>) -> Ext {
            let fake = self.sb.fake("herdr-app");
            fake.clear_rules();
            match shown {
                Some(name) => {
                    fake.respond("status", Reply::ok(format!(r#"{{"layout":"{name}"}}"#)));
                }
                None => {
                    fake.respond("status", Reply::ok("").code(1).stderr("no window"));
                }
            }
            fake.respond("do layout.switch *", Reply::ok("{}"));
            let path = fake.path().to_owned();
            Ext::from_vars(|k| (k == "CHUTE_HERDR_APP").then(|| path.clone().into()))
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.sb.fake("herdr-app").calls()
        }

        fn files(&self) -> BTreeSet<String> {
            let root = self.base();
            let mut found = BTreeSet::new();
            let mut pending = vec![root.clone()];
            while let Some(dir) = pending.pop() {
                for entry in fs::read_dir(&dir).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        pending.push(path);
                    } else {
                        found.insert(path.strip_prefix(&root).unwrap().display().to_string());
                    }
                }
            }
            found
        }
    }

    fn switch_main() -> Vec<String> {
        ["do", "layout.switch", r#"{"name":"main"}"#]
            .map(str::to_owned)
            .to_vec()
    }

    #[test]
    fn switches_to_main_when_chute_is_shown_then_removes_only_its_files() {
        let fx = Fixture::new().installed();
        let ext = fx.ext(Some("chute"));
        let report = uninstall_layout(&fx.layout, &ext);
        assert!(!report.failed, "{}", report.text);
        assert_eq!(fx.calls(), [vec!["status".to_owned()], switch_main()]);
        let left: Vec<String> = fx.files().into_iter().collect();
        assert_eq!(left, ["layouts/main.json", "mods/tabs/main.js"]);
        assert!(!fx.base().join("mods/chute-tabs").exists());
    }

    #[test]
    fn does_not_switch_when_main_is_shown() {
        let fx = Fixture::new().installed();
        let ext = fx.ext(Some("main"));
        uninstall_layout(&fx.layout, &ext);
        assert_eq!(fx.calls(), [vec!["status".to_owned()]]);
        assert!(!fx.base().join("layouts/chute.json").exists());
    }

    #[test]
    fn files_go_even_when_herdr_cannot_be_reached_and_the_user_is_told_what_to_do() {
        let fx = Fixture::new().installed();
        let ext = fx.ext(None);
        let report = uninstall_layout(&fx.layout, &ext);
        assert!(!report.failed);
        assert!(report.text.contains("ctrl-shift-l"), "{}", report.text);
        assert!(!fx.base().join("layouts/chute.json").exists());
        assert!(!fx.base().join("mods/chute-tabs").exists());
    }

    #[test]
    fn a_failing_switch_still_removes_the_files() {
        let fx = Fixture::new().installed();
        let ext = fx.ext(Some("chute"));
        fx.sb.fake("herdr-app").clear_rules();
        fx.sb
            .fake("herdr-app")
            .respond("status", Reply::ok(r#"{"layout":"chute"}"#));
        fx.sb
            .fake("herdr-app")
            .respond("do *", Reply::ok("").code(1).stderr("boom"));
        let report = uninstall_layout(&fx.layout, &ext);
        assert!(report.text.contains("ctrl-shift-l"), "{}", report.text);
        assert!(!fx.base().join("layouts/chute.json").exists());
    }

    #[test]
    fn a_second_run_gives_the_same_result() {
        let fx = Fixture::new().installed();
        let ext = fx.ext(Some("main"));
        uninstall_layout(&fx.layout, &ext);
        let before = fx.files();
        let again = uninstall_layout(&fx.layout, &ext);
        assert!(!again.failed);
        assert_eq!(fx.files(), before);
        assert!(again.text.contains("없다"));
    }

    #[test]
    fn a_layout_that_is_not_chutes_stays_and_is_not_switched_away_from() {
        let fx = Fixture::new().installed();
        let theirs = fx.base().join("layouts/chute.json");
        fs::write(&theirs, r#"{"id":"tabs","content":{"mod":"mine"}}"#).unwrap();
        let ext = fx.ext(Some("chute"));
        let report = uninstall_layout(&fx.layout, &ext);
        assert!(theirs.exists());
        assert!(report.text.contains("남겼다"), "{}", report.text);
        assert!(fx.calls().is_empty());
        assert!(!fx.base().join("mods/chute-tabs").exists());
    }

    #[test]
    fn a_tabs_mod_that_is_not_chutes_stays() {
        let fx = Fixture::new().installed();
        fs::write(fx.base().join("mods/chute-tabs/main.js"), "// mine\n").unwrap();
        let ext = fx.ext(Some("main"));
        uninstall_layout(&fx.layout, &ext);
        assert!(fx.base().join("mods/chute-tabs/main.js").exists());
        assert!(fx.base().join("mods/chute-tabs/chute.js").exists());
    }

    #[test]
    fn extra_files_keep_the_folder_and_are_named() {
        let fx = Fixture::new().installed();
        fs::write(fx.base().join("mods/chute-tabs/notes.txt"), "keep").unwrap();
        let ext = fx.ext(Some("main"));
        let report = uninstall_layout(&fx.layout, &ext);
        let left: Vec<String> = fx
            .files()
            .into_iter()
            .filter(|f| f.starts_with("mods/chute-tabs"))
            .collect();
        assert_eq!(left, ["mods/chute-tabs/notes.txt"]);
        assert!(report.text.contains("notes.txt"), "{}", report.text);
    }

    #[test]
    fn other_herdr_files_are_untouched() {
        let fx = Fixture::new().installed();
        for rel in [
            "keys.json",
            "seen.json",
            "layouts/.state/chute.json",
            "mods/chute/main.js",
        ] {
            let path = fx.base().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }
        let before: BTreeSet<String> = fx.files();
        let ext = fx.ext(Some("main"));
        uninstall_layout(&fx.layout, &ext);
        let removed: BTreeSet<String> = before.difference(&fx.files()).cloned().collect();
        let expect: BTreeSet<String> = [
            "layouts/chute.json",
            "mods/chute-tabs/main.js",
            "mods/chute-tabs/chute.js",
            "mods/chute-tabs/mod.json",
            "mods/chute-tabs/herdr.d.ts",
        ]
        .map(str::to_owned)
        .into();
        assert_eq!(removed, expect);
    }

    #[test]
    fn without_a_herdr_home_there_is_nothing_to_do() {
        let sb = Sandbox::new();
        let home = sb.home().to_owned();
        let layout = Layout::from_vars(|k| (k == "HOME").then(|| home.clone().into())).unwrap();
        let report = uninstall_layout(&layout, &Ext::from_vars(|_| None));
        assert!(!report.failed);
        assert!(report.text.contains("지울 것이 없다"), "{}", report.text);
        assert!(!home.join(".config/herdr-app").exists());
    }

    #[test]
    fn a_folder_whose_main_js_is_gone_is_still_cleaned_up() {
        let fx = Fixture::new().installed();
        fs::remove_file(fx.base().join("mods/chute-tabs/main.js")).unwrap();
        let ext = fx.ext(Some("main"));
        let report = uninstall_layout(&fx.layout, &ext);
        assert!(!report.failed);
        assert!(
            !fx.base().join("mods/chute-tabs").exists(),
            "{}",
            report.text
        );
        assert!(!report.text.contains("없다: ") || !report.text.contains("mods/chute-tabs"));
    }

    #[test]
    fn a_folder_without_main_js_that_holds_their_files_stays() {
        let fx = Fixture::new();
        let dir = fx.base().join("mods/chute-tabs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mod.json"), r#"{"name":"mine"}"#).unwrap();
        let ext = fx.ext(Some("main"));
        uninstall_layout(&fx.layout, &ext);
        assert!(dir.join("mod.json").exists());
    }
}
