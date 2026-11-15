pub mod approve;
pub mod batches;
pub mod daemon;
pub mod down;
pub mod drop;
pub mod giveup;
pub mod init;
pub mod install;
pub mod preflight;
pub mod retry;
pub mod revert;
pub mod skill;
pub mod status;
pub mod submit;
pub mod up;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};

use crate::ext::Ext;
use crate::model::RepoInfo;
use crate::queue::TransitionError;
use crate::repo;
use crate::store::{Home, RepoStore};

pub const CANNOT_RUN: u8 = 2;
pub const REFUSED: u8 = 1;

pub struct Ctx {
    pub home: Home,
    pub ext: Ext,
    repo: Option<PathBuf>,
    terminal: Option<String>,
}

impl Ctx {
    pub fn new(home: Home, ext: Ext, repo: Option<PathBuf>) -> Ctx {
        Ctx {
            home,
            ext,
            repo,
            terminal: None,
        }
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
        })
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

    pub fn registered_repo(&self) -> Result<(RepoInfo, RepoStore)> {
        let slug = repo::slug(&self.ext, &self.target_dir()?)?;
        let store = self.home.repo(&slug);
        let info = store.info()?.ok_or_else(|| {
            anyhow!("chute에 등록되지 않은 저장소다: {slug}. 저장소에서 `chute init`을 실행한다")
        })?;
        Ok((info, store))
    }
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
