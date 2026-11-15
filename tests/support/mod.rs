#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

pub struct Sandbox {
    dir: TempDir,
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox::new()
    }
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = tempfile::Builder::new()
            .prefix("chute-test-")
            .tempdir()
            .unwrap();
        let sb = Sandbox { dir };
        for sub in ["home", "chute-home", "xdg-config", "bin", "fakes"] {
            fs::create_dir_all(sb.root().join(sub)).unwrap();
        }
        fs::write(
            sb.gitconfig(),
            "[user]\n\tname = chute test\n\temail = test@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        sb.fake("gh");
        sb.fake("hc");
        sb
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn chute_home(&self) -> PathBuf {
        self.root().join("chute-home")
    }

    pub fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    fn gitconfig(&self) -> PathBuf {
        self.root().join("gitconfig")
    }

    pub fn envs(&self) -> Vec<(OsString, Option<OsString>)> {
        let set = |k: &str, v: PathBuf| (OsString::from(k), Some(v.into_os_string()));
        let mut envs = vec![
            set("HOME", self.home()),
            set("CHUTE_HOME", self.chute_home()),
            set("XDG_CONFIG_HOME", self.root().join("xdg-config")),
            set("GIT_CONFIG_GLOBAL", self.gitconfig()),
            (OsString::from("GIT_CONFIG_NOSYSTEM"), Some("1".into())),
            (OsString::from("XDG_STATE_HOME"), None),
            (OsString::from("HC_TERMINAL_ID"), None),
            (OsString::from("HERDR_APP_HOME"), None),
            (OsString::from("GIT_DIR"), None),
            (OsString::from("GIT_WORK_TREE"), None),
            (OsString::from("GIT_INDEX_FILE"), None),
        ];
        for (key, _) in std::env::vars_os() {
            let is_git = key.to_str().is_some_and(|k| k.starts_with("GIT_"));
            if is_git && !envs.iter().any(|(k, _)| *k == key) {
                envs.push((key, None));
            }
        }
        for (tool, var) in [("git", "CHUTE_GIT"), ("gh", "CHUTE_GH"), ("hc", "CHUTE_HC")] {
            let path = self.root().join("bin").join(tool);
            envs.push((var.into(), path.exists().then(|| path.into_os_string())));
        }
        envs
    }

    pub fn var(&self, key: &str) -> Option<OsString> {
        self.envs()
            .into_iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v)
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        for (k, v) in self.envs() {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        cmd
    }

    pub fn fake(&self, name: &str) -> FakeTool {
        let dir = self.root().join("fakes").join(name);
        let path = self.root().join("bin").join(name);
        if !path.exists() {
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("rules"), "").unwrap();
            let body = FAKE_SCRIPT
                .replace("@DIR@", &dir.to_string_lossy())
                .replace("@NAME@", name);
            write_executable(&path, &body);
        }
        FakeTool { dir, path }
    }

    pub fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root().join("bin").join(name);
        write_executable(&path, &format!("#!/bin/sh\n{body}"));
        path
    }
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

const FAKE_SCRIPT: &str = r#"#!/bin/sh
d='@DIR@'
us=$(printf '\037')
rec=''
for a in "$@"; do rec="$rec$a$us"; done
printf '%s\036' "$rec" >> "$d/calls"
args="$*"
tab=$(printf '\t')
while IFS="$tab" read -r code out err pattern; do
  case "$args" in
    $pattern)
      [ "$out" = - ] || cat "$d/$out"
      [ "$err" = - ] || cat "$d/$err" >&2
      exit "$code" ;;
  esac
done < "$d/rules"
printf 'fake @NAME@: 맞는 규칙이 없다: %s\n' "$args" >&2
exit 97
"#;

pub struct FakeTool {
    dir: PathBuf,
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Reply {
    code: i32,
    stdout: String,
    stderr: Option<String>,
}

impl Reply {
    pub fn ok(stdout: impl Into<String>) -> Reply {
        Reply {
            code: 0,
            stdout: stdout.into(),
            stderr: None,
        }
    }

    pub fn file(path: impl AsRef<Path>) -> Reply {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        Reply::ok(fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
    }

    pub fn code(mut self, code: i32) -> Reply {
        self.code = code;
        self
    }

    pub fn stderr(mut self, stderr: impl Into<String>) -> Reply {
        self.stderr = Some(stderr.into());
        self
    }
}

impl FakeTool {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn respond(&self, pattern: &str, reply: Reply) -> &FakeTool {
        assert!(
            !pattern.contains(['\t', '\n']),
            "규칙 패턴에 탭이나 줄바꿈을 쓸 수 없다"
        );
        let rules = self.dir.join("rules");
        let n = fs::read_to_string(&rules).unwrap().lines().count();
        let out = format!("r{n}.out");
        fs::write(self.dir.join(&out), &reply.stdout).unwrap();
        let err = match &reply.stderr {
            Some(text) => {
                let name = format!("r{n}.err");
                fs::write(self.dir.join(&name), text).unwrap();
                name
            }
            None => "-".to_owned(),
        };
        let mut text = fs::read_to_string(&rules).unwrap();
        text.push_str(&format!("{}\t{out}\t{err}\t{pattern}\n", reply.code));
        fs::write(rules, text).unwrap();
        self
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        let Ok(text) = fs::read_to_string(self.dir.join("calls")) else {
            return Vec::new();
        };
        let mut records: Vec<&str> = text.split('\x1e').collect();
        records.pop();
        records
            .into_iter()
            .map(|rec| {
                let mut args: Vec<String> = rec.split('\x1f').map(str::to_owned).collect();
                args.pop();
                args
            })
            .collect()
    }
}

pub struct TestRepo {
    work: PathBuf,
    origin: PathBuf,
    envs: Vec<(OsString, Option<OsString>)>,
}

impl TestRepo {
    pub fn new(sb: &Sandbox, slug: &str) -> TestRepo {
        let (owner, name) = slug.split_once('/').expect("owner/name");
        let origin = sb
            .root()
            .join("remotes")
            .join(owner)
            .join(format!("{name}.git"));
        let work = sb.root().join("work").join(format!("{owner}-{name}"));
        fs::create_dir_all(&origin).unwrap();
        fs::create_dir_all(work.parent().unwrap()).unwrap();
        let repo = TestRepo {
            work,
            origin,
            envs: sb.envs(),
        };
        repo.run_git(
            &repo.origin,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        repo.run_git(
            sb.root(),
            &[
                "clone",
                "--quiet",
                &repo.origin.to_string_lossy(),
                &repo.work.to_string_lossy(),
            ],
        );
        repo.commit_file("README.md", "test\n", "chore: 저장소 초기화");
        repo.git(&["push", "--quiet", "-u", "origin", "main"]);
        repo
    }

    pub fn path(&self) -> &Path {
        &self.work
    }

    pub fn origin(&self) -> &Path {
        &self.origin
    }

    fn run_git(&self, cwd: &Path, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        for (k, v) in &self.envs {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        let out = cmd.current_dir(cwd).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} 실패: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    pub fn git(&self, args: &[&str]) -> String {
        self.run_git(&self.work, args)
    }

    pub fn write(&self, path: &str, content: &str) {
        let full = self.work.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    pub fn commit_file(&self, path: &str, content: &str, message: &str) -> String {
        self.write(path, content);
        self.git(&["add", path]);
        self.git(&["commit", "--quiet", "-m", message]);
        self.head()
    }

    pub fn pr_branch(&self, branch: &str, path: &str, content: &str) -> String {
        self.git(&["checkout", "--quiet", "-b", branch, "main"]);
        let head = self.commit_file(path, content, &format!("feat: {branch}"));
        self.git(&["push", "--quiet", "origin", branch]);
        self.git(&["checkout", "--quiet", "main"]);
        head
    }
}
