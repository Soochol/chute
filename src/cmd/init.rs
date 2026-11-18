use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};

use super::Ctx;
use crate::config;
use crate::model::RepoInfo;
use crate::repo;

mod instructions;
mod setup;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 등록한 뒤 검증 명령으로 설정을 검사하고 이 PC에서 검증한다
    #[arg(long, hide = true)]
    pub setup: bool,
    /// --setup이 검증할 명령. 여러 번 줄 수 있다
    #[arg(long, hide = true, requires = "setup", value_name = "명령")]
    pub verify: Vec<String>,
    /// --setup이 한 줄을 더할 지침 파일. 저장소 안의 상대 경로
    #[arg(long, hide = true, requires = "setup", value_name = "파일")]
    pub instructions: Option<String>,
}

pub const AGENTS_LINE: &str = "- 이 저장소의 PR은 chute로 머지한다. PR을 올리기 전에 `chute --skill`을 실행해 나온 절차를 따르고, `gh pr merge`나 기본 브랜치로의 직접 push는 하지 않는다.";

pub(crate) const FALLBACK_VERIFY: &str =
    "echo 'chute.toml의 [[verify]] run을 이 저장소의 검사 명령으로 바꾼다' >&2; exit 1";

pub fn run(args: Args, ctx: &Ctx) -> Result<ExitCode> {
    if args.setup {
        return setup::run(&args, ctx, &mut io::stdout(), &mut io::stderr());
    }
    let out = &mut io::stdout().lock();
    let dir = ctx.target_dir()?;
    let info = repo::discover(&ctx.ext, &dir, &|| ctx.home.repos())?;
    announce_same_url_remotes(ctx, &dir, &info, out)?;

    let config_path = info.path.join(config::FILE_NAME);
    let on_remote = format!(
        "{}/{}:{}",
        info.remote_name,
        info.default_branch,
        config::FILE_NAME
    );
    if config_path.exists() {
        writeln!(
            out,
            "설정 파일이 이미 있어 그대로 둔다: {}",
            config_path.display()
        )?;
    } else if ctx
        .ext
        .git(&info.path, ["cat-file", "-e", &on_remote])?
        .success()
    {
        writeln!(
            out,
            "기본 브랜치에 설정 파일이 이미 있어 초안을 쓰지 않는다: {on_remote}"
        )?;
    } else {
        fs::write(&config_path, config::draft(guess_verify(&info.path)))
            .with_context(|| format!("쓰지 못했다: {}", config_path.display()))?;
        writeln!(out, "설정 초안을 썼다: {}", config_path.display())?;
        writeln!(
            out,
            "chute는 {}/{}에 올라간 chute.toml만 읽는다. 검증 명령을 확인하고 커밋해 올린다.",
            info.remote_name, info.default_branch
        )?;
    }

    register(ctx, &info, out)?;

    writeln!(out)?;
    writeln!(
        out,
        "대상 저장소의 AGENTS.md에 다음 한 줄을 넣는다. chute는 AGENTS.md를 고치지 않는다."
    )?;
    writeln!(out, "{AGENTS_LINE}")?;
    Ok(ExitCode::SUCCESS)
}

fn announce_same_url_remotes(
    ctx: &Ctx,
    dir: &Path,
    info: &RepoInfo,
    out: &mut dyn Write,
) -> Result<()> {
    let all = repo::remotes(&ctx.ext, dir)?;
    if let Some(chosen) = all.iter().find(|r| r.name == info.remote_name) {
        let peers = repo::same_url_peers(&all, chosen);
        if peers.len() > 1 {
            let names: Vec<&str> = peers.iter().map(|r| r.name.as_str()).collect();
            writeln!(
                out,
                "주소가 같은 원격이 {}개({}) 있다. 쓰는 원격: {}",
                peers.len(),
                names.join(", "),
                chosen.name
            )?;
        }
    }
    Ok(())
}

fn register(ctx: &Ctx, info: &RepoInfo, out: &mut dyn Write) -> Result<()> {
    let store = ctx.home.repo(&info.slug);
    let previous = store.info()?;
    store.register(info)?;
    match previous {
        None => writeln!(
            out,
            "등록했다: {} (상태 폴더 {})",
            info.slug,
            store.dir().display()
        )?,
        Some(old) if old == *info => writeln!(
            out,
            "이미 등록돼 있다: {} (상태 폴더 {})",
            info.slug,
            store.dir().display()
        )?,
        Some(old) if old.path == info.path => writeln!(
            out,
            "등록한 원격을 바꿨다: {} (이전 {}, 지금 {})",
            info.slug, old.remote_name, info.remote_name
        )?,
        Some(old) => writeln!(
            out,
            "등록 경로를 바꿨다: {} (이전 {}, 지금 {})",
            info.slug,
            old.path.display(),
            info.path.display()
        )?,
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    use crate::cli::Cli;

    #[test]
    fn verify_and_instructions_are_refused_without_setup() {
        for args in [
            ["chute", "init", "--verify", "true"],
            ["chute", "init", "--instructions", "AGENTS.md"],
        ] {
            let err = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument, "{args:?}");
        }
    }

    #[test]
    fn setup_takes_verify_many_times_and_an_instructions_file() {
        let cli = Cli::try_parse_from([
            "chute",
            "init",
            "--setup",
            "--verify",
            "pio run",
            "--verify",
            "python3 t.py",
            "--instructions",
            "CLAUDE.md",
        ])
        .unwrap();
        let Some(crate::cli::Command::Init(args)) = cli.command else {
            panic!("not init");
        };
        assert!(args.setup);
        assert_eq!(args.verify, ["pio run", "python3 t.py"]);
        assert_eq!(args.instructions.as_deref(), Some("CLAUDE.md"));
    }

    #[test]
    fn plain_init_takes_no_options() {
        let cli = Cli::try_parse_from(["chute", "init"]).unwrap();
        let Some(crate::cli::Command::Init(args)) = cli.command else {
            panic!("not init");
        };
        assert!(!args.setup && args.verify.is_empty() && args.instructions.is_none());
    }

    #[test]
    fn init_help_does_not_show_the_setup_options() {
        let mut command = Cli::command();
        let init = command.find_subcommand_mut("init").unwrap();
        let help = init.render_long_help().to_string();
        for hidden in ["--setup", "--verify", "--instructions"] {
            assert!(!help.contains(hidden), "{hidden}: {help}");
        }
    }
}
