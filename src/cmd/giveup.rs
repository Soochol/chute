use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{Ctx, Refusal, finish, run_on_hub};
use crate::hub::{Route, route};
use crate::model::{EpochMs, Event, EventKind, HumanReason, PrNumber};
use crate::notify::{Line, Notifier};
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
    let submitter = store.update(|state| {
        queue::give_up(state, pr)?;
        Ok(state
            .queue
            .iter()
            .find(|i| i.pr == pr)
            .and_then(|i| i.submitter.clone()))
    })?;
    let at_ms = EpochMs::now();
    let recorded = store.append_history(&Event {
        at_ms,
        kind: EventKind::GaveUp {
            pr,
            reason: reason.to_owned(),
        },
    });
    if let Err(e) = recorded {
        eprintln!("경고: 이유를 history.jsonl에 남기지 못했다: {e:#}");
    }
    let line = Line::Human {
        pr,
        reason: HumanReason::GaveUp,
        detail: Some(reason.to_owned()),
    };
    Notifier::new(&ctx.ext, &info, &store).notify(submitter.as_ref(), at_ms, &line);
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
