use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, finish};
use crate::model::PrNumber;
use crate::queue;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호
    pub pr: PrNumber,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    finish(drop_pr(&args, ctx))
}

fn drop_pr(args: &Args, ctx: &Ctx) -> Result<()> {
    let (_, store) = ctx.registered_repo()?;
    let item = store.update(|state| Ok(queue::drop_item(state, args.pr)?))?;
    println!("큐에서 뺐다: {} ({})", item.pr, item.title);
    Ok(())
}
