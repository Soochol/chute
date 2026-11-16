use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand};

use crate::cmd::{self, Ctx};
use crate::model::Slug;

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
    /// 등록된 저장소를 owner/name으로 고른다. 허브로 넘긴 호출이 쓴다
    #[arg(long, global = true, value_name = "owner/name", conflicts_with = "repo", value_parser = parse_slug)]
    pub slug: Option<Slug>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 저장소에 chute.toml 초안을 쓰고 상태 폴더에 등록한다
    Init(cmd::init::Args),
    /// 기본 브랜치 + 큐 + 지금 HEAD를 합쳐 이 PC에서 검증한다. 큐는 허브에서 읽는다
    Preflight(cmd::preflight::Args),
    /// PR을 큐에 올린다
    Submit(cmd::submit::Args),
    /// PR을 포기하고 사람 차례로 넘긴다
    Giveup(cmd::giveup::Args),
    /// chute.toml을 읽어 검증 단계와 설정 오류를 보인다. 등록하거나 상태 폴더를 쓰지 않는다
    ConfigCheck(cmd::config_check::Args),
    /// 큐, batch, 사람 차례, 오늘의 숫자를 보인다
    Status(cmd::status::Args),
    /// 승인을 기다리는 PR을 큐에 넣는다
    Approve(cmd::approve::Args),
    /// 사람 차례의 PR을 예산을 0으로 하고 큐에 다시 넣는다
    Retry(cmd::retry::Args),
    /// PR을 큐에서 뺀다
    Drop(cmd::drop::Args),
    /// 데몬을 hc 터미널로 띄운다. 허브가 다른 PC면 허브에 띄운다
    Up(cmd::up::Args),
    /// 데몬을 지금 batch 뒤(또는 즉시) 멈춘다. 허브가 다른 PC면 허브의 데몬을 멈춘다
    Down(cmd::down::Args),
    /// 데몬을 전경에서 돈다. 허브에서만 돌고 다른 PC에서는 거절한다
    Daemon(cmd::daemon::Args),
    /// 최근 batch 목록을 보인다
    Batches(cmd::batches::Args),
    /// 놓친 알림 따라잡기: history의 사건을 시간 순으로 보인다. 허브가 다른 PC면 허브의 것을 보인다
    Events(cmd::events::Args),
    /// 흔들린 시험의 장부: 시험마다 횟수, 마지막 시각, 근거, 만성 여부. 허브가 다른 PC면 허브의 것을 보인다
    Flaky(cmd::flaky::Args),
    /// 오래된 로그, 보고서, 실패 트리 참조와 남는 preflight 슬롯을 지운다. 이 PC만 다룬다
    Gc(cmd::gc::Args),
    /// batch를 되돌린다. 허브에서만 실행하고 다른 PC에서는 허브에서 실행할 명령을 알려 준다
    Revert(cmd::revert::Args),
    /// mod와 Codex 스킬 파일을 설치한다
    Install(cmd::install::Args),
    /// 이 저장소의 허브(대기열을 가진 머신)를 보거나 정한다
    Hub(cmd::hub::Args),
    /// 다른 머신의 허브가 보고서 묶음을 넘길 때 쓴다. 사람이 쓰지 않는다
    #[command(hide = true)]
    Deliver(cmd::deliver::Args),
    /// Claude Code PreToolUse 훅. 기본 브랜치로의 push와 `gh pr merge`를 막는다
    Guard(cmd::guard::Args),
}

fn parse_slug(text: &str) -> Result<Slug, String> {
    Slug::try_from(text.to_owned())
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
    if let (false, Some(Command::ConfigCheck(args))) = (cli.skill, &cli.command) {
        return cmd::config_check::run(args, cli.repo.as_deref());
    }
    let ctx = match Ctx::from_env(cli.repo.clone()) {
        Ok(ctx) => ctx,
        Err(_) if cli.skill && cli.command.is_none() => return cmd::skill::run_without_state(),
        Err(e) => return Err(e),
    };
    let ctx = ctx.with_slug(cli.slug.clone());
    dispatch(cli, &ctx)
}

pub fn dispatch(cli: Cli, ctx: &Ctx) -> Result<ExitCode> {
    if cli.skill {
        if cli.command.is_some() {
            bail!("--skill은 하위 명령과 함께 쓸 수 없다");
        }
        return cmd::skill::run(ctx);
    }
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        return Ok(ExitCode::from(cmd::CANNOT_RUN));
    };
    if let Some(code) = cmd::answer_not_hub(ctx)? {
        return Ok(code);
    }
    match command {
        Command::Init(args) => cmd::init::run(args, ctx),
        Command::Preflight(args) => cmd::preflight::run(args, ctx),
        Command::Submit(args) => cmd::submit::run(args, ctx),
        Command::Giveup(args) => cmd::giveup::run(args, ctx),
        Command::ConfigCheck(args) => cmd::config_check::run(&args, ctx.repo_arg()),
        Command::Status(args) => cmd::status::run(args, ctx),
        Command::Approve(args) => cmd::approve::run(args, ctx),
        Command::Retry(args) => cmd::retry::run(args, ctx),
        Command::Drop(args) => cmd::drop::run(args, ctx),
        Command::Up(args) => cmd::up::run(args, ctx),
        Command::Down(args) => cmd::down::run(args, ctx),
        Command::Daemon(args) => cmd::daemon::run(args, ctx),
        Command::Batches(args) => cmd::batches::run(args, ctx),
        Command::Events(args) => cmd::events::run(args, ctx),
        Command::Flaky(args) => cmd::flaky::run(args, ctx),
        Command::Gc(args) => cmd::gc::run(args, ctx),
        Command::Revert(args) => cmd::revert::run(args, ctx),
        Command::Hub(args) => cmd::hub::run(args, ctx),
        Command::Install(args) => cmd::install::run(args, ctx),
        Command::Guard(args) => cmd::guard::run(args, ctx),
        Command::Deliver(args) => cmd::deliver::run(args, ctx),
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
