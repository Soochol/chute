#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chute::engine::{Engine, Timing};
use chute::ext::Ext;
use chute::model::{
    Agent, Approval, Batch, BatchId, BounceReason, EpochMs, EventKind, ItemState, Phase, PrNumber,
    QueueItem, RepoInfo, Sha, State, TerminalId,
};
use chute::queue::{self, Submission};
use chute::store::{Home, RepoStore};

use crate::support::{Reply, Sandbox, TestRepo};

pub const AGENT_CREATED_MS: u64 = 1763188200000;
pub const ORCH_CREATED_MS: u64 = 1763188300000;

pub struct Lab {
    pub sb: Sandbox,
    pub repo: TestRepo,
    pub store: RepoStore,
    pub info: RepoInfo,
    gh_dir: PathBuf,
    pub log: PathBuf,
}

const PRELUDE: &str = r#"unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
export GIT_CONFIG_GLOBAL='@GITCONFIG@' GIT_CONFIG_NOSYSTEM=1
LOG='@LOG@'
tree=$(git rev-parse 'HEAD^{tree}')
fail() { echo "$tree fail" >> "$LOG"; echo "$1"; exit 1; }
"#;

impl Lab {
    pub fn new(verify_body: &str) -> Lab {
        Lab::build(verify_body, "", 60)
    }

    pub fn build(verify_body: &str, top: &str, timeout_s: u64) -> Lab {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        let log = sb.root().join("verified.log");
        let gitconfig = sb.root().join("gitconfig");
        let script = sb.root().join("verify.sh");
        let status = format!(
            "env CHUTE_HOME='{}' HOME='{}' CHUTE_GIT='{}' '{}' status --json --repo '{}' > '{}'",
            sb.chute_home().display(),
            sb.home().display(),
            sb.root().join("bin/git").display(),
            env!("CARGO_BIN_EXE_chute"),
            repo.path().display(),
            sb.root().join("status-during.json").display(),
        );
        let verify_body = verify_body
            .replace("@STATUS@", &status)
            .replace("@ROOT@", &sb.root().to_string_lossy())
            .replace("@ORIGIN@", &repo.origin().to_string_lossy());
        let body = PRELUDE
            .replace("@GITCONFIG@", &gitconfig.to_string_lossy())
            .replace("@LOG@", &log.to_string_lossy())
            + &verify_body
            + "\necho \"$tree pass\" >> \"$LOG\"\n";
        fs::write(&script, body).unwrap();

        let mut git = String::new();
        for (key, value) in sb.envs() {
            let key = key.to_string_lossy();
            match value {
                Some(value) => {
                    git.push_str(&format!("export {key}='{}'\n", value.to_string_lossy()))
                }
                None => git.push_str(&format!("unset {key}\n")),
            }
        }
        git.push_str("exec git \"$@\"\n");
        sb.script("git", &git);

        let gh_dir = sb.root().join("gh-state");
        fs::create_dir_all(&gh_dir).unwrap();
        let gh = format!(
            r#"d='{gh}'
if [ "$1" = pr ] && [ "$2" = checks ]; then
  echo "$*" >> "$d/checks-calls"
  if [ -e "$d/$3.checks" ]; then cat "$d/$3.checks"; exit "$(cat "$d/$3.checks.code")"; fi
  echo "no checks reported on the 'pr-$3' branch" >&2; exit 1
fi
[ "$1" = pr ] && [ "$2" = view ] || {{ echo "fake gh: $*" >&2; exit 97; }}
n=$3
echo "$*" >> "$d/calls"
head=$(cat "$d/$n.head")
state=OPEN
[ -e "$d/$n.closed" ] && state=CLOSED
if git --git-dir='{origin}' merge-base --is-ancestor "$head" main 2>/dev/null; then state=MERGED; fi
[ -e "$d/$n.stuck" ] && state=OPEN
printf '{{"state":"%s","headRefOid":"%s","mergeCommit":null}}\n' "$state" "$head"
"#,
            gh = gh_dir.display(),
            origin = repo.origin().display()
        );
        sb.script("gh", &gh);

        let config = format!(
            "{top}\n[[verify]]\nname = \"check\"\nrun = \"sh {}\"\ntimeout = {timeout_s}\nfailed = 'FAILED (\\S+)'\n",
            script.display()
        );
        repo.commit_file("chute.toml", &config, "chore: chute 설정");
        repo.git(&["push", "--quiet", "origin", "main"]);

        let ext = Ext::from_vars(|k| sb.var(k));
        let info = chute::repo::discover(&ext, repo.path()).unwrap();
        let store = Home::at(sb.chute_home()).repo(&info.slug);
        store.register(&info).unwrap();
        let lab = Lab {
            sb,
            repo,
            store,
            info,
            gh_dir,
            log,
        };
        lab.hc_knows_the_agent();
        lab
    }

    pub fn engine(&self) -> Engine {
        self.engine_rechecking_ci_every(Duration::ZERO)
    }

    pub fn engine_rechecking_ci_every(&self, ci_recheck: Duration) -> Engine {
        let ext = Ext::from_vars(|k| self.sb.var(k));
        Engine::new(
            ext,
            Home::at(self.sb.chute_home()),
            self.info.clone(),
            self.sb.home().join(".herdr/worktrees"),
        )
        .with_timing(Timing {
            merged_check: Duration::ZERO,
            merged_poll: Duration::from_millis(10),
            ci_recheck,
        })
    }

    fn hc_knows_the_agent(&self) {
        let hc = self.sb.fake("hc");
        hc.respond("get t1rs --json", Reply::file("tests/fixtures/hc/get.json"));
        hc.respond("send t1rs *", Reply::ok(""));
        hc.respond(
            "get t1orch --json",
            Reply::ok(format!(
                r#"{{"address":"t1orch","name":"orch","state":"live","created_ms":{ORCH_CREATED_MS},"foreground":{{"name":"claude"}}}}"#
            )),
        );
        hc.respond("send t1orch *", Reply::ok(""));
    }

    pub fn orchestrator(&self) -> Agent {
        Agent {
            terminal: TerminalId("t1orch".into()),
            created_ms: EpochMs(ORCH_CREATED_MS),
            name: Some("orch".into()),
            workdir: self.sb.root().join("o"),
            alive: true,
            machine: None,
        }
    }

    pub fn lines_sent_to(&self, terminal: &str) -> Vec<String> {
        self.hc_calls("send")
            .into_iter()
            .filter(|c| c.get(1).is_some_and(|t| t == terminal))
            .map(|c| c[2].clone())
            .collect()
    }

    pub fn notified(&self) -> Vec<(chute::model::NotifyKind, chute::model::Delivered)> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                EventKind::Notified {
                    kind, delivered, ..
                } => Some((kind, delivered)),
                _ => None,
            })
            .collect()
    }

    pub fn agent(&self) -> Agent {
        Agent {
            terminal: TerminalId("t1rs".into()),
            created_ms: EpochMs(AGENT_CREATED_MS),
            name: Some("impl-a".into()),
            workdir: self.sb.root().join("w"),
            alive: true,
            machine: None,
        }
    }

    pub fn open_pr(&self, n: u64, files: &[(&str, &str)]) -> Sha {
        self.open_pr_for(n, files, Some(self.agent()), Approval::Agent)
    }

    pub fn open_pr_for(
        &self,
        n: u64,
        files: &[(&str, &str)],
        agent: Option<Agent>,
        approval: Approval,
    ) -> Sha {
        let head = self.push_pr_branch(n, files);
        self.queue_pr(n, &head, agent, approval);
        head
    }

    pub fn push_pr_branch(&self, n: u64, files: &[(&str, &str)]) -> Sha {
        let branch = format!("pr-{n}");
        self.repo.git(&["fetch", "--quiet", "origin"]);
        self.repo
            .git(&["checkout", "--quiet", "-B", &branch, "origin/main"]);
        for (path, content) in files {
            self.repo.write(path, content);
            self.repo.git(&["add", path]);
        }
        self.repo
            .git(&["commit", "--quiet", "-m", &format!("feat: change {n}")]);
        let head = self.repo.head();
        self.repo.git(&[
            "push",
            "--quiet",
            "origin",
            &format!("{head}:refs/heads/{branch}"),
            &format!("{head}:refs/pull/{n}/head"),
        ]);
        self.repo.git(&["checkout", "--quiet", "main"]);
        self.gh_says_head(n, &head);
        sha(&head)
    }

    pub fn open_pr_removing(&self, n: u64, path: &str) -> Sha {
        let branch = format!("pr-{n}");
        self.repo.git(&["fetch", "--quiet", "origin"]);
        self.repo
            .git(&["checkout", "--quiet", "-B", &branch, "origin/main"]);
        self.repo.git(&["rm", "--quiet", path]);
        self.repo
            .git(&["commit", "--quiet", "-m", &format!("feat: change {n}")]);
        let head = self.repo.head();
        self.repo.git(&[
            "push",
            "--quiet",
            "origin",
            &format!("{head}:refs/heads/{branch}"),
            &format!("{head}:refs/pull/{n}/head"),
        ]);
        self.repo.git(&["checkout", "--quiet", "main"]);
        self.gh_says_head(n, &head);
        let head = sha(&head);
        self.queue_pr(n, &head, Some(self.agent()), Approval::Agent);
        head
    }

    pub fn queue_pr(&self, n: u64, head: &Sha, agent: Option<Agent>, approval: Approval) {
        self.queue_pr_by(n, head, agent, None, approval);
    }

    pub fn queue_pr_by(
        &self,
        n: u64,
        head: &Sha,
        agent: Option<Agent>,
        submitter: Option<Agent>,
        approval: Approval,
    ) {
        let submission = Submission {
            pr: PrNumber(n),
            title: format!("feat: change {n}"),
            branch: format!("pr-{n}"),
            head: head.clone(),
            agent,
            submitter,
            touches_config: false,
            approval,
            priority: false,
            after: Vec::new(),
            now: EpochMs::now(),
        };
        self.store
            .update(|s| Ok(queue::submit(s, submission)?))
            .unwrap();
    }

    pub fn gh_says_head(&self, n: u64, head: &str) {
        fs::write(self.gh_dir.join(format!("{n}.head")), head).unwrap();
    }

    pub fn gh_checks(&self, n: u64, checks: &[(&str, &str)]) {
        let list: Vec<String> = checks
            .iter()
            .map(|(name, bucket)| {
                format!(r#"{{"name":"{name}","bucket":"{bucket}","link":"https://ci.example/{n}/{name}"}}"#)
            })
            .collect();
        let has = |buckets: &[&str]| checks.iter().any(|(_, b)| buckets.contains(b));
        let code = if has(&["fail", "cancel"]) {
            1
        } else if has(&["pending"]) {
            8
        } else {
            0
        };
        fs::write(
            self.gh_dir.join(format!("{n}.checks")),
            format!("[{}]\n", list.join(",")),
        )
        .unwrap();
        fs::write(
            self.gh_dir.join(format!("{n}.checks.code")),
            code.to_string(),
        )
        .unwrap();
    }

    pub fn gh_checks_error(&self, n: u64, code: i32) {
        fs::write(self.gh_dir.join(format!("{n}.checks")), "").unwrap();
        fs::write(
            self.gh_dir.join(format!("{n}.checks.code")),
            code.to_string(),
        )
        .unwrap();
    }

    pub fn gh_check_reads(&self) -> usize {
        fs::read_to_string(self.gh_dir.join("checks-calls"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    pub fn gh_never_shows_merged(&self, n: u64) {
        fs::write(self.gh_dir.join(format!("{n}.stuck")), "").unwrap();
    }

    pub fn gh_says_closed(&self, n: u64) {
        fs::write(self.gh_dir.join(format!("{n}.closed")), "").unwrap();
    }

    pub fn gh_calls(&self) -> usize {
        fs::read_to_string(self.gh_dir.join("calls"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    pub fn main_commit(&self, files: &[(&str, &str)], message: &str) -> String {
        self.repo.git(&["fetch", "--quiet", "origin"]);
        self.repo
            .git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
        for (path, content) in files {
            self.repo.write(path, content);
            self.repo.git(&["add", path]);
        }
        self.repo.git(&["commit", "--quiet", "-m", message]);
        self.repo.git(&["push", "--quiet", "origin", "main"]);
        self.repo.head()
    }

    pub fn remove_from_main(&self, path: &str, message: &str) -> String {
        self.repo.git(&["fetch", "--quiet", "origin"]);
        self.repo
            .git(&["checkout", "--quiet", "-B", "main", "origin/main"]);
        self.repo.git(&["rm", "--quiet", path]);
        self.repo.git(&["commit", "--quiet", "-m", message]);
        self.repo.git(&["push", "--quiet", "origin", "main"]);
        self.repo.head()
    }

    pub fn origin_git(&self, args: &[&str]) -> String {
        let out = self
            .sb
            .command("git")
            .arg("--git-dir")
            .arg(self.repo.origin())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    pub fn origin_main(&self) -> String {
        self.origin_git(&["rev-parse", "main"])
    }

    pub fn merge_commits_on_main(&self) -> Vec<String> {
        let text = self.origin_git(&[
            "rev-list",
            "--first-parent",
            "--merges",
            "--reverse",
            "main",
        ]);
        text.lines().map(str::to_owned).collect()
    }

    pub fn strand_in_batch(&self) {
        self.store
            .update(|s| {
                let taken = {
                    let due = s.queue.iter().map(|i| i.pr).collect();
                    queue::take_batch(s, 8, &due)
                };
                s.batch = Some(Batch {
                    id: BatchId("b-crashed".into()),
                    base: sha(&self.origin_main()),
                    prs: taken,
                    phase: Phase::Verifying,
                    step: Some("check".into()),
                    round: 1,
                    started_ms: EpochMs::now(),
                });
                Ok(())
            })
            .unwrap();
    }

    pub fn state(&self) -> State {
        self.store.load().unwrap()
    }

    pub fn item(&self, pr: u64) -> Option<QueueItem> {
        self.state()
            .queue
            .into_iter()
            .find(|i| i.pr == PrNumber(pr))
    }

    pub fn item_state(&self, pr: u64) -> Option<ItemState> {
        self.item(pr).map(|i| i.state)
    }

    pub fn events(&self) -> Vec<EventKind> {
        self.store
            .history()
            .unwrap()
            .events
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    pub fn bounced_events(&self) -> Vec<(u64, BounceReason)> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                EventKind::Bounced { pr, reason, .. } => Some((pr.0, reason)),
                _ => None,
            })
            .collect()
    }

    pub fn merged_prs(&self) -> Vec<u64> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                EventKind::Merged { pr, .. } => Some(pr.0),
                _ => None,
            })
            .collect()
    }

    pub fn hc_calls(&self, verb: &str) -> Vec<Vec<String>> {
        self.sb
            .fake("hc")
            .calls()
            .into_iter()
            .filter(|c| c.first().is_some_and(|v| v == verb))
            .collect()
    }

    pub fn batch_ids(&self) -> Vec<BatchId> {
        let dir = self.store.dir().join("batches");
        let mut ids: Vec<String> = fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        ids.into_iter().map(BatchId).collect()
    }

    fn verified_trees(&self) -> BTreeSet<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_suffix(" pass"))
            .map(str::to_owned)
            .collect()
    }

    pub fn verify_runs(&self) -> usize {
        fs::read_to_string(&self.log)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    pub fn assert_every_pushed_commit_was_verified(&self) -> usize {
        let verified = self.verified_trees();
        let mut checked = 0;
        for id in self.batch_ids() {
            let record = self.store.read_batch(&id).unwrap().unwrap();
            for tip in record.pushed {
                let tree = self.origin_git(&["rev-parse", &format!("{tip}^{{tree}}")]);
                assert!(
                    verified.contains(&tree),
                    "batch {} pushed {tip} whose tree {tree} never passed verification",
                    id.0
                );
                self.origin_git(&["merge-base", "--is-ancestor", tip.as_str(), "main"]);
                checked += 1;
            }
        }
        checked
    }
}

pub fn sha(text: &str) -> Sha {
    Sha::try_from(text.trim().to_owned()).unwrap()
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}
