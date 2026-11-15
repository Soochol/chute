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

use anyhow::{Context, Result};

use crate::ext::Ext;
use crate::store::Home;

pub const CANNOT_RUN: u8 = 2;

pub struct Ctx {
    pub home: Home,
    pub ext: Ext,
    repo: Option<PathBuf>,
}

impl Ctx {
    pub fn new(home: Home, ext: Ext, repo: Option<PathBuf>) -> Ctx {
        Ctx { home, ext, repo }
    }

    pub fn from_env(repo: Option<PathBuf>) -> Result<Ctx> {
        Ok(Ctx {
            home: Home::from_env()?,
            ext: Ext::from_env(),
            repo,
        })
    }

    pub fn target_dir(&self) -> Result<PathBuf> {
        match &self.repo {
            Some(dir) => Ok(dir.clone()),
            None => std::env::current_dir().context("지금 폴더를 알지 못했다"),
        }
    }
}

pub fn not_implemented(command: &str) -> Result<ExitCode> {
    eprintln!("chute {command}: 아직 구현되지 않았다");
    Ok(ExitCode::from(CANNOT_RUN))
}

/// Parses `argv` as the real command line would and runs it against an empty home.
/// Commands that touch nothing use this in their tests.
#[cfg(test)]
pub(crate) fn run_args(argv: &[&str]) -> ExitCode {
    use clap::Parser;
    let cli = crate::cli::Cli::try_parse_from(std::iter::once("chute").chain(argv.iter().copied()))
        .unwrap();
    let ctx = Ctx::new(
        Home::at("/nonexistent/chute"),
        Ext::from_vars(|_| None),
        None,
    );
    crate::cli::dispatch(cli, &ctx).unwrap()
}
