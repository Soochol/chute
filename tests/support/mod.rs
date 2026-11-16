#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

pub const SANDBOX_MACHINE_ID: &str = "0000000000000000000000000000000a";

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
        sb.fake("hc")
            .respond(
                "server status --json",
                Reply::ok(format!(
                    r#"{{"core":{{"id":"{SANDBOX_MACHINE_ID}","name":"sandbox-pc"}}}}"#
                )),
            )
            .respond("machine ls --json", Reply::ok(r#"{"machines":[]}"#));
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

    pub fn clear_rules(&self) -> &FakeTool {
        fs::write(self.dir.join("rules"), "").unwrap();
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

    pub fn clone_of(sb: &Sandbox, other: &TestRepo) -> TestRepo {
        let work = sb
            .root()
            .join("work")
            .join(other.work.file_name().expect("work folder name"));
        fs::create_dir_all(work.parent().unwrap()).unwrap();
        let repo = TestRepo {
            work,
            origin: other.origin.clone(),
            envs: sb.envs(),
        };
        repo.run_git(
            sb.root(),
            &[
                "clone",
                "--quiet",
                &repo.origin.to_string_lossy(),
                &repo.work.to_string_lossy(),
            ],
        );
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

pub struct Net {
    pub nodes: Vec<Node>,
}

pub struct Node {
    pub name: String,
    pub id: String,
    pub sb: Sandbox,
    pub repo: TestRepo,
}

const NET_HC: &str = r#"#!/bin/sh
d='@DIR@'
fail() { printf '{"error":{"hint":"","kind":"%s","message":"%s"}}\n' "$1" "$2" >&2; exit "$3"; }
case "$1 $2" in
  "server status")
    printf '{"core":{"id":"%s","name":"%s"},"live":1}\n' "$(cat "$d/id")" "$(cat "$d/name")"; exit 0 ;;
  "machine ls")
    printf '{"machines":['
    sep=''
    tab=$(printf '\t')
    while IFS="$tab" read -r alias id host state launcher; do
      [ -n "$alias" ] || continue
      printf '%s{"id":"%s","name":"%s","host":"%s","state":"%s"}' "$sep" "$id" "$alias" "$host" "$state"
      sep=','
    done < "$d/peers"
    printf ']}\n'; exit 0 ;;
  "exec "*) ;;
  *) exec '@RULES@' "$@" ;;
esac
shift
on=''; secs=60; feed=0; unset CHUTE_FORWARDED
while [ $# -gt 0 ]; do
  case "$1" in
    --json) shift ;;
    --on) on=$2; shift 2 ;;
    --timeout) secs=$2; shift 2 ;;
    --cwd) shift 2 ;;
    --env) export "$2"; shift 2 ;;
    -i|--stdin) feed=1; shift ;;
    --) shift; break ;;
    *) break ;;
  esac
done
tab=$(printf '\t')
found=''
while IFS="$tab" read -r alias id host state launcher; do
  [ "$alias" = "$on" ] && { found=1; break; }
done < "$d/peers"
[ -n "$found" ] || fail machine_unknown "unknown machine \\\"$on\\\"" 4
case "$state" in
  connected) ;;
  needs_login) fail needs_login "machine $on needs login" 5 ;;
  *) fail unreachable "machine $on is $state" 5 ;;
esac
[ "$1" = chute ] && [ -x "$launcher" ] || fail spawn_failed "cannot start $1: No such file or directory (os error 2)" 1
shift
out=$(mktemp); err=$(mktemp)
if [ "$feed" = 1 ]; then in=/dev/stdin; else in=/dev/null; fi
timeout "$secs" "$launcher" "$@" < "$in" > "$out" 2> "$err"
code=$?
timed_out=false
truncated=false
[ -e "$d/truncate" ] && truncated=true
if [ "$code" = 124 ]; then timed_out=true; code=null; fi
printf '{"code":%s,"ms":1,"signal":null,"stderr":"%s","stdout":"%s","timed_out":%s,"truncated":%s}\n' \
  "$code" "$(base64 < "$err" | tr -d '\n')" "$(base64 < "$out" | tr -d '\n')" "$timed_out" "$truncated"
rm -f "$out" "$err"
"#;

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

impl Net {
    pub fn new(slug: &str, names: &[&str], chute: &str) -> Net {
        let mut nodes: Vec<Node> = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let sb = Sandbox::new();
            let repo = match nodes.first() {
                None => TestRepo::new(&sb, slug),
                Some(first) => TestRepo::clone_of(&sb, &first.repo),
            };
            let node = Node {
                name: (*name).to_owned(),
                id: format!("{:032x}", 0xa0 + i),
                sb,
                repo,
            };
            node.install_hc();
            node.set_chute(chute);
            nodes.push(node);
        }
        Net { nodes }
    }

    pub fn node(&self, name: &str) -> &Node {
        self.nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("{name}라는 머신이 없다"))
    }

    pub fn connect(&self, from: &str, to: &str, alias: &str) {
        self.node(from).add_peer(self.node(to), alias, "connected");
    }

    pub fn connect_all(&self) {
        for from in &self.nodes {
            for to in &self.nodes {
                if from.name != to.name {
                    from.add_peer(to, &to.name, "connected");
                }
            }
        }
    }

    pub fn set_state(&self, from: &str, alias: &str, state: &str) {
        self.node(from).set_peer_state(alias, state);
    }
}

impl Node {
    fn net_dir(&self) -> PathBuf {
        self.sb.root().join("net")
    }

    fn install_hc(&self) {
        let dir = self.net_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("id"), &self.id).unwrap();
        fs::write(dir.join("name"), &self.name).unwrap();
        fs::write(dir.join("peers"), "").unwrap();
        let rules = self.sb.root().join("bin").join("hc-rules");
        fs::rename(self.sb.root().join("bin").join("hc"), &rules).unwrap();
        let body = NET_HC
            .replace("@DIR@", &dir.to_string_lossy())
            .replace("@RULES@", &rules.to_string_lossy());
        write_executable(&self.sb.root().join("bin").join("hc"), &body);
        self.sb.fake("hc").clear_rules();
    }

    /// Makes the fake `hc exec` report that it cut the output short.
    pub fn truncate_exec_output(&self) {
        fs::write(self.net_dir().join("truncate"), "").unwrap();
    }

    pub fn set_chute(&self, chute: &str) {
        let mut body = String::from("#!/bin/sh\n");
        for (key, value) in self.sb.envs() {
            let key = key.to_string_lossy();
            match value {
                Some(value) => {
                    body.push_str(&format!(
                        "export {key}={}\n",
                        shell_quote(&value.to_string_lossy())
                    ));
                }
                None => body.push_str(&format!("unset {key}\n")),
            }
        }
        body.push_str(&format!(
            "cd {}\nexec {} \"$@\"\n",
            shell_quote(&self.sb.home().to_string_lossy()),
            shell_quote(chute)
        ));
        write_executable(&self.net_dir().join("chute"), &body);
    }

    fn peers_path(&self) -> PathBuf {
        self.net_dir().join("peers")
    }

    fn add_peer(&self, to: &Node, alias: &str, state: &str) {
        let mut peers = fs::read_to_string(self.peers_path()).unwrap();
        peers.push_str(&format!(
            "{alias}\t{}\t{}\t{state}\t{}\n",
            to.id,
            to.name,
            to.net_dir().join("chute").display()
        ));
        fs::write(self.peers_path(), peers).unwrap();
    }

    fn set_peer_state(&self, alias: &str, state: &str) {
        let text = fs::read_to_string(self.peers_path()).unwrap();
        let mut changed = false;
        let lines: Vec<String> = text
            .lines()
            .map(|line| {
                let mut f: Vec<&str> = line.split('\t').collect();
                if f[0] == alias {
                    f[3] = state;
                    changed = true;
                }
                f.join("\t")
            })
            .collect();
        assert!(changed, "{alias}라는 이웃이 없다");
        fs::write(self.peers_path(), lines.join("\n") + "\n").unwrap();
    }

    pub fn chute(&self, bin: &str, args: &[&str]) -> std::process::Output {
        self.sb
            .command(bin)
            .current_dir(self.repo.path())
            .args(args)
            .output()
            .unwrap()
    }
}
