//! The only place that runs `git`, `gh`, and `hc`.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
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

    /// The environment variable that replaces the executable.
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
    #[error("{tool} 실행 파일 {}을 띄우지 못했다: {source}", program.display())]
    Spawn {
        tool: Tool,
        program: PathBuf,
        source: io::Error,
    },
    #[error("`{call}`이 {}초 안에 끝나지 않아 멈췄다", after.as_secs_f32())]
    Timeout { call: String, after: Duration },
    #[error("`{call}`이 실패했다(종료 코드 {code:?}): {stderr}")]
    Failed {
        call: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("`{call}`의 출력을 읽지 못했다: {source}")]
    Io { call: String, source: io::Error },
}

#[derive(Clone, Debug)]
pub struct Output {
    /// The command line, for messages.
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

    /// The standard output of a successful call, or `ExtError::Failed`.
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

    /// Like `from_env`, reading variables through `var` so tests need not touch the process environment.
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
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|source| ExtError::Spawn {
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
        let left = || deadline.saturating_duration_since(Instant::now());
        let (stdout, stderr) = match (stdout.recv_timeout(left()), stderr.recv_timeout(left())) {
            (Ok(Ok(out)), Ok(Ok(err))) => (out, err),
            (Ok(Err(source)), _) | (_, Ok(Err(source))) => {
                return Err(ExtError::Io { call, source });
            }
            _ => return Err(time_out(&mut child, call, timeout)),
        };
        Ok(Output {
            code: status.code(),
            stdout,
            stderr,
            call,
        })
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = tx.send(pipe.read_to_end(&mut buf).map(|_| buf));
        });
    } else {
        let _ = tx.send(Ok(Vec::new()));
    }
    rx
}

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
    fn missing_executable_is_a_spawn_error() {
        let ext = Ext::from_vars(|k| (k == "CHUTE_HC").then(|| "/nonexistent/hc".into()));
        assert!(matches!(
            ext.hc(["ls"]),
            Err(ExtError::Spawn { tool: Tool::Hc, .. })
        ));
    }
}
