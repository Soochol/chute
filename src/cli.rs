use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand};

use crate::cmd::{self, Ctx};

#[derive(Parser, Debug)]
#[command(
    name = "chute",
    version,
    about = "herdr 에이전트의 PR을 묶어 로컬에서 검증하고 기본 브랜치에 머지하는 머지 큐"
)]
pub struct Cli {
    /// 에이전트용 설명서를 출력한다
    #[arg(long)]
    pub skill: bool,
    /// 대상 저장소의 경로. 없으면 지금 폴더
    #[arg(long, global = true, value_name = "경로")]
    pub repo: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 저장소에 chute.toml 초안을 쓰고 상태 폴더에 등록한다
    Init(cmd::init::Args),
    /// 기본 브랜치 + 큐 + 지금 HEAD를 합쳐 검증한다
    Preflight(cmd::preflight::Args),
    /// PR을 큐에 올린다
    Submit(cmd::submit::Args),
    /// PR을 포기하고 사람 차례로 넘긴다
    Giveup(cmd::giveup::Args),
    /// 큐, batch, 사람 차례, 오늘의 숫자를 보인다
    Status(cmd::status::Args),
    /// 승인을 기다리는 PR을 큐에 넣는다
    Approve(cmd::approve::Args),
    /// 사람 차례의 PR을 예산을 0으로 하고 큐에 다시 넣는다
    Retry(cmd::retry::Args),
    /// PR을 큐에서 뺀다
    Drop(cmd::drop::Args),
    /// 데몬을 hc 터미널로 띄운다
    Up(cmd::up::Args),
    /// 데몬을 지금 batch 뒤(또는 즉시) 멈춘다
    Down(cmd::down::Args),
    /// 데몬을 전경에서 돈다
    Daemon(cmd::daemon::Args),
    /// 최근 batch 목록을 보인다
    Batches(cmd::batches::Args),
    /// batch를 되돌린다
    Revert(cmd::revert::Args),
    /// mod와 스킬 파일을 설치한다
    Install(cmd::install::Args),
}

pub fn main() -> ExitCode {
    restore_sigpipe();
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("chute: {e:#}");
            ExitCode::from(cmd::CANNOT_RUN)
        }
    }
}

/// Rust ignores SIGPIPE, which turns `chute status | head` into a panic. A command-line tool
/// should end quietly instead.
fn restore_sigpipe() {
    use nix::sys::signal::{SigHandler, Signal, signal};
    // SAFETY: runs first in `main`, before any other thread exists.
    let _ = unsafe { signal(Signal::SIGPIPE, SigHandler::SigDfl) };
}

fn run(cli: Cli) -> Result<ExitCode> {
    let ctx = Ctx::from_env(cli.repo)?;
    if cli.skill {
        if cli.command.is_some() {
            bail!("--skill은 하위 명령과 함께 쓸 수 없다");
        }
        return cmd::skill::run(&ctx);
    }
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        return Ok(ExitCode::from(cmd::CANNOT_RUN));
    };
    match command {
        Command::Init(args) => cmd::init::run(args, &ctx),
        Command::Preflight(args) => cmd::preflight::run(args, &ctx),
        Command::Submit(args) => cmd::submit::run(args, &ctx),
        Command::Giveup(args) => cmd::giveup::run(args, &ctx),
        Command::Status(args) => cmd::status::run(args, &ctx),
        Command::Approve(args) => cmd::approve::run(args, &ctx),
        Command::Retry(args) => cmd::retry::run(args, &ctx),
        Command::Drop(args) => cmd::drop::run(args, &ctx),
        Command::Up(args) => cmd::up::run(args, &ctx),
        Command::Down(args) => cmd::down::run(args, &ctx),
        Command::Daemon(args) => cmd::daemon::run(args, &ctx),
        Command::Batches(args) => cmd::batches::run(args, &ctx),
        Command::Revert(args) => cmd::revert::run(args, &ctx),
        Command::Install(args) => cmd::install::run(args, &ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_definitions_are_valid() {
        Cli::command().debug_assert();
    }
}
