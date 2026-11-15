use std::fs;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use super::{Ctx, finish};
use crate::ext::Output;
use crate::model::{RepoInfo, TerminalId};
use crate::store::RepoStore;

const LAUNCH_MARKER: &str = "daemon-launched";
const LAUNCH_GRACE: Duration = Duration::from_secs(15);
const HC_NAME_LIMIT: usize = 32;
const NAME_TRIES: u32 = 5;

#[derive(clap::Args, Debug)]
pub struct Args {}

pub fn run(_args: Args, ctx: &Ctx) -> Result<ExitCode> {
    finish(up(ctx))
}

fn up(ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    match ensure_daemon(ctx, &info, &store)? {
        Launch::Running => println!("데몬이 이미 떠 있다: {}", info.slug),
        Launch::Pending => println!("데몬을 방금 띄웠다. 곧 시작한다: {}", info.slug),
        Launch::Started(Some(address)) => {
            println!("데몬을 띄웠다: {} ({})", info.slug, address.0);
        }
        Launch::Started(None) => println!("데몬을 띄웠다: {}", info.slug),
    }
    Ok(())
}

pub fn clear_launch_marker(store: &RepoStore) {
    let _ = fs::remove_file(store.dir().join(LAUNCH_MARKER));
}

#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    Running,
    Pending,
    Started(Option<TerminalId>),
}

pub fn ensure_daemon(ctx: &Ctx, info: &RepoInfo, store: &RepoStore) -> Result<Launch> {
    let _launching = store.lock_launch()?;
    if store.daemon_running()? {
        return Ok(Launch::Running);
    }
    let marker = store.dir().join(LAUNCH_MARKER);
    let recent = fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age < LAUNCH_GRACE);
    if recent {
        return Ok(Launch::Pending);
    }
    let out = launch(ctx, info)?;
    fs::write(&marker, "").with_context(|| format!("쓰지 못했다: {}", marker.display()))?;
    #[derive(Deserialize)]
    struct Launched {
        address: TerminalId,
    }
    let address = serde_json::from_slice::<Launched>(&out.stdout)
        .ok()
        .map(|l| l.address);
    Ok(Launch::Started(address))
}

/// A finished terminal keeps its name until `hc rm`, so a name that is taken is followed by
/// `-2`, `-3` and so on.
fn launch(ctx: &Ctx, info: &RepoInfo) -> Result<Output> {
    for attempt in 1..=NAME_TRIES {
        let name = daemon_name(info, attempt);
        let out = ctx.ext.hc([
            "run",
            "--bg",
            "--json",
            "--no-parent",
            "--name",
            &name,
            "--cwd",
            &info.path.to_string_lossy(),
            "--",
            "chute",
            "daemon",
        ])?;
        if out.success() {
            return Ok(out);
        }
        if !(is_name_taken(&out) && attempt < NAME_TRIES) {
            return Err(anyhow!(
                "hc run으로 데몬을 띄우지 못했다 (종료 코드 {:?}): {}",
                out.code,
                out.stderr_text().trim()
            ));
        }
    }
    unreachable!("the last attempt returns")
}

fn is_name_taken(out: &Output) -> bool {
    #[derive(Deserialize)]
    struct Failure {
        error: Kind,
    }
    #[derive(Deserialize)]
    struct Kind {
        kind: String,
    }
    serde_json::from_slice::<Failure>(&out.stderr).is_ok_and(|f| f.error.kind == "name_taken")
}

fn daemon_name(info: &RepoInfo, attempt: u32) -> String {
    let suffix = if attempt == 1 {
        String::new()
    } else {
        format!("-{attempt}")
    };
    let repo: String = info
        .slug
        .name()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let room = HC_NAME_LIMIT - suffix.len();
    let stem: String = format!("chute-{repo}").chars().take(room).collect();
    format!("{stem}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Slug;

    fn info(name: &str) -> RepoInfo {
        RepoInfo {
            slug: Slug::new("acme", name).unwrap(),
            path: "/w/repo".into(),
            remote: "git@github.com:acme/repo.git".to_owned(),
            default_branch: "main".to_owned(),
        }
    }

    #[test]
    fn the_terminal_name_fits_hc_and_has_no_odd_characters() {
        assert_eq!(daemon_name(&info("widget"), 1), "chute-widget");
        assert_eq!(daemon_name(&info("widget"), 3), "chute-widget-3");
        assert_eq!(daemon_name(&info("my.repo_x"), 1), "chute-my-repo-x");
        for attempt in 1..=NAME_TRIES {
            let long = daemon_name(&info(&"x".repeat(80)), attempt);
            assert_eq!(long.len(), HC_NAME_LIMIT);
        }
        assert!(daemon_name(&info(&"x".repeat(80)), 2).ends_with("xx-2"));
    }
}
