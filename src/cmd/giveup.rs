use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, not_implemented};
use crate::model::PrNumber;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호. 없으면 지금 브랜치의 PR
    pub pr: Option<PrNumber>,
    /// 포기하는 이유
    #[arg(long, value_name = "글")]
    pub reason: String,
}

pub fn run(_args: Args, _ctx: &Ctx) -> Result<ExitCode> {
    not_implemented("giveup")
}
