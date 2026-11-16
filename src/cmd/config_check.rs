use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Serialize;

use super::{REFUSED, target_dir_of};
use crate::config::{self, Config};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// 읽을 설정 파일. 없으면 작업 폴더의 chute.toml
    #[arg(long, value_name = "경로")]
    pub file: Option<PathBuf>,
    /// 결과를 JSON으로 출력한다
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Serialize)]
struct Step {
    name: String,
    paths: Option<Vec<String>>,
    timeout_s: u64,
}

#[derive(Debug, Serialize)]
struct Checked {
    ok: bool,
    file: PathBuf,
    error: Option<String>,
    verify: Vec<Step>,
}

pub fn run(args: &Args, repo: Option<&Path>) -> Result<ExitCode> {
    let file = match &args.file {
        Some(file) => std::path::absolute(file)
            .with_context(|| format!("경로를 절대 경로로 바꾸지 못했다: {}", file.display()))?,
        None => target_dir_of(repo)?.join(config::FILE_NAME),
    };
    let checked = check(file);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&checked)?);
    } else if let Some(error) = &checked.error {
        eprintln!("chute: {error}");
    } else {
        print!("{}", render(&checked));
    }
    Ok(if checked.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(REFUSED)
    })
}

fn check(file: PathBuf) -> Checked {
    match read(&file) {
        Ok(config) => Checked {
            ok: true,
            file,
            error: None,
            verify: config
                .verify
                .iter()
                .map(|step| Step {
                    name: step.name.clone(),
                    paths: step.paths.as_ref().map(|paths| paths.to_vec()),
                    timeout_s: step.timeout.as_secs(),
                })
                .collect(),
        },
        Err(e) => Checked {
            ok: false,
            file,
            error: Some(format!("{e:#}")),
            verify: Vec::new(),
        },
    }
}

fn read(file: &std::path::Path) -> Result<Config> {
    let text = fs::read_to_string(file)
        .with_context(|| format!("설정 파일을 읽지 못했다: {}", file.display()))?;
    Config::parse(&text)
}

fn render(checked: &Checked) -> String {
    let mut out = format!(
        "설정이 올바르다: {} (검증 단계 {}개)\n",
        checked.file.display(),
        checked.verify.len()
    );
    for step in &checked.verify {
        let paths = match &step.paths {
            Some(paths) => paths.join(", "),
            None => "없음(늘 돈다)".to_owned(),
        };
        out.push_str(&format!(
            "  {} · paths: {paths} · 시간 제한 {}초\n",
            step.name, step.timeout_s
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sandbox;

    const GOOD: &str = r#"
[[verify]]
name = "check"
run = "true"

[[verify]]
name = "e2e"
run = "true"
timeout = 1800
paths = ["web/", "Cargo.toml"]
"#;

    fn write(sb: &Sandbox, text: &str) -> PathBuf {
        let dir = sb.root().join("work");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(config::FILE_NAME);
        fs::write(&file, text).unwrap();
        file
    }

    #[test]
    fn a_good_config_lists_each_step_with_its_paths_and_timeout() {
        let sb = Sandbox::new();
        let checked = check(write(&sb, GOOD));
        assert!(checked.ok);
        assert_eq!(checked.verify.len(), 2);
        assert_eq!(checked.verify[0].paths, None);
        assert_eq!(checked.verify[0].timeout_s, 3600);
        assert_eq!(
            checked.verify[1].paths,
            Some(vec!["web/".to_owned(), "Cargo.toml".to_owned()])
        );
        assert_eq!(checked.verify[1].timeout_s, 1800);
        let text = render(&checked);
        assert!(text.contains("e2e · paths: web/, Cargo.toml · 시간 제한 1800초"));
        assert!(text.contains("check · paths: 없음(늘 돈다) · 시간 제한 3600초"));
    }

    #[test]
    fn an_empty_paths_list_is_a_config_error() {
        let sb = Sandbox::new();
        let bad = "[[verify]]\nname = \"e2e\"\nrun = \"true\"\npaths = []\n";
        let checked = check(write(&sb, bad));
        assert!(!checked.ok);
        let error = checked.error.unwrap();
        assert!(error.contains("e2e"), "{error}");
        assert!(error.contains("paths가 비어 있다"), "{error}");
    }

    #[test]
    fn a_blank_path_entry_and_a_missing_file_are_config_errors() {
        let sb = Sandbox::new();
        let blank = "[[verify]]\nname = \"e2e\"\nrun = \"true\"\npaths = [\"web/\", \" \"]\n";
        let checked = check(write(&sb, blank));
        assert!(!checked.ok);
        assert!(checked.error.unwrap().contains("빈 항목"));
        let missing = check(sb.root().join("nowhere").join(config::FILE_NAME));
        assert!(!missing.ok);
        assert!(missing.error.unwrap().contains("읽지 못했다"));
    }

    #[test]
    fn exit_code_follows_the_result_and_the_state_folder_stays_empty() {
        let sb = Sandbox::new();
        let file = write(&sb, GOOD);
        let dir = file.parent().unwrap();
        let args = |file| Args { file, json: true };
        let ok = run(&args(None), Some(dir)).unwrap();
        assert_eq!(format!("{ok:?}"), format!("{:?}", ExitCode::SUCCESS));
        fs::write(&file, "[[verify]]\nname = \"a\"\nrun = \"\"\n").unwrap();
        let bad = run(&args(Some(file.clone())), Some(dir)).unwrap();
        assert_eq!(format!("{bad:?}"), format!("{:?}", ExitCode::from(1)));
        assert_eq!(fs::read_dir(sb.chute_home()).unwrap().count(), 0);
    }
}
