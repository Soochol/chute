use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, finish, run_on_hub};
use crate::model::PrNumber;
use crate::queue;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호
    pub pr: PrNumber,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let pr = args.pr.0.to_string();
    if let Some(code) = run_on_hub(ctx, &["retry", &pr])? {
        return Ok(code);
    }
    finish(retry(&args, ctx))
}

fn retry(args: &Args, ctx: &Ctx) -> Result<()> {
    let (_, store) = ctx.registered_repo()?;
    store.update(|state| Ok(queue::retry(state, args.pr)?))?;
    println!("다시 큐에 넣었다: {}. 반송 횟수를 0으로 돌렸다", args.pr);
    Ok(())
}
