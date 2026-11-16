use std::io::Write;
use std::process::ExitCode;

use anyhow::{Context, Result};

use super::Ctx;
use crate::config::Config;

pub const MANUAL: &str = include_str!("../../skill/manual.md");

pub const REQUIRE_HEAD_NOTICE: &str =
    "이 저장소에서는 submit하지 말고 PR과 HEAD를 보고한다 (`require_head = true`)";

pub fn run(ctx: &Ctx) -> Result<ExitCode> {
    print(&text_for(ctx))
}

pub fn run_without_state() -> Result<ExitCode> {
    print(MANUAL)
}

fn print(text: &str) -> Result<ExitCode> {
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .context("설명서를 출력하지 못했다")?;
    Ok(ExitCode::SUCCESS)
}

fn text_for(ctx: &Ctx) -> String {
    if requires_head(ctx) {
        format!("{REQUIRE_HEAD_NOTICE}\n\n{MANUAL}")
    } else {
        MANUAL.to_owned()
    }
}

fn requires_head(ctx: &Ctx) -> bool {
    try_requires_head(ctx).unwrap_or(false)
}

fn try_requires_head(ctx: &Ctx) -> Result<bool> {
    let (info, _) = ctx.registered_repo()?;
    let config = Config::load_remote_branch(
        &ctx.ext,
        &info.path,
        &info.remote_name,
        &info.default_branch,
    )?;
    Ok(config.is_some_and(|config| config.require_head))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::CommandFactory;
    use regex::Regex;

    use super::{MANUAL, REQUIRE_HEAD_NOTICE, text_for};
    use crate::cli::Cli;
    use crate::cmd::Ctx;
    use crate::ext::Ext;
    use crate::store::Home;
    use crate::testkit::{Sandbox, TestRepo};

    const AGENT_COMMANDS: [&str; 4] = ["preflight", "submit", "giveup", "status"];

    fn mentioned_subcommands() -> BTreeSet<String> {
        let re = Regex::new(r"chute ([a-z][a-z-]*)").unwrap();
        re.captures_iter(MANUAL).map(|c| c[1].to_owned()).collect()
    }

    #[test]
    fn every_command_in_the_manual_exists() {
        let known: BTreeSet<String> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_owned())
            .collect();
        let unknown: Vec<_> = mentioned_subcommands()
            .difference(&known)
            .cloned()
            .collect();
        assert!(
            unknown.is_empty(),
            "설명서에 없는 명령이 나온다: {unknown:?}"
        );
    }

    #[test]
    fn every_agent_command_is_in_the_manual() {
        let mentioned = mentioned_subcommands();
        for name in AGENT_COMMANDS {
            assert!(mentioned.contains(name), "설명서에 {name}이 없다");
        }
    }

    #[test]
    fn manual_forbids_merging_and_pushing_to_main() {
        assert!(MANUAL.contains("직접 머지하지 않는다"));
        assert!(MANUAL.contains("main에 push하지 않는다"));
    }

    #[test]
    fn manual_tells_agents_what_humans_run() {
        for name in [
            "approve", "retry", "drop", "revert", "up", "down", "install",
        ] {
            assert!(MANUAL.contains(&format!("`chute {name}")), "{name}");
        }
    }

    #[test]
    fn manual_warns_respawned_agents_about_permissions() {
        assert!(MANUAL.contains("권한 확인 없이"));
        assert!(MANUAL.contains("범위 밖을 건드리지 않는다"));
    }

    #[test]
    fn manual_says_nothing_is_respawned_by_default() {
        assert!(MANUAL.contains("기본 설정에서는 chute가 새 에이전트를 띄우지 않고"));
        assert!(!MANUAL.contains("기본 설정에서는 이 에이전트가"));
    }

    const NOTICE_KINDS: [&str; 8] = [
        "merged", "bounced", "human", "halted", "resumed", "reverted", "waiting", "flaky",
    ];

    #[test]
    fn manual_has_the_orchestrator_section_with_all_eight_notice_kinds() {
        let start = MANUAL
            .find("## 오케스트레이터 아래에서 일할 때")
            .expect("절이 없다");
        let section = &MANUAL[start..];
        for kind in NOTICE_KINDS {
            assert!(
                section.contains(&format!("| `{kind}` |")),
                "알림 종류 {kind}가 표에 없다"
            );
        }
        for needed in [
            "chute 결과 <kind> slug=<owner/name> at=<epoch ms>",
            "`--priority`",
            "`--after <PR>`",
            "`--repair`",
            "chute events",
            "chute flaky",
            "chute gc",
            "--since",
            "submit하지 않는다",
            "확정된 main",
            "blocked로 보고한다",
            "`require_head = true`",
        ] {
            assert!(section.contains(needed), "{needed}");
        }
    }

    fn registered(chute_toml: Option<&str>) -> (Sandbox, TestRepo, Ctx) {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        if let Some(text) = chute_toml {
            repo.commit_file("chute.toml", text, "chore: 설정");
            repo.git(&["push", "--quiet", "origin", "main"]);
        }
        let ext = Ext::from_vars(|k| sb.var(k));
        let home = Home::at(sb.chute_home());
        let info = crate::repo::discover(&ext, repo.path(), &[]).unwrap();
        home.repo(&info.slug).register(&info).unwrap();
        let ctx = Ctx::new(home, ext, Some(repo.path().to_path_buf()));
        (sb, repo, ctx)
    }

    const STEP: &str = "[[verify]]\nname = \"t\"\nrun = \"true\"\n";

    #[test]
    fn a_require_head_repo_gets_the_report_notice_as_the_first_line() {
        let (_sb, _repo, ctx) = registered(Some(&format!("require_head = true\n{STEP}")));
        let text = text_for(&ctx);
        assert_eq!(text.lines().next(), Some(REQUIRE_HEAD_NOTICE));
        assert!(text.ends_with(MANUAL));
    }

    #[test]
    fn other_repos_get_the_manual_alone() {
        let (_sb, _repo, ctx) = registered(Some(STEP));
        assert_eq!(text_for(&ctx), MANUAL);
        let (_sb, _repo, ctx) = registered(Some(&format!("require_head = false\n{STEP}")));
        assert_eq!(text_for(&ctx), MANUAL);
    }

    #[test]
    fn an_unreadable_config_or_an_unregistered_folder_gets_the_manual_alone() {
        let (_sb, _repo, ctx) = registered(None);
        assert_eq!(text_for(&ctx), MANUAL);
        let (_sb, _repo, ctx) = registered(Some("require_head = true\nnot toml ["));
        assert_eq!(text_for(&ctx), MANUAL);
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/other");
        let ext = Ext::from_vars(|k| sb.var(k));
        let ctx = Ctx::new(
            Home::at(sb.chute_home()),
            ext,
            Some(repo.path().to_path_buf()),
        );
        assert_eq!(text_for(&ctx), MANUAL);
    }
}
