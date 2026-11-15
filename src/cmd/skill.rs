use std::io::Write;
use std::process::ExitCode;

use anyhow::{Context, Result};

use super::Ctx;

pub const MANUAL: &str = include_str!("../../skill/manual.md");

pub fn run(_ctx: &Ctx) -> Result<ExitCode> {
    let mut out = std::io::stdout().lock();
    out.write_all(MANUAL.as_bytes())
        .and_then(|()| out.flush())
        .context("설명서를 출력하지 못했다")?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::CommandFactory;
    use regex::Regex;

    use super::MANUAL;
    use crate::cli::Cli;

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
}
