pub mod approve;
pub mod batches;
pub mod daemon;
pub mod deliver;
pub mod down;
pub mod drop;
pub mod events;
pub mod giveup;
pub mod guard;
pub mod hub;
pub mod init;
pub mod install;
pub mod preflight;
pub mod retry;
pub mod revert;
pub mod skill;
pub mod status;
pub mod submit;
pub mod up;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use crate::ext::Ext;
use crate::hub::{Forwarded, Route, elsewhere, forward, route};
use crate::model::{RepoInfo, Slug};
use crate::queue::TransitionError;
use crate::repo;
use crate::store::{Home, RepoStore};

pub const CANNOT_RUN: u8 = 2;
pub const REFUSED: u8 = 1;
pub const NOT_HUB: u8 = 3;

pub const FORWARD_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Ctx {
    pub home: Home,
    pub ext: Ext,
    repo: Option<PathBuf>,
    terminal: Option<String>,
    slug: Option<Slug>,
    forwarded: bool,
}

impl Ctx {
    pub fn new(home: Home, ext: Ext, repo: Option<PathBuf>) -> Ctx {
        Ctx {
            home,
            ext,
            repo,
            terminal: None,
            slug: None,
            forwarded: false,
        }
    }

    pub fn with_slug(mut self, slug: Option<Slug>) -> Ctx {
        self.slug = slug;
        self
    }

    pub fn with_forwarded(mut self, forwarded: bool) -> Ctx {
        self.forwarded = forwarded;
        self
    }

    pub fn from_env(repo: Option<PathBuf>) -> Result<Ctx> {
        let terminal = std::env::var("HC_TERMINAL_ID")
            .ok()
            .filter(|id| !id.is_empty());
        Ok(Ctx {
            home: Home::from_env()?,
            ext: Ext::from_env(),
            repo,
            terminal,
            slug: None,
            forwarded: std::env::var("CHUTE_FORWARDED").is_ok_and(|v| v == "1"),
        })
    }

    pub fn forwarded(&self) -> bool {
        self.forwarded
    }

    pub fn terminal(&self) -> Option<&str> {
        self.terminal.as_deref()
    }

    pub fn target_dir(&self) -> Result<PathBuf> {
        match &self.repo {
            Some(dir) => std::path::absolute(dir)
                .with_context(|| format!("경로를 절대 경로로 바꾸지 못했다: {}", dir.display())),
            None => std::env::current_dir().context("지금 폴더를 알지 못했다"),
        }
    }

    pub fn slug(&self) -> Result<Slug> {
        match &self.slug {
            Some(slug) => Ok(slug.clone()),
            None => repo::slug(&self.ext, &self.target_dir()?),
        }
    }

    pub fn registered_repo(&self) -> Result<(RepoInfo, RepoStore)> {
        let slug = self.slug()?;
        let store = self.home.repo(&slug);
        let info = store.info()?.ok_or_else(|| {
            anyhow!("chute에 등록되지 않은 저장소다: {slug}. 저장소에서 `chute init`을 실행한다")
        })?;
        Ok((info, store))
    }
}

pub fn answer_not_hub(ctx: &Ctx) -> Result<Option<ExitCode>> {
    if !ctx.forwarded {
        return Ok(None);
    }
    let Some(hub) = elsewhere(&ctx.home, &ctx.ext, &ctx.slug()?)? else {
        return Ok(None);
    };
    println!("{}", serde_json::json!({"not_hub": {"hub": hub}}));
    Ok(Some(ExitCode::from(NOT_HUB)))
}

pub fn forward_to_hub(ctx: &Ctx, args: &[&str], timeout: Duration) -> Result<Option<Forwarded>> {
    if ctx.forwarded {
        return Ok(None);
    }
    let slug = ctx.slug()?;
    match route(&ctx.home, &ctx.ext, &slug)? {
        Route::Local => Ok(None),
        remote => forward(&ctx.home, &ctx.ext, &slug, &remote, args, timeout).map(Some),
    }
}

pub fn relay(answer: &Forwarded) -> ExitCode {
    let _ = std::io::stdout().write_all(&answer.stdout);
    let _ = std::io::stderr().write_all(&answer.stderr);
    if answer.truncated {
        eprintln!("경고: 허브의 출력이 너무 커서 뒤가 잘렸다");
    }
    ExitCode::from(u8::try_from(answer.code).unwrap_or(CANNOT_RUN))
}

pub fn run_on_hub(ctx: &Ctx, args: &[&str]) -> Result<Option<ExitCode>> {
    Ok(forward_to_hub(ctx, args, FORWARD_TIMEOUT)?.map(|answer| relay(&answer)))
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Refusal(pub String);

pub fn finish(result: Result<()>) -> Result<ExitCode> {
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e)
            if e.downcast_ref::<Refusal>().is_some()
                || e.downcast_ref::<TransitionError>().is_some() =>
        {
            eprintln!("chute: {e}");
            Ok(ExitCode::from(REFUSED))
        }
        Err(e) => Err(e),
    }
}
