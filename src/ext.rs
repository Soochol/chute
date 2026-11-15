use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

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
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());

        let status = match wait_until(&mut child, deadline) {
            Ok(Some(status)) => status,
            Ok(None) => return Err(time_out(&mut child, call, timeout)),
            Err(source) => return Err(ExtError::Io { call, source }),
        };
        let until = Instant::now() + PIPE_GRACE;
        let (stdout, stderr) = match (stdout.finish(until), stderr.finish(until)) {
            (Ok(out), Ok(err)) => (out, err),
            (Err(source), _) | (_, Err(source)) => return Err(ExtError::Io { call, source }),
        };
        Ok(Output {
            code: status.code(),
            stdout,
            stderr,
            call,
        })
    }
}

/// How long to wait for end-of-file after the process exits. A background grandchild may
/// keep the pipe open forever; what the process itself wrote is read well within this.
const PIPE_GRACE: Duration = Duration::from_millis(500);

struct Drain {
    bytes: Arc<Mutex<Option<Vec<u8>>>>,
    done: mpsc::Receiver<io::Result<()>>,
}

impl Drain {
    /// Takes what was read. A reader still blocked on a grandchild's pipe stops at its next
    /// read and closes the pipe, so nothing keeps buffering after the call returns.
    fn finish(self, until: Instant) -> io::Result<Vec<u8>> {
        let wait = until.saturating_duration_since(Instant::now());
        if let Ok(Err(e)) = self.done.recv_timeout(wait) {
            return Err(e);
        }
        let taken = self.bytes.lock().unwrap_or_else(|e| e.into_inner()).take();
        Ok(taken.unwrap_or_default())
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> Drain {
    let bytes = Arc::new(Mutex::new(Some(Vec::new())));
    let (tx, done) = mpsc::channel();
    match pipe {
        Some(mut pipe) => {
            let bytes = Arc::clone(&bytes);
            thread::spawn(move || {
                let mut chunk = [0u8; 64 * 1024];
                let result = loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) => break Ok(()),
                        Ok(n) => match bytes.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                            Some(buf) => buf.extend_from_slice(&chunk[..n]),
                            None => break Ok(()),
                        },
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => break Err(e),
                    }
                };
                let _ = tx.send(result);
            });
        }
        None => {
            let _ = tx.send(Ok(()));
        }
    }
    Drain { bytes, done }
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

fn wait_until(
    child: &mut Child,
    deadline: Instant,
) -> io::Result<Option<std::process::ExitStatus>> {
    let mut pause = Duration::from_millis(1);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

fn time_out(child: &mut Child, call: String, after: Duration) -> ExtError {
    let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
    let _ = child.wait();
    ExtError::Timeout { call, after }
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

    #[test]
    fn a_chatty_grandchild_stops_being_buffered_after_return() {
        let sb = Sandbox::new();
        let hc = sb.script("yes-hc", "yes xxxxxxxxxxxxxxxx &\necho started\nexit 0\n");
        let ext = Ext::from_vars(|k| (k == "CHUTE_HC").then(|| hc.clone().into()));
        let out = ext
            .call(Tool::Hc, None, ["run"], Duration::from_secs(3))
            .unwrap();
        assert!(out.stdout_text().contains("started\n"));
        let rss = || {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.trim().trim_end_matches(" kB").parse::<u64>().ok())
                .unwrap()
        };
        drop(out);
        let before = rss();
        thread::sleep(Duration::from_millis(500));
        assert!(rss() < before + 100_000, "호출이 끝난 뒤에도 출력이 쌓인다");
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
