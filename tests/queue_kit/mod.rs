#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Barrier;
use std::thread;

use chute::model::{EpochMs, ItemState, PrNumber, QueueItem, Sha, Slug, State};
use chute::store::{Home, RepoStore};
use serde_json::Value;

use crate::support::{Sandbox, TestRepo};

pub struct World {
    pub sb: Sandbox,
    pub repo: TestRepo,
}

impl World {
    pub fn new() -> World {
        World::with_config(Some(chute::config::draft("true")))
    }

    pub fn human() -> World {
        let draft = chute::config::draft("true");
        World::with_config(Some(
            draft.replace("approval = \"agent\"", "approval = \"human\""),
        ))
    }

    /// `config` is committed and pushed to the default branch. `None` leaves it out.
    pub fn with_config(config: Option<String>) -> World {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        if let Some(text) = config {
            repo.commit_file(chute::config::FILE_NAME, &text, "chore: chute 설정");
            repo.git(&["push", "--quiet", "origin", "main"]);
        }
        let world = World { sb, repo };
        world.ok(world.repo.path(), &["init"]);
        world
    }

    pub fn chute(&self, cwd: &Path, args: &[&str]) -> Output {
        self.chute_with_env(cwd, args, &[])
    }

    pub fn chute_with_env(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = self.sb.command(env!("CARGO_BIN_EXE_chute"));
        command.current_dir(cwd).args(args);
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    pub fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.chute(cwd, args);
        assert!(
            out.status.success(),
            "chute {args:?}: 종료 코드 {:?}\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs every command at the same moment, each as its own process, and returns the
    /// outputs in the order given.
    pub fn run_parallel(&self, commands: &[Vec<String>]) -> Vec<Output> {
        let start = Barrier::new(commands.len());
        thread::scope(|scope| {
            let handles: Vec<_> = commands
                .iter()
                .map(|command| {
                    let start = &start;
                    scope.spawn(move || {
                        let args: Vec<&str> = command.iter().map(String::as_str).collect();
                        start.wait();
                        self.chute(self.repo.path(), &args)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    }

    pub fn store(&self) -> RepoStore {
        let slug = Slug::new("acme", "widget").unwrap();
        Home::at(self.sb.chute_home()).repo(&slug)
    }

    pub fn state(&self) -> State {
        self.store().load().unwrap()
    }

    pub fn state_file(&self) -> PathBuf {
        self.store().dir().join("state.json")
    }

    pub fn seed(&self, items: Vec<QueueItem>) {
        self.store()
            .update(|state| {
                state.queue.extend(items);
                Ok(())
            })
            .unwrap();
    }

    pub fn status(&self, cwd: &Path) -> Value {
        serde_json::from_str(&self.ok(cwd, &["status", "--json"])).unwrap()
    }

    /// The queue entry of `pr` in `chute status --json`.
    pub fn entry(&self, pr: u64) -> Option<Value> {
        self.status(self.repo.path())["queue"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["pr"] == pr)
            .cloned()
    }

    /// Replaces `state.json` with text no one can read as a state, and returns that text.
    pub fn corrupt_state(&self) -> String {
        let garbage = "{\"queue\": [";
        std::fs::write(self.state_file(), garbage).unwrap();
        garbage.to_owned()
    }
}

pub fn item(pr: u64, state: ItemState) -> QueueItem {
    QueueItem {
        pr: PrNumber(pr),
        title: format!("PR {pr}"),
        branch: format!("feat/{pr}"),
        head: Sha::try_from("abc1234".to_owned()).unwrap(),
        state,
        agent: None,
        attempts: 0,
        submitted_ms: EpochMs(pr),
        touches_config: false,
        last_failure: None,
    }
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
