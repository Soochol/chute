use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, finish, run_on_hub};
use crate::model::{EpochMs, Event, EventKind, PrNumber};
use crate::notify::{Notifier, WaitingWhy};
use crate::queue;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호
    pub pr: PrNumber,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let pr = args.pr.0.to_string();
    if let Some(code) = run_on_hub(ctx, &["drop", &pr])? {
        return Ok(code);
    }
    finish(drop_pr(&args, ctx))
}

fn drop_pr(args: &Args, ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    let item = store.update(|state| Ok(queue::drop_item(state, args.pr)?))?;
    let at_ms = EpochMs::now();
    let recorded = store.append_history(&Event {
        at_ms,
        kind: EventKind::Dropped { pr: item.pr },
    });
    if let Err(e) = recorded {
        eprintln!("경고: 뺀 사실을 history.jsonl에 남기지 못했다: {e:#}");
    }
    Notifier::new(&ctx.ext, &info, &store).notify_waiters(item.pr, WaitingWhy::Dropped, at_ms);
    println!("큐에서 뺐다: {} ({})", item.pr, item.title);
    Ok(())
}
