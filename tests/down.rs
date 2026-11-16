mod forward_kit;
mod support;

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use forward_kit::{BIN, err, out, state_file, store};

#[test]
fn down_on_another_pc_asks_the_hubs_daemon_to_stop_after_the_batch() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    let _daemon = store(b).try_lock_daemon().unwrap().unwrap();
    let a_before = state_file(a);

    let result = a.chute(BIN, &["down"]);

    assert_eq!(result.status.code(), Some(0), "{}", err(&result));
    assert!(
        out(&result).contains("지금 batch가 끝나면 데몬이 멈춘다"),
        "{}",
        out(&result)
    );
    assert_eq!(
        std::fs::read_to_string(store(b).dir().join("stop")).unwrap(),
        "after"
    );
    assert!(!store(a).dir().join("stop").exists());
    assert_eq!(state_file(a), a_before);
}

#[test]
fn down_now_on_another_pc_waits_for_the_hubs_daemon_to_stop() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    let hub_store = store(b);
    let (ready, taken) = mpsc::channel();
    let daemon = thread::spawn(move || {
        let lock = hub_store.try_lock_daemon().unwrap().unwrap();
        ready.send(()).unwrap();
        let stop = hub_store.dir().join("stop");
        let deadline = Instant::now() + Duration::from_secs(30);
        while std::fs::read_to_string(&stop).ok().as_deref() != Some("now") {
            assert!(Instant::now() < deadline, "stop now가 오지 않았다");
            thread::sleep(Duration::from_millis(20));
        }
        drop(lock);
    });
    taken.recv().unwrap();

    let result = a.chute(BIN, &["down", "--now"]);
    daemon.join().unwrap();

    assert_eq!(result.status.code(), Some(0), "{}", err(&result));
    assert!(out(&result).contains("데몬을 멈췄다"), "{}", out(&result));
}

#[test]
fn down_on_another_pc_reports_a_hub_without_a_daemon_like_the_hub_does() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));

    let remote = a.chute(BIN, &["down"]);
    let direct = b.chute(BIN, &["down"]);

    assert_eq!(remote.status.code(), Some(0), "{}", err(&remote));
    assert_eq!(remote.stdout, direct.stdout);
    assert!(out(&remote).contains("데몬이 떠 있지 않다"));
}

#[test]
fn down_on_another_pc_says_so_when_the_hub_is_down_or_this_is_not_the_hub() {
    let net = forward_kit::net();
    forward_kit::assert_unreachable_hub(&net, &["down"]);
    forward_kit::assert_not_hub_answer(&net, &["down"]);
}
