use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 지금 batch를 기다리지 않고 바로 멈춘다
    #[arg(long)]
    pub now: bool,
}

pub fn run(_args: Args, _ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("down")
}
