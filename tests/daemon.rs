mod engine_kit;
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
    assert!(exits(&mut daemon) || daemon.0.wait().unwrap().success());
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
