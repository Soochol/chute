mod engine_kit;
mod forward_kit;
mod support;

use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chute::model::{ItemState, Phase};
use engine_kit::Lab;
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;

const PATIENCE: Duration = Duration::from_secs(30);

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn chute(lab: &Lab, args: &[&str]) -> Command {
    let mut command = lab.sb.command(env!("CARGO_BIN_EXE_chute"));
    command.current_dir(lab.repo.path()).args(args);
    command
}

fn start_daemon(lab: &Lab) -> Daemon {
    let child = chute(lab, &["daemon"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Daemon(child)
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(25));
    }
}

fn exits(daemon: &mut Daemon) -> bool {
    daemon.0.try_wait().unwrap().is_some()
}

fn ok(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn is_gone(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None) == Err(Errno::ESRCH)
}

#[test]
fn a_second_daemon_ends_at_once_and_down_stops_the_first_after_its_batch() {
    let lab = Lab::new("");
    let mut first = start_daemon(&lab);
    wait_until("데몬이 뜨지 않았다", || {
        lab.store.daemon_running().unwrap()
    });
    let info = lab.store.daemon().unwrap().unwrap();
    assert_eq!(info.pid, first.0.id());
    assert!(info.doing.is_some());

    let started = Instant::now();
    let second = chute(&lab, &["daemon"]).output().unwrap();

    assert!(second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("이미 돌고 있다"));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!exits(&mut first), "첫 데몬은 계속 돈다");
    assert_eq!(lab.store.daemon().unwrap().unwrap().pid, first.0.id());

    let said = ok(chute(&lab, &["down"]).output().unwrap());
    assert!(said.contains("batch가 끝나면"), "{said}");
    wait_until("데몬이 멈추지 않았다", || exits(&mut first));
    assert!(first.0.wait().unwrap().success());
    assert!(!lab.store.daemon_running().unwrap());
    assert!(lab.store.daemon().unwrap().is_none());
    assert!(!lab.store.dir().join("stop").exists());
}

#[test]
fn the_daemon_merges_what_is_queued_and_what_a_dead_daemon_left_in_a_batch() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    lab.strand_in_batch();
    assert_eq!(lab.item_state(1), Some(ItemState::InBatch));
    lab.open_pr(2, &[("b.txt", "b\n")]);

    let mut daemon = start_daemon(&lab);
    wait_until("큐가 비지 않았다", || lab.state().queue.is_empty());

    assert_eq!(lab.merge_commits_on_main().len(), 2);
    lab.assert_every_pushed_commit_was_verified();
    ok(chute(&lab, &["down"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut daemon));
}

#[test]
fn down_now_ends_the_running_verification_and_leaves_nothing_in_a_batch() {
    let lab = Lab::new("echo $$ > '@ROOT@/verify.pid'\nsleep 120");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    let mut daemon = start_daemon(&lab);
    let pid_file = lab.sb.root().join("verify.pid");
    wait_until("검증이 시작되지 않았다", || {
        lab.state()
            .batch
            .is_some_and(|b| b.phase == Phase::Verifying)
            && pid_file.exists()
            && std::fs::read_to_string(&pid_file).is_ok_and(|t| t.trim().parse::<i32>().is_ok())
    });
    let verifying: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!is_gone(verifying));

    let said = ok(chute(&lab, &["down", "--now"]).output().unwrap());

    assert!(said.contains("멈췄다"), "{said}");
    assert!(
        !lab.store.daemon_running().unwrap(),
        "--now는 데몬이 멈춘 뒤에 돌아온다"
    );
    assert!(exits(&mut daemon) || daemon.0.wait().unwrap().success());
    assert!(lab.hc_calls("send").is_empty(), "취소는 반송이 아니다");
    assert!(lab.events().is_empty(), "{:?}", lab.events());
    wait_until("검증 프로세스가 남았다", || is_gone(verifying));
    let state = lab.state();
    assert!(state.batch.is_none());
    assert!(state.queue.iter().all(|i| i.state != ItemState::InBatch));
    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert!(!lab.store.daemon_running().unwrap());
    assert_eq!(lab.verify_runs(), 0);
}

#[test]
fn down_without_a_daemon_says_so_and_leaves_no_stop_request() {
    let lab = Lab::new("");

    let said = ok(chute(&lab, &["down"]).output().unwrap());

    assert!(said.contains("떠 있지 않다"), "{said}");
    assert!(!lab.store.dir().join("stop").exists());
}

#[test]
fn a_stop_request_left_by_an_earlier_run_does_not_stop_the_new_daemon() {
    let lab = Lab::new("");
    std::fs::create_dir_all(lab.store.dir()).unwrap();
    let stop = lab.store.dir().join("stop");
    std::fs::write(&stop, "now").unwrap();
    let long_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&stop)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();

    let mut daemon = start_daemon(&lab);
    wait_until("데몬이 뜨지 않았다", || {
        lab.store.daemon_running().unwrap()
    });
    thread::sleep(Duration::from_millis(600));

    assert!(!exits(&mut daemon));
    ok(chute(&lab, &["down", "--now"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut daemon));
}

#[test]
fn up_starts_the_daemon_again_right_after_down() {
    let lab = Lab::new("");
    lab.sb
        .fake("hc")
        .respond("run *", support::Reply::ok(r#"{"address":"t1dd"}"#));
    ok(chute(&lab, &["up"]).output().unwrap());
    let mut daemon = start_daemon(&lab);
    wait_until("데몬이 뜨지 않았다", || {
        lab.store.daemon_running().unwrap()
    });
    ok(chute(&lab, &["down", "--now"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut daemon));

    ok(chute(&lab, &["up"]).output().unwrap());

    assert_eq!(lab.hc_calls("run").len(), 2);
}

#[test]
fn a_stop_request_made_just_now_is_kept_when_the_daemon_starts() {
    let lab = Lab::new("");
    std::fs::create_dir_all(lab.store.dir()).unwrap();
    std::fs::write(lab.store.dir().join("stop"), "now").unwrap();

    let mut daemon = start_daemon(&lab);

    wait_until("방금 한 정지 요청이 무시됐다", || {
        exits(&mut daemon)
    });
}

#[test]
fn a_push_the_server_refuses_leaves_a_warning_and_the_pr_in_the_queue() {
    let lab = Lab::new("");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    let hook = lab.repo.origin().join("hooks/pre-receive");
    std::fs::write(
        &hook,
        "#!/bin/sh\nwhile read old new ref; do\n  [ \"$ref\" = refs/heads/main ] && { echo 'protected branch: PR required' >&2; exit 1; }\ndone\nexit 0\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut daemon = start_daemon(&lab);
    wait_until("경고가 남지 않았다", || {
        lab.state()
            .warnings
            .iter()
            .any(|w| w.kind == chute::model::WarningKind::Engine)
    });

    let warning = lab.state().warnings.remove(0);
    assert!(warning.message.contains("protected branch"), "{warning:?}");
    assert_eq!(lab.item_state(1), Some(ItemState::Queued(None)));
    assert_eq!(lab.verify_runs(), 1);
    ok(chute(&lab, &["down", "--now"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut daemon));
}

#[test]
fn a_daemon_killed_before_it_told_the_agent_tells_it_after_the_restart() {
    let lab = Lab::new("[ -e BAD ] && fail \"FAILED bad_test\"");
    let root = lab.sb.root();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hc/get.json");
    lab.sb.script(
        "hc",
        &format!(
            "echo \"$*\" >> '{root}/hc-calls'\nif [ \"$1\" = get ]; then\n  if [ -e '{root}/hc-hang' ]; then echo $$ > '{root}/hc-hang-pid'; sleep 60; fi\n  cat '{fixture}'\nfi\nexit 0\n",
            root = root.display(),
            fixture = fixture.display()
        ),
    );
    std::fs::write(root.join("hc-hang"), "").unwrap();
    lab.open_pr(1, &[("BAD", "")]);

    let mut first = start_daemon(&lab);
    wait_until("전달이 시작되지 않았다", || {
        root.join("hc-hang-pid").exists()
    });
    assert_eq!(
        lab.item_state(1),
        Some(ItemState::InBatch),
        "알리기 전에는 반송 상태를 쓰지 않는다"
    );
    first.0.kill().unwrap();
    first.0.wait().unwrap();
    let hung: i32 = std::fs::read_to_string(root.join("hc-hang-pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let _ = kill(Pid::from_raw(hung), nix::sys::signal::Signal::SIGKILL);
    std::fs::remove_file(root.join("hc-hang")).unwrap();

    let mut second = start_daemon(&lab);
    wait_until("다시 띄운 데몬이 알리지 않았다", || {
        std::fs::read_to_string(root.join("hc-calls"))
            .is_ok_and(|calls| calls.lines().any(|l| l.starts_with("send ")))
    });

    assert_eq!(
        lab.item_state(1),
        Some(ItemState::Bounced(chute::model::BounceReason::Verify))
    );
    ok(chute(&lab, &["down", "--now"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut second));
    let sends = std::fs::read_to_string(root.join("hc-calls"))
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("send "))
        .count();
    assert_eq!(sends, 1);
}

#[test]
fn down_now_also_ends_a_step_process_that_left_the_process_group() {
    let lab = Lab::new("timeout 300 sh -c \"echo \\$\\$ > '@ROOT@/leaked.pid'; exec sleep 300\"");
    lab.open_pr(1, &[("a.txt", "a\n")]);
    let mut daemon = start_daemon(&lab);
    let pid_file = lab.sb.root().join("leaked.pid");
    wait_until("검증 단계가 시작되지 않았다", || {
        std::fs::read_to_string(&pid_file).is_ok_and(|t| t.trim().parse::<i32>().is_ok())
    });
    let leaked: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!is_gone(leaked));

    ok(chute(&lab, &["down", "--now"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut daemon));

    wait_until("그룹을 떠난 프로세스가 남았다", || {
        is_gone(leaked)
    });
}

#[test]
fn a_new_daemon_clears_an_engine_warning_an_earlier_one_left() {
    let lab = Lab::new("");
    lab.store
        .update(|s| {
            s.warnings.push(chute::model::Warning {
                kind: chute::model::WarningKind::Engine,
                message: "낡은 경고".to_owned(),
                pr: None,
                at_ms: chute::model::EpochMs::now(),
            });
            Ok(())
        })
        .unwrap();

    let mut daemon = start_daemon(&lab);
    wait_until("낡은 경고가 지워지지 않았다", || {
        lab.state().warnings.is_empty()
    });

    ok(chute(&lab, &["down", "--now"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut daemon));
}

#[test]
fn a_daemon_whose_state_write_failed_after_telling_the_agent_recovers_the_pr_by_itself() {
    let lab = Lab::new("[ -e BAD ] && fail \"FAILED bad_test\"");
    lab.open_pr(1, &[("BAD", "")]);
    let root = lab.sb.root();
    let store_dir = lab.store.dir().to_path_buf();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hc/get.json");
    lab.sb.script(
        "hc",
        &format!(
            "echo \"$*\" >> '{root}/hc-calls'\ncase \"$1\" in\nget) cat '{fixture}';;\nsend)\n  if [ ! -e '{root}/fault-made' ]; then touch '{root}/fault-made'; mkdir -p '{store}/.state.json.tmp-'$PPID; echo $PPID > '{root}/fault-pid'; fi;;\n*) exit 97;;\nesac\n",
            root = root.display(),
            fixture = fixture.display(),
            store = store_dir.display(),
        ),
    );

    let mut daemon = start_daemon(&lab);
    wait_until("상태 쓰기가 실패하지 않았다", || {
        root.join("fault-pid").exists()
    });
    let pid = std::fs::read_to_string(root.join("fault-pid")).unwrap();
    wait_until("경고 없이 장애가 이어졌다", || {
        std::fs::read_to_string(root.join("hc-calls")).is_ok_and(|t| t.contains("send "))
    });
    std::fs::remove_dir(store_dir.join(format!(".state.json.tmp-{}", pid.trim()))).unwrap();

    wait_until("데몬이 스스로 PR을 되살리지 못했다", || {
        lab.item_state(1) == Some(ItemState::Bounced(chute::model::BounceReason::Verify))
            && lab.state().batch.is_none()
    });

    ok(chute(&lab, &["down", "--now"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut daemon));
}

#[test]
fn daemon_on_a_pc_that_is_not_the_hub_is_refused_and_starts_nothing() {
    let net = forward_kit::net();
    let a = net.node("a");

    let out = forward_kit::chute_within(a, &["daemon"], Duration::from_secs(20));

    assert_eq!(out.status.code(), Some(2), "{}", forward_kit::err(&out));
    let said = forward_kit::err(&out);
    assert!(said.contains("데몬은 허브에서만 돈다"), "{said}");
    assert!(said.contains("`b`"), "{said}");
    assert!(!forward_kit::store(a).daemon_running().unwrap());
    assert!(forward_kit::out(&out).is_empty());
}

#[test]
fn the_daemon_runs_gc_when_it_starts_and_leaves_the_records() {
    let lab = Lab::new("");
    let log = lab.store.dir().join("preflight/old-1");
    std::fs::create_dir_all(&log).unwrap();
    std::fs::write(log.join("a.log"), "x").unwrap();
    let long_ago = std::time::SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
    std::fs::File::open(&log)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    let record = lab.store.dir().join("batches/b1/batch.json");
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "{}").unwrap();

    let mut daemon = start_daemon(&lab);
    wait_until("시작할 때 정리가 돌지 않았다", || !log.exists());

    assert!(lab.store.dir().join("gc.json").is_file());
    assert!(record.exists());
    ok(chute(&lab, &["down", "--now"]).output().unwrap());
    wait_until("데몬이 멈추지 않았다", || exits(&mut daemon));
}
