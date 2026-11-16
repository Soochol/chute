use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, finish, run_on_hub};
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
    let pr = args.pr.0.to_string();
    let head = args.head.as_deref().map(|head| format!("--head={head}"));
    let forwarded: Vec<&str> = ["approve", pr.as_str()]
        .into_iter()
        .chain(head.as_deref())
        .collect();
    if let Some(code) = run_on_hub(ctx, &forwarded)? {
        return Ok(code);
    }
    finish(approve(&args, ctx))
}

fn approve(args: &Args, ctx: &Ctx) -> Result<()> {
    let (_, store) = ctx.registered_repo()?;
    store.update(|state| Ok(queue::approve(state, args.pr, args.head.as_deref())?))?;
    println!("승인했다: {}. 큐에 들어갔다", args.pr);
    Ok(())
}
