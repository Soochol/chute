use std::fs;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};

use super::Ctx;
use crate::config;
use crate::repo;

#[derive(clap::Args, Debug)]
pub struct Args {}

pub const AGENTS_LINE: &str = "- 이 저장소의 PR은 chute로 머지한다. PR을 올리기 전에 `chute --skill`을 실행해 나온 절차를 따르고, `gh pr merge`나 기본 브랜치로의 직접 push는 하지 않는다.";

const FALLBACK_VERIFY: &str =
    "echo 'chute.toml의 [[verify]] run을 이 저장소의 검사 명령으로 바꾼다' >&2; exit 1";

pub fn run(_args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let dir = ctx.target_dir()?;
    let info = repo::discover(&ctx.ext, &dir, &|| ctx.home.repos())?;
    let all = repo::remotes(&ctx.ext, &dir)?;
    if let Some(chosen) = all.iter().find(|r| r.name == info.remote_name) {
        let peers = repo::same_url_peers(&all, chosen);
        if peers.len() > 1 {
            let names: Vec<&str> = peers.iter().map(|r| r.name.as_str()).collect();
            println!(
                "주소가 같은 원격이 {}개({}) 있어 {}을 쓴다.",
                peers.len(),
                names.join(", "),
                chosen.name
            );
        }
    }

    let config_path = info.path.join(config::FILE_NAME);
    let on_remote = format!(
        "{}/{}:{}",
        info.remote_name,
        info.default_branch,
        config::FILE_NAME
    );
    if config_path.exists() {
        println!(
            "설정 파일이 이미 있어 그대로 둔다: {}",
            config_path.display()
        );
    } else if ctx
        .ext
        .git(&info.path, ["cat-file", "-e", &on_remote])?
        .success()
    {
        println!("기본 브랜치에 설정 파일이 이미 있어 초안을 쓰지 않는다: {on_remote}");
    } else {
        fs::write(&config_path, config::draft(guess_verify(&info.path)))
            .with_context(|| format!("쓰지 못했다: {}", config_path.display()))?;
        println!("설정 초안을 썼다: {}", config_path.display());
        println!(
            "chute는 {}/{}에 올라간 chute.toml만 읽는다. 검증 명령을 확인하고 커밋해 올린다.",
            info.remote_name, info.default_branch
        );
    }

    let store = ctx.home.repo(&info.slug);
    let previous = store.info()?;
    store.register(&info)?;
    match previous {
        None => println!(
            "등록했다: {} (상태 폴더 {})",
            info.slug,
            store.dir().display()
        ),
        Some(old) if old == info => println!(
            "이미 등록돼 있다: {} (상태 폴더 {})",
            info.slug,
            store.dir().display()
        ),
        Some(old) if old.path == info.path => println!(
            "등록한 원격을 바꿨다: {} (이전 {}, 지금 {})",
            info.slug, old.remote_name, info.remote_name
        ),
        Some(old) => println!(
            "등록 경로를 바꿨다: {} (이전 {}, 지금 {})",
            info.slug,
            old.path.display(),
            info.path.display()
        ),
    }

    println!();
    println!("대상 저장소의 AGENTS.md에 다음 한 줄을 넣는다. chute는 AGENTS.md를 고치지 않는다.");
    println!("{AGENTS_LINE}");
    Ok(ExitCode::SUCCESS)
}

fn guess_verify(repo: &Path) -> &'static str {
    [
        ("Cargo.toml", "cargo test"),
        ("package.json", "npm test"),
        ("go.mod", "go test ./..."),
    ]
    .into_iter()
    .find(|(marker, _)| repo.join(marker).exists())
    .map_or(FALLBACK_VERIFY, |(_, run)| run)
}
