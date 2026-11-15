use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};
use crate::model::PrNumber;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호
    pub pr: PrNumber,
}

pub fn run(_args: Args, _ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("drop")
}
