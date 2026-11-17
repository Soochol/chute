use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{Ctx, Refusal, finish, run_on_hub};
use crate::hub::{Route, route};
use crate::model::{Delivered, EpochMs, Event, EventKind, PrNumber, RemovedReason};
use crate::notify::{Line, Notifier, Removal, WaitingWhy};
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
    if let Err(e) = reason_of(&args) {
        return finish(Err(e));
    }
    if !ctx.forwarded() && route(&ctx.home, &ctx.ext, &ctx.slug()?)? != Route::Local {
        let pr = match args.pr {
            Some(pr) => pr,
            None => current_pr(ctx)?,
        }
        .0
        .to_string();
        let reason = format!("--reason={}", args.reason);
        let forwarded = ["giveup", pr.as_str(), reason.as_str()];
        if let Some(code) = run_on_hub(ctx, &forwarded)? {
            return Ok(code);
        }
    }
    finish(giveup(&args, ctx))
}

fn reason_of(args: &Args) -> Result<&str> {
    let reason = args.reason.trim();
    if reason.is_empty() {
        return Err(Refusal("--reason에 포기하는 이유를 적어야 한다".into()).into());
    }
    Ok(reason)
}

fn giveup(args: &Args, ctx: &Ctx) -> Result<()> {
    let reason = reason_of(args)?;
    let (info, store) = ctx.registered_repo()?;
    let pr = match args.pr {
        Some(pr) => pr,
        None => current_pr(ctx)?,
    };
    let gone = store.update(|state| Ok(queue::give_up(state, pr)?))?;
    let at_ms = EpochMs::now();
    let recorded = store.append_history(&Event {
        at_ms,
        kind: EventKind::Removed {
            pr,
            reason: RemovedReason::GaveUp,
            detail: Some(reason.to_owned()),
            report: None,
        },
    });
    if let Err(e) = recorded {
        eprintln!("경고: 이유를 history.jsonl에 남기지 못했다: {e:#}");
    }
    let line = Line::Removed {
        pr,
        head: gone.head.clone(),
        removal: Removal::GaveUp {
            reason: reason.to_owned(),
        },
    };
    let notifier = Notifier::new(&ctx.ext, &info, &store);
    let delivered = notifier.notify(gone.submitter.as_ref(), at_ms, &line);
    notifier.notify_waiters(pr, WaitingWhy::Removed, at_ms);
    match delivered {
        Delivered::Sent => println!("큐에서 뺐다: {pr} (포기: {reason}). submit한 터미널에 알렸다"),
        _ => println!(
            "큐에서 뺐다: {pr} (포기: {reason}). submit한 터미널에는 알리지 못했다. `chute events`에 남겼다"
        ),
    }
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
