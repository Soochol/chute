use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, finish};
use crate::model::PrNumber;
use crate::queue;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호
    pub pr: PrNumber,
    /// 승인하려고 본 HEAD. 지금 HEAD와 다르면 승인하지 않는다
    #[arg(long, value_name = "SHA")]
    pub head: Option<String>,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    finish(approve(&args, ctx))
}

fn approve(args: &Args, ctx: &Ctx) -> Result<()> {
    let (_, store) = ctx.registered_repo()?;
    store.update(|state| Ok(queue::approve(state, args.pr, args.head.as_deref())?))?;
    println!("승인했다: {}. 큐에 들어갔다", args.pr);
    Ok(())
}
