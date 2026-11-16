mod engine_kit;
mod forward_kit;
mod support;

use std::process::Output;

use engine_kit::Lab;
use support::Reply;

fn up(lab: &Lab) -> Output {
    lab.sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .current_dir(lab.repo.path())
        .arg("up")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn up_starts_the_daemon_in_the_background_without_a_parent_in_the_repository() {
    let lab = Lab::new("");
    lab.sb
        .fake("hc")
        .respond("run *", Reply::ok(r#"{"address":"t1dd"}"#));

    let out = up(&lab);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("t1dd"), "{}", stdout(&out));
    let runs = lab.hc_calls("run");
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    for flag in ["--bg", "--no-parent"] {
        assert!(run.iter().any(|a| a == flag), "{flag}: {run:?}");
    }
    let value_of = |flag: &str| {
        let at = run.iter().position(|a| a == flag).unwrap();
        run[at + 1].clone()
    };
    assert_eq!(value_of("--name"), "chute-widget");
    assert_eq!(
        std::fs::canonicalize(value_of("--cwd")).unwrap(),
        std::fs::canonicalize(lab.repo.path()).unwrap()
    );
    assert_eq!(&run[run.len() - 3..], ["--", "chute", "daemon"]);
}

#[test]
fn up_does_nothing_when_a_daemon_already_runs() {
    let lab = Lab::new("");
    let _daemon = lab.store.try_lock_daemon().unwrap().unwrap();

    let out = up(&lab);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("이미 떠 있다"), "{}", stdout(&out));
    assert!(lab.hc_calls("run").is_empty());
}

#[test]
fn up_twice_in_a_row_starts_one_daemon() {
    let lab = Lab::new("");
    lab.sb
        .fake("hc")
        .respond("run *", Reply::ok(r#"{"address":"t1dd"}"#));

    assert!(up(&lab).status.success());
    let second = up(&lab);

    assert!(second.status.success());
    assert!(
        stdout(&second).contains("방금 띄웠다"),
        "{}",
        stdout(&second)
    );
    assert_eq!(lab.hc_calls("run").len(), 1);
}

#[test]
fn up_reports_it_when_hc_cannot_start_the_daemon() {
    let lab = Lab::new("");
    lab.sb
        .fake("hc")
        .respond("run *", Reply::ok("").code(5).stderr("코어가 없다"));

    let out = up(&lab);

    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("코어가 없다"), "{}", stderr(&out));
    let again = up(&lab);
    assert_eq!(
        lab.hc_calls("run").len(),
        2,
        "실패한 뒤에는 다시 띄울 수 있다: {}",
        stderr(&again)
    );
}

#[test]
fn up_takes_another_name_when_a_finished_daemon_terminal_still_holds_it() {
    let lab = Lab::new("");
    let hc = lab.sb.fake("hc");
    hc.respond(
        "run * --name chute-widget --cwd *",
        Reply::ok("").code(1).stderr(
            r#"{"error":{"kind":"name_taken","message":"이름이 쓰인다","hint":"다른 이름을 쓴다"}}"#,
        ),
    );
    hc.respond("run *", Reply::ok(r#"{"address":"t1dd"}"#));

    let out = up(&lab);

    assert!(out.status.success(), "{}", stderr(&out));
    let runs = lab.hc_calls("run");
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(
        runs[1].iter().any(|a| a == "chute-widget-2"),
        "{:?}",
        runs[1]
    );
}

#[test]
fn agents_starting_the_daemon_at_the_same_moment_start_one() {
    let lab = Lab::new("");
    let calls = lab.sb.root().join("slow-hc-calls");
    lab.sb.script(
        "hc",
        &format!(
            "echo \"$*\" >> '{}'\nsleep 0.4\necho '{{\"address\":\"t1dd\"}}'\n",
            calls.display()
        ),
    );
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let mut command = lab.sb.command(env!("CARGO_BIN_EXE_chute"));
            command
                .current_dir(lab.repo.path())
                .arg("up")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            command.spawn().unwrap()
        })
        .collect();
    for mut child in handles {
        assert!(child.wait().unwrap().success());
    }

    let started = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(started.lines().count(), 1, "{started}");
}

#[test]
fn up_on_another_pc_starts_the_daemon_on_the_hub_and_not_here() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    b.sb.fake("hc")
        .respond("run *", Reply::ok(r#"{"address":"t1dd"}"#));

    let out = a.chute(forward_kit::BIN, &["up"]);

    assert!(out.status.success(), "{}", forward_kit::err(&out));
    assert!(
        forward_kit::out(&out).contains("t1dd"),
        "{}",
        forward_kit::out(&out)
    );
    let runs: Vec<Vec<String>> =
        b.sb.fake("hc")
            .calls()
            .into_iter()
            .filter(|c| c[0] == "run")
            .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    for flag in ["--bg", "--no-parent"] {
        assert!(runs[0].iter().any(|x| x == flag), "{flag}: {:?}", runs[0]);
    }
    assert_eq!(&runs[0][runs[0].len() - 3..], ["--", "chute", "daemon"]);
    assert!(a.sb.fake("hc").calls().iter().all(|c| c[0] != "run"));
}

#[test]
fn up_on_another_pc_says_so_when_the_hub_is_down_or_this_is_not_the_hub() {
    let net = forward_kit::net();
    forward_kit::assert_unreachable_hub(&net, &["up"]);
    forward_kit::assert_not_hub_answer(&net, &["up"]);
    for node in &net.nodes {
        assert!(node.sb.fake("hc").calls().iter().all(|c| c[0] != "run"));
    }
}
