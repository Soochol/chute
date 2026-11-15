use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::ext::Ext;
use crate::model::Approval;

pub const FILE_NAME: &str = "chute.toml";
pub const DEFAULT_BUDGET: u32 = 3;

#[derive(Clone, Debug)]
pub struct Config {
    pub approval: Approval,
    pub budget: u32,
    pub max_batch: u32,
    pub serialize: bool,
    /// Read and carried only. The gate itself belongs to `ci-gate`.
    pub require_ci: bool,
    /// Empty means no agent is spawned.
    pub respawn: Vec<String>,
    /// Never empty.
    pub verify: Vec<VerifyStep>,
}

#[derive(Clone, Debug)]
pub struct VerifyStep {
    pub name: String,
    pub run: String,
    pub timeout: Duration,
    pub failed: Option<Regex>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    approval: Approval,
    #[serde(default = "default_budget")]
    budget: u32,
    #[serde(default = "default_max_batch")]
    max_batch: u32,
    #[serde(default)]
    serialize: bool,
    #[serde(default)]
    require_ci: bool,
    #[serde(default = "default_respawn")]
    respawn: Vec<String>,
    #[serde(default)]
    verify: Vec<RawStep>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    name: String,
    run: String,
    #[serde(default = "default_timeout")]
    timeout: u64,
    failed: Option<String>,
}

fn default_budget() -> u32 {
    DEFAULT_BUDGET
}

fn default_max_batch() -> u32 {
    8
}

fn default_respawn() -> Vec<String> {
    vec!["claude".into(), "--dangerously-skip-permissions".into()]
}

fn default_timeout() -> u64 {
    3600
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text).context("chute.toml을 읽지 못했다")?;
        if raw.verify.is_empty() {
            bail!("chute.toml에 [[verify]] 단계가 하나도 없다");
        }
        if raw.max_batch == 0 {
            bail!("chute.toml의 max_batch는 1 이상이어야 한다");
        }
        let mut names = BTreeSet::new();
        let mut verify = Vec::with_capacity(raw.verify.len());
        for step in raw.verify {
            if step.name.trim().is_empty() || step.run.trim().is_empty() {
                bail!("chute.toml의 [[verify]] 단계에 name이나 run이 비어 있다");
            }
            if !names.insert(step.name.clone()) {
                bail!("chute.toml의 verify 단계 이름 {:?}이 겹친다", step.name);
            }
            if step.timeout == 0 {
                bail!(
                    "verify 단계 {:?}의 timeout은 1초 이상이어야 한다",
                    step.name
                );
            }
            let failed = step
                .failed
                .as_deref()
                .map(Regex::new)
                .transpose()
                .with_context(|| {
                    format!("verify 단계 {:?}의 failed 정규식이 잘못됐다", step.name)
                })?;
            verify.push(VerifyStep {
                name: step.name,
                run: step.run,
                timeout: Duration::from_secs(step.timeout),
                failed,
            });
        }
        Ok(Config {
            approval: raw.approval,
            budget: raw.budget,
            max_batch: raw.max_batch,
            serialize: raw.serialize,
            require_ci: raw.require_ci,
            respawn: raw.respawn,
            verify,
        })
    }

    /// Reads `chute.toml` from a git revision such as `origin/main`, never from the work tree,
    /// so a PR cannot change the rules that judge it.
    pub fn load_at(ext: &Ext, repo: &Path, rev: &str) -> Result<Config> {
        let spec = format!("{rev}:{FILE_NAME}");
        let out = ext.git(repo, ["show", spec.as_str()])?;
        if !out.success() {
            bail!(
                "설정을 읽지 못했다: {spec}. chute.toml을 커밋해 기본 브랜치에 올렸는지 확인한다 ({})",
                out.stderr_text().trim()
            );
        }
        Config::parse(&out.stdout_text()).with_context(|| format!("설정이 잘못됐다: {spec}"))
    }
}

/// The draft `chute init` writes. `verify_run` is the repository's check command.
pub fn draft(verify_run: &str) -> String {
    let run = toml::Value::String(verify_run.to_owned());
    format!(
        r#"# chute 설정. 기본 브랜치에 올라간 이 파일을 읽는다.
approval = "agent"      # "agent"면 자동 머지, "human"이면 승인 버튼을 기다린다
budget = 3              # 연속 반송 한도. 넘으면 사람 차례로 간다
max_batch = 8           # 한 batch의 PR 수 상한
serialize = false       # true면 검증을 한 번에 하나만 돈다
require_ci = false      # true면 GitHub check가 통과한 PR만 batch에 넣는다
# 에이전트가 없을 때 띄울 명령. 빈 배열이면 띄우지 않는다.
# 주의: 이 기본값은 새 에이전트가 권한 확인 없이 명령을 실행하게 한다.
respawn = ["claude", "--dangerously-skip-permissions"]

[[verify]]
name = "test"
# 검증 worktree에서 sh -c로 실행한다
run = {run}
timeout = 3600          # 초
# failed = 'test (\S+) \.\.\. FAILED'   # 실패한 시험 이름을 뽑는 정규식
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Sandbox, TestRepo};

    const MINIMAL: &str = "[[verify]]\nname = \"test\"\nrun = \"cargo test\"\n";

    #[test]
    fn defaults_match_the_plan() {
        let c = Config::parse(MINIMAL).unwrap();
        assert_eq!(c.approval, Approval::Agent);
        assert_eq!(c.budget, 3);
        assert_eq!(c.max_batch, 8);
        assert!(!c.serialize);
        assert!(!c.require_ci);
        assert_eq!(c.respawn, ["claude", "--dangerously-skip-permissions"]);
        assert_eq!(c.verify.len(), 1);
        assert_eq!(c.verify[0].timeout, Duration::from_secs(3600));
        assert!(c.verify[0].failed.is_none());
    }

    #[test]
    fn reads_every_key() {
        let c = Config::parse(
            r#"
approval = "human"
budget = 5
max_batch = 2
serialize = true
require_ci = true
respawn = []
[[verify]]
name = "test"
run = "cargo test"
timeout = 1800
failed = 'test (\S+) \.\.\. FAILED'
"#,
        )
        .unwrap();
        assert_eq!(c.approval, Approval::Human);
        assert_eq!((c.budget, c.max_batch), (5, 2));
        assert!(c.serialize && c.require_ci);
        assert!(c.respawn.is_empty());
        assert_eq!(c.verify[0].timeout, Duration::from_secs(1800));
        let re = c.verify[0].failed.as_ref().unwrap();
        assert_eq!(&re.captures("test a::b ... FAILED").unwrap()[1], "a::b");
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::parse(&format!("aproval = \"agent\"\n{MINIMAL}")).is_err());
        assert!(Config::parse(&format!("{MINIMAL}timout = 10\n")).is_err());
    }

    #[test]
    fn rejects_missing_or_empty_verify() {
        assert!(Config::parse("budget = 3\n").is_err());
        assert!(Config::parse("verify = []\n").is_err());
    }

    #[test]
    fn rejects_bad_regex() {
        let text = format!("{MINIMAL}failed = 'test ('\n");
        assert!(Config::parse(&text).is_err());
    }

    #[test]
    fn draft_parses_and_warns_above_respawn() {
        let text = draft("cargo test");
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.respawn, ["claude", "--dangerously-skip-permissions"]);
        assert_eq!(c.verify[0].run, "cargo test");
        let lines: Vec<&str> = text.lines().collect();
        let at = lines.iter().position(|l| l.starts_with("respawn")).unwrap();
        assert!(lines[at - 1].starts_with('#') && lines[at - 1].contains("권한 확인 없이"));
    }

    #[test]
    fn draft_quotes_any_command() {
        let run = r#"echo "a\b" 'c'"#;
        assert_eq!(Config::parse(&draft(run)).unwrap().verify[0].run, run);
    }

    #[test]
    fn this_repository_config_is_valid() {
        let c = Config::parse(include_str!("../chute.toml")).unwrap();
        let names: Vec<&str> = c.verify.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fmt", "clippy", "test"]);
    }

    #[test]
    fn loads_from_the_remote_ref_not_the_work_tree() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        repo.commit_file("chute.toml", &draft("echo origin"), "chore: chute 설정");
        repo.git(&["push", "origin", "main"]);
        repo.write("chute.toml", &draft("echo worktree"));
        repo.git(&["commit", "-am", "local only"]);
        repo.write("chute.toml", &draft("echo dirty"));

        let c =
            Config::load_at(&Ext::from_vars(|k| sb.var(k)), repo.path(), "origin/main").unwrap();
        assert_eq!(c.verify[0].run, "echo origin");
    }

    #[test]
    fn missing_file_on_ref_is_an_error() {
        let sb = Sandbox::new();
        let repo = TestRepo::new(&sb, "acme/widget");
        assert!(
            Config::load_at(&Ext::from_vars(|k| sb.var(k)), repo.path(), "origin/main").is_err()
        );
    }
}
