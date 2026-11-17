use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

const BASELINE_PATH: &str = "tests/test-rules-baseline.json";
const GUIDE: &str = "docs/testing.md";
const LOCKS_TEST: &str = "tests/locks.rs";
const CLOCK_SOURCE: &str = "src/clock.rs";
const FEEDBACK_TESTS: &str = "src/feedback/tests.rs";
const EXCLUDED: [&str; 2] = ["tests/support/mod.rs", "tests/test_rules.rs"];

type Counts = BTreeMap<String, usize>;
type Baseline = BTreeMap<String, Counts>;
type Found = BTreeMap<String, Vec<usize>>;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scope {
    TestCode,
    LocksFile,
    ProductCode,
}

struct Rule {
    name: &'static str,
    scope: Scope,
    fix: &'static str,
    find: fn(&str) -> Vec<usize>,
}

const TIME_FIX: &str = "`wait_until`로 관찰할 수 있는 상태를 기다린다. 시간 판정은 `Clock::frozen()`과 `advance`로 한다. \
                        일어나지 않음을 볼 때만 `fixed_wait(ms, 이유)`를 쓴다";

const RULES: [Rule; 8] = [
    Rule {
        name: "sleep",
        scope: Scope::TestCode,
        fix: TIME_FIX,
        find: find_sleep,
    },
    Rule {
        name: "instant-now",
        scope: Scope::TestCode,
        fix: TIME_FIX,
        find: find_instant_now,
    },
    Rule {
        name: "short-duration",
        scope: Scope::TestCode,
        fix: "멈춘 시계와 긴 한도를 쓰고, 관찰한 뒤 시계를 한도 너머로 옮긴다",
        find: find_short_duration,
    },
    Rule {
        name: "ignore",
        scope: Scope::TestCode,
        fix: "흔들리는 시험은 `#[ignore]`로 덮지 않고 원인을 고친다. 자식 프로세스로 도는 진입점만 \
              `#[ignore = \"run as a child process by <시험 이름>\"]`로 쓰고 그 시험을 같은 파일에 둔다",
        find: find_ignore,
    },
    Rule {
        name: "process-env",
        scope: Scope::TestCode,
        fix: "프로세스 환경을 바꾸지 말고 `Ext::from_vars`, `Home::from_vars`, `Sandbox::command`, \
              `Command::current_dir`로 넘긴다",
        find: find_process_env,
    },
    Rule {
        name: "spawn",
        scope: Scope::TestCode,
        fix: "`tests/support/mod.rs`의 `spawn(command)`로 띄운다",
        find: find_spawn,
    },
    Rule {
        name: "locks-no-process",
        scope: Scope::LocksFile,
        fix: "프로세스가 필요한 시험은 다른 시험 파일에 둔다. 다른 시험 스레드가 fork한 자식이 열린 잠금 파일을 쥔다",
        find: find_process_start,
    },
    Rule {
        name: "product-instant-now",
        scope: Scope::ProductCode,
        fix: "`Clock`을 받아 `clock.now()`를 읽는다(`src/clock.rs`)",
        find: find_instant_now,
    },
];

fn re(pattern: &'static str) -> Regex {
    Regex::new(pattern).unwrap()
}

fn find_sleep(text: &str) -> Vec<usize> {
    static QUALIFIED: LazyLock<Regex> = LazyLock::new(|| re(r"\bthread\s*::\s*sleep\s*\("));
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\buse\s+std\s*::\s*thread\s*::\s*(?:sleep\b|\{[^}]*\bsleep\b[^}]*\})")
    });
    static BARE: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|[^:\w.])(sleep\s*\()"));
    let mut found: Vec<usize> = QUALIFIED.find_iter(text).map(|m| m.start()).collect();
    if IMPORT.is_match(text) {
        found.extend(BARE.captures_iter(text).map(|c| c.get(1).unwrap().start()));
    }
    found.sort_unstable();
    found
}

fn find_instant_now(text: &str) -> Vec<usize> {
    static NOW: LazyLock<Regex> = LazyLock::new(|| re(r"\bInstant\s*::\s*now\s*\("));
    NOW.find_iter(text).map(|m| m.start()).collect()
}

fn find_short_duration(text: &str) -> Vec<usize> {
    static SHORT: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(advance\s*\(\s*)?\bDuration\s*::\s*(?:from_millis|from_micros|from_nanos|from_secs_f32|from_secs_f64)\s*\(",
        )
    });
    SHORT
        .captures_iter(text)
        .filter(|c| c.get(1).is_none())
        .map(|c| c.get(0).unwrap().start())
        .collect()
}

fn find_ignore(text: &str) -> Vec<usize> {
    static ANY: LazyLock<Regex> = LazyLock::new(|| re(r"#\[\s*ignore"));
    static CHILD_ENTRY: LazyLock<Regex> =
        LazyLock::new(|| re(r#"^#\[\s*ignore\s*=\s*"run as a child process by (\w+)"\s*\]"#));
    ANY.find_iter(text)
        .filter(|m| {
            let Some(entry) = CHILD_ENTRY.captures(&text[m.start()..]) else {
                return true;
            };
            let declared = Regex::new(&format!(r"\bfn\s+{}\b", &entry[1])).unwrap();
            !declared.is_match(text)
        })
        .map(|m| m.start())
        .collect()
}

fn find_process_env(text: &str) -> Vec<usize> {
    static ENV: LazyLock<Regex> =
        LazyLock::new(|| re(r"\b(?:set_var|remove_var|set_current_dir)\s*\("));
    ENV.find_iter(text).map(|m| m.start()).collect()
}

fn find_spawn(text: &str) -> Vec<usize> {
    static SPAWN: LazyLock<Regex> = LazyLock::new(|| re(r"\.\s*spawn\s*\(\s*\)"));
    SPAWN.find_iter(text).map(|m| m.start()).collect()
}

fn find_process_start(text: &str) -> Vec<usize> {
    static NAMES: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:Command|Sandbox|TestRepo)\b"));
    static SPAWN_CALL: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?:^|[^.\w:])((?:\w+\s*::\s*)*)(spawn\s*\()"));
    let mut found: Vec<usize> = NAMES.find_iter(text).map(|m| m.start()).collect();
    for call in SPAWN_CALL.captures_iter(text) {
        let path = call.get(1).unwrap().as_str();
        let parent = path.rsplit("::").nth(1).map(str::trim);
        if parent != Some("thread") {
            found.push(call.get(2).unwrap().start());
        }
    }
    found.sort_unstable();
    found
}

struct Parts {
    test: String,
    product: String,
}

fn split_code(path: &str, text: &str) -> Result<Parts, String> {
    let blank = |text: &str| "\n".repeat(text.matches('\n').count());
    if EXCLUDED.contains(&path) {
        return Ok(Parts {
            test: blank(text),
            product: blank(text),
        });
    }
    if path.starts_with("tests/") || path == FEEDBACK_TESTS {
        return Ok(Parts {
            test: text.to_owned(),
            product: blank(text),
        });
    }
    static TEST_MOD_OPEN: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{"));
    static ITEM_START: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"^(?:pub|fn|struct|enum|impl|const|static|use|trait|type|mod|macro_rules!|extern|unsafe|async)\b",
        )
    });
    let (mut test, mut product) = (Vec::new(), Vec::new());
    let (mut in_region, mut cfg_test_pending) = (false, false);
    for (index, line) in text.split('\n').enumerate() {
        if line.starts_with("#[") {
            cfg_test_pending |= line.starts_with("#[cfg(test)]");
        } else if ITEM_START.is_match(line) {
            if cfg_test_pending && TEST_MOD_OPEN.is_match(line) {
                in_region = true;
            } else if in_region && !cfg_test_pending {
                return Err(format!(
                    "{path}:{}: 인라인 시험 모듈 뒤에 `#[cfg(test)]`가 없는 코드가 있다. 인라인 시험 모듈은 파일 끝에 둔다",
                    index + 1
                ));
            }
            cfg_test_pending = false;
        }
        let (keep, drop) = if in_region {
            (&mut test, &mut product)
        } else {
            (&mut product, &mut test)
        };
        keep.push(line);
        drop.push("");
    }
    Ok(Parts {
        test: test.join("\n"),
        product: product.join("\n"),
    })
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

fn scoped_text<'a>(rule: &Rule, path: &str, parts: &'a Parts) -> Option<&'a str> {
    match rule.scope {
        Scope::TestCode => Some(&parts.test),
        Scope::LocksFile => (path == LOCKS_TEST).then_some(&parts.test),
        Scope::ProductCode => (path != CLOCK_SOURCE).then_some(&parts.product),
    }
}

fn find_violations(rule: &Rule, sources: &[(String, String)]) -> Result<Found, String> {
    let mut found = Found::new();
    for (path, text) in sources {
        let parts = split_code(path, text)?;
        let Some(scoped) = scoped_text(rule, path, &parts) else {
            continue;
        };
        let lines: Vec<usize> = (rule.find)(scoped)
            .into_iter()
            .map(|offset| line_of(scoped, offset))
            .collect();
        if !lines.is_empty() {
            found.insert(path.clone(), lines);
        }
    }
    Ok(found)
}

fn judge(rule: &Rule, found: &Found, allowed: &Counts) -> Vec<String> {
    let mut messages = Vec::new();
    for (path, lines) in found {
        let limit = allowed.get(path).copied().unwrap_or(0);
        if lines.len() > limit {
            for line in lines {
                messages.push(format!(
                    "{path}:{line} [{}] 허용 {limit}곳, 실제 {}곳. {}. 기준은 {GUIDE}",
                    rule.name,
                    lines.len(),
                    rule.fix
                ));
            }
        }
    }
    for (path, &limit) in allowed {
        let actual = found.get(path).map_or(0, Vec::len);
        if actual < limit {
            let edit = if actual == 0 {
                "이 항목을 지운다".to_owned()
            } else {
                format!("이 항목: {limit} → {actual}")
            };
            messages.push(format!(
                "{path} [{}] 허용 목록을 낮춘다(허용 {limit}곳, 실제 {actual}곳). {BASELINE_PATH}의 {edit}",
                rule.name
            ));
        }
    }
    messages
}

fn render_baseline(baseline: &Baseline) -> String {
    let rules: Vec<String> = baseline
        .iter()
        .filter(|(_, files)| files.values().any(|&n| n > 0))
        .map(|(rule, files)| {
            let files: Vec<String> = files
                .iter()
                .filter(|(_, n)| **n > 0)
                .map(|(path, n)| format!("    {path:?}: {n}"))
                .collect();
            format!("  {rule:?}: {{\n{}\n  }}", files.join(",\n"))
        })
        .collect();
    if rules.is_empty() {
        return "{}\n".to_owned();
    }
    format!("{{\n{}\n}}\n", rules.join(",\n"))
}

fn repo_file(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn collect_rust_files(dir: &Path, relative: &str, out: &mut Vec<String>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{relative}를 읽지 못했다: {e}"))
        .map(|e| e.unwrap())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().into_string().unwrap();
        let path = format!("{relative}/{name}");
        if entry.file_type().unwrap().is_dir() {
            collect_rust_files(&entry.path(), &path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

fn load_sources() -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for dir in ["src", "tests"] {
        collect_rust_files(&repo_file(dir), dir, &mut paths);
    }
    paths
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(repo_file(&path)).unwrap();
            (path, text)
        })
        .collect()
}

fn load_baseline() -> Baseline {
    let text = fs::read_to_string(repo_file(BASELINE_PATH)).unwrap();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{BASELINE_PATH}를 읽지 못했다: {e}"))
}

fn check(rule_name: &str) {
    let rule = RULES.iter().find(|r| r.name == rule_name).unwrap();
    let sources = load_sources();
    if rule.scope == Scope::LocksFile {
        assert!(
            sources.iter().any(|(path, _)| path == LOCKS_TEST),
            "{LOCKS_TEST}가 없다"
        );
    }
    let found = find_violations(rule, &sources).unwrap_or_else(|e| panic!("{e}"));
    let allowed = load_baseline().remove(rule.name).unwrap_or_default();
    let messages = judge(rule, &found, &allowed);
    assert!(messages.is_empty(), "\n{}\n", messages.join("\n"));
}

#[test]
fn test_code_does_not_sleep() {
    check("sleep");
}

#[test]
fn test_code_does_not_read_instant_now() {
    check("instant-now");
}

#[test]
fn test_code_has_no_short_durations() {
    check("short-duration");
}

#[test]
fn test_code_has_no_unexplained_ignore() {
    check("ignore");
}

#[test]
fn test_code_does_not_change_the_process_environment() {
    check("process-env");
}

#[test]
fn test_code_spawns_children_through_the_helper() {
    check("spawn");
}

#[test]
fn locks_tests_start_no_process() {
    check("locks-no-process");
}

#[test]
fn product_code_reads_time_through_the_clock() {
    check("product-instant-now");
}

#[test]
fn baseline_names_only_known_rules_and_is_written_canonically() {
    let text = fs::read_to_string(repo_file(BASELINE_PATH)).unwrap();
    let baseline = load_baseline();
    for rule in baseline.keys() {
        assert!(
            RULES.iter().any(|r| r.name == rule),
            "{BASELINE_PATH}에 규칙 표에 없는 이름이 있다: {rule}"
        );
    }
    assert!(
        render_baseline(&baseline) == text,
        "{BASELINE_PATH}는 규칙과 경로를 정렬하고 파일마다 한 줄로 적는다. 개수가 0인 항목은 적지 않는다"
    );
}

fn rule(name: &str) -> &'static Rule {
    RULES.iter().find(|r| r.name == name).unwrap()
}

fn lines_found(name: &str, path: &str, text: &str) -> Vec<usize> {
    let sources = [(path.to_owned(), text.to_owned())];
    find_violations(rule(name), &sources)
        .unwrap()
        .remove(path)
        .unwrap_or_default()
}

fn in_test_file(name: &str, text: &str) -> Vec<usize> {
    lines_found(name, "tests/x.rs", text)
}

#[test]
fn sleep_is_found_through_whitespace_line_breaks_and_imports() {
    assert_eq!(in_test_file("sleep", "std::thread::sleep(d);"), [1]);
    assert_eq!(in_test_file("sleep", "a;\nthread :: sleep (d);"), [2]);
    assert_eq!(in_test_file("sleep", "thread::\n    sleep\n(d);"), [1]);
    assert_eq!(
        in_test_file(
            "sleep",
            "use std::thread::sleep;\nfn f() {\n    sleep(d);\n}"
        ),
        [3]
    );
    assert_eq!(
        in_test_file("sleep", "use std::thread::{self, sleep};\nsleep(d);"),
        [2]
    );
    assert_eq!(
        in_test_file(
            "sleep",
            "use std::thread::sleep;\nstd::thread::sleep(d);\nsleep(d);"
        ),
        [2, 3]
    );
}

#[test]
fn sleep_is_not_found_in_strings_methods_or_unimported_names() {
    assert!(in_test_file("sleep", "let script = \"import time\\ntime.sleep(30)\";").is_empty());
    assert!(in_test_file("sleep", "let script = \"sleep 30\";").is_empty());
    assert!(in_test_file("sleep", "fake.sleep(d);").is_empty());
    assert!(in_test_file("sleep", "fn sleep(d: u8) {}\nsleep(d);").is_empty());
    assert!(in_test_file("sleep", "use std::thread::spawn;\nsleep(d);").is_empty());
}

#[test]
fn instant_now_is_found_with_and_without_the_path() {
    assert_eq!(
        in_test_file("instant-now", "let a = std::time::Instant::now();"),
        [1]
    );
    assert_eq!(
        in_test_file("instant-now", "\nlet a = Instant :: now ();"),
        [2]
    );
    assert!(in_test_file("instant-now", "let a = clock.now();").is_empty());
}

#[test]
fn short_durations_are_found_except_the_first_argument_of_advance() {
    assert!(in_test_file("short-duration", "clock.advance(Duration::from_millis(5));").is_empty());
    assert!(
        in_test_file(
            "short-duration",
            "clock.advance(\n    Duration::from_micros(5),\n);"
        )
        .is_empty()
    );
    assert_eq!(
        in_test_file("short-duration", "let d = Duration::from_millis(5);"),
        [1]
    );
    assert_eq!(
        in_test_file(
            "short-duration",
            "advance(Duration::from_secs(1));\nlet d = Duration :: from_millis (5);"
        ),
        [2]
    );
    assert_eq!(
        in_test_file(
            "short-duration",
            "std::time::Duration::from_secs_f64(0.5);\nDuration::from_nanos(1);\nDuration::from_secs_f32(0.5);"
        ),
        [1, 2, 3]
    );
    assert_eq!(
        in_test_file(
            "short-duration",
            "advance(Duration::from_secs(1) + Duration::from_millis(5));"
        ),
        [1]
    );
    assert!(in_test_file("short-duration", "Duration::from_secs(3600);").is_empty());
}

#[test]
fn ignore_is_allowed_only_as_a_child_process_entry_declared_in_the_same_file() {
    assert_eq!(in_test_file("ignore", "#[test]\n#[ignore]\nfn a() {}"), [2]);
    assert_eq!(
        in_test_file("ignore", "#[ignore = \"flaky\"]\nfn a() {}"),
        [1]
    );
    assert_eq!(in_test_file("ignore", "#[ ignore ]\nfn a() {}"), [1]);
    let entry = "#[ignore = \"run as a child process by parent\"]\nfn child() {}\n";
    assert_eq!(in_test_file("ignore", entry), [1]);
    assert!(in_test_file("ignore", &format!("{entry}fn parent() {{}}")).is_empty());
    assert_eq!(
        in_test_file(
            "ignore",
            &format!(
                "{entry}fn parent_helper() {{}}\n#[ignore = \"run as a child process by parent\"]"
            )
        ),
        [1, 4]
    );
}

#[test]
fn process_env_changes_are_found() {
    assert_eq!(in_test_file("process-env", "std::env::set_var(a, b);"), [1]);
    assert_eq!(in_test_file("process-env", "\nenv::remove_var (a);"), [2]);
    assert_eq!(
        in_test_file("process-env", "std::env::set_current_dir(p).unwrap();"),
        [1]
    );
    assert!(in_test_file("process-env", "cmd.current_dir(p).env_remove(k);").is_empty());
}

#[test]
fn spawn_is_found_only_without_arguments() {
    assert_eq!(in_test_file("spawn", "let c = cmd.spawn().unwrap();"), [1]);
    assert_eq!(
        in_test_file("spawn", "cmd\n    .stdout(x)\n    .spawn()\n    .unwrap();"),
        [3]
    );
    assert_eq!(in_test_file("spawn", "cmd.spawn (\n);"), [1]);
    assert!(in_test_file("spawn", "std::thread::spawn(move || run());").is_empty());
    assert!(in_test_file("spawn", "thread::Builder::new().spawn(|| run());").is_empty());
    assert!(in_test_file("spawn", "let c = spawn(&mut command);").is_empty());
}

#[test]
fn locks_test_may_not_name_process_starters() {
    let found = |text: &str| lines_found("locks-no-process", LOCKS_TEST, text);
    assert_eq!(found("use std::process::Command;"), [1]);
    assert_eq!(
        found("let sb = Sandbox::new();\nTestRepo::new(&sb);"),
        [1, 2]
    );
    assert_eq!(found("let c = spawn(&mut command);"), [1]);
    assert_eq!(found("let c = support::spawn(&mut command);"), [1]);
    assert!(found("std::thread::spawn(move || run());").is_empty());
    assert!(found("thread :: spawn(move || run());").is_empty());
    assert!(found("let h = s.spawn(|| run());").is_empty());
    assert!(found("let commands = 1;").is_empty());
    assert!(lines_found("locks-no-process", "tests/other.rs", "Command::new(a);").is_empty());
}

#[test]
fn inline_test_modules_are_test_code_and_the_rest_of_the_file_is_product() {
    let text = "fn a() { Instant::now(); }\n\n#[cfg(test)]\nmod tests {\n    fn t() { Instant::now(); }\n}\n";
    assert_eq!(lines_found("instant-now", "src/a.rs", text), [5]);
    assert_eq!(lines_found("product-instant-now", "src/a.rs", text), [1]);
}

#[test]
fn a_file_may_hold_several_inline_test_modules() {
    let text = "fn a() { Instant::now(); }\n#[cfg(test)]\npub mod fake {\n    fn t() { Instant::now(); }\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    fn u() { Instant::now(); }\n}\n";
    assert_eq!(lines_found("instant-now", "src/a.rs", text), [4, 10]);
    assert_eq!(lines_found("product-instant-now", "src/a.rs", text), [1]);
}

#[test]
fn code_after_a_test_module_declared_in_another_file_stays_product() {
    let text = "#[cfg(test)]\n#[path = \"../tests/support/mod.rs\"]\npub(crate) mod testkit;\n\nfn a() { Instant::now(); }\n";
    assert!(lines_found("instant-now", "src/lib.rs", text).is_empty());
    assert_eq!(lines_found("product-instant-now", "src/lib.rs", text), [5]);
    let split =
        "fn a() { Instant::now(); }\n#[cfg(test)]\nmod tests;\nfn b() { Instant::now(); }\n";
    assert_eq!(
        lines_found("product-instant-now", "src/b.rs", split),
        [1, 4]
    );
}

#[test]
fn product_code_after_an_inline_test_module_is_an_error() {
    let text = "#[cfg(test)]\nmod tests {\n}\n\nfn product() {}\n";
    let err = split_code("src/a.rs", text).err().unwrap();
    assert!(err.starts_with("src/a.rs:5:"), "{err}");
}

#[test]
fn files_are_classified_by_path() {
    let text = "Instant::now();";
    assert_eq!(lines_found("instant-now", "tests/engine.rs", text), [1]);
    assert_eq!(
        lines_found("instant-now", "tests/engine_kit/mod.rs", text),
        [1]
    );
    assert_eq!(lines_found("instant-now", FEEDBACK_TESTS, text), [1]);
    assert!(lines_found("product-instant-now", "tests/engine.rs", text).is_empty());
    assert!(lines_found("instant-now", "tests/support/mod.rs", text).is_empty());
    assert!(lines_found("instant-now", "src/engine.rs", text).is_empty());
    assert_eq!(
        lines_found("product-instant-now", "src/engine.rs", text),
        [1]
    );
    assert!(lines_found("product-instant-now", CLOCK_SOURCE, text).is_empty());
}

fn counts(entries: &[(&str, usize)]) -> Counts {
    entries.iter().map(|&(p, n)| (p.to_owned(), n)).collect()
}

fn found(entries: &[(&str, &[usize])]) -> Found {
    entries
        .iter()
        .map(|&(p, l)| (p.to_owned(), l.to_vec()))
        .collect()
}

#[test]
fn a_larger_count_fails_on_every_violating_line_with_the_fix() {
    let messages = judge(
        rule("short-duration"),
        &found(&[("tests/a.rs", &[4, 9, 12])]),
        &counts(&[("tests/a.rs", 2)]),
    );
    assert_eq!(messages.len(), 3);
    for (message, line) in messages.iter().zip([4, 9, 12]) {
        assert!(
            message.starts_with(&format!(
                "tests/a.rs:{line} [short-duration] 허용 2곳, 실제 3곳. "
            )),
            "{message}"
        );
        assert!(message.contains(rule("short-duration").fix), "{message}");
        assert!(message.ends_with("기준은 docs/testing.md"), "{message}");
    }
}

#[test]
fn a_file_missing_from_the_baseline_has_a_limit_of_zero() {
    let messages = judge(
        rule("sleep"),
        &found(&[("tests/new.rs", &[7])]),
        &Counts::new(),
    );
    assert_eq!(messages.len(), 1);
    assert!(messages[0].starts_with("tests/new.rs:7 [sleep] 허용 0곳, 실제 1곳. "));
}

#[test]
fn a_smaller_count_or_a_vanished_file_asks_to_lower_the_baseline() {
    let messages = judge(
        rule("sleep"),
        &found(&[("tests/a.rs", &[1, 2])]),
        &counts(&[("tests/a.rs", 5), ("tests/gone.rs", 3)]),
    );
    assert_eq!(
        messages,
        [
            "tests/a.rs [sleep] 허용 목록을 낮춘다(허용 5곳, 실제 2곳). tests/test-rules-baseline.json의 이 항목: 5 → 2",
            "tests/gone.rs [sleep] 허용 목록을 낮춘다(허용 3곳, 실제 0곳). tests/test-rules-baseline.json의 이 항목을 지운다",
        ]
    );
}

#[test]
fn an_equal_count_passes() {
    let messages = judge(
        rule("sleep"),
        &found(&[("tests/a.rs", &[1, 2])]),
        &counts(&[("tests/a.rs", 2)]),
    );
    assert!(messages.is_empty());
}

#[test]
fn the_baseline_is_rendered_sorted_with_one_line_per_file_and_no_zeros() {
    let baseline: Baseline = [
        (
            "spawn".to_owned(),
            counts(&[("tests/b.rs", 2), ("src/a.rs", 1)]),
        ),
        ("ignore".to_owned(), counts(&[("src/a.rs", 0)])),
    ]
    .into();
    assert_eq!(
        render_baseline(&baseline),
        "{\n  \"spawn\": {\n    \"src/a.rs\": 1,\n    \"tests/b.rs\": 2\n  }\n}\n"
    );
    assert_eq!(render_baseline(&Baseline::new()), "{}\n");
}
