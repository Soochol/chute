use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use super::{
    Args, FALLBACK_VERIFY, announce_same_url_remotes, guess_verify, instructions, register,
};
use crate::cmd::{Ctx, REFUSED, Refusal, config_check, preflight};
use crate::config::{self, Config};
use crate::gitops::Gitops;
use crate::model::{EpochMs, RepoInfo, Sha};
use crate::pc_limit::Wait;
use crate::repo;
use crate::reuse;
use crate::store::{RepoStore, SetupRecord};
use crate::verify::{self, Cause, Detail, Failed, Options, Outcome};

const COMMIT_MESSAGE: &str = "chute: 머지 큐 설정 추가";
const LOG_TAIL_LINES: usize = 20;
const HOUR_MS: u64 = 60 * 60 * 1000;

#[derive(Debug)]
pub(super) struct Ready {
    pub info: RepoInfo,
    pub base: Sha,
    pub commit: Sha,
    pub instructions: instructions::Chosen,
}

pub(super) fn run(
    args: &Args,
    ctx: &Ctx,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode> {
    run_at(args, ctx, out, err, EpochMs::now())
}

fn run_at(
    args: &Args,
    ctx: &Ctx,
    out: &mut dyn Write,
    err: &mut dyn Write,
    now: EpochMs,
) -> Result<ExitCode> {
    match prepare(args, ctx, out, now) {
        Ok(None) => Ok(ExitCode::SUCCESS),
        Ok(Some(ready)) => {
            show(ctx, &ready, out)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => match e.downcast::<Refusal>() {
            Ok(refusal) => {
                writeln!(err, "chute: {refusal}")?;
                Ok(ExitCode::from(REFUSED))
            }
            Err(e) => Err(e),
        },
    }
}

fn show(ctx: &Ctx, ready: &Ready, out: &mut dyn Write) -> Result<()> {
    writeln!(out)?;
    writeln!(out, "지침 파일: {}", ready.instructions.file)?;
    if ready.instructions.content.is_none() {
        writeln!(out, "지침 파일에 같은 줄이 이미 있어 바꾸지 않는다")?;
    }
    writeln!(out)?;
    write!(
        out,
        "{}",
        setup_diff(ctx, &ready.info.path, &ready.base, &ready.commit)?
    )?;
    Ok(())
}

pub(super) fn setup_diff(ctx: &Ctx, repo: &Path, base: &Sha, commit: &Sha) -> Result<String> {
    let diff = ctx.ext.git(
        repo,
        [
            "diff",
            "--no-ext-diff",
            "--no-color",
            base.as_str(),
            commit.as_str(),
        ],
    )?;
    Ok(diff.ok()?)
}

pub(super) fn prepare(
    args: &Args,
    ctx: &Ctx,
    out: &mut dyn Write,
    now: EpochMs,
) -> Result<Option<Ready>> {
    let dir = ctx.target_dir()?;
    let info = repo::discover(&ctx.ext, &dir, &|| ctx.home.repos())?;
    announce_same_url_remotes(ctx, &dir, &info, out)?;
    register(ctx, &info, out)?;
    let gitops = Gitops::new(&ctx.ext, &ctx.home, &info);
    let base = gitops.fetch_base_tip()?;
    if base_has_config(ctx, &info, &base)? {
        writeln!(
            out,
            "기본 브랜치에 chute.toml이 이미 있어 등록만 했다: {}/{}",
            info.remote_name, info.default_branch
        )?;
        return Ok(None);
    }

    let config_path = info.path.join(config::FILE_NAME);
    prepare_draft(args, &info, &config_path, out)?;
    let config = check_config(&config_path, &args.verify)?;
    writeln!(
        out,
        "설정을 검사했다: {} (검증 단계 {}개)",
        config_path.display(),
        config.verify.len()
    )?;

    let chosen = instructions::choose(&ctx.ext, &info.path, &base, args.instructions.as_deref())?;
    let gitops = gitops.with_cache_seed(config.cache.seed.clone());
    let slot = gitops.lock_preflight_slot(config.max_parallel)?.open()?;
    let draft = fs::read(&config_path)
        .with_context(|| format!("읽지 못했다: {}", config_path.display()))?;
    let mut files = vec![(config::FILE_NAME, draft.as_slice())];
    if let Some(content) = &chosen.content {
        files.push((chosen.file.as_str(), content.as_slice()));
    }
    let commit = slot.commit_files(&base, &files, COMMIT_MESSAGE)?;

    let store = ctx.home.repo(&info.slug);
    let tree = slot.tree_of(&commit)?;
    let fingerprint = reuse::fingerprint(&ctx.ext, slot.path(), &config)?;
    match reusable_record(&store, &config, &base, &tree, &fingerprint, now) {
        Some(at) => writeln!(
            out,
            "같은 기본 브랜치와 같은 트리를 이미 검증해 통과 기록을 다시 쓴다(기록 시각 {} ms)",
            at.0
        )?,
        None => {
            let log_dir = preflight::log_dir(&store, &commit, now)?;
            let on_step = |name: &str| eprintln!("검증 단계: {name}");
            let on_machine_wait = |wait: Wait| {
                if let Wait::Begin { running } = wait {
                    eprintln!("이 PC에서 검증 {running}개가 돌고 있어 기다린다");
                }
            };
            let options = Options {
                cache_dir: Some(gitops.cache_dir()),
                on_step: Some(&on_step),
                on_machine_wait: Some(&on_machine_wait),
                changed: None,
                ..Options::default()
            };
            let outcome = verify::run(&config, &store, slot.path(), &log_dir, &options)
                .with_context(|| format!("검증을 돌리지 못했다 (저장소 {})", info.slug))?;
            drop(options);
            match outcome {
                Outcome::Passed { flaky } if flaky.is_empty() => {
                    store.write_setup_record(&SetupRecord {
                        base: base.clone(),
                        tree,
                        config: fingerprint,
                        at_ms: now,
                    })?;
                    writeln!(out, "검증을 통과했다")?;
                }
                Outcome::Passed { flaky } => {
                    let names: Vec<&str> = flaky.iter().map(|f| f.step.as_str()).collect();
                    writeln!(
                        out,
                        "검증을 통과했지만 한 번 실패했다가 다시 돈 단계가 있어 통과 기록을 남기지 않는다: {}",
                        names.join(", ")
                    )?;
                }
                Outcome::Failed(failed) => return Err(Refusal(failure_text(&failed)).into()),
                Outcome::Cancelled => bail!("검증이 취소됐다"),
            }
        }
    }
    Ok(Some(Ready {
        info,
        base,
        commit,
        instructions: chosen,
    }))
}

fn base_has_config(ctx: &Ctx, info: &RepoInfo, base: &Sha) -> Result<bool> {
    let spec = format!("{base}:{}", config::FILE_NAME);
    Ok(ctx
        .ext
        .git(&info.path, ["cat-file", "-e", &spec])?
        .success())
}

fn prepare_draft(args: &Args, info: &RepoInfo, path: &Path, out: &mut dyn Write) -> Result<()> {
    if args.verify.iter().any(|run| run.trim().is_empty()) {
        return Err(Refusal("--verify에 빈 명령을 줄 수 없다".into()).into());
    }
    if path.exists() {
        return Ok(());
    }
    let (draft, stop) = if args.verify.is_empty() {
        (config::draft(guess_verify(&info.path)), true)
    } else {
        (config::draft_steps(&args.verify), false)
    };
    fs::write(path, draft).with_context(|| format!("쓰지 못했다: {}", path.display()))?;
    writeln!(out, "설정 초안을 썼다: {}", path.display())?;
    if stop {
        return Err(Refusal(format!(
            "--verify를 주거나 초안의 [[verify]]를 고친 뒤 다시 실행한다: {}",
            path.display()
        ))
        .into());
    }
    Ok(())
}

fn check_config(path: &Path, verify: &[String]) -> Result<Config> {
    let config = config_check::read(path).map_err(|e| Refusal(format!("{e:#}")))?;
    if !verify.is_empty() {
        let wanted = verify.iter().map(|run| run.trim());
        if !config.verify.iter().map(|s| s.run.trim()).eq(wanted) {
            return Err(Refusal(format!(
                "초안의 [[verify]]가 --verify와 다르다. 초안을 고치거나 지운 뒤 다시 실행한다: {}",
                path.display()
            ))
            .into());
        }
    }
    let unfinished: Vec<&str> = config
        .verify
        .iter()
        .filter(|step| step.run.trim() == FALLBACK_VERIFY)
        .map(|step| step.name.as_str())
        .collect();
    if !unfinished.is_empty() {
        return Err(Refusal(format!(
            "[[verify]] 단계 {}의 run이 초안의 안내 명령 그대로다. 이 저장소의 검사 명령으로 바꾼 뒤 다시 실행한다: {}",
            unfinished.join(", "),
            path.display()
        ))
        .into());
    }
    Ok(config)
}

fn reusable_record(
    store: &RepoStore,
    config: &Config,
    base: &Sha,
    tree: &Sha,
    fingerprint: &str,
    now: EpochMs,
) -> Option<EpochMs> {
    if !config.reuse.enabled {
        return None;
    }
    let record = match store.setup_record() {
        Ok(record) => record?,
        Err(e) => {
            eprintln!("경고: 이전 통과 기록을 읽지 못해 다시 검증한다: {e:#}");
            return None;
        }
    };
    let window = u64::from(config.reuse.hours) * HOUR_MS;
    let fresh = record.at_ms <= now && now.0 - record.at_ms.0 <= window;
    (fresh && record.base == *base && record.tree == *tree && record.config == fingerprint)
        .then_some(record.at_ms)
}

fn failure_text(failed: &Failed) -> String {
    let cause = match failed.cause {
        Cause::Exited(code) => format!("종료 코드 {code}"),
        Cause::Signaled(signal) => format!("신호 {signal}"),
        Cause::TimedOut => "시간 제한을 넘겼다".to_owned(),
    };
    let mut text = format!(
        "검증이 실패했다: 단계 {} ({cause})\n로그: {}",
        failed.step,
        failed.log.display()
    );
    if let Detail::Tests(tests) = &failed.detail {
        text.push_str(&format!("\n실패한 시험: {}", tests.join(", ")));
    }
    if let Some(tail) = log_tail(&failed.log) {
        text.push_str(&format!("\n로그 끝:\n{tail}"));
    }
    text
}

fn log_tail(log: &Path) -> Option<String> {
    let text = String::from_utf8_lossy(&fs::read(log).ok()?).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(LOG_TAIL_LINES);
    Some(lines[from..].join("\n"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cmd::init::AGENTS_LINE;
    use crate::ext::Ext;
    use crate::model::Slug;
    use crate::store::Home;
    use crate::testkit::{Sandbox, TestRepo};

    const NOW: EpochMs = EpochMs(1_800_000_000_000);
    const HOUR: u64 = HOUR_MS;

    struct World {
        sb: Sandbox,
        repo: TestRepo,
        ctx: Ctx,
        slug: Slug,
        counter: PathBuf,
    }

    struct Ran {
        code: String,
        text: String,
    }

    fn code(code: ExitCode) -> String {
        format!("{code:?}")
    }

    impl World {
        fn new() -> World {
            let sb = Sandbox::new();
            let repo = TestRepo::new(&sb, "acme/widget");
            let ctx = Ctx::new(
                Home::at(sb.chute_home()),
                Ext::from_vars(|k| sb.var(k)),
                Some(repo.path().to_path_buf()),
            );
            let counter = sb.root().join("runs");
            World {
                sb,
                repo,
                ctx,
                slug: Slug::new("acme", "widget").unwrap(),
                counter,
            }
        }

        fn counting(&self, name: &str) -> String {
            let body = format!("echo {name} >> '{}'\n", self.counter.display());
            self.sb.script(name, &body).to_string_lossy().into_owned()
        }

        fn failing(&self, name: &str) -> String {
            let body = format!(
                "echo {name} >> '{}'\necho boom-from-{name}\nexit 1\n",
                self.counter.display()
            );
            self.sb.script(name, &body).to_string_lossy().into_owned()
        }

        fn runs(&self) -> usize {
            std::fs::read_to_string(&self.counter).map_or(0, |text| text.lines().count())
        }

        fn args(verify: &[&str], instructions: Option<&str>) -> Args {
            Args {
                setup: true,
                verify: verify.iter().map(|run| (*run).to_owned()).collect(),
                instructions: instructions.map(str::to_owned),
            }
        }

        fn exec_at(&self, args: &Args, now: EpochMs) -> Ran {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_at(args, &self.ctx, &mut out, &mut err, now).unwrap();
            let mut text = String::from_utf8(out).unwrap();
            text.push_str(&String::from_utf8(err).unwrap());
            Ran {
                code: self::code(code),
                text,
            }
        }

        fn exec(&self, verify: &[&str]) -> Ran {
            self.exec_at(&World::args(verify, None), NOW)
        }

        fn prepare(&self, verify: &[&str], instructions: Option<&str>) -> Ready {
            let args = World::args(verify, instructions);
            prepare(&self.ctx, &args)
                .unwrap()
                .expect("the default branch has no chute.toml")
        }

        fn work_config(&self) -> PathBuf {
            self.repo.path().join("chute.toml")
        }

        fn record_file(&self) -> PathBuf {
            self.ctx.home.repo(&self.slug).dir().join("setup.json")
        }

        fn push_main(&self, path: &str, content: &str) {
            self.repo.commit_file(path, content, "chore: 시험용 커밋");
            self.repo.git(&["push", "--quiet", "origin", "main"]);
        }

        fn show(&self, commit: &Sha, path: &str) -> String {
            self.repo.git(&["show", &format!("{commit}:{path}")])
        }

        fn changed_files(&self, ready: &Ready) -> String {
            self.repo
                .git(&[
                    "diff",
                    "--name-only",
                    ready.base.as_str(),
                    ready.commit.as_str(),
                ])
                .trim()
                .to_owned()
        }
    }

    fn prepare(ctx: &Ctx, args: &Args) -> Result<Option<Ready>> {
        super::prepare(args, ctx, &mut Vec::new(), NOW)
    }

    fn step(name: &str, run: &str) -> String {
        format!("[[verify]]\nname = \"{name}\"\nrun = \"{run}\"\n")
    }

    #[test]
    fn two_verify_options_become_test_1_and_test_2_in_the_order_given() {
        let w = World::new();
        let (first, second) = (w.counting("first"), w.counting("second"));
        let ran = w.exec(&[&first, &second]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);

        let written = std::fs::read_to_string(w.work_config()).unwrap();
        let config = Config::parse(&written).unwrap();
        let steps: Vec<(&str, &str)> = config
            .verify
            .iter()
            .map(|s| (s.name.as_str(), s.run.as_str()))
            .collect();
        assert_eq!(
            steps,
            [("test-1", first.as_str()), ("test-2", second.as_str())]
        );
        assert_eq!(
            std::fs::read_to_string(&w.counter).unwrap(),
            "first\nsecond\n"
        );
    }

    #[test]
    fn one_verify_option_is_named_test() {
        let w = World::new();
        let only = w.counting("only");
        let ran = w.exec(&[&only]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);

        let config = Config::parse(&std::fs::read_to_string(w.work_config()).unwrap()).unwrap();
        assert_eq!(config.verify.len(), 1);
        assert_eq!(config.verify[0].name, "test");
        assert_eq!(config.verify[0].run, only);
    }

    #[test]
    fn without_verify_and_without_a_config_a_draft_is_written_and_the_run_stops() {
        let w = World::new();
        let ran = w.exec(&[]);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(ran.text.contains("--verify를 주거나"), "{}", ran.text);
        assert!(ran.text.contains("[[verify]]를 고친 뒤 다시 실행한다"));
        assert_eq!(
            std::fs::read_to_string(w.work_config()).unwrap(),
            config::draft(FALLBACK_VERIFY)
        );
        assert!(!w.ctx.home.slots_dir(&w.slug).exists());
        assert_eq!(w.runs(), 0);
    }

    #[test]
    fn without_verify_a_config_that_was_there_before_the_run_goes_on_to_the_verification() {
        let w = World::new();
        let mine = w.counting("mine");
        w.repo.write("chute.toml", &step("mine", &mine));
        let ran = w.exec(&[]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert_eq!(w.runs(), 1);
        assert!(w.record_file().exists());
    }

    #[test]
    fn a_draft_that_still_has_the_fallback_command_stops_at_the_check_and_names_the_step() {
        let w = World::new();
        let counted = w.counting("counted");
        let text = format!(
            "{}{}",
            step("build-step", "placeholder"),
            step("counted", &counted)
        )
        .replace("placeholder", FALLBACK_VERIFY);
        w.repo.write("chute.toml", &text);
        let ran = w.exec(&[]);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(ran.text.contains("build-step"), "{}", ran.text);
        assert!(ran.text.contains("검사 명령으로 바꾼"), "{}", ran.text);
        assert_eq!(w.runs(), 0);
        assert!(!w.record_file().exists());
    }

    #[test]
    fn the_unedited_draft_the_run_itself_wrote_stops_at_the_fallback_check_on_the_next_run() {
        let w = World::new();
        w.exec(&[]);
        let ran = w.exec(&[]);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(ran.text.contains("test"), "{}", ran.text);
        assert!(ran.text.contains("안내 명령"), "{}", ran.text);
        assert!(!w.ctx.home.slots_dir(&w.slug).exists());
    }

    #[test]
    fn a_config_error_stops_with_the_config_check_message_and_runs_nothing() {
        let w = World::new();
        let counted = w.counting("counted");
        w.repo.write(
            "chute.toml",
            &format!("{}paths = []\n", step("e2e", &counted)),
        );
        let ran = w.exec(&[]);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(ran.text.contains("e2e"), "{}", ran.text);
        assert!(ran.text.contains("paths가 비어 있다"), "{}", ran.text);
        assert_eq!(w.runs(), 0);
        assert!(!w.ctx.home.slots_dir(&w.slug).exists());
    }

    #[test]
    fn a_verify_list_that_differs_from_the_config_stops_and_leaves_the_file_byte_for_byte() {
        let w = World::new();
        let (mine, other) = (w.counting("mine"), w.counting("other"));
        let text = format!("# edited by hand\n{}", step("mine", &mine));
        w.repo.write("chute.toml", &text);
        let ran = w.exec(&[&other]);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(ran.text.contains("--verify와 다르다"), "{}", ran.text);
        assert_eq!(std::fs::read(w.work_config()).unwrap(), text.as_bytes());
        assert_eq!(w.runs(), 0);

        let ran = w.exec(&[&mine, &other]);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert_eq!(std::fs::read(w.work_config()).unwrap(), text.as_bytes());
    }

    #[test]
    fn a_verify_list_equal_to_the_config_goes_on() {
        let w = World::new();
        let mine = w.counting("mine");
        let text = format!("# edited by hand\n{}", step("mine", &mine));
        w.repo.write("chute.toml", &text);
        let ran = w.exec(&[&mine]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert_eq!(std::fs::read(w.work_config()).unwrap(), text.as_bytes());
        assert_eq!(w.runs(), 1);
    }

    #[test]
    fn a_failing_verification_stops_with_the_step_and_the_log_and_leaves_no_record() {
        let w = World::new();
        let failing = w.failing("failing");
        let ran = w.exec(&[&failing]);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(ran.text.contains("단계 test"), "{}", ran.text);
        assert!(ran.text.contains("종료 코드 1"), "{}", ran.text);
        assert!(ran.text.contains("boom-from-failing"), "{}", ran.text);
        let logs = w.ctx.home.repo(&w.slug).dir().join("preflight");
        assert!(
            ran.text.contains(&logs.display().to_string()),
            "{}",
            ran.text
        );
        assert!(ran.text.contains(".log"), "{}", ran.text);
        assert!(!w.record_file().exists());
        assert!(!ran.text.contains("지침 파일"), "{}", ran.text);
    }

    #[test]
    fn a_passing_verification_is_recorded_and_the_same_command_does_not_run_it_again() {
        let w = World::new();
        let counted = w.counting("counted");
        let first = w.exec(&[&counted]);
        assert_eq!(first.code, code(ExitCode::SUCCESS), "{}", first.text);
        assert_eq!(w.runs(), 1);

        let ready = w.prepare(&[&counted], None);
        let record: SetupRecord =
            serde_json::from_slice(&std::fs::read(w.record_file()).unwrap()).unwrap();
        assert_eq!(record.base, ready.base);
        assert_eq!(
            record.tree.as_str(),
            w.repo
                .git(&["rev-parse", &format!("{}^{{tree}}", ready.commit)])
                .trim()
        );
        assert_eq!(record.at_ms, NOW);
        assert!(!record.config.is_empty());

        let again = w.exec(&[&counted]);
        assert_eq!(again.code, code(ExitCode::SUCCESS), "{}", again.text);
        assert_eq!(w.runs(), 1);
        assert!(
            again.text.contains("통과 기록을 다시 쓴다"),
            "{}",
            again.text
        );
        assert!(
            again.text.contains("지침 파일: AGENTS.md"),
            "{}",
            again.text
        );
    }

    #[test]
    fn a_new_commit_on_the_default_branch_makes_the_rerun_verify_again() {
        let w = World::new();
        let counted = w.counting("counted");
        w.exec(&[&counted]);
        assert_eq!(w.runs(), 1);

        w.push_main("src.txt", "moved\n");
        let ran = w.exec(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert_eq!(w.runs(), 2);
    }

    #[test]
    fn a_record_older_than_reuse_hours_is_not_reused() {
        let w = World::new();
        let counted = w.counting("counted");
        w.exec(&[&counted]);
        assert_eq!(w.runs(), 1);

        let inside = EpochMs(NOW.0 + 23 * HOUR);
        w.exec_at(&World::args(&[&counted], None), inside);
        assert_eq!(w.runs(), 1);
        let outside = EpochMs(NOW.0 + 25 * HOUR);
        let ran = w.exec_at(&World::args(&[&counted], None), outside);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert_eq!(w.runs(), 2);
    }

    #[test]
    fn a_config_that_turns_reuse_off_verifies_on_every_run() {
        let w = World::new();
        let counted = w.counting("counted");
        w.repo.write(
            "chute.toml",
            &format!("reuse_verified = false\n{}", step("counted", &counted)),
        );
        w.exec(&[]);
        w.exec(&[]);
        assert_eq!(w.runs(), 2);
    }

    #[test]
    fn a_step_whose_paths_match_no_changed_file_still_runs_in_setup() {
        let w = World::new();
        let counted = w.counting("counted");
        w.repo.write(
            "chute.toml",
            &format!(
                "{}paths = [\"nowhere/\"]\n",
                step("only-for-nowhere", &counted)
            ),
        );
        let ran = w.exec(&[]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert_eq!(w.runs(), 1);
    }

    #[test]
    fn a_default_branch_that_already_has_a_config_is_only_registered() {
        let w = World::new();
        let counted = w.counting("counted");
        w.repo
            .commit_file("chute.toml", &step("t", &counted), "chore: chute 설정");
        w.repo.git(&["push", "--quiet", "origin", "main"]);
        w.repo.git(&["checkout", "--quiet", "-b", "old", "HEAD~1"]);

        let ran = w.exec(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(
            ran.text
                .contains("기본 브랜치에 chute.toml이 이미 있어 등록만 했다"),
            "{}",
            ran.text
        );
        assert!(!w.work_config().exists());
        assert_eq!(w.runs(), 0);
        assert!(!w.ctx.home.slots_dir(&w.slug).exists());
        assert!(w.ctx.home.repo(&w.slug).info().unwrap().is_some());
        assert!(!w.record_file().exists());
    }

    #[test]
    fn the_setup_commit_holds_the_work_config_and_the_agents_file_with_the_line_added() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("AGENTS.md", "# rules");
        let ready = w.prepare(&[&counted], None);

        assert_eq!(ready.instructions.file, "AGENTS.md");
        assert_eq!(
            w.show(&ready.commit, "AGENTS.md"),
            format!("# rules\n{AGENTS_LINE}\n")
        );
        assert_eq!(
            w.show(&ready.commit, "chute.toml"),
            std::fs::read_to_string(w.work_config()).unwrap()
        );
        assert_eq!(w.changed_files(&ready), "AGENTS.md\nchute.toml");
        let parent = w.repo.git(&["rev-parse", &format!("{}^", ready.commit)]);
        assert_eq!(parent.trim(), ready.base.as_str());
        assert_eq!(
            ready.base.as_str(),
            w.repo.git(&["rev-parse", "origin/main"]).trim()
        );
    }

    #[test]
    fn a_missing_agents_file_is_created_with_the_line_alone() {
        let w = World::new();
        let counted = w.counting("counted");
        let ready = w.prepare(&[&counted], None);
        assert_eq!(
            w.show(&ready.commit, "AGENTS.md"),
            format!("{AGENTS_LINE}\n")
        );
    }

    #[test]
    fn an_agents_file_that_has_the_line_stays_as_it_is() {
        let w = World::new();
        let counted = w.counting("counted");
        let existing = format!("# rules\n{AGENTS_LINE}\nmore\n");
        w.push_main("AGENTS.md", &existing);
        let ran = w.exec(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(ran.text.contains("지침 파일: AGENTS.md"), "{}", ran.text);
        assert!(ran.text.contains("이미 있어 바꾸지 않는다"), "{}", ran.text);

        let ready = w.prepare(&[&counted], None);
        assert_eq!(ready.instructions.content, None);
        assert_eq!(w.show(&ready.commit, "AGENTS.md"), existing);
        assert_eq!(w.changed_files(&ready), "chute.toml");
    }

    #[test]
    fn with_only_a_claude_file_the_line_goes_in_it() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("CLAUDE.md", "# claude\n");
        let ran = w.exec(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(ran.text.contains("지침 파일: CLAUDE.md"), "{}", ran.text);

        let ready = w.prepare(&[&counted], None);
        assert_eq!(
            w.show(&ready.commit, "CLAUDE.md"),
            format!("# claude\n{AGENTS_LINE}\n")
        );
        assert_eq!(w.changed_files(&ready), "CLAUDE.md\nchute.toml");
    }

    #[test]
    fn the_instructions_option_picks_the_file_even_when_agents_md_exists() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("AGENTS.md", "# rules\n");
        let ran = w.exec_at(&World::args(&[&counted], Some("./docs/ai.md")), NOW);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(ran.text.contains("지침 파일: docs/ai.md"), "{}", ran.text);
        let ready = w.prepare(&[&counted], Some("docs/ai.md"));
        assert_eq!(
            w.show(&ready.commit, "docs/ai.md"),
            format!("{AGENTS_LINE}\n")
        );
        assert_eq!(w.changed_files(&ready), "chute.toml\ndocs/ai.md");
    }

    #[test]
    fn instructions_paths_that_leave_the_repository_or_name_chute_toml_are_refused() {
        let w = World::new();
        let counted = w.counting("counted");
        for path in [
            "/etc/passwd",
            "../outside.md",
            "docs/../AGENTS.md",
            "chute.toml",
            "./chute.toml",
            ".git/config",
            "",
        ] {
            let ran = w.exec_at(&World::args(&[&counted], Some(path)), NOW);
            assert_eq!(
                ran.code,
                code(ExitCode::from(REFUSED)),
                "{path}: {}",
                ran.text
            );
            assert!(ran.text.contains("--instructions"), "{path}: {}", ran.text);
        }
        assert_eq!(w.runs(), 0);
        assert!(!w.record_file().exists());
    }

    fn push_symlink(w: &World, link: &str, target: &str) {
        std::os::unix::fs::symlink(target, w.repo.path().join(link)).unwrap();
        w.repo.git(&["add", link]);
        w.repo
            .git(&["commit", "--quiet", "-m", "chore: 심볼릭 링크"]);
        w.repo.git(&["push", "--quiet", "origin", "main"]);
    }

    #[test]
    fn an_instructions_file_that_is_a_symlink_on_the_default_branch_is_refused() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("AGENTS.md", "# rules\n");
        push_symlink(&w, "CLAUDE.md", "AGENTS.md");

        let ran = w.exec_at(&World::args(&[&counted], Some("CLAUDE.md")), NOW);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(ran.text.contains("심볼릭 링크"), "{}", ran.text);
        assert!(ran.text.contains("--instructions로 고른다"), "{}", ran.text);
        assert_eq!(w.runs(), 0);
    }

    #[test]
    fn a_default_pick_that_is_a_symlink_is_refused_the_same_way() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("CLAUDE.md", "# claude\n");
        push_symlink(&w, "AGENTS.md", "CLAUDE.md");

        let ran = w.exec(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(
            ran.text.contains("AGENTS.md은 심볼릭 링크다"),
            "{}",
            ran.text
        );
        assert_eq!(w.runs(), 0);
    }

    #[test]
    fn an_instructions_path_below_a_symlinked_folder_is_refused() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("real/keep.txt", "x\n");
        push_symlink(&w, "docs", "real");

        let ran = w.exec_at(&World::args(&[&counted], Some("docs/ai.md")), NOW);
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        assert!(ran.text.contains("docs가 폴더가 아니라"), "{}", ran.text);
        assert!(!w.repo.path().join("real/ai.md").exists());
    }

    #[test]
    fn after_setup_the_work_folder_is_as_it_was_except_for_the_draft_it_wrote() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("AGENTS.md", "# rules\n");
        w.repo.git(&["checkout", "--quiet", "-b", "feature"]);
        w.repo.write("staged.txt", "staged\n");
        w.repo.git(&["add", "staged.txt"]);
        let snapshot = |w: &World| {
            [
                w.repo.git(&["rev-parse", "HEAD"]),
                w.repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
                w.repo.git(&["status", "--porcelain"]),
                w.repo.git(&["ls-files", "--stage"]),
                w.repo.git(&["ls-remote", "--heads", "origin"]),
                std::fs::read_to_string(w.repo.path().join("AGENTS.md")).unwrap(),
            ]
        };
        let before = snapshot(&w);

        let ran = w.exec(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        let after = snapshot(&w);
        assert_eq!(after[..2], before[..2]);
        assert_eq!(after[3..], before[3..]);
        assert_eq!(before[2], "A  staged.txt\n");
        assert_eq!(after[2], "A  staged.txt\n?? chute.toml\n");
        assert_eq!(w.repo.git(&["branch", "--list"]).trim().lines().count(), 2);
    }
}
