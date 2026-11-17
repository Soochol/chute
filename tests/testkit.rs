mod support;

use std::time::Duration;

use chute::ext::{Ext, Tool};
use serde_json::Value;
use support::{Reply, Sandbox, TestRepo, fixed_wait, spawn, wait_until, wait_until_within};

#[test]
fn test_repo_has_a_bare_origin_and_pr_branches() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    let head = repo.pr_branch("feat/a", "a.txt", "a\n");

    let remote = repo.git(&["ls-remote", "origin", "refs/heads/feat/a"]);
    assert!(remote.starts_with(&head), "{remote}");
    assert_eq!(repo.git(&["branch", "--show-current"]).trim(), "main");
    assert!(!repo.path().join("a.txt").exists());
}

#[test]
fn fake_gh_and_hc_answer_from_fixtures_and_record_calls() {
    let sb = Sandbox::new();
    let repo = TestRepo::new(&sb, "acme/widget");
    sb.fake("gh")
        .respond(
            "pr view 12 *",
            Reply::file("tests/fixtures/gh/pr-view.json"),
        )
        .respond(
            "repo view *",
            Reply::file("tests/fixtures/gh/repo-view.json"),
        );
    sb.fake("hc")
        .respond("get t1rs*", Reply::file("tests/fixtures/hc/get.json"))
        .respond(
            "send t1rs *",
            Reply::ok("")
                .code(9)
                .stderr(include_str!("fixtures/hc/send-blocked.json")),
        )
        .respond(
            "run *",
            Reply::ok("{\"first_message\":{\"sent\":false}}").code(0),
        );
    let ext = Ext::from_vars(|k| sb.var(k));

    let pr: Value = serde_json::from_str(
        &ext.gh(
            repo.path(),
            ["pr", "view", "12", "--json", "number,headRefOid"],
        )
        .unwrap()
        .ok()
        .unwrap(),
    )
    .unwrap();
    assert_eq!(pr["number"], 12);
    let view = ext
        .gh(repo.path(), ["repo", "view", "--json", "defaultBranchRef"])
        .unwrap();
    assert!(view.stdout_text().contains("\"main\""));

    let get: Value = serde_json::from_str(&ext.hc(["get", "t1rs"]).unwrap().ok().unwrap()).unwrap();
    assert_eq!(get["state"], "live");
    let send = ext.hc(["send", "t1rs", "반송: 보고서\n두 줄"]).unwrap();
    assert_eq!(send.code, Some(9));
    let err: Value = serde_json::from_slice(&send.stderr).unwrap();
    assert_eq!(err["error"]["kind"], "blocked");
    let run = ext
        .call(
            Tool::Hc,
            None,
            ["run", "--bg", "--no-parent", "--", "claude"],
            Duration::from_secs(10),
        )
        .unwrap();
    assert!(run.success());
    let unknown = ext.hc(["rm", "t1rs"]).unwrap();
    assert_eq!(unknown.code, Some(97));

    assert_eq!(
        sb.fake("gh").calls(),
        [
            vec!["pr", "view", "12", "--json", "number,headRefOid"],
            vec!["repo", "view", "--json", "defaultBranchRef"],
        ]
    );
    assert_eq!(
        sb.fake("hc").calls(),
        [
            vec!["get", "t1rs"],
            vec!["send", "t1rs", "반송: 보고서\n두 줄"],
            vec!["run", "--bg", "--no-parent", "--", "claude"],
            vec!["rm", "t1rs"],
        ]
    );
}

#[test]
fn sandbox_points_home_and_state_into_the_temp_folder() {
    let sb = Sandbox::new();
    for key in [
        "HOME",
        "CHUTE_HOME",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_GLOBAL",
        "CHUTE_GH",
        "CHUTE_HC",
    ] {
        let value = sb.var(key).unwrap_or_else(|| panic!("{key}"));
        assert!(std::path::Path::new(&value).starts_with(sb.root()), "{key}");
    }
    assert_eq!(sb.var("XDG_STATE_HOME"), None);
}

#[test]
fn sandbox_clears_every_git_variable_of_the_parent_but_keeps_its_own() {
    if std::env::var_os("CHUTE_SANDBOX_PROBE").is_some() {
        let sb = Sandbox::new();
        let envs = sb.envs();
        let value = |key: &str| envs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        for key in [
            "GIT_OBJECT_DIRECTORY",
            "GIT_COMMON_DIR",
            "GIT_TEMPLATE_DIR",
            "GIT_DIR",
        ] {
            assert_eq!(value(key), Some(None), "{key}가 지워지지 않는다");
        }
        assert!(matches!(value("GIT_CONFIG_GLOBAL"), Some(Some(_))));
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "sandbox_clears_every_git_variable_of_the_parent_but_keeps_its_own",
            "--exact",
            "--test-threads=1",
        ])
        .env("CHUTE_SANDBOX_PROBE", "1")
        .env("GIT_OBJECT_DIRECTORY", "/nonexistent/objects")
        .env("GIT_COMMON_DIR", "/nonexistent/common")
        .env("GIT_TEMPLATE_DIR", "/nonexistent/template")
        .env("GIT_DIR", "/nonexistent/dir")
        .env("GIT_CONFIG_GLOBAL", "/nonexistent/config")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn wait_until_returns_as_soon_as_the_condition_holds() {
    let mut calls = 0;
    wait_until("세 번째 확인", || {
        calls += 1;
        calls == 3
    });
    assert_eq!(calls, 3);
}

#[test]
fn wait_until_fails_naming_what_it_waited_for_when_the_limit_passes() {
    let failure = std::panic::catch_unwind(|| {
        wait_until_within(Duration::ZERO, "오지 않는 상태", || false);
    })
    .unwrap_err();
    let message = failure.downcast_ref::<String>().unwrap();
    assert!(message.contains("오지 않는 상태"), "{message}");
}

#[test]
fn wait_until_checks_the_condition_once_even_when_the_limit_is_zero() {
    wait_until_within(Duration::ZERO, "이미 참인 상태", || true);
}

#[test]
fn fixed_wait_refuses_an_empty_or_blank_reason() {
    for reason in ["", "   ", "\n\t"] {
        assert!(
            std::panic::catch_unwind(|| fixed_wait(0, reason)).is_err(),
            "{reason:?}"
        );
    }
    fixed_wait(0, "아무 일도 일어나지 않아야 한다");
}

fn is_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(stat) => !stat
            .rsplit(')')
            .next()
            .unwrap()
            .trim_start()
            .starts_with('Z'),
    }
}

#[test]
fn dropping_spawned_kills_grandchildren_in_the_same_process_group() {
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", "sleep 300 & echo $!; wait"])
        .stdout(std::process::Stdio::piped());
    let mut child = spawn(&mut command);
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(child.stdout.take().unwrap()),
        &mut line,
    )
    .unwrap();
    let grandchild: u32 = line.trim().parse().unwrap();
    assert!(is_alive(grandchild));
    assert_eq!(child.try_wait().unwrap(), None);

    drop(child);

    wait_until("손자 프로세스가 사라진다", || {
        !is_alive(grandchild)
    });
}

#[test]
fn spawned_starts_a_new_process_group_led_by_the_child() {
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", "read line"])
        .stdin(std::process::Stdio::piped());
    let child = spawn(&mut command);
    let pgid = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(child.id() as i32))).unwrap();
    assert_eq!(pgid.as_raw() as u32, child.id());
    assert_ne!(pgid, nix::unistd::getpgrp());
}

#[test]
fn dropping_spawned_after_the_child_ended_does_not_fail() {
    let mut command = std::process::Command::new("true");
    let mut child = spawn(&mut command);
    assert!(child.wait().unwrap().success());
    drop(child);
}

#[test]
fn spawned_gives_the_output_of_a_finished_child() {
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", "echo done"])
        .stdout(std::process::Stdio::piped());
    let out = spawn(&mut command).wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
}
