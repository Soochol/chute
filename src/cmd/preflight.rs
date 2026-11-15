use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 결과를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
}

pub fn run(_args: Args, _ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("preflight")
}
