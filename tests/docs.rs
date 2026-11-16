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
