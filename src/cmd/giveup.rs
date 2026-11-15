use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{Ctx, Refusal, finish};
use crate::model::{EpochMs, Event, EventKind, PrNumber};
use crate::queue;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// PR 번호. 없으면 지금 브랜치의 PR
    pub pr: Option<PrNumber>,
    /// 포기하는 이유
    #[arg(long, value_name = "글")]
    pub reason: String,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    finish(giveup(&args, ctx))
}

fn giveup(args: &Args, ctx: &Ctx) -> Result<()> {
    let reason = args.reason.trim();
    if reason.is_empty() {
        return Err(Refusal("--reason에 포기하는 이유를 적어야 한다".into()).into());
    }
    let (_, store) = ctx.registered_repo()?;
    let pr = match args.pr {
        Some(pr) => pr,
        None => current_pr(ctx)?,
    };
    store.update(|state| Ok(queue::give_up(state, pr)?))?;
    let recorded = store.append_history(&Event {
        at_ms: EpochMs::now(),
        kind: EventKind::GaveUp {
            pr,
            reason: reason.to_owned(),
        },
    });
    if let Err(e) = recorded {
        eprintln!("경고: 이유를 history.jsonl에 남기지 못했다: {e:#}");
    }
    println!("사람 차례로 넘겼다: {pr} (포기: {reason})");
    Ok(())
}

fn current_pr(ctx: &Ctx) -> Result<PrNumber> {
    #[derive(Deserialize)]
    struct Found {
        number: PrNumber,
    }
    let dir = ctx.target_dir()?;
    let text = ctx
        .ext
        .gh(&dir, ["pr", "view", "--json", "number"])?
        .ok()
        .context("지금 브랜치의 PR을 찾지 못했다. PR 번호를 인자로 준다")?;
    let found: Found = serde_json::from_str(&text).context("gh pr view의 출력을 읽지 못했다")?;
    Ok(found.number)
}
