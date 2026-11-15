//! Lock semantics. Kept in its own test binary that never spawns a process: a child forked
//! by another test thread holds copies of open lock files until it execs, which makes
//! these checks flaky anywhere else.

use std::sync::Arc;
use std::thread;

use chute::model::Slug;
use chute::store::{Home, RepoStore};

fn store() -> (tempfile::TempDir, RepoStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = Home::at(dir.path()).repo(&Slug::new("acme", "widget").unwrap());
    (dir, store)
}

fn probe_in_threads(store: &Arc<RepoStore>) -> Vec<thread::JoinHandle<usize>> {
    (0..4)
        .map(|_| {
            let store = Arc::clone(store);
            thread::spawn(move || {
                (0..2000)
                    .filter(|_| store.daemon_running().unwrap())
                    .count()
            })
        })
        .collect()
}

#[test]
fn daemon_lock_shows_whether_the_daemon_runs() {
    let (_dir, store) = store();
    assert!(!store.daemon_running().unwrap());
    assert!(!store.dir().join("daemon.lock").exists());
    let held = store.try_lock_daemon().unwrap().unwrap();
    assert!(store.daemon_running().unwrap());
    assert!(store.try_lock_daemon().unwrap().is_none());
    drop(held);
    assert!(!store.daemon_running().unwrap());
}

#[test]
fn concurrent_probes_never_see_a_daemon() {
    let (_dir, store) = store();
    drop(store.try_lock_daemon().unwrap().unwrap());
    let store = Arc::new(store);
    let seen: usize = probe_in_threads(&store)
        .into_iter()
        .map(|p| p.join().unwrap())
        .sum();
    assert_eq!(seen, 0);
}

#[test]
fn probes_do_not_block_a_starting_daemon() {
    let (_dir, store) = store();
    drop(store.try_lock_daemon().unwrap().unwrap());
    let store = Arc::new(store);
    let probes = probe_in_threads(&store);
    let refused = (0..200)
        .filter(|_| store.try_lock_daemon().unwrap().is_none())
        .count();
    for p in probes {
        p.join().unwrap();
    }
    assert_eq!(refused, 0);
}
