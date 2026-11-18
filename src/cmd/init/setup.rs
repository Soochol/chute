use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use super::merge::{self, Console, Landed};
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

pub(super) const COMMIT_MESSAGE: &str = "chute: 머지 큐 설정 추가";
const LOG_TAIL_LINES: usize = 20;
const HOUR_MS: u64 = 60 * 60 * 1000;

#[derive(Debug)]
pub(super) struct Ready {
    pub info: RepoInfo,
    pub base: Sha,
    pub commit: Sha,
    pub instructions: instructions::Chosen,
    pub files: Vec<(String, Vec<u8>)>,
    pub tree: Sha,
    pub steps: Vec<String>,
    pub max_parallel: u32,
}

const MAX_ATTEMPTS: u32 = 3;

pub(super) fn run(
    args: &Args,
    ctx: &Ctx,
    console: &mut Console,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode> {
    run_at(args, ctx, console, out, err, EpochMs::now())
}

fn run_at(
    args: &Args,
    ctx: &Ctx,
    console: &mut Console,
    out: &mut dyn Write,
    err: &mut dyn Write,
    now: EpochMs,
) -> Result<ExitCode> {
    match drive(args, ctx, console, out, now) {
        Ok(code) => Ok(code),
        Err(e) => match e.downcast::<Refusal>() {
            Ok(refusal) => {
                writeln!(err, "chute: {refusal}")?;
                Ok(ExitCode::from(REFUSED))
            }
            Err(e) => Err(e),
        },
    }
}

fn drive(
    args: &Args,
    ctx: &Ctx,
    console: &mut Console,
    out: &mut dyn Write,
    now: EpochMs,
) -> Result<ExitCode> {
    let mut confirmed: Option<(String, Vec<Sha>)> = None;
    for _ in 0..MAX_ATTEMPTS {
        let Some(ready) = prepare(args, ctx, out, now)? else {
            return Ok(ExitCode::SUCCESS);
        };
        let diff = show(ctx, &ready, out)?;
        if !args.yes && !console.tty {
            return Err(Refusal(
                "터미널이 아니라 확인을 받을 수 없어 멈춘다. 위 diff를 확인한 뒤 `--yes`로 다시 실행한다"
                    .into(),
            )
            .into());
        }
        let open = merge::find_open_pr(ctx, &ready.info)?;
        let left_over = match &open {
            Some(_) => None,
            None => merge::remote_setup_tip(ctx, &ready.info)?,
        };
        let target = open.as_ref().map(|pr| &pr.head).or(left_over.as_ref());
        let covered = |(agreed, heads): &(String, Vec<Sha>)| {
            *agreed == diff && target.is_none_or(|head| heads.contains(head))
        };
        if !args.yes && !confirmed.as_ref().is_some_and(covered) {
            let merge_into = format!("{}/{}", ready.info.remote_name, ready.info.default_branch);
            match (&open, &left_over) {
                (Some(pr), _) => writeln!(
                    out,
                    "열린 PR {}을 이어 쓴다(HEAD {}). 그 HEAD가 검증한 setup 커밋이 아니면 새 setup 커밋으로 덮어쓴다.",
                    pr.number, pr.head
                )?,
                (None, Some(tip)) => writeln!(
                    out,
                    "원격에 {}이 이미 있고 열린 PR이 없다(HEAD {tip}). 그 HEAD가 검증한 setup 커밋이 아니면 새 setup 커밋으로 덮어쓴다.",
                    merge::BRANCH
                )?,
                (None, None) => writeln!(out, "PR을 새로 만든다.")?,
            }
            writeln!(
                out,
                "setup 브랜치 {}를 push하고 PR을 만들어 {merge_into}에 바로 머지한다. 큐를 거치지 않는다.",
                merge::BRANCH
            )?;
            if !console.confirm(out)? {
                return Err(Refusal("확인하지 않아 아무것도 바꾸지 않았다".into()).into());
            }
            let mut heads: Vec<Sha> = target.cloned().into_iter().collect();
            if let Some((_, earlier)) = confirmed.take() {
                heads.extend(earlier);
            }
            confirmed = Some((diff.clone(), heads));
        }
        let overwrite = if args.yes {
            merge::Overwrite::Refuse
        } else {
            merge::Overwrite::Confirmed
        };
        match merge::land(ctx, &ready, open, left_over, overwrite, out)? {
            Landed::Merged | Landed::AlreadyConfigured => return Ok(ExitCode::SUCCESS),
            Landed::Moved { head } => {
                if let Some((_, heads)) = confirmed.as_mut() {
                    heads.push(head);
                }
            }
        }
    }
    Err(Refusal("기본 브랜치가 계속 바뀐다. 잠시 뒤 다시 실행한다".into()).into())
}

fn show(ctx: &Ctx, ready: &Ready, out: &mut dyn Write) -> Result<String> {
    writeln!(out)?;
    writeln!(out, "지침 파일: {}", ready.instructions.file)?;
    if ready.instructions.content.is_none() {
        writeln!(out, "지침 파일에 같은 줄이 이미 있어 바꾸지 않는다")?;
    }
    writeln!(out)?;
    let diff = setup_diff(ctx, &ready.info.path, &ready.base, &ready.commit)?;
    write!(out, "{diff}")?;
    writeln!(out)?;
    Ok(diff)
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
    let mut files = vec![(config::FILE_NAME.to_owned(), draft)];
    if let Some(content) = &chosen.content {
        files.push((chosen.file.clone(), content.clone()));
    }
    let staged: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_slice()))
        .collect();
    let commit = slot.commit_files(&base, &staged, COMMIT_MESSAGE)?;

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
                        tree: tree.clone(),
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
        files,
        tree,
        steps: config.verify.iter().map(|step| step.name.clone()).collect(),
        max_parallel: config.max_parallel,
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
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::cmd::init::AGENTS_LINE;
    use crate::ext::Ext;
    use crate::gitops::Gitops;
    use crate::model::PrNumber;
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
        gh_state: PathBuf,
        git_log: PathBuf,
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
            let gh_state = install_fake_gh(&sb, repo.origin());
            let git_log = install_logging_git(&sb);
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
                gh_state,
                git_log,
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
                yes: false,
            }
        }

        fn yes_args(verify: &[&str]) -> Args {
            Args {
                yes: true,
                ..World::args(verify, None)
            }
        }

        fn exec_at(&self, args: &Args, now: EpochMs) -> Ran {
            self.go(args, None, now)
        }

        fn go(&self, args: &Args, tty: Option<&str>, now: EpochMs) -> Ran {
            let mut input = tty.unwrap_or("").as_bytes();
            self.go_reading(args, tty.is_some(), &mut input, now)
        }

        fn go_reading(
            &self,
            args: &Args,
            tty: bool,
            input: &mut dyn std::io::BufRead,
            now: EpochMs,
        ) -> Ran {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let mut console = Console { tty, input };
            let code = run_at(args, &self.ctx, &mut console, &mut out, &mut err, now).unwrap();
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

        fn exec_yes(&self, verify: &[&str]) -> Ran {
            self.exec_at(&World::yes_args(verify), NOW)
        }

        fn gh_calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.gh_state.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn gh_called(&self, prefix: &str) -> usize {
            self.gh_calls()
                .iter()
                .filter(|call| call.starts_with(prefix))
                .count()
        }

        fn setup_branch_pushes(&self) -> usize {
            std::fs::read_to_string(&self.git_log)
                .unwrap_or_default()
                .lines()
                .filter(|line| line.contains(" push ") && line.contains(":refs/heads/chute/setup"))
                .count()
        }

        fn gh_knob(&self, name: &str, value: &str) {
            std::fs::write(self.gh_state.join(name), value).unwrap();
        }

        fn origin_ref(&self, name: &str) -> Option<String> {
            let out = self.repo.git(&["ls-remote", "origin", name]);
            out.split_whitespace().next().map(str::to_owned)
        }

        fn origin_main(&self) -> String {
            self.origin_ref("refs/heads/main").unwrap()
        }

        fn origin_git(&self, args: &[&str]) -> String {
            let mut full = vec!["--git-dir", self.repo.origin().to_str().unwrap()];
            full.extend_from_slice(args);
            self.repo.git(&full)
        }

        fn pending_main(&self, path: &str, content: &str) -> String {
            let main = self.origin_main();
            self.repo.git(&["fetch", "--quiet", "origin"]);
            self.repo.git(&["checkout", "--quiet", "-b", "side", &main]);
            self.repo.commit_file(path, content, "chore: 시험용 커밋");
            self.repo
                .git(&["push", "--quiet", "origin", "side:refs/heads/pending"]);
            let tip = self.repo.head();
            self.repo.git(&["checkout", "--quiet", "main"]);
            tip
        }

        fn merge_parents(&self, commit: &str) -> Vec<String> {
            let line = self.origin_git(&["rev-list", "--parents", "-n", "1", commit]);
            line.split_whitespace().skip(1).map(str::to_owned).collect()
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

    /// A `gh` that keeps its state in files and a bare remote, the way GitHub would: `pr create`
    /// opens PR 1 and points `refs/pull/1/head` at the setup branch, which later pushes move too.
    /// A file `on_<list|create|view>` in the state folder is run after that call; the knobs
    /// `state` and `base` change what `pr view` answers, and `frozen` stops the pull ref following
    /// the branch.
    fn install_fake_gh(sb: &Sandbox, origin: &Path) -> PathBuf {
        let state = sb.root().join("gh-state");
        std::fs::create_dir_all(&state).unwrap();
        let body = r#"d='@STATE@'; o='@ORIGIN@'
echo "$*" >> "$d/calls"
g() { git --git-dir="$o" "$@"; }
sync() {
  n=$(cat "$d/pr" 2>/dev/null)
  if [ -n "$n" ] && [ ! -f "$d/frozen" ]; then
    g update-ref "refs/pull/$n/head" "$(g rev-parse refs/heads/chute/setup)"
  fi
}
hook() { if [ -f "$d/on_$1" ]; then sh "$d/on_$1"; fi; }
case "$1 $2" in
'pr list')
  sync
  n=$(cat "$d/pr" 2>/dev/null)
  if [ -n "$n" ]; then
    printf '[{"number":%s,"headRefOid":"%s","isCrossRepository":false}]\n' "$n" "$(g rev-parse "refs/pull/$n/head")"
  else
    echo '[]'
  fi
  hook list ;;
'pr create')
  n=$(( $(cat "$d/count" 2>/dev/null || echo 0) + 1 ))
  echo $n > "$d/count"; echo $n > "$d/pr"
  g update-ref "refs/pull/$n/head" "$(g rev-parse refs/heads/chute/setup)"
  echo "https://github.com/acme/widget/pull/$n"
  hook create ;;
'pr view')
  sync
  printf '{"state":"%s","baseRefName":"%s","headRefName":"chute/setup","isCrossRepository":false}\n' "$(cat "$d/state" 2>/dev/null || echo OPEN)" "$(cat "$d/base" 2>/dev/null || echo main)"
  hook view ;;
*)
  echo "fake gh: $*" >&2
  exit 97 ;;
esac
"#
        .replace("@STATE@", &state.to_string_lossy())
        .replace("@ORIGIN@", &origin.to_string_lossy());
        sb.script("gh", &body);
        state
    }

    fn install_logging_git(sb: &Sandbox) -> PathBuf {
        let real = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout;
        let real = String::from_utf8(real).unwrap().trim().to_owned();
        let log = sb.root().join("git-calls");
        sb.script(
            "git",
            &format!(
                "echo \" $*\" >> '{}'\nexec '{real}' \"$@\"\n",
                log.display()
            ),
        );
        log
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);

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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);

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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
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
        assert_eq!(first.code, code(ExitCode::from(REFUSED)), "{}", first.text);
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
        assert_eq!(again.code, code(ExitCode::from(REFUSED)), "{}", again.text);
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
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
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
        let after = snapshot(&w);
        assert_eq!(after[..2], before[..2]);
        assert_eq!(after[3..], before[3..]);
        assert_eq!(before[2], "A  staged.txt\n");
        assert_eq!(after[2], "A  staged.txt\n?? chute.toml\n");
        assert_eq!(w.repo.git(&["branch", "--list"]).trim().lines().count(), 2);
    }

    fn refused(ran: &Ran) {
        assert_eq!(ran.code, code(ExitCode::from(REFUSED)), "{}", ran.text);
    }

    fn assert_no_gh_writes(w: &World) {
        assert_eq!(w.gh_called("pr create"), 0, "{:?}", w.gh_calls());
        assert_eq!(w.gh_called("pr merge"), 0, "{:?}", w.gh_calls());
    }

    fn leave_an_open_pr(w: &World, verify: &[&str]) -> String {
        let main = w.origin_main();
        w.gh_knob("state", "CLOSED");
        let ran = w.exec_yes(verify);
        refused(&ran);
        assert!(ran.text.contains("열려 있지 않다"), "{}", ran.text);
        w.gh_knob("state", "OPEN");
        assert_eq!(w.origin_main(), main);
        w.origin_ref("refs/heads/chute/setup").unwrap()
    }

    #[test]
    fn without_a_terminal_and_without_yes_the_diff_and_the_yes_hint_are_printed_and_nothing_is_written()
     {
        let w = World::new();
        let counted = w.counting("counted");
        let main = w.origin_main();
        let ran = w.exec(&[&counted]);
        refused(&ran);
        assert!(ran.text.contains("+++ b/chute.toml"), "{}", ran.text);
        assert!(ran.text.contains("`--yes`로 다시 실행한다"), "{}", ran.text);
        assert_eq!(w.origin_ref("refs/heads/chute/setup"), None);
        assert_eq!(w.origin_main(), main);
        assert_eq!(w.gh_calls(), Vec::<String>::new());
    }

    #[test]
    fn a_terminal_answer_of_n_writes_nothing_outside_and_y_goes_the_whole_way() {
        let w = World::new();
        let counted = w.counting("counted");
        let main = w.origin_main();
        let no = w.go(&World::args(&[&counted], None), Some("n\n"), NOW);
        refused(&no);
        assert!(no.text.contains("[y/N]"), "{}", no.text);
        assert!(no.text.contains("PR을 새로 만든다"), "{}", no.text);
        assert_eq!(w.origin_ref("refs/heads/chute/setup"), None);
        assert_eq!(w.origin_main(), main);
        assert_no_gh_writes(&w);

        let empty = w.go(&World::args(&[&counted], None), Some(""), NOW);
        refused(&empty);
        assert_eq!(w.origin_main(), main);

        let yes = w.go(&World::args(&[&counted], None), Some("y\n"), NOW);
        assert_eq!(yes.code, code(ExitCode::SUCCESS), "{}", yes.text);
        assert_ne!(w.origin_main(), main);
        assert_eq!(w.gh_called("pr create"), 1);
    }

    #[test]
    fn yes_pushes_the_branch_opens_the_pr_and_merges_the_verified_tree_by_a_merge_commit() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("AGENTS.md", "# rules\n");
        let before = w.origin_main();
        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(!ran.text.contains("[y/N]"), "{}", ran.text);

        let calls = w.gh_calls();
        assert!(
            calls
                .iter()
                .any(|call| call.starts_with("pr create --base main --head chute/setup ")),
            "{calls:?}"
        );
        assert_eq!(w.gh_called("pr merge"), 0, "{calls:?}");

        let merged = w.origin_main();
        let setup = w.origin_ref("refs/heads/chute/setup").unwrap();
        assert_eq!(w.merge_parents(&merged), [before, setup]);
        let record: SetupRecord =
            serde_json::from_slice(&std::fs::read(w.record_file()).unwrap()).unwrap();
        assert_eq!(
            w.origin_git(&["rev-parse", &format!("{merged}^{{tree}}")])
                .trim(),
            record.tree.as_str()
        );
        let subject = w.origin_git(&["log", "-1", "--format=%B", &merged]);
        assert!(
            subject.starts_with(
                "Merge pull request #1 from acme/chute/setup\n\nchute: 머지 큐 설정 추가"
            ),
            "{subject}"
        );
        assert_eq!(
            w.origin_git(&["show", &format!("{merged}:AGENTS.md")]),
            format!("# rules\n{AGENTS_LINE}\n")
        );
        assert!(!w.work_config().exists());
        assert!(ran.text.contains("지웠다"), "{}", ran.text);
        assert!(ran.text.contains("`git pull`"), "{}", ran.text);
        assert!(ran.text.contains("chute install"), "{}", ran.text);
        assert!(ran.text.contains("chute submit"), "{}", ran.text);
    }

    #[test]
    fn a_work_config_that_differs_from_the_merged_one_is_kept() {
        let w = World::new();
        let counted = w.counting("counted");
        let draft_edit = format!(
            "echo '# edited after the confirmation' >> '{}'\n",
            w.work_config().display()
        );
        w.gh_knob("on_view", &draft_edit);
        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(w.work_config().exists());
        assert!(ran.text.contains("달라 그대로 둔다"), "{}", ran.text);
    }

    #[test]
    fn a_work_config_that_cannot_be_read_after_the_merge_is_kept_and_the_run_still_succeeds() {
        let w = World::new();
        let counted = w.counting("counted");
        w.gh_knob(
            "on_view",
            &format!("chmod 000 '{}'\n", w.work_config().display()),
        );
        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(ran.text.contains("머지했다"), "{}", ran.text);
        assert!(ran.text.contains("달라 그대로 둔다"), "{}", ran.text);
        assert!(w.work_config().exists());
    }

    #[test]
    fn a_rerun_with_an_open_pr_of_the_same_content_neither_creates_nor_pushes_again() {
        let w = World::new();
        let counted = w.counting("counted");
        let setup = leave_an_open_pr(&w, &[&counted]);
        assert_eq!(w.gh_called("pr create"), 1);
        assert_eq!(w.setup_branch_pushes(), 1);

        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert_eq!(w.gh_called("pr create"), 1);
        assert_eq!(w.setup_branch_pushes(), 1);
        assert!(ran.text.contains("내용이 같아 그대로 쓴다"), "{}", ran.text);
        let merged = w.origin_main();
        assert_eq!(w.merge_parents(&merged)[1], setup);
        assert_eq!(w.gh_called("pr merge"), 0);
    }

    #[test]
    fn a_rerun_after_the_draft_was_edited_pushes_a_new_commit_by_lease_to_the_same_pr() {
        let w = World::new();
        let counted = w.counting("counted");
        let first = leave_an_open_pr(&w, &[&counted]);

        let mut text = std::fs::read_to_string(w.work_config()).unwrap();
        text.push_str("# edited\n");
        std::fs::write(w.work_config(), text).unwrap();
        let ran = w.exec_yes(&[]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);

        assert_eq!(w.gh_called("pr create"), 1);
        assert_eq!(w.setup_branch_pushes(), 2);
        let log = std::fs::read_to_string(&w.git_log).unwrap();
        assert!(
            log.contains(&format!(
                "--force-with-lease=refs/heads/chute/setup:{first} "
            )),
            "{log}"
        );
        let second = w.origin_ref("refs/heads/chute/setup").unwrap();
        assert_ne!(second, first);
        let merged = w.origin_main();
        assert_eq!(w.merge_parents(&merged)[1], second);
        assert!(
            w.origin_git(&["log", "-1", "--format=%B", &merged])
                .starts_with("Merge pull request #1 ")
        );
    }

    #[test]
    fn a_first_push_leases_on_the_branch_not_existing() {
        let w = World::new();
        let counted = w.counting("counted");
        w.exec_yes(&[&counted]);
        let log = std::fs::read_to_string(&w.git_log).unwrap();
        assert!(
            log.contains("--force-with-lease=refs/heads/chute/setup: "),
            "{log}"
        );
    }

    #[test]
    fn a_pull_ref_that_differs_from_the_confirmed_setup_commit_is_refused() {
        let w = World::new();
        let counted = w.counting("counted");
        let main = w.origin_main();
        w.gh_knob(
            "on_view",
            &format!(
                "touch '{}/frozen'\ngit --git-dir='{}' update-ref refs/pull/1/head refs/heads/main\n",
                w.gh_state.display(),
                w.repo.origin().display()
            ),
        );
        let ran = w.exec_yes(&[&counted]);
        refused(&ran);
        assert!(ran.text.contains("확인한 setup 커밋"), "{}", ran.text);
        assert_eq!(w.origin_main(), main);
        assert_eq!(w.gh_called("pr merge"), 0);
    }

    fn push_on_top_of(w: &World, setup: &str, path: &str, content: &str) -> String {
        let draft = w.work_config();
        let aside = w.sb.root().join("draft-aside");
        std::fs::rename(&draft, &aside).unwrap();
        w.repo.git(&["fetch", "--quiet", "origin"]);
        w.repo.git(&["checkout", "--quiet", "-b", "other", setup]);
        w.repo.commit_file(path, content, "chore: 다른 사람의 커밋");
        let tip = w.repo.head();
        w.repo.git(&[
            "push",
            "--quiet",
            "--force",
            "origin",
            "other:refs/heads/chute/setup",
        ]);
        w.repo.git(&["checkout", "--quiet", "main"]);
        std::fs::rename(&aside, &draft).unwrap();
        tip
    }

    fn lease_in_git_log(w: &World, head: &str) -> bool {
        std::fs::read_to_string(&w.git_log)
            .unwrap()
            .contains(&format!(
                "--force-with-lease=refs/heads/chute/setup:{head} "
            ))
    }

    fn conditions_of(w: &World, verify: &[&str], head: &str) -> anyhow::Error {
        let ready = w.prepare(verify, None);
        let gitops = Gitops::new(&w.ctx.ext, &w.ctx.home, &ready.info);
        let slot = gitops
            .lock_preflight_slot(ready.max_parallel)
            .unwrap()
            .open()
            .unwrap();
        slot.fetch_base().unwrap();
        let head = Sha::try_from(head.to_owned()).unwrap();
        slot.fetch_pr_head(PrNumber(1), &head).unwrap();
        let err = merge::conditions(&w.ctx, &slot, &ready, PrNumber(1), &head).unwrap_err();
        assert!(err.downcast_ref::<Refusal>().is_some(), "{err:#}");
        err
    }

    #[test]
    fn a_pr_head_with_another_file_is_rebuilt_and_the_file_stays_out_of_main() {
        let w = World::new();
        let counted = w.counting("counted");
        let setup = leave_an_open_pr(&w, &[&counted]);
        let evil = push_on_top_of(&w, &setup, "evil.sh", "rm -rf /\n");

        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);

        assert!(lease_in_git_log(&w, &evil));
        assert!(ran.text.contains(&evil), "{}", ran.text);
        let merged = w.origin_main();
        assert!(
            !w.origin_git(&["ls-tree", "-r", "--name-only", &merged])
                .contains("evil.sh")
        );
        assert_eq!(w.gh_called("pr create"), 1);
    }

    #[test]
    fn the_conditions_refuse_a_head_that_changes_another_file() {
        let w = World::new();
        let counted = w.counting("counted");
        let setup = leave_an_open_pr(&w, &[&counted]);
        let main = w.origin_main();
        let evil = push_on_top_of(&w, &setup, "evil.sh", "rm -rf /\n");

        let text = format!("{}", conditions_of(&w, &[&counted], &evil));
        assert!(text.contains("evil.sh"), "{text}");
        assert!(text.contains("밖의 파일"), "{text}");
        assert_eq!(w.origin_main(), main);
    }

    fn push_head_with_an_extra_parent(w: &World, setup: &str) -> (String, String) {
        let draft = w.work_config();
        let aside = w.sb.root().join("draft-aside");
        std::fs::rename(&draft, &aside).unwrap();
        w.repo.git(&["fetch", "--quiet", "origin"]);
        w.repo.git(&["checkout", "--quiet", "--orphan", "junk"]);
        w.repo.git(&["rm", "-rfq", "."]);
        let junk = w
            .repo
            .commit_file("secret.txt", "never shown in the diff\n", "junk");
        w.repo.git(&["checkout", "--quiet", "-f", "main"]);
        let tree = format!("{setup}^{{tree}}");
        let smuggled = w
            .repo
            .git(&[
                "commit-tree",
                &tree,
                "-p",
                setup,
                "-p",
                &junk,
                "-m",
                COMMIT_MESSAGE,
            ])
            .trim()
            .to_owned();
        w.repo.git(&[
            "push",
            "--quiet",
            "--force",
            "origin",
            &format!("{smuggled}:refs/heads/chute/setup"),
        ]);
        std::fs::rename(&aside, &draft).unwrap();
        (smuggled, junk)
    }

    #[test]
    fn a_reused_pr_head_with_an_extra_parent_is_rebuilt_and_its_history_stays_out_of_main() {
        let w = World::new();
        let counted = w.counting("counted");
        let setup = leave_an_open_pr(&w, &[&counted]);
        let before = w.origin_main();
        let (smuggled, junk) = push_head_with_an_extra_parent(&w, &setup);

        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);

        assert!(lease_in_git_log(&w, &smuggled));
        let merged = w.origin_main();
        let parents = w.merge_parents(&merged);
        assert_eq!(parents[0], before);
        assert_eq!(w.merge_parents(&parents[1]), std::slice::from_ref(&before));
        let hits = w.origin_git(&[
            "log",
            "--full-history",
            "--format=%H",
            &merged,
            "--",
            "secret.txt",
        ]);
        assert!(hits.trim().is_empty(), "{hits}");
        assert!(
            !w.origin_git(&["rev-list", &merged]).contains(&junk),
            "the extra parent is in the history of main"
        );
    }

    #[test]
    fn the_conditions_refuse_a_head_with_the_verified_tree_but_another_parent() {
        let w = World::new();
        let counted = w.counting("counted");
        let setup = leave_an_open_pr(&w, &[&counted]);
        let main = w.origin_main();
        let (smuggled, _) = push_head_with_an_extra_parent(&w, &setup);

        let text = format!("{}", conditions_of(&w, &[&counted], &smuggled));
        assert!(text.contains("부모"), "{text}");
        assert_eq!(w.origin_main(), main);
    }

    #[test]
    fn a_reused_pr_head_that_only_sets_the_exec_bit_is_rebuilt_and_merged() {
        let w = World::new();
        let counted = w.counting("counted");
        let setup = leave_an_open_pr(&w, &[&counted]);
        let draft = w.work_config();
        let aside = w.sb.root().join("draft-aside");
        std::fs::rename(&draft, &aside).unwrap();
        w.repo.git(&["fetch", "--quiet", "origin"]);
        w.repo.git(&["checkout", "--quiet", "-b", "other", &setup]);
        w.repo.git(&["update-index", "--chmod=+x", "chute.toml"]);
        w.repo.git(&["commit", "--quiet", "-m", "chmod"]);
        let chmod = w.repo.head();
        w.repo.git(&[
            "push",
            "--quiet",
            "--force",
            "origin",
            "other:refs/heads/chute/setup",
        ]);
        w.repo.git(&["checkout", "--quiet", "-f", "main"]);
        std::fs::rename(&aside, &draft).unwrap();
        let main = w.origin_main();

        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);

        assert!(lease_in_git_log(&w, &chmod));
        let merged = w.origin_main();
        assert_ne!(merged, main);
        let mode = w.origin_git(&["ls-tree", &merged, "chute.toml"]);
        assert!(mode.starts_with("100644 "), "{mode}");
        let record: SetupRecord =
            serde_json::from_slice(&std::fs::read(w.record_file()).unwrap()).unwrap();
        assert_eq!(
            w.origin_git(&["rev-parse", &format!("{merged}^{{tree}}")])
                .trim(),
            record.tree.as_str()
        );
    }

    #[test]
    fn a_crlf_draft_under_text_auto_merges_and_the_rerun_only_registers() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main(".gitattributes", "* text=auto\n");
        w.repo.write(
            "chute.toml",
            &step("counted", &counted).replace('\n', "\r\n"),
        );
        let main = w.origin_main();

        let first = w.exec_yes(&[]);
        assert_eq!(first.code, code(ExitCode::SUCCESS), "{}", first.text);
        let merged = w.origin_main();
        assert_ne!(merged, main);
        let record: SetupRecord =
            serde_json::from_slice(&std::fs::read(w.record_file()).unwrap()).unwrap();
        assert_eq!(
            w.origin_git(&["rev-parse", &format!("{merged}^{{tree}}")])
                .trim(),
            record.tree.as_str()
        );
        assert!(!w.work_config().exists());
        assert!(first.text.contains("같아 지웠다"), "{}", first.text);
        assert_eq!(w.setup_branch_pushes(), 1);

        let second = w.exec_yes(&[]);
        assert_eq!(second.code, code(ExitCode::SUCCESS), "{}", second.text);
        assert!(second.text.contains("등록만 했다"), "{}", second.text);
        assert_eq!(w.setup_branch_pushes(), 1);
        assert_eq!(w.origin_main(), merged);
    }

    fn left_over_branch(w: &World) -> String {
        w.repo
            .pr_branch("chute/setup", "theirs.txt", "work in progress\n")
    }

    #[test]
    fn a_left_over_setup_branch_is_named_with_its_head_before_the_terminal_question() {
        let w = World::new();
        let counted = w.counting("counted");
        let theirs = left_over_branch(&w);
        let main = w.origin_main();

        let no = w.go(&World::args(&[&counted], None), Some("n\n"), NOW);
        refused(&no);
        let asked = no.text.split("[y/N]").next().unwrap();
        assert!(asked.contains("chute/setup"), "{}", no.text);
        assert!(asked.contains(&theirs), "{}", no.text);
        assert!(asked.contains("새 setup 커밋으로 덮어쓴다"), "{}", no.text);
        assert_eq!(w.origin_ref("refs/heads/chute/setup").unwrap(), theirs);
        assert_eq!(w.origin_main(), main);
        assert_eq!(w.gh_called("pr create"), 0);
        assert_eq!(w.setup_branch_pushes(), 0);

        let yes = w.go(&World::args(&[&counted], None), Some("y\n"), NOW);
        assert_eq!(yes.code, code(ExitCode::SUCCESS), "{}", yes.text);
        assert!(lease_in_git_log(&w, &theirs));
        assert_ne!(w.origin_main(), main);
        assert_eq!(w.gh_called("pr create"), 1);
    }

    #[test]
    fn yes_does_not_overwrite_a_left_over_setup_branch() {
        let w = World::new();
        let counted = w.counting("counted");
        let theirs = left_over_branch(&w);
        let main = w.origin_main();

        let ran = w.exec_yes(&[&counted]);
        refused(&ran);
        assert!(ran.text.contains(&theirs), "{}", ran.text);
        assert!(
            ran.text.contains("git push origin --delete chute/setup"),
            "{}",
            ran.text
        );
        assert_eq!(w.origin_ref("refs/heads/chute/setup").unwrap(), theirs);
        assert_eq!(w.origin_main(), main);
        assert_eq!(w.gh_called("pr create"), 0);
        assert_eq!(w.setup_branch_pushes(), 0);
    }

    #[test]
    fn a_left_over_setup_branch_that_is_the_verified_commit_gets_a_pr_without_a_push() {
        let w = World::new();
        let counted = w.counting("counted");
        let setup = leave_an_open_pr(&w, &[&counted]);
        std::fs::remove_file(w.gh_state.join("pr")).unwrap();
        let (pushes, creates) = (w.setup_branch_pushes(), w.gh_called("pr create"));

        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);

        assert_eq!(w.setup_branch_pushes(), pushes);
        assert_eq!(w.gh_called("pr create"), creates + 1);
        let merged = w.origin_main();
        assert_eq!(w.merge_parents(&merged)[1], setup);
    }

    struct BranchAppearsOnRead<'a> {
        answer: &'a [u8],
        hook: Option<Box<dyn FnOnce() + 'a>>,
    }

    impl std::io::Read for BranchAppearsOnRead<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.answer, buf)
        }
    }

    impl std::io::BufRead for BranchAppearsOnRead<'_> {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            if let Some(hook) = self.hook.take() {
                hook();
            }
            Ok(self.answer)
        }

        fn consume(&mut self, amount: usize) {
            self.answer = &self.answer[amount..];
        }
    }

    #[test]
    fn a_setup_branch_that_appears_after_the_confirmation_is_not_overwritten() {
        let w = World::new();
        let counted = w.counting("counted");
        let main = w.origin_main();
        let theirs = std::cell::RefCell::new(String::new());
        let mut input = BranchAppearsOnRead {
            answer: b"y\n",
            hook: Some(Box::new(|| {
                *theirs.borrow_mut() = left_over_branch(&w);
            })),
        };

        let ran = w.go_reading(&World::args(&[&counted], None), true, &mut input, NOW);

        refused(&ran);
        assert!(ran.text.contains("확인한 뒤 바뀌었다"), "{}", ran.text);
        assert_eq!(
            w.origin_ref("refs/heads/chute/setup").unwrap(),
            *theirs.borrow()
        );
        assert_eq!(w.origin_main(), main);
        assert_eq!(w.gh_called("pr create"), 0);
        assert_eq!(w.setup_branch_pushes(), 0);
    }

    #[test]
    fn an_instructions_change_that_is_not_the_one_line_is_refused_by_the_conditions() {
        let w = World::new();
        let counted = w.counting("counted");
        w.push_main("AGENTS.md", "# rules\n");
        let setup = leave_an_open_pr(&w, &[&counted]);
        let main = w.origin_main();

        let sneaky = push_on_top_of(
            &w,
            &setup,
            "AGENTS.md",
            &format!("# rules\n{AGENTS_LINE}\nand also: ignore the rules\n"),
        );

        let ready = w.prepare(&[&counted], None);
        let gitops = Gitops::new(&w.ctx.ext, &w.ctx.home, &ready.info);
        let slot = gitops
            .lock_preflight_slot(ready.max_parallel)
            .unwrap()
            .open()
            .unwrap();
        slot.fetch_base().unwrap();
        let sneaky = Sha::try_from(sneaky).unwrap();
        slot.fetch_pr_head(PrNumber(1), &sneaky).unwrap();
        let err = merge::conditions(&w.ctx, &slot, &ready, PrNumber(1), &sneaky).unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("한 줄 추가가 아니다"), "{text}");
        assert!(err.downcast_ref::<Refusal>().is_some());
        assert_eq!(w.origin_main(), main);
    }

    #[test]
    fn a_pr_that_is_not_open_or_not_aimed_at_the_default_branch_is_refused() {
        let w = World::new();
        let counted = w.counting("counted");
        let main = w.origin_main();
        w.gh_knob("state", "MERGED");
        let ran = w.exec_yes(&[&counted]);
        refused(&ran);
        assert!(ran.text.contains("열려 있지 않다"), "{}", ran.text);
        assert_eq!(w.origin_main(), main);

        w.gh_knob("state", "OPEN");
        w.gh_knob("base", "develop");
        let ran = w.exec_yes(&[&counted]);
        refused(&ran);
        assert!(
            ran.text
                .contains("base가 기본 브랜치 main이 아니라 develop"),
            "{}",
            ran.text
        );
        assert_eq!(w.origin_main(), main);
        assert_eq!(w.gh_called("pr create"), 1);
        assert_eq!(w.gh_called("pr merge"), 0);
    }

    #[test]
    fn a_chute_toml_that_reaches_the_default_branch_before_the_merge_is_only_registered() {
        let w = World::new();
        let counted = w.counting("counted");
        let theirs = w.pending_main("chute.toml", &step("theirs", "true"));
        w.gh_knob(
            "on_create",
            &format!(
                "git --git-dir='{}' update-ref refs/heads/main refs/heads/pending\n",
                w.repo.origin().display()
            ),
        );
        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(
            ran.text
                .contains("기본 브랜치에 chute.toml이 이미 있어 등록만 했다"),
            "{}",
            ran.text
        );
        assert_eq!(w.origin_main(), theirs);
        assert_eq!(w.gh_called("pr merge"), 0);
        assert!(w.ctx.home.repo(&w.slug).info().unwrap().is_some());
    }

    #[test]
    fn a_default_branch_that_moved_after_the_verification_is_verified_again_and_merged_on_top() {
        let w = World::new();
        let counted = w.counting("counted");
        let other = w.pending_main("other.txt", "someone else\n");
        w.gh_knob(
            "on_create",
            &format!(
                "git --git-dir='{}' update-ref refs/heads/main refs/heads/pending\n",
                w.repo.origin().display()
            ),
        );
        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert_eq!(w.runs(), 2);

        let merged = w.origin_main();
        let setup = w.origin_ref("refs/heads/chute/setup").unwrap();
        assert_eq!(w.merge_parents(&merged), [other.clone(), setup]);
        assert_eq!(
            w.origin_git(&["show", &format!("{merged}:other.txt")]),
            "someone else\n"
        );
        assert!(
            w.origin_git(&["show", &format!("{merged}:chute.toml")])
                .contains("test")
        );
        assert_eq!(w.gh_called("pr create"), 1);
    }

    #[test]
    fn a_push_lost_to_a_moved_default_branch_starts_over_from_step_zero() {
        let w = World::new();
        let counted = w.counting("counted");
        w.pending_main("other.txt", "someone else\n");
        w.gh_knob(
            "on_view",
            &format!(
                "git --git-dir='{}' update-ref refs/heads/main refs/heads/pending\nrm \"$0\"\n",
                w.repo.origin().display()
            ),
        );
        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(ran.text.contains("처음부터 다시 판정한다"), "{}", ran.text);
        assert_eq!(w.runs(), 2);
        let merged = w.origin_main();
        assert!(
            w.origin_git(&["show", &format!("{merged}:other.txt")])
                .contains("someone else")
        );
    }

    #[test]
    fn a_default_branch_that_keeps_moving_stops_the_run_after_three_attempts() {
        let w = World::new();
        let counted = w.counting("counted");
        let bump = format!(
            "o='{}'\nc=$(git --git-dir=$o commit-tree \"$(git --git-dir=$o rev-parse 'refs/heads/main^{{tree}}')\" -p refs/heads/main -m bump)\ngit --git-dir=$o update-ref refs/heads/main $c\n",
            w.repo.origin().display()
        );
        w.gh_knob("on_list", &bump);
        let ran = w.exec_yes(&[&counted]);
        refused(&ran);
        assert!(ran.text.contains("계속 바뀐다"), "{}", ran.text);
        assert_eq!(w.runs(), 3);
        assert_eq!(w.gh_called("pr merge"), 0);
        assert!(w.work_config().exists());
    }

    #[test]
    fn a_verification_that_fails_the_second_time_does_not_merge() {
        let w = World::new();
        let body = format!(
            "if [ -f '{c}' ]; then echo second >> '{c}'; echo boom-again; exit 1; fi\necho first >> '{c}'\n",
            c = w.counter.display()
        );
        let flaky = w.sb.script("once", &body).to_string_lossy().into_owned();
        let pending = w.pending_main("other.txt", "x\n");
        w.gh_knob(
            "on_create",
            &format!(
                "git --git-dir='{}' update-ref refs/heads/main refs/heads/pending\n",
                w.repo.origin().display()
            ),
        );
        let ran = w.exec_yes(&[&flaky]);
        refused(&ran);
        assert!(ran.text.contains("boom-again"), "{}", ran.text);
        assert!(w.runs() >= 2);
        assert_eq!(w.origin_main(), pending);
        assert_eq!(w.gh_called("pr merge"), 0);
    }

    #[test]
    fn after_a_merge_the_same_command_only_registers() {
        let w = World::new();
        let counted = w.counting("counted");
        let ran = w.exec_yes(&[&counted]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        let merged = w.origin_main();
        let calls = w.gh_calls().len();

        let again = w.exec_yes(&[&counted]);
        assert_eq!(again.code, code(ExitCode::SUCCESS), "{}", again.text);
        assert!(
            again
                .text
                .contains("기본 브랜치에 chute.toml이 이미 있어 등록만 했다"),
            "{}",
            again.text
        );
        assert_eq!(w.origin_main(), merged);
        assert_eq!(w.gh_calls().len(), calls);
        assert_eq!(w.runs(), 1);
    }

    #[test]
    fn a_flaky_pass_still_merges_the_tree_it_passed_on_but_leaves_no_record() {
        let w = World::new();
        let body = format!(
            "if [ -f '{c}.seen' ]; then echo ok >> '{c}'; exit 0; fi\ntouch '{c}.seen'\necho flake >> '{c}'\nexit 1\n",
            c = w.counter.display()
        );
        let flaky = w.sb.script("flaky", &body).to_string_lossy().into_owned();
        w.repo.write("chute.toml", &step("flaky", &flaky));
        let before = w.origin_main();
        let ran = w.exec_yes(&[]);
        assert_eq!(ran.code, code(ExitCode::SUCCESS), "{}", ran.text);
        assert!(!w.record_file().exists());
        assert_ne!(w.origin_main(), before);
    }
}
