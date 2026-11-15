use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};

#[derive(clap::Args, Debug)]
pub struct Args {}

pub fn run(_args: Args, _ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("batches")
}
