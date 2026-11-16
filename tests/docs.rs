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
    assert!(text.contains(r#"["claude", "--dangerously-skip-permissions"]"#));
    assert!(text.contains("권한 확인 없이 명령을 실행하게 한다"));
    assert!(text.contains("respawn = []"));
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
