use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Git,
    Gh,
    Hc,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Git, Tool::Gh, Tool::Hc];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Git => "git",
            Tool::Gh => "gh",
            Tool::Hc => "hc",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Tool::Git => "CHUTE_GIT",
            Tool::Gh => "CHUTE_GH",
            Tool::Hc => "CHUTE_HC",
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    #[error("{tool} 실행 파일을 띄우지 못했다: {}", program.display())]
    Spawn {
        tool: Tool,
        program: PathBuf,
        source: io::Error,
    },
    #[error("`{call}`을 실행할 폴더가 없다: {}", dir.display())]
    NoDir { call: String, dir: PathBuf },
    #[error("`{call}`이 {}초 안에 끝나지 않아 멈췄다", after.as_secs_f32())]
    Timeout { call: String, after: Duration },
    #[error("`{call}`이 실패했다(종료 코드 {code:?}): {stderr}")]
    Failed {
        call: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("`{call}`의 출력이 {limit}바이트를 넘어 멈췄다")]
    TooMuchOutput { call: String, limit: usize },
    #[error("`{call}`의 출력을 읽지 못했다")]
    Io { call: String, source: io::Error },
}

#[derive(Clone, Debug)]
pub struct Output {
    pub call: String,
    /// `None` when a signal ended the process.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    pub fn ok(self) -> Result<String, ExtError> {
        if self.success() {
            Ok(self.stdout_text())
        } else {
            Err(ExtError::Failed {
                stderr: self.stderr_text().trim().to_owned(),
                call: self.call,
                code: self.code,
            })
        }
    }
}

#[derive(Clone, Debug)]
pub struct Ext {
    git: PathBuf,
    gh: PathBuf,
    hc: PathBuf,
    timeout: Duration,
}

impl Ext {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

    pub fn from_env() -> Ext {
        Ext::from_vars(|key| std::env::var_os(key))
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Ext {
        let program = |tool: Tool| {
            var(tool.env_var())
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(tool.name()))
        };
        Ext {
            git: program(Tool::Git),
            gh: program(Tool::Gh),
            hc: program(Tool::Hc),
            timeout: Ext::DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Ext {
        self.timeout = timeout;
        self
    }

    pub fn program(&self, tool: Tool) -> &Path {
        match tool {
            Tool::Git => &self.git,
            Tool::Gh => &self.gh,
            Tool::Hc => &self.hc,
        }
    }

    pub fn git<I, S>(&self, cwd: &Path, args: I) -> Result<Output, ExtError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.call(Tool::Git, Some(cwd), args, self.timeout)
    }

    pub fn gh<I, S>(&self, cwd: &Path, args: I) -> Result<Output, ExtError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.call(Tool::Gh, Some(cwd), args, self.timeout)
    }

    pub fn hc<I, S>(&self, args: I) -> Result<Output, ExtError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.call(Tool::Hc, None, args, self.timeout)
    }

    /// Runs one call in its own process group and kills the whole group when `timeout` passes.
    /// A non-zero exit is not an error here; callers read `Output::code`.
    pub fn call<I, S>(
        &self,
        tool: Tool,
        cwd: Option<&Path>,
        args: I,
        timeout: Duration,
    ) -> Result<Output, ExtError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        let call = std::iter::once(tool.name().into())
            .chain(args.iter().map(|a| a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        let program = self.program(tool);
        let mut command = Command::new(program);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if tool == Tool::Git {
            for var in REPO_LOCATING_ENV {
                command.env_remove(var);
            }
        }
        if let Some(cwd) = cwd {
            if !cwd.is_dir() {
                return Err(ExtError::NoDir {
                    call,
                    dir: cwd.to_owned(),
                });
            }
            command.current_dir(cwd);
        }
        let mut child = spawn(&mut command).map_err(|source| ExtError::Spawn {
            tool,
            program: program.to_owned(),
            source,
        })?;
        let deadline = Instant::now() + timeout;
        let io_error = |call: String| move |source| ExtError::Io { call, source };
        let mut pipes = [
            Pipe::new(child.stdout.take().map(OwnedFd::from)).map_err(io_error(call.clone()))?,
            Pipe::new(child.stderr.take().map(OwnedFd::from)).map_err(io_error(call.clone()))?,
        ];
        let mut exited = None;
        let mut grace_end = deadline;
        let mut pause = Duration::from_millis(1);
        loop {
            if exited.is_none() {
                exited = child.try_wait().map_err(io_error(call.clone()))?;
                if exited.is_some() {
                    grace_end = Instant::now() + PIPE_GRACE;
                }
            }
            let now = Instant::now();
            let limit = match exited {
                Some(_) if pipes.iter().all(Pipe::is_closed) || now >= grace_end => break,
                Some(_) => grace_end,
                None if now >= deadline => return Err(time_out(&mut child, call, timeout)),
                None => deadline,
            };
            wait_readable(&pipes, pause.min(limit - now)).map_err(io_error(call.clone()))?;
            for pipe in &mut pipes {
                pipe.read_available().map_err(io_error(call.clone()))?;
            }
            if pipes.iter().any(|p| p.overflowed) {
                if exited.is_none() {
                    kill_group(&mut child);
                }
                return Err(ExtError::TooMuchOutput {
                    call,
                    limit: MAX_OUTPUT,
                });
            }
            pause = (pause * 2).min(Duration::from_millis(20));
        }
        let [stdout, stderr] = pipes.map(|p| p.bytes);
        Ok(Output {
            code: exited.and_then(|s| s.code()),
            stdout,
            stderr,
            call,
        })
    }
}

/// What git exports to the hooks and `!` aliases it runs to say which repository it is in.
/// Inherited, they point every git call at that repository instead of the call's own `cwd`.
/// This is `git rev-parse --local-env-vars` without `GIT_CONFIG`, `GIT_CONFIG_PARAMETERS` and
/// `GIT_CONFIG_COUNT`.
const REPO_LOCATING_ENV: [&str; 12] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// How long to wait for end-of-file after the process exits. A background grandchild may
/// keep the pipe open forever; what the process itself wrote is read well within this.
/// The pipes close when the call returns, so nothing keeps reading after that.
const PIPE_GRACE: Duration = Duration::from_millis(500);

/// Per pipe. Past this the group is killed instead of growing memory without bound.
const MAX_OUTPUT: usize = 256 * 1024 * 1024;

/// Reads per pipe per turn, so the deadline is checked even while a writer never pauses.
const CHUNKS_PER_TURN: usize = 16;

struct Pipe {
    file: Option<File>,
    bytes: Vec<u8>,
    overflowed: bool,
}

impl Pipe {
    fn new(fd: Option<OwnedFd>) -> io::Result<Pipe> {
        let file = fd.map(File::from);
        if let Some(file) = &file {
            fcntl(file, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
        }
        Ok(Pipe {
            file,
            bytes: Vec::new(),
            overflowed: false,
        })
    }

    fn is_closed(&self) -> bool {
        self.file.is_none()
    }

    fn read_available(&mut self) -> io::Result<()> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };
        let mut chunk = [0u8; 64 * 1024];
        for _ in 0..CHUNKS_PER_TURN {
            match file.read(&mut chunk) {
                Ok(0) => {
                    self.file = None;
                    return Ok(());
                }
                Ok(n) => {
                    let room = MAX_OUTPUT - self.bytes.len();
                    let take = n.min(room);
                    let need = self.bytes.len() + take;
                    if need > self.bytes.capacity() {
                        let grown = (self.bytes.capacity() * 2).max(need).min(MAX_OUTPUT);
                        self.bytes.reserve_exact(grown - self.bytes.len());
                    }
                    self.bytes.extend_from_slice(&chunk[..take]);
                    if n > room {
                        self.overflowed = true;
                        return Ok(());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

fn wait_readable(pipes: &[Pipe], wait: Duration) -> io::Result<()> {
    let mut fds: Vec<PollFd> = pipes
        .iter()
        .filter_map(|p| p.file.as_ref())
        .map(|f| PollFd::new(f.as_fd(), PollFlags::POLLIN))
        .collect();
    if fds.is_empty() {
        thread::sleep(wait);
        return Ok(());
    }
    match poll(
        &mut fds,
        PollTimeout::try_from(wait).unwrap_or(PollTimeout::MAX),
    ) {
        Ok(_) | Err(Errno::EINTR) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Retries `ETXTBSY`: a script written a moment ago can still be open for writing in a
/// child that another thread forked and has not yet exec'd.
fn spawn(command: &mut Command) -> io::Result<Child> {
    for _ in 1..SPAWN_TRIES {
        match command.spawn() {
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy => {
                thread::sleep(Duration::from_millis(10))
            }
            result => return result,
        }
    }
    command.spawn()
}

const SPAWN_TRIES: u32 = 50;

fn time_out(child: &mut Child, call: String, after: Duration) -> ExtError {
    kill_group(child);
    ExtError::Timeout { call, after }
}

/// Only for a child not yet reaped: its pid is then still the group id and cannot have
/// been reused. The child itself is killed too, in case it left the group.
fn kill_group(child: &mut Child) {
    let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Reply, Sandbox};

    #[test]
    fn env_vars_choose_the_executables() {
        let sb = Sandbox::new();
        let git = sb.fake("git");
        git.respond("--version", Reply::ok("fake git\n"));
        sb.fake("gh").respond("repo view *", Reply::ok("{}"));
        sb.fake("hc").respond("get t1 *", Reply::ok("{}"));
        let ext = Ext::from_vars(|k| sb.var(k));

        assert_eq!(
            ext.git(sb.root(), ["--version"]).unwrap().ok().unwrap(),
            "fake git\n"
        );
        ext.gh(sb.root(), ["repo", "view", "--json", "name"])
            .unwrap()
            .ok()
            .unwrap();
        ext.hc(["get", "t1", "--json"]).unwrap().ok().unwrap();

        assert_eq!(git.calls(), [["--version"]]);
        assert_eq!(sb.fake("gh").calls(), [["repo", "view", "--json", "name"]]);
        assert_eq!(sb.fake("hc").calls(), [["get", "t1", "--json"]]);
    }

    #[test]
    fn unset_vars_fall_back_to_path_names() {
        assert_eq!(Ext::DEFAULT_TIMEOUT, Duration::from_secs(300));
        let ext = Ext::from_vars(|_| None);
        for tool in Tool::ALL {
            assert_eq!(ext.program(tool), Path::new(tool.name()));
        }
    }

    #[test]
    fn returns_code_stdout_and_stderr() {
        let sb = Sandbox::new();
        sb.fake("hc")
            .respond("send *", Reply::ok("out").code(9).stderr("{\"error\":{}}"));
        let out = Ext::from_vars(|k| sb.var(k))
            .hc(["send", "t1", "hi"])
            .unwrap();
        assert_eq!(out.code, Some(9));
        assert_eq!(out.stdout_text(), "out");
        assert_eq!(out.stderr_text(), "{\"error\":{}}");
        assert!(matches!(
            out.ok(),
            Err(ExtError::Failed { code: Some(9), .. })
        ));
    }

    #[test]
    fn timeout_kills_the_process_group() {
        let sb = Sandbox::new();
        let slow = sb.script("slow-gh", "sleep 30 & sleep 30\n");
        let ext = Ext::from_vars(|k| (k == "CHUTE_GH").then(|| slow.clone().into()));
        let started = Instant::now();
        let err = ext
            .call(Tool::Gh, None, ["pr", "view"], Duration::from_millis(200))
            .unwrap_err();
        assert!(matches!(err, ExtError::Timeout { .. }), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_background_grandchild_does_not_turn_success_into_a_timeout() {
        let sb = Sandbox::new();
        let hc = sb.script("bg-hc", "echo started\nsleep 5 &\nexit 0\n");
        let ext = Ext::from_vars(|k| (k == "CHUTE_HC").then(|| hc.clone().into()));
        let started = Instant::now();
        let out = ext
            .call(Tool::Hc, None, ["run", "--bg"], Duration::from_secs(3))
            .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout_text(), "started\n");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn wait_for(path: &Path) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if path.exists() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The grandchild writes after the call returned. The write fails only if chute closed its end.
    fn late_writer(mark: &Path) -> String {
        format!(
            "sh -c \"trap '' PIPE; sleep 1; echo late || touch '{}'\"",
            mark.display()
        )
    }

    #[test]
    fn pipes_close_when_the_call_returns() {
        let sb = Sandbox::new();
        let mark = sb.root().join("pipe-closed");
        let hc = sb.script(
            "late-hc",
            &format!("echo started\n{} &\nexit 0\n", late_writer(&mark)),
        );
        let ext = Ext::from_vars(|k| (k == "CHUTE_HC").then(|| hc.clone().into()));
        let out = ext
            .call(Tool::Hc, None, ["run"], Duration::from_secs(5))
            .unwrap();
        assert_eq!(out.stdout_text(), "started\n");
        assert!(wait_for(&mark), "호출이 끝난 뒤에도 파이프가 열려 있다");
    }

    #[test]
    fn pipes_close_after_a_timeout_even_for_an_escaped_grandchild() {
        let sb = Sandbox::new();
        let mark = sb.root().join("pipe-closed");
        let gh = sb.script(
            "escape-gh",
            &format!("setsid {} &\nsleep 30\n", late_writer(&mark)),
        );
        let ext = Ext::from_vars(|k| (k == "CHUTE_GH").then(|| gh.clone().into()));
        let err = ext
            .call(Tool::Gh, None, ["pr", "view"], Duration::from_millis(200))
            .unwrap_err();
        assert!(matches!(err, ExtError::Timeout { .. }), "{err}");
        assert!(wait_for(&mark), "시간 초과 뒤에도 파이프가 열려 있다");
    }

    #[test]
    fn writers_that_never_pause_still_time_out() {
        let sb = Sandbox::new();
        let gh = sb.script("flood-gh", "cat /dev/zero & cat /dev/zero & wait\n");
        let ext = Ext::from_vars(|k| (k == "CHUTE_GH").then(|| gh.clone().into()));
        let started = Instant::now();
        let err = ext
            .call(Tool::Gh, None, ["api"], Duration::from_millis(200))
            .unwrap_err();
        assert!(
            matches!(
                err,
                ExtError::Timeout { .. } | ExtError::TooMuchOutput { .. }
            ),
            "{err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_child_that_leaves_its_group_is_still_killed_on_timeout() {
        let sb = Sandbox::new();
        let gh = sb.script(
            "leave-gh",
            "exec python3 -I -c 'import os,time; os.setpgid(0, os.getpgid(os.getppid())); time.sleep(30)'\n",
        );
        let ext = Ext::from_vars(|k| (k == "CHUTE_GH").then(|| gh.clone().into()));
        let started = Instant::now();
        let err = ext
            .call(Tool::Gh, None, ["api"], Duration::from_millis(300))
            .unwrap_err();
        assert!(matches!(err, ExtError::Timeout { .. }), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_just_under_the_limit_is_kept_without_doubling_the_buffer() {
        let sb = Sandbox::new();
        let total = MAX_OUTPUT - 1000;
        let gh = sb.script(
            "seed-gh",
            &format!(
                "head -c 65535 /dev/zero\nsleep 0.2\nhead -c {} /dev/zero\n",
                total - 65535
            ),
        );
        let ext = Ext::from_vars(|k| (k == "CHUTE_GH").then(|| gh.clone().into()));
        let out = ext.gh(sb.root(), ["api"]).unwrap();
        assert_eq!(out.stdout.len(), total);
        assert!(
            out.stdout.capacity() <= MAX_OUTPUT,
            "{}",
            out.stdout.capacity()
        );
    }

    #[test]
    fn endless_output_stops_at_the_limit() {
        let sb = Sandbox::new();
        let gh = sb.script("endless-gh", "exec cat /dev/zero\n");
        let ext = Ext::from_vars(|k| (k == "CHUTE_GH").then(|| gh.clone().into()));
        let err = ext
            .call(Tool::Gh, None, ["api"], Duration::from_secs(60))
            .unwrap_err();
        assert!(matches!(err, ExtError::TooMuchOutput { .. }), "{err}");
    }

    #[test]
    fn large_output_on_both_pipes_is_read_whole() {
        let sb = Sandbox::new();
        let gh = sb.script(
            "loud-gh",
            "head -c 3000000 /dev/zero; head -c 3000000 /dev/zero >&2\n",
        );
        let ext = Ext::from_vars(|k| (k == "CHUTE_GH").then(|| gh.clone().into()));
        let out = ext.gh(sb.root(), ["api"]).unwrap();
        assert_eq!((out.stdout.len(), out.stderr.len()), (3_000_000, 3_000_000));
    }

    #[test]
    fn a_missing_work_folder_is_named_as_such() {
        let sb = Sandbox::new();
        let missing = sb.root().join("gone");
        let err = Ext::from_vars(|_| None)
            .git(&missing, ["status"])
            .unwrap_err();
        assert!(
            matches!(&err, ExtError::NoDir { dir, .. } if *dir == missing),
            "{err}"
        );
    }

    #[test]
    fn missing_executable_is_a_spawn_error() {
        let ext = Ext::from_vars(|k| (k == "CHUTE_HC").then(|| "/nonexistent/hc".into()));
        assert!(matches!(
            ext.hc(["ls"]),
            Err(ExtError::Spawn { tool: Tool::Hc, .. })
        ));
    }
}
