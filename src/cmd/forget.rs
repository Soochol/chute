use std::process::ExitCode;

use anyhow::Result;

use super::{Ctx, Refusal, finish};

#[derive(clap::Args, Debug)]
pub struct Args {}

pub fn run(_args: Args, ctx: &Ctx) -> Result<ExitCode> {
    finish(forget(ctx))
}

/// Takes the repository off this PC's list and keeps its records, so `chute init` brings it back
/// with its history. The launch lock keeps `chute up` from starting a daemon in between, and the
/// state lock keeps a submit from adding to the queue while the check runs.
fn forget(ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    let _launching = store.lock_launch()?;
    if store.daemon_running()? {
        return Err(Refusal(format!(
            "데몬이 돌고 있다: {}. `chute down`으로 멈춘 뒤 다시 실행한다",
            info.slug
        ))
        .into());
    }
    store.update(|state| {
        if state.batch.is_some() {
            return Err(Refusal(format!(
                "검증 중인 batch가 남아 있다: {}. `chute up`으로 batch를 끝낸 뒤 `chute down`하고 다시 실행한다",
                info.slug
            ))
            .into());
        }
        if !state.queue.is_empty() {
            let prs: Vec<String> = state
                .queue
                .iter()
                .map(|item| format!("#{}", item.pr.0))
                .collect();
            return Err(Refusal(format!(
                "큐에 PR이 남아 있다: {}. `chute drop <pr>`로 빼거나 머지된 뒤 다시 실행한다",
                prs.join(", ")
            ))
            .into());
        }
        store.unregister()
    })?;
    println!("등록을 풀었다: {}", info.slug);
    println!(
        "기록은 {}에 남는다. 이 PC의 등록만 풀었고, 다른 PC는 그 PC에서 `chute forget`을 실행한다",
        store.dir().display()
    );
    println!(
        "훅은 이 저장소의 기본 브랜치 push를 더는 막지 않는다. 다시 쓰려면 저장소에서 `chute init`을 실행한다"
    );
    Ok(())
}
