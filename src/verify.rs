use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid, waitpid};
use nix::unistd::Pid;

use crate::config::{Config, PROBE_PLACEHOLDER, VerifyStep};
use crate::flaky;
use crate::model::{EpochMs, NonEmpty};
use crate::pc_limit::{self, Wait};
use crate::store::RepoStore;

const LOCK_FILE: &str = "verify.lock";
const LOCK_POLL: Duration = Duration::from_millis(50);
const MAX_STEP_POLL: Duration = Duration::from_millis(20);
const GROUP_POLL: Duration = Duration::from_millis(10);

const KILL_WAIT: Duration = Duration::from_secs(1);

/// Linux refuses a single `sh -c` argument of 128 KiB or more.
const MAX_ARG_BYTES: usize = 128 * 1024;
const SCAN_BYTES: u64 = 16 * 1024 * 1024;
const TAIL_LINES: usize = 40;
const TAIL_BYTES: usize = 8 * 1024;

/// Lets another thread end a verification. Clones share one flag.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct Options<'a> {
    pub rerun_on_exit_failure: bool,
    pub rerun_with_probe: bool,
    pub cancel: Cancel,
    pub kill_grace: Duration,
    /// Passed to the steps as `CHUTE_CACHE_DIR`; created before the first step.
    pub cache_dir: Option<PathBuf>,
    /// Called with the step's name as each step starts.
    pub on_step: Option<&'a dyn Fn(&str)>,
    /// Called when the run starts and stops waiting for a free PC slot.
    pub on_machine_wait: Option<&'a dyn Fn(Wait)>,
    /// The files the verified tree changes against its base. A step with `paths` runs only when
    /// one of them starts with a path. `None` runs every step.
    pub changed: Option<&'a [String]>,
    /// Called with the step's name for each step left out because of `paths`.
    pub on_skip: Option<&'a dyn Fn(&str)>,
    pub wanted: Option<Wanted<'a>>,
}

/// The failure a culprit search looks for: one step, and the tests that failed twice at the tip.
#[derive(Clone, Copy, Debug)]
pub struct Wanted<'a> {
    pub step: &'a str,
    pub tests: &'a [String],
    /// The `failed` pattern that named `tests`. A step whose pattern is another one does not
    /// run `tests` by `probe`, since the names may not be the ones its `probe` takes.
    pub named_by: Option<&'a str>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            rerun_on_exit_failure: true,
            rerun_with_probe: false,
            cancel: Cancel::new(),
            kill_grace: Duration::from_secs(5),
            cache_dir: None,
            on_step: None,
            on_machine_wait: None,
            changed: None,
            on_skip: None,
            wanted: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed { flaky: Vec<FlakyStep> },
    Failed(Failed),
    Cancelled,
}

/// A step that failed, then passed when run again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlakyStep {
    pub step: String,
    pub tests: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub step: String,
    pub cause: Cause,
    pub detail: Detail,
    /// The log of the attempt that ended the run.
    pub log: PathBuf,
    /// With `rerun_with_probe`, when the step failed twice: the tests that both attempts named.
    /// Empty when an attempt named none. `None` when the step was not run twice.
    pub confirmed: Option<Vec<String>>,
    /// The `failed` pattern of the step, which named the tests.
    pub named_by: Option<String>,
}

impl Failed {
    pub fn tests(&self) -> Vec<String> {
        match &self.detail {
            Detail::Tests(tests) => tests.to_vec(),
            Detail::Tail(_) => Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    Exited(i32),
    Signaled(i32),
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detail {
    Tests(NonEmpty<String>),
    Tail(String),
}

/// Step logs go to `log_dir` as `<step>.<attempt>.log`, with stdout and stderr together.
/// A reused folder loses a step's earlier logs when that step starts. Logs of steps never reached
/// stay.
///
/// A step that cannot be run at all is an `Err`. A step that runs and fails is an `Outcome`.
/// Every step of one call sees the same `CHUTE_RUN_ID`. Whatever a step leaves running in its
/// process group is ended with the attempt. A process that moved to another group, as `setsid`
/// and GNU `timeout` do, is not.
pub fn run(
    config: &Config,
    store: &RepoStore,
    dir: &Path,
    log_dir: &Path,
    opts: &Options,
) -> Result<Outcome> {
    if !dir.is_dir() {
        bail!("검증을 돌릴 폴더가 없다: {}", dir.display());
    }
    fs::create_dir_all(log_dir).with_context(|| format!("만들지 못했다: {}", log_dir.display()))?;
    if let Some(cache) = &opts.cache_dir {
        fs::create_dir_all(cache).with_context(|| format!("만들지 못했다: {}", cache.display()))?;
    }
    if opts.cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let _lock = if config.serialize {
        match acquire_lock(store, &opts.cancel)? {
            Lock::Held(file) => Some(file),
            Lock::Cancelled => return Ok(Outcome::Cancelled),
        }
    } else {
        None
    };
    let _slot = match pc_limit::acquire(store.home_dir(), &opts.cancel, opts.on_machine_wait)? {
        pc_limit::Slot::Held(file) => file,
        pc_limit::Slot::Cancelled => return Ok(Outcome::Cancelled),
    };
    let ctx = Ctx {
        dir,
        log_dir,
        run_id: run_id(),
        opts,
    };
    let mut flaky = Vec::new();
    for step in config.verify.iter() {
        if opts.cancel.is_cancelled() {
            return Ok(Outcome::Cancelled);
        }
        if opts.changed.is_some_and(|changed| !step.runs_for(changed)) {
            if let Some(on_skip) = opts.on_skip {
                on_skip(&step.name);
            }
            continue;
        }
        if let Some(on_step) = opts.on_step {
            on_step(&step.name);
        }
        match ctx.step(step)? {
            StepEnd::Passed { flaky: None } => {}
            StepEnd::Passed { flaky: Some(step) } => flaky.push(step),
            StepEnd::Failed(failed) => return Ok(Outcome::Failed(failed)),
            StepEnd::Cancelled => return Ok(Outcome::Cancelled),
        }
    }
    Ok(Outcome::Passed { flaky })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandEnd {
    Passed,
    Failed,
    TimedOut,
    Cancelled,
}

/// Runs `command` once in `dir` the way a step runs, in its own process group, with the same
/// environment, the verification lock when the config asks for one, and the cancel flag. Its
/// output goes to `log`, which must be in an existing folder.
pub fn run_probe(
    config: &Config,
    store: &RepoStore,
    dir: &Path,
    log: &Path,
    command: &str,
    timeout: Duration,
    opts: &Options,
) -> Result<CommandEnd> {
    if opts.cancel.is_cancelled() {
        return Ok(CommandEnd::Cancelled);
    }
    let _lock = if config.serialize {
        match acquire_lock(store, &opts.cancel)? {
            Lock::Held(file) => Some(file),
            Lock::Cancelled => return Ok(CommandEnd::Cancelled),
        }
    } else {
        None
    };
    let _slot = match pc_limit::acquire(store.home_dir(), &opts.cancel, opts.on_machine_wait)? {
        pc_limit::Slot::Held(file) => file,
        pc_limit::Slot::Cancelled => return Ok(CommandEnd::Cancelled),
    };
    if let Some(cache) = &opts.cache_dir {
        fs::create_dir_all(cache).with_context(|| format!("만들지 못했다: {}", cache.display()))?;
    }
    let ctx = Ctx {
        dir,
        log_dir: dir,
        run_id: run_id(),
        opts,
    };
    let step = VerifyStep {
        name: "probe".to_owned(),
        run: command.to_owned(),
        timeout,
        failed: None,
        probe: None,
        paths: None,
    };
    Ok(match ctx.attempt(&step, log)? {
        End::Passed => CommandEnd::Passed,
        End::Failed(Cause::TimedOut) => CommandEnd::TimedOut,
        End::Failed(_) => CommandEnd::Failed,
        End::Cancelled => CommandEnd::Cancelled,
    })
}

pub fn log_path(log_dir: &Path, step: &str, attempt: u32) -> PathBuf {
    log_dir.join(format!("{step}.{attempt}.log"))
}

struct Ctx<'a> {
    dir: &'a Path,
    log_dir: &'a Path,
    run_id: String,
    opts: &'a Options<'a>,
}

enum StepEnd {
    Passed { flaky: Option<FlakyStep> },
    Failed(Failed),
    Cancelled,
}

pub enum End {
    Passed,
    Failed(Cause),
    Cancelled,
}

impl Ctx<'_> {
    fn step(&self, step: &VerifyStep) -> Result<StepEnd> {
        let first_log = log_path(self.log_dir, &step.name, 1);
        let second_log = log_path(self.log_dir, &step.name, 2);
        remove_log(&second_log)?;
        let cause = match self.attempt(step, &first_log)? {
            End::Passed => return Ok(StepEnd::Passed { flaky: None }),
            End::Cancelled => return Ok(StepEnd::Cancelled),
            End::Failed(cause) => cause,
        };
        if !self.opts.rerun_on_exit_failure || cause == Cause::TimedOut {
            return Ok(StepEnd::Failed(failure(step, cause, first_log)?));
        }
        let first_tests = failed_tests(step, &read_window(&first_log)?);
        let searched = self
            .opts
            .wanted
            .filter(|w| w.step == step.name && !w.tests.is_empty())
            .filter(|w| w.named_by == step.failed.as_ref().map(|re| re.as_str()));
        let by_wanted = searched.and_then(|w| self.probe_step(step, w.tests));
        let probe = by_wanted
            .clone()
            .or_else(|| self.probe_step(step, &first_tests));
        match self.attempt(probe.as_ref().unwrap_or(step), &second_log)? {
            End::Passed => {
                let named = !first_tests.is_empty();
                let tests = match searched.filter(|_| by_wanted.is_some()) {
                    Some(w) => first_tests
                        .into_iter()
                        .filter(|test| w.tests.contains(test))
                        .collect(),
                    None => first_tests,
                };
                let flaky = (!named || !tests.is_empty()).then(|| FlakyStep {
                    step: step.name.clone(),
                    tests,
                });
                Ok(StepEnd::Passed { flaky })
            }
            End::Cancelled => Ok(StepEnd::Cancelled),
            End::Failed(cause) => {
                let mut failed = failure(step, cause, second_log)?;
                if self.opts.rerun_with_probe {
                    let second = failed.tests();
                    let both: Vec<String> = second
                        .into_iter()
                        .filter(|test| first_tests.contains(test))
                        .collect();
                    failed.confirmed = Some(both.clone());
                    if let Some(tests) = NonEmpty::new(both) {
                        failed.detail = Detail::Tests(tests);
                    } else if probe.is_some()
                        && matches!(failed.detail, Detail::Tail(_))
                        && let Some(tests) = NonEmpty::new(first_tests)
                    {
                        failed.detail = Detail::Tests(tests);
                    }
                }
                Ok(StepEnd::Failed(failed))
            }
        }
    }

    fn probe_step(&self, step: &VerifyStep, tests: &[String]) -> Option<VerifyStep> {
        let probe = step.probe.as_ref().filter(|_| self.opts.rerun_with_probe)?;
        if tests
            .iter()
            .any(|test| test.contains(['\0', char::REPLACEMENT_CHARACTER]))
        {
            return None;
        }
        let slots = probe.matches(PROBE_PLACEHOLDER).count().max(1);
        let (quoted, taken) = flaky::quote_within(tests, flaky::MAX_TESTS_BYTES / slots)?;
        if taken.len() != tests.len() {
            return None;
        }
        let run = probe.replace(PROBE_PLACEHOLDER, &quoted);
        if run.len() >= MAX_ARG_BYTES {
            return None;
        }
        Some(VerifyStep {
            run,
            ..step.clone()
        })
    }

    fn attempt(&self, step: &VerifyStep, log: &Path) -> Result<End> {
        if self.opts.cancel.is_cancelled() {
            return Ok(End::Cancelled);
        }
        let out = create_fresh_log(log)?;
        let err = out
            .try_clone()
            .with_context(|| format!("로그 파일을 복제하지 못했다: {}", log.display()))?;
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&step.run)
            .current_dir(self.dir)
            .env("CHUTE_RUN_ID", &self.run_id);
        match &self.opts.cache_dir {
            Some(cache) => command.env("CHUTE_CACHE_DIR", cache),
            None => command.env_remove("CHUTE_CACHE_DIR"),
        };
        let mut child = command
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0)
            .spawn()
            .with_context(|| format!("검증 단계 {:?}를 띄우지 못했다", step.name))?;
        let end = wait(&mut child, &step.name, step.timeout, &self.opts.cancel);
        terminate(&child, self.opts.kill_grace);
        end
    }
}

pub fn run_command(
    dir: &Path,
    run: &str,
    log: &Path,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<End> {
    let out = create_fresh_log(log)?;
    let err = out
        .try_clone()
        .with_context(|| format!("로그 파일을 복제하지 못했다: {}", log.display()))?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(run)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()
        .with_context(|| format!("명령 {run:?}을 띄우지 못했다"))?;
    let end = wait(&mut child, run, timeout, cancel);
    terminate(&child, Duration::from_secs(1));
    end
}

fn wait(child: &mut Child, name: &str, timeout: Duration, cancel: &Cancel) -> Result<End> {
    let leader = Pid::from_raw(child.id() as i32);
    // WNOWAIT leaves the exited leader unreaped so that its pid, which `terminate` signals as a
    // group id, cannot be reused before the SIGTERM.
    let exited = WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT;
    let deadline = Instant::now().checked_add(timeout);
    let mut pause = Duration::from_millis(1);
    loop {
        match waitid(Id::Pid(leader), exited) {
            Ok(WaitStatus::StillAlive) => {}
            Ok(WaitStatus::Exited(_, 0)) => return Ok(End::Passed),
            Ok(WaitStatus::Exited(_, code)) => return Ok(End::Failed(Cause::Exited(code))),
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                return Ok(End::Failed(Cause::Signaled(signal as i32)));
            }
            Ok(other) => bail!(
                "검증 단계 {:?}가 예상하지 못한 상태로 끝났다: {other:?}",
                name
            ),
            Err(Errno::EINVAL) => {
                // nix cannot name a real-time signal, so std reads the status instead.
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return Ok(End::Failed(Cause::Signaled(
                            status.signal().unwrap_or_default(),
                        )));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("검증 단계 {:?}의 끝을 보지 못했다", name));
                    }
                }
            }
            Err(e) => {
                return Err(e).with_context(|| format!("검증 단계 {:?}의 끝을 보지 못했다", name));
            }
        }
        if cancel.is_cancelled() {
            return Ok(End::Cancelled);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(End::Failed(Cause::TimedOut));
        }
        thread::sleep(pause);
        pause = (pause * 2).min(MAX_STEP_POLL);
    }
}

/// Append mode, so a step that truncates the log by reopening `/dev/stderr` leaves no hole of NULs.
fn create_fresh_log(path: &Path) -> Result<File> {
    remove_log(path)?;
    OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(path)
        .with_context(|| format!("로그 파일을 만들지 못했다: {}", path.display()))
}

fn remove_log(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("지우지 못했다: {}", path.display())),
    }
}

fn failure(step: &VerifyStep, cause: Cause, log: PathBuf) -> Result<Failed> {
    let window = read_window(&log)?;
    let detail = match NonEmpty::new(failed_tests(step, &window)) {
        Some(tests) => Detail::Tests(tests),
        None => Detail::Tail(tail(&window)),
    };
    Ok(Failed {
        step: step.name.clone(),
        cause,
        detail,
        log,
        confirmed: None,
        named_by: step.failed.as_ref().map(|re| re.as_str().to_owned()),
    })
}

fn failed_tests(step: &VerifyStep, text: &str) -> Vec<String> {
    let Some(pattern) = &step.failed else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    pattern
        .captures_iter(text)
        .map(|caps| {
            let group = caps.iter().skip(1).flatten().next();
            group.map_or(&caps[0], |m| m.as_str()).to_owned()
        })
        .filter(|name| !name.is_empty() && seen.insert(name.clone()))
        .collect()
}

fn tail(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().rev().take(TAIL_LINES).collect();
    lines.reverse();
    let tail = lines.join("\n");
    if tail.len() <= TAIL_BYTES {
        return tail;
    }
    let mut cut = tail.len() - TAIL_BYTES;
    while !tail.is_char_boundary(cut) {
        cut += 1;
    }
    tail[cut..].to_owned()
}

fn read_window(path: &Path) -> Result<String> {
    let read = || -> io::Result<String> {
        let mut file = File::open(path)?;
        let start = file.metadata()?.len().saturating_sub(SCAN_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(SCAN_BYTES).read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        Ok(match text.find('\n') {
            Some(newline) if start > 0 => text[newline + 1..].to_owned(),
            _ => text.into_owned(),
        })
    };
    read().with_context(|| format!("로그를 읽지 못했다: {}", path.display()))
}

fn run_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "r{:x}_{:x}_{:x}",
        EpochMs::now().0,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

enum Lock {
    Held(File),
    Cancelled,
}

fn acquire_lock(store: &RepoStore, cancel: &Cancel) -> Result<Lock> {
    fs::create_dir_all(store.dir())
        .with_context(|| format!("만들지 못했다: {}", store.dir().display()))?;
    let path = store.dir().join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("열지 못했다: {}", path.display()))?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Lock::Held(file)),
            Err(fs::TryLockError::WouldBlock) if cancel.is_cancelled() => {
                return Ok(Lock::Cancelled);
            }
            Err(fs::TryLockError::WouldBlock) => thread::sleep(LOCK_POLL),
            Err(fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("잠그지 못했다: {}", path.display()));
            }
        }
    }
}

fn terminate(child: &Child, grace: Duration) {
    let leader = Pid::from_raw(child.id() as i32);
    end_group(leader, grace);
    end_leader(leader);
}

fn end_group(group: Pid, grace: Duration) {
    let _ = killpg(group, Signal::SIGTERM);
    let deadline = Instant::now().checked_add(grace);
    while deadline.is_none_or(|deadline| Instant::now() < deadline) {
        reap_group(group);
        if group_is_gone(group) {
            return;
        }
        thread::sleep(GROUP_POLL);
    }
    let _ = killpg(group, Signal::SIGKILL);
    let deadline = Instant::now() + KILL_WAIT;
    loop {
        reap_group(group);
        if group_is_gone(group) || Instant::now() >= deadline {
            return;
        }
        thread::sleep(GROUP_POLL);
    }
}

/// A leader that moved to another group is out of reach of both `killpg` and `waitpid(-pgid)`.
/// It is still an unreaped child of this process, so its pid cannot have been reused.
fn end_leader(leader: Pid) {
    let flags = WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT;
    match waitid(Id::Pid(leader), flags) {
        Err(Errno::EINVAL) => {
            let _ = waitpid(leader, None);
        }
        Err(_) => {}
        Ok(WaitStatus::StillAlive) => {
            let _ = kill(leader, Signal::SIGKILL);
            let _ = waitpid(leader, None);
        }
        Ok(_) => {
            let _ = waitpid(leader, None);
        }
    }
}

/// A zombie keeps `killpg` succeeding, so the group counts as alive until its zombies are reaped.
fn reap_group(group: Pid) {
    while let Ok(status) = waitpid(Pid::from_raw(-group.as_raw()), Some(WaitPidFlag::WNOHANG)) {
        if status == WaitStatus::StillAlive {
            break;
        }
    }
}

fn group_is_gone(group: Pid) -> bool {
    killpg(group, None) == Err(Errno::ESRCH)
}
