use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};

use super::Ctx;
use crate::config::Config;
use crate::feedback::{self, Delivery, Feedback, PARCEL_LIMIT, Parcel};
use crate::gc;

#[derive(clap::Args, Debug)]
pub struct Args {}

pub fn run(_args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(PARCEL_LIMIT as u64 + 1)
        .read_to_end(&mut input)
        .context("stdin을 읽지 못했다")?;
    let delivery = deliver(ctx, &input)?;
    println!("{}", serde_json::to_string(&delivery)?);
    Ok(ExitCode::SUCCESS)
}

pub fn deliver(ctx: &Ctx, input: &[u8]) -> Result<Delivery> {
    if input.len() > PARCEL_LIMIT {
        bail!("받은 묶음이 한도({PARCEL_LIMIT}바이트)를 넘는다");
    }
    let parcel = Parcel::decode(input)?;
    let (info, store) = ctx.registered_repo()?;
    fetch_remote(ctx, &info.path, &info.remote_name);
    let config = Config::load_remote_branch(
        &ctx.ext,
        &info.path,
        &info.remote_name,
        &info.default_branch,
    )?
    .ok_or_else(|| {
        anyhow!(
            "{}/{}에서 chute.toml을 읽지 못했다. 기본 브랜치에 올렸는지 확인한다",
            info.remote_name,
            info.default_branch
        )
    })?;
    let worktrees_root = feedback::worktrees_root(|key| std::env::var_os(key))?;
    let feedback = Feedback {
        ext: &ctx.ext,
        repo: &info,
        store: &store,
        config: &config,
        worktrees_root: &worktrees_root,
    };
    let delivery = feedback.receive(&parcel)?;
    gc::auto(
        &gc::Site {
            ext: &ctx.ext,
            home: &ctx.home,
            info: &info,
            store: &store,
        },
        false,
    );
    Ok(delivery)
}

fn fetch_remote(ctx: &Ctx, repo: &Path, remote: &str) {
    match ctx.ext.git(repo, ["fetch", "--quiet", remote]) {
        Ok(out) if out.success() => {}
        Ok(out) => eprintln!(
            "경고: {remote}을 가져오지 못했다: {}",
            out.stderr_text().trim()
        ),
        Err(e) => eprintln!("경고: {remote}을 가져오지 못했다: {e}"),
    }
}
