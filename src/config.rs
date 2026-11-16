use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde::Deserialize;

use crate::ext::Ext;
use crate::model::{Approval, NonEmpty, PreflightPolicy};

pub const FILE_NAME: &str = "chute.toml";
pub const DEFAULT_BUDGET: u32 = 3;
pub const PROBE_PLACEHOLDER: &str = "{tests}";

#[derive(Clone, Debug)]
pub struct Config {
    pub remote: Option<String>,
    pub approval: Approval,
    pub budget: u32,
    pub max_batch: u32,
    pub serialize: bool,
    pub require_ci: bool,
    pub respawn: Vec<String>,
    pub require_head: bool,
    pub max_parallel: u32,
    pub preflight: PreflightPolicy,
    pub env_check: Vec<String>,
    pub retention: Retention,
    pub cache: Cache,
    pub flaky: FlakyPolicy,
    pub verify: NonEmpty<VerifyStep>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    pub logs_days: u32,
    pub reports_days: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cache {
    pub seed: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlakyPolicy {
    pub probe_repeats: u32,
    pub probe_seconds: u32,
    pub chronic: u32,
    pub chronic_days: u32,
}

impl Default for FlakyPolicy {
    fn default() -> Self {
        FlakyPolicy {
            probe_repeats: default_probe_repeats(),
            probe_seconds: default_probe_seconds(),
            chronic: default_chronic(),
            chronic_days: default_chronic_days(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct VerifyStep {
    pub name: String,
    pub run: String,
    pub timeout: Duration,
    pub failed: Option<Regex>,
    pub probe: Option<String>,
    /// The step runs only when a changed file starts with one of these. `None` means always.
    pub paths: Option<NonEmpty<String>>,
}

impl VerifyStep {
    pub fn runs_for(&self, changed: &[String]) -> bool {
        self.paths.as_ref().is_none_or(|paths| {
            changed
                .iter()
                .any(|file| paths.iter().any(|prefix| file.starts_with(prefix.as_str())))
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    remote: Option<String>,
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
    #[serde(default)]
    respawn: Vec<String>,
    #[serde(default)]
    require_head: bool,
    #[serde(default = "default_max_parallel")]
    max_parallel: u32,
    #[serde(default)]
    preflight: PreflightPolicy,
    #[serde(default)]
    env_check: Vec<String>,
    #[serde(default)]
    retention: RawRetention,
    #[serde(default)]
    cache: RawCache,
    #[serde(default)]
    flaky: RawFlaky,
    #[serde(default)]
    verify: Vec<RawStep>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRetention {
    #[serde(default = "default_logs_days")]
    logs_days: u32,
    #[serde(default = "default_reports_days")]
    reports_days: u32,
}

impl Default for RawRetention {
    fn default() -> Self {
        RawRetention {
            logs_days: default_logs_days(),
            reports_days: default_reports_days(),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCache {
    #[serde(default)]
    seed: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFlaky {
    #[serde(default = "default_probe_repeats")]
    probe_repeats: u32,
    #[serde(default = "default_probe_seconds")]
    probe_seconds: u32,
    #[serde(default = "default_chronic")]
    chronic: u32,
    #[serde(default = "default_chronic_days")]
    chronic_days: u32,
}

impl Default for RawFlaky {
    fn default() -> Self {
        RawFlaky {
            probe_repeats: default_probe_repeats(),
            probe_seconds: default_probe_seconds(),
            chronic: default_chronic(),
            chronic_days: default_chronic_days(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    name: String,
    run: String,
    #[serde(default = "default_timeout")]
    timeout: u64,
    failed: Option<String>,
    probe: Option<String>,
    paths: Option<Vec<String>>,
}

fn default_budget() -> u32 {
    DEFAULT_BUDGET
}

fn default_max_batch() -> u32 {
    8
}

/// Also the limit of a run that has not fetched the repository's `chute.toml` yet.
pub const DEFAULT_MAX_PARALLEL: u32 = 2;

fn default_max_parallel() -> u32 {
    DEFAULT_MAX_PARALLEL
}

fn default_logs_days() -> u32 {
    7
}

fn default_reports_days() -> u32 {
    30
}

fn default_probe_repeats() -> u32 {
    20
}

fn default_probe_seconds() -> u32 {
    180
}

fn default_chronic() -> u32 {
    3
}

fn default_chronic_days() -> u32 {
    30
}

fn default_timeout() -> u64 {
    3600
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text).context("chute.toml을 읽지 못했다")?;
        if raw.max_batch == 0 {
            bail!("chute.toml의 max_batch는 1 이상이어야 한다");
        }
        if raw.respawn.iter().any(|arg| arg.trim().is_empty()) {
            bail!("chute.toml의 respawn에 빈 항목이 있다. 띄우지 않으려면 respawn = []로 둔다");
        }
        let at_least_one = [
            ("max_parallel", raw.max_parallel),
            ("retention.logs_days", raw.retention.logs_days),
            ("retention.reports_days", raw.retention.reports_days),
            ("flaky.probe_repeats", raw.flaky.probe_repeats),
            ("flaky.probe_seconds", raw.flaky.probe_seconds),
            ("flaky.chronic", raw.flaky.chronic),
            ("flaky.chronic_days", raw.flaky.chronic_days),
        ];
        if let Some((key, _)) = at_least_one.iter().find(|(_, value)| *value == 0) {
            bail!("chute.toml의 {key}는 1 이상이어야 한다");
        }
        if let Some(name) = &raw.remote
            && !is_remote_name(name)
        {
            bail!("chute.toml의 remote {name:?}은 git 원격 이름이 아니다");
        }
        if raw.env_check.iter().any(|run| run.trim().is_empty()) {
            bail!("chute.toml의 env_check에 빈 항목이 있다");
        }
        if raw.cache.seed.iter().any(|path| path.trim().is_empty()) {
            bail!("chute.toml의 cache.seed에 빈 항목이 있다");
        }
        let mut names = BTreeSet::new();
        let mut steps = Vec::with_capacity(raw.verify.len());
        for step in raw.verify {
            if step.name.is_empty() || step.name.starts_with('.') || step.name.contains(['/', '\0'])
            {
                bail!(
                    "chute.toml의 verify 단계 이름 {:?}은 비어 있거나 '.'으로 시작하거나 '/'를 담을 수 없다",
                    step.name
                );
            }
            if !names.insert(step.name.clone()) {
                bail!("chute.toml의 verify 단계 이름 {:?}이 겹친다", step.name);
            }
            if step.run.trim().is_empty() {
                bail!("verify 단계 {:?}의 run이 비어 있다", step.name);
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
            if failed.as_ref().is_some_and(|re| re.as_str().is_empty()) {
                bail!("verify 단계 {:?}의 failed 정규식이 비어 있다", step.name);
            }
            if let Some(probe) = &step.probe
                && !probe.contains(PROBE_PLACEHOLDER)
            {
                bail!(
                    "verify 단계 {:?}의 probe에 {PROBE_PLACEHOLDER} 자리가 없다",
                    step.name
                );
            }
            let paths = step
                .paths
                .map(|paths| {
                    if paths.iter().any(|path| path.trim().is_empty()) {
                        bail!("verify 단계 {:?}의 paths에 빈 항목이 있다", step.name);
                    }
                    NonEmpty::new(paths).ok_or_else(|| {
                        anyhow!(
                            "verify 단계 {:?}의 paths가 비어 있다. 늘 돌리려면 paths 키를 지운다",
                            step.name
                        )
                    })
                })
                .transpose()?;
            steps.push(VerifyStep {
                name: step.name,
                run: step.run,
                timeout: Duration::from_secs(step.timeout),
                failed,
                probe: step.probe,
                paths,
            });
        }
        let verify = NonEmpty::new(steps)
            .ok_or_else(|| anyhow!("chute.toml에 [[verify]] 단계가 하나도 없다"))?;
        Ok(Config {
            remote: raw.remote,
            approval: raw.approval,
            budget: raw.budget,
            max_batch: raw.max_batch,
            serialize: raw.serialize,
            require_ci: raw.require_ci,
            respawn: raw.respawn,
            require_head: raw.require_head,
            max_parallel: raw.max_parallel,
            preflight: raw.preflight,
            env_check: raw.env_check,
            retention: Retention {
                logs_days: raw.retention.logs_days,
                reports_days: raw.retention.reports_days,
            },
            cache: Cache {
                seed: raw.cache.seed,
            },
            flaky: FlakyPolicy {
                probe_repeats: raw.flaky.probe_repeats,
                probe_seconds: raw.flaky.probe_seconds,
                chronic: raw.flaky.chronic,
                chronic_days: raw.flaky.chronic_days,
            },
            verify,
        })
    }

    /// Reads `chute.toml` from `refs/remotes/<remote>/<branch>`, or returns `None` when that ref
    /// or the file is missing. The exact ref is named on purpose: `<remote>/<branch>` resolves
    /// to a local branch or tag of that name first, and even the full ref name falls back to
    /// `refs/tags/<name>` and `refs/heads/<name>` when the remote-tracking ref is absent.
    pub fn load_remote_branch(
        ext: &Ext,
        repo: &Path,
        remote: &str,
        branch: &str,
    ) -> Result<Option<Config>> {
        let remote_ref = format!("refs/remotes/{remote}/{branch}");
        let found = ext.git(
            repo,
            ["show-ref", "--verify", "--hash", remote_ref.as_str()],
        )?;
        let commit = found.stdout_text().trim().to_owned();
        if !found.success() || commit.is_empty() {
            return Ok(None);
        }
        let spec = format!("{commit}:{FILE_NAME}");
        let shown = ext.git(repo, ["--no-replace-objects", "show", spec.as_str()])?;
        if !shown.success() {
            return Ok(None);
        }
        Config::parse(&shown.stdout_text())
            .map(Some)
            .with_context(|| format!("{remote}/{branch}의 설정을 읽지 못했다"))
    }

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

fn is_remote_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains(|c: char| c.is_whitespace() || c.is_control())
}

pub fn remote_key(text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(text).ok()?;
    let name = value.get("remote")?.as_str()?;
    (!name.is_empty()).then(|| name.to_owned())
}

pub fn draft(verify_run: &str) -> String {
    let run = toml::Value::String(verify_run.to_owned());
    format!(
        r#"# chute 설정. 기본 브랜치에 올라간 이 파일을 읽는다.
approval = "agent"      # "agent"면 자동 머지, "human"이면 승인 버튼을 기다린다
budget = 3              # 연속 반송 한도. 넘으면 사람 차례로 간다
max_batch = 8           # 한 batch의 PR 수 상한
serialize = false       # true면 검증을 한 번에 하나만 돈다
require_ci = false      # true면 GitHub check가 통과한 PR만 batch에 넣는다
require_head = false    # true면 --head 없는 submit을 거절한다
max_parallel = 2        # 동시에 도는 preflight 수 상한(슬롯 수 상한)
preflight = "when_queued"  # "when_queued"면 합칠 PR이 큐에 있을 때만 검사하고, "always"면 늘, "never"면 안 한다
env_check = []          # 검증 전에 돌려 결과를 남길 명령. 예: ["cargo --version"]
# 에이전트가 없을 때 띄울 명령. 비어 있으면 띄우지 않고 사람 차례로 넘긴다.
# 값을 넣으면 그 명령으로 새 에이전트를 띄운다.
# 주의: --dangerously-skip-permissions를 넣으면 새 에이전트가 권한 확인 없이 명령을 실행한다.
respawn = []
# 원격이 여러 개면 chute가 쓸 git 원격의 이름을 적는다. 예: remote = "upstream"
# remote = "origin"

[retention]
logs_days = 7           # 검증 로그와 preflight 로그를 남기는 날수
reports_days = 30       # 반송 보고서, preflight 보고서, 실패 트리 참조를 남기는 날수

[cache]
seed = []               # 새 슬롯에 복사해 올 git이 무시하는 경로. 예: ["target"]

[flaky]
probe_repeats = 20      # 흔들린 시험을 되풀이하는 횟수 상한
probe_seconds = 180     # 되풀이하는 시간 상한(초)
chronic = 3             # 이만큼 흔들리면 만성으로 본다
chronic_days = 30       # 만성을 세는 기간(일)

[[verify]]
name = "test"
# 검증 worktree에서 sh -c로 실행한다
run = {run}
timeout = 3600          # 초
# failed = 'test (\S+) \.\.\. FAILED'   # 실패한 시험 이름을 뽑는 정규식
# probe = "cargo test {{tests}} -- --exact"   # 흔들린 시험만 되풀이하는 명령. {{tests}} 자리가 필요하다
# paths = ["src/", "Cargo.toml"]   # 바뀐 파일이 이 접두어로 시작할 때만 이 단계를 돈다. 비우면 안 된다
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
        assert!(c.respawn.is_empty());
        assert!(!c.require_head);
        assert_eq!(c.max_parallel, 2);
        assert_eq!(c.preflight, PreflightPolicy::WhenQueued);
        assert!(c.env_check.is_empty());
        assert_eq!(
            c.retention,
            Retention {
                logs_days: 7,
                reports_days: 30
            }
        );
        assert!(c.cache.seed.is_empty());
        assert_eq!(
            c.flaky,
            FlakyPolicy {
                probe_repeats: 20,
                probe_seconds: 180,
                chronic: 3,
                chronic_days: 30
            }
        );
        assert_eq!(c.verify.len(), 1);
        assert_eq!(c.verify[0].timeout, Duration::from_secs(3600));
        assert!(c.verify[0].failed.is_none());
        assert!(c.verify[0].probe.is_none());
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
respawn = ["codex", "--no-daemon"]
require_head = true
max_parallel = 4
preflight = "never"
env_check = ["cargo --version", "git --version"]
[retention]
logs_days = 3
reports_days = 10
[cache]
seed = ["target", "node_modules"]
[flaky]
probe_repeats = 5
probe_seconds = 60
chronic = 2
chronic_days = 14
[[verify]]
name = "test"
run = "cargo test"
timeout = 1800
failed = 'test (\S+) \.\.\. FAILED'
probe = "cargo test {tests} -- --exact"
"#,
        )
        .unwrap();
        assert_eq!(c.approval, Approval::Human);
        assert_eq!((c.budget, c.max_batch), (5, 2));
        assert!(c.serialize && c.require_ci);
        assert_eq!(c.respawn, ["codex", "--no-daemon"]);
        assert!(c.require_head);
        assert_eq!(c.max_parallel, 4);
        assert_eq!(c.preflight, PreflightPolicy::Never);
        assert_eq!(c.env_check, ["cargo --version", "git --version"]);
        assert_eq!(
            c.retention,
            Retention {
                logs_days: 3,
                reports_days: 10
            }
        );
        assert_eq!(c.cache.seed, ["target", "node_modules"]);
        assert_eq!(
            c.flaky,
            FlakyPolicy {
                probe_repeats: 5,
                probe_seconds: 60,
                chronic: 2,
                chronic_days: 14
            }
        );
        assert_eq!(
            c.verify[0].probe.as_deref(),
            Some("cargo test {tests} -- --exact")
        );
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

    fn with_paths(paths: &str) -> String {
        format!("{MINIMAL}paths = {paths}\n")
    }

    #[test]
    fn a_step_without_paths_always_runs_and_one_with_paths_runs_on_a_prefix_match() {
        let always = Config::parse(MINIMAL).unwrap();
        assert!(always.verify[0].paths.is_none());
        assert!(always.verify[0].runs_for(&[]));

        let c = Config::parse(&with_paths(r#"["web/", "Cargo.toml"]"#)).unwrap();
        let step = &c.verify[0];
        let files = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        assert!(step.runs_for(&files(&["docs/a.md", "web/app.js"])));
        assert!(step.runs_for(&files(&["Cargo.toml"])));
        assert!(step.runs_for(&files(&["Cargo.toml.bak"])), "접두어 비교다");
        assert!(!step.runs_for(&files(&["docs/web/app.js", "src/web/x", "cargo.toml"])));
        assert!(!step.runs_for(&[]));
    }

    #[test]
    fn rejects_empty_or_blank_paths() {
        for paths in ["[]", r#"[""]"#, r#"["web/", " "]"#] {
            let e = Config::parse(&with_paths(paths)).unwrap_err().to_string();
            assert!(e.contains("paths"), "{paths}: {e}");
        }
    }

    #[test]
    fn rejects_bad_regex() {
        for failed in ["test (", ""] {
            let text = format!("{MINIMAL}failed = '{failed}'\n");
            assert!(Config::parse(&text).is_err(), "{failed:?}");
        }
    }

    #[test]
    fn accepts_korean_step_names_and_regexes_without_groups() {
        let text = "[[verify]]\nname = \"단위 시험\"\nrun = \"true\"\nfailed = 'FAILED'\n";
        assert_eq!(Config::parse(text).unwrap().verify[0].name, "단위 시험");
    }

    #[test]
    fn rejects_blank_respawn_arguments() {
        assert!(Config::parse(&format!("respawn = [\"\"]\n{MINIMAL}")).is_err());
        assert!(Config::parse(&format!("respawn = [\"claude\", \" \"]\n{MINIMAL}")).is_err());
        assert!(Config::parse(&format!("respawn = []\n{MINIMAL}")).is_ok());
    }

    #[test]
    fn rejects_zero_limits() {
        for key in [
            "max_parallel = 0\n",
            "[retention]\nlogs_days = 0\n",
            "[retention]\nreports_days = 0\n",
            "[flaky]\nprobe_repeats = 0\n",
            "[flaky]\nprobe_seconds = 0\n",
            "[flaky]\nchronic = 0\n",
            "[flaky]\nchronic_days = 0\n",
        ] {
            let text = if key.starts_with('[') {
                format!("{MINIMAL}{key}")
            } else {
                format!("{key}{MINIMAL}")
            };
            let err = Config::parse(&text).unwrap_err();
            assert!(format!("{err:#}").contains("1 이상"), "{key:?}: {err:#}");
        }
    }

    #[test]
    fn rejects_blank_env_check_and_seed_entries() {
        assert!(Config::parse(&format!("env_check = [\"\"]\n{MINIMAL}")).is_err());
        assert!(Config::parse(&format!("env_check = [\" \"]\n{MINIMAL}")).is_err());
        assert!(Config::parse(&format!("{MINIMAL}[cache]\nseed = [\"\"]\n")).is_err());
        assert!(Config::parse(&format!("env_check = []\n{MINIMAL}[cache]\nseed = []\n")).is_ok());
    }

    #[test]
    fn a_probe_needs_the_tests_placeholder() {
        let step = |probe: &str| {
            format!("[[verify]]\nname = \"test\"\nrun = \"true\"\nprobe = '{probe}'\n")
        };
        assert!(Config::parse(&step("cargo test")).is_err());
        assert!(Config::parse(&step("")).is_err());
        assert!(Config::parse(&step("cargo test {tests}")).is_ok());
    }

    #[test]
    fn rejects_unknown_keys_in_the_new_tables() {
        for table in ["retention", "cache", "flaky"] {
            let text = format!("{MINIMAL}[{table}]\nbogus = 1\n");
            assert!(Config::parse(&text).is_err(), "{table}");
        }
    }

    #[test]
    fn rejects_step_names_that_are_not_file_names() {
        for name in ["../x", "a/b", ".hidden", ""] {
            let text = format!("[[verify]]\nname = \"{name}\"\nrun = \"true\"\n");
            assert!(Config::parse(&text).is_err(), "{name:?}");
        }
    }

    #[test]
    fn draft_parses_and_warns_above_respawn() {
        let text = draft("cargo test");
        let c = Config::parse(&text).unwrap();
        assert!(c.respawn.is_empty());
        assert_eq!(c.verify[0].run, "cargo test");
        let lines: Vec<&str> = text.lines().collect();
        let at = lines.iter().position(|l| l.starts_with("respawn")).unwrap();
        let comment = &lines[at - 3..at];
        assert!(comment.iter().all(|l| l.starts_with('#')), "{comment:?}");
        assert!(comment[0].contains("비어 있으면 띄우지 않고 사람 차례로 넘긴다"));
        assert!(comment[1].contains("값을 넣으면 그 명령으로 새 에이전트를 띄운다"));
        assert!(
            comment[2].contains("--dangerously-skip-permissions")
                && comment[2].contains("권한 확인 없이")
        );
    }

    #[test]
    fn draft_carries_every_key_and_reads_back_as_the_defaults() {
        let text = draft("cargo test");
        let table: toml::Table = text.parse().unwrap();
        for key in [
            "require_head",
            "max_parallel",
            "preflight",
            "env_check",
            "respawn",
            "retention",
            "cache",
            "flaky",
        ] {
            assert!(table.contains_key(key), "초안에 {key}가 없다");
        }
        let minimal = Config::parse(MINIMAL).unwrap();
        let drafted = Config::parse(&text).unwrap();
        assert_eq!(drafted.max_parallel, minimal.max_parallel);
        assert_eq!(drafted.preflight, minimal.preflight);
        assert_eq!(drafted.retention, minimal.retention);
        assert_eq!(drafted.flaky, minimal.flaky);
        assert_eq!(drafted.cache, minimal.cache);
        assert_eq!(drafted.env_check, minimal.env_check);
        assert_eq!(drafted.require_head, minimal.require_head);
        assert!(text.contains("# probe = "));
    }

    #[test]
    fn the_preflight_key_takes_three_words_and_nothing_else() {
        for (word, policy) in [
            ("when_queued", PreflightPolicy::WhenQueued),
            ("always", PreflightPolicy::Always),
            ("never", PreflightPolicy::Never),
        ] {
            let text = format!("preflight = \"{word}\"\n{MINIMAL}");
            assert_eq!(Config::parse(&text).unwrap().preflight, policy);
        }
        let err = Config::parse(&format!("preflight = \"sometimes\"\n{MINIMAL}")).unwrap_err();
        assert!(format!("{err:#}").contains("sometimes"), "{err:#}");
    }

    #[test]
    fn the_remote_key_names_a_git_remote() {
        assert_eq!(Config::parse(MINIMAL).unwrap().remote, None);
        let named = format!("remote = \"upstream\"\n{MINIMAL}");
        assert_eq!(
            Config::parse(&named).unwrap().remote.as_deref(),
            Some("upstream")
        );
        for bad in ["", "a b", "-x"] {
            let text = format!("remote = {bad:?}\n{MINIMAL}");
            assert!(Config::parse(&text).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_remote_key_is_read_even_when_the_rest_of_the_file_is_wrong() {
        assert_eq!(
            remote_key("remote = \"upstream\"\nbudget = \"x\"\n").as_deref(),
            Some("upstream")
        );
        assert_eq!(remote_key("budget = 3\n"), None);
        assert_eq!(
            remote_key("remote = \"team/up\"\n").as_deref(),
            Some("team/up")
        );
        assert_eq!(remote_key("not toml ["), None);
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
