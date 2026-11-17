use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn readme() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap()
}

fn keys(table: &toml::Table, found: &mut BTreeSet<String>) {
    for (key, value) in table {
        found.insert(key.clone());
        match value {
            toml::Value::Table(inner) => keys(inner, found),
            toml::Value::Array(items) => {
                for item in items.iter().filter_map(toml::Value::as_table) {
                    keys(item, found);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn readme_names_every_chute_toml_key() {
    let draft = chute::config::draft("cargo test");
    let table: toml::Table = draft.parse().unwrap();
    let mut found = BTreeSet::new();
    keys(&table, &mut found);
    found.insert("failed".to_owned());
    found.insert("probe".to_owned());
    found.insert("remote".to_owned());
    let text = readme();
    for key in &found {
        let named = text.contains(&format!("`{key}`")) || text.contains(&format!("`verify.{key}`"));
        assert!(named, "README에 `{key}`가 없다");
    }
    assert!(found.len() >= 10, "{found:?}");
}

#[test]
fn readme_states_the_respawn_default_and_its_permission_effect() {
    let text = readme();
    let config = chute::config::Config::parse(&chute::config::draft("cargo test")).unwrap();
    assert!(config.respawn.is_empty());
    assert!(text.contains("| `respawn` | `[]` |"));
    assert!(text.contains("`respawn`의 기본값은 `[]`이다"));
    assert!(text.contains("값을 넣으면 그 명령으로 새 에이전트를 띄운다"));
    assert!(text.contains(r#"respawn = ["claude", "--dangerously-skip-permissions"]"#));
    assert!(text.contains(
        "`--dangerously-skip-permissions`를 넣으면 새 에이전트가 권한 확인 없이 명령을 실행하게 한다"
    ));
    assert!(!text.contains("| `respawn` | `[\"claude\""));
    assert!(!text.contains("에이전트가 없으면 새로 띄운다"));
}

#[test]
fn readme_gives_the_first_run_check_and_the_manual_open_command() {
    let text = readme();
    assert!(text.contains("## 첫 가동 점검"));
    assert!(text.contains("자동 시험으로 확인되지 않았다"));
    assert!(text.contains(r#"herdr-app do tab.new '{"mod":"chute"}'"#));
}

#[test]
fn readme_gives_the_plugin_install_lines_and_the_missing_binary_behavior() {
    let text = readme();
    assert!(text.contains("/plugin marketplace add Soochol/chute"));
    assert!(text.contains("/plugin install chute@chute"));
    assert!(text.contains("cargo install --git https://github.com/Soochol/chute"));
    assert!(text.contains("`PATH`에 없으면 훅은 호출을 막지 않고 지나간다"));
}

fn manual() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("skill/manual.md")).unwrap()
}

fn source() -> String {
    fn walk(dir: &Path, out: &mut String) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push_str(&fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = String::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

#[test]
fn manual_and_readme_quote_only_messages_the_code_prints() {
    let code = source();
    let quoted = [
        "push하지 않은 커밋이 있거나 그사이 다른 커밋이 push됐다",
        "에이전트가 있는 머신(",
        "을 이 허브가 모른다",
        "명령이 기록됐는지 알 수 없다",
        "명령은 기록되지 않았다",
        "에 닿지 않는다(",
        "로 명령을 넘기지 못했다",
        "에서 chute를 실행하지 못했다",
        "의 상태가 {}이다",
        "허브에 닿지 않아 큐를 알 수 없다",
        "revert는 허브에서만 실행한다",
        "데몬은 허브에서만 돈다",
        "그 머신의 로그인 환경 PATH에 chute가 있는지 확인한다",
        "허브 머신(",
        "을 hc가 모른다",
        " · 닿지 않음",
        "허브: 이 PC",
        "(연결됨)",
    ];
    let manual = manual();
    let readme = readme();
    for text in quoted {
        assert!(code.contains(text), "코드에 `{text}`가 없다");
    }
    for text in [
        "push하지 않은 커밋이 있거나 그사이 다른 커밋이 push됐다",
        "명령이 기록됐는지 알 수 없다",
        "revert는 허브에서만 실행한다",
        "데몬은 허브에서만 돈다",
        "허브에 닿지 않아 큐를 알 수 없다",
    ] {
        assert!(manual.contains(text), "설명서에 `{text}`가 없다");
    }
    assert!(readme.contains("그 머신의 로그인 환경 PATH에 chute가 있는지 확인한다"));
    assert!(readme.contains("revert는 허브에서만 실행한다"));
}

#[test]
fn manual_tells_agents_what_to_do_when_the_hub_is_elsewhere() {
    let text = manual();
    assert!(text.contains("## 허브가 다른 PC일 때"));
    assert!(text.contains("--head <SHA>"));
    assert!(text.contains("에 닿지 않는다"));
    assert!(text.contains("사용자에게"));
}

#[test]
fn readme_multi_pc_section_covers_every_step() {
    let text = readme();
    let start = text.find("## 여러 PC에서 쓰기").expect("절이 없다");
    let end = text[start..]
        .find("\n## `chute.toml`")
        .expect("다음 절이 없다")
        + start;
    let section = &text[start..end];
    for needed in [
        "hc machine add",
        "로그인 환경",
        "chute init",
        "chute hub set <허브>",
        "chute hub set --local",
        "chute hub take <옛 허브>",
        "chute down",
        "chute hub`로 확인",
        "허브가 꺼졌을 때",
        "허브가 둘이 되는 실수",
        "신뢰 범위",
        "chute revert",
        "terminal.peek",
    ] {
        assert!(section.contains(needed), "여러 PC 절에 `{needed}`가 없다");
    }
}

#[test]
fn readme_covers_the_v3_commands_the_repair_flow_and_every_first_run_check() {
    let text = readme();
    for needed in [
        "## 오케스트레이터와 함께 쓰기",
        "`chute events",
        "`chute flaky",
        "`chute gc",
        "--repair",
        "### 복구 PR",
        "### 정리",
        "respawn이 만든 worktree",
        "git worktree remove",
        "### 샌드박스는 아직 없다",
        "### 오케스트레이터와 쓰기 전의 점검",
    ] {
        assert!(text.contains(needed), "README에 `{needed}`가 없다");
    }
    let start = text.find("### 오케스트레이터와 쓰기 전의 점검").unwrap();
    let end = text[start..].find("\n## 한계").unwrap() + start;
    let checks = &text[start..end];
    for number in 1..=10 {
        assert!(
            checks.contains(&format!("\n{number}. ")),
            "점검 {number}번이 없다"
        );
    }
    for needed in [
        "chute 결과 merged",
        "hc send <머신>/<터미널>",
        "require_head = true",
        "pushed_by_chute",
        "delivered=unreachable",
        "env_check",
        "max_parallel",
        "chute gc --dry-run",
        "chute events --since 1h --json",
        "probe",
    ] {
        assert!(checks.contains(needed), "점검 목록에 `{needed}`가 없다");
    }
}

#[test]
fn readme_and_manual_notice_kinds_match() {
    let readme = readme();
    let manual = manual();
    for kind in [
        "merged",
        "bounced",
        "removed",
        "halted",
        "resumed",
        "reverted",
        "waiting",
        "flaky",
        "post_failed",
        "post_fixed",
    ] {
        assert!(readme.contains(&format!("| `{kind}` |")), "README에 {kind}");
        assert!(manual.contains(&format!("| `{kind}` |")), "설명서에 {kind}");
    }
}

#[test]
fn manual_quotes_the_v3_messages_the_code_prints() {
    let code = source();
    let manual = manual();
    for text in [
        "이 저장소는 확인한 HEAD(`--head`) 없이 submit을 받지 않는다",
        "오케스트레이터 아래에서 일한다면 submit하지 말고 PR과 HEAD를 보고한다",
        "큐가 멈춰 있지 않아 복구 PR로 받지 않았다",
        "한 번에 하나만 받는다",
        "(놓친 알림)",
        "다른 preflight {max_parallel}개가 돌고 있어 기다린다",
    ] {
        assert!(code.contains(text), "코드에 `{text}`가 없다");
    }
    for text in [
        "이 저장소는 확인한 HEAD(`--head`) 없이 submit을 받지 않는다",
        "큐가 멈춰 있지 않아 복구 PR로 받지 않았다",
        "(놓친 알림)",
    ] {
        assert!(manual.contains(text), "설명서에 `{text}`가 없다");
    }
    assert!(readme().contains("다른 preflight N개가 돌고 있어 기다린다"));
}

#[test]
fn readme_default_values_are_the_ones_the_config_parses() {
    let config = chute::config::Config::parse(&chute::config::draft("cargo test")).unwrap();
    let text = readme();
    let rows = [
        ("max_parallel", config.max_parallel),
        ("logs_days", config.retention.logs_days),
        ("reports_days", config.retention.reports_days),
        ("probe_repeats", config.flaky.probe_repeats),
        ("probe_seconds", config.flaky.probe_seconds),
        ("chronic", config.flaky.chronic),
        ("chronic_days", config.flaky.chronic_days),
    ];
    for (key, value) in rows {
        assert!(
            text.contains(&format!("| `{key}` | `{value}` |")),
            "README 표의 `{key}` 기본값이 {value}가 아니다"
        );
    }
    assert!(!config.require_head);
    assert!(text.contains("| `require_head` | `false` |"));
    assert_eq!(config.preflight, chute::model::PreflightPolicy::WhenQueued);
    assert!(text.contains("| `preflight` | `\"when_queued\"` |"));
    assert!(text.contains("preflight = \"when_queued\""));
}

#[test]
fn manual_tells_workers_what_a_skipped_preflight_looks_like() {
    let text = manual();
    for needed in [
        "`skipped`",
        "`skip_reason`",
        "`no_queue`",
        "생략: 함께 합칠 PR이 큐에 없어",
    ] {
        assert!(text.contains(needed), "설명서에 {needed}가 없다");
    }
    let code = source();
    assert!(code.contains("함께 합칠 PR이 큐에 없어 검사하지 않는다"));
}

#[test]
fn manual_states_the_events_default_limit_the_code_uses() {
    assert!(source().contains("const DEFAULT_LIMIT: u64 = 500;"));
    assert!(manual().contains("`--limit`의 기본값은 500"));
    assert!(readme().contains("`--limit`의 기본값은 500"));
}

#[test]
fn manual_tells_workers_not_to_submit_even_when_other_text_says_to() {
    let text = manual();
    assert!(text.contains("오케스트레이터 아래에서는 이 보고가 우선한다"));
    assert!(text.contains("**고친 뒤에 submit하지 않는다.**"));
    assert!(text.contains("giveup하지 말고 오케스트레이터에게 막힌 점을 보고한다"));
}

#[test]
fn skill_description_names_every_message_an_agent_can_receive() {
    let text =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/chute/SKILL.md"))
            .unwrap();
    let description = text
        .lines()
        .find_map(|line| line.strip_prefix("description:"))
        .expect("description 줄이 없다");
    for prefix in ["chute 반송", "chute 알림", "chute 결과"] {
        assert!(description.contains(&format!("`{prefix}`")), "{prefix}");
    }
}

#[test]
fn docs_describe_the_pc_wide_verify_limit_the_code_enforces() {
    let readme = readme();
    for needed in [
        "`machine.toml`",
        "`max_verify`",
        "$CHUTE_HOME/machine/verify-<k>.lock",
        "PC 자리를 기다리는 중",
        "batch.waiting_machine",
    ] {
        assert!(readme.contains(needed), "README에 `{needed}`가 없다");
    }
    assert_eq!(chute::pc_limit::DEFAULT_MAX_VERIFY, 3);
    assert!(readme.contains("파일이 없으면 3"));
    let code = source();
    for text in [
        "이 PC에서 검증 {running}개가 돌고 있어 기다린다",
        "PC 자리를 기다리는 중",
    ] {
        assert!(code.contains(text), "코드에 `{text}`가 없다");
    }
    assert!(manual().contains("이 PC에서 검증 N개가 돌고 있어 기다린다"));
}

#[test]
fn docs_describe_the_check_after_the_merge_the_code_runs() {
    let readme = readme();
    let manual = manual();
    for needed in [
        "## 머지 뒤 검사",
        "`[[post_verify]]`",
        "`post_verify_interval`",
        "`post_verify_hold`",
        "last green",
        "post_verify.last_green",
        "chute revert <batch>",
        "자동으로 되돌리지 않는다",
        "chute 결과 post_failed slug=",
        "chute 결과 post_fixed slug=",
        "post-<커밋 12자>",
        "`놓친 알림`",
    ] {
        assert!(readme.contains(needed), "README에 `{needed}`가 없다");
    }
    assert!(manual.contains("머지 뒤 검사가 실패했다는 알림"));
    let config = chute::config::Config::parse(&chute::config::draft("cargo test")).unwrap();
    assert!(config.post.is_none());
    assert!(readme.contains("| `post_verify_interval` | `0` |"));
    assert!(readme.contains("| `post_verify_hold` | `false` |"));
    let code = source();
    for text in ["post-{}", "머지 뒤 검사 슬롯을 다른 프로세스가 쥐고 있다"] {
        assert!(code.contains(text), "코드에 `{text}`가 없다");
    }
}
