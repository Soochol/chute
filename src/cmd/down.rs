use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::daemon::{Stop, request_stop};
use super::{Ctx, finish};

const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 지금 batch를 기다리지 않고 돌던 검증을 취소해 바로 멈춘다
    #[arg(long)]
    pub now: bool,
}

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    finish(down(&args, ctx))
}

fn down(args: &Args, ctx: &Ctx) -> Result<()> {
    let (info, store) = ctx.registered_repo()?;
    if !store.daemon_running()? {
        println!("데몬이 떠 있지 않다: {}", info.slug);
        return Ok(());
    }
    if !args.now {
        request_stop(&store, Stop::AfterBatch)?;
        println!("지금 batch가 끝나면 데몬이 멈춘다: {}", info.slug);
        return Ok(());
    }
    request_stop(&store, Stop::Now)?;
    let deadline = Instant::now() + WAIT;
    while store.daemon_running()? {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "{}초 안에 데몬이 멈추지 않았다. 검증을 끝내는 중일 수 있다. `chute status`로 본다",
                WAIT.as_secs()
            );
        }
        thread::sleep(POLL);
    }
    println!("데몬을 멈췄다: {}", info.slug);
    Ok(())
}
