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

mod slots {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use chute::ext::Ext;
    use chute::gitops::{Gitops, SlotName};
    use chute::model::{RepoInfo, Slug};
    use chute::store::Home;

    fn gitops(root: &Path) -> Gitops {
        let info = RepoInfo {
            slug: Slug::new("acme", "widget").unwrap(),
            path: root.join("work"),
            remote: "unused".into(),
            default_branch: "main".into(),
        };
        Gitops::new(&Ext::from_vars(|_| None), &Home::at(root), &info)
    }

    #[test]
    fn a_slot_has_one_holder_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let git = gitops(dir.path());

        let held = git.lock_slot(SlotName::Batch).unwrap().unwrap();
        assert!(git.lock_slot(SlotName::Batch).unwrap().is_none());
        assert!(git.lock_slot(SlotName::Revert).unwrap().is_some());
        drop(held);
        assert!(git.lock_slot(SlotName::Batch).unwrap().is_some());
        assert!(dir.path().join("slots/acme__widget/batch.lock").exists());
    }

    #[test]
    fn of_many_racing_holders_exactly_one_gets_the_slot() {
        let dir = tempfile::tempdir().unwrap();
        let git = gitops(dir.path());
        let barrier = Arc::new(Barrier::new(8));
        let racers: Vec<_> = (0..8)
            .map(|_| {
                let (git, barrier) = (git.clone(), Arc::clone(&barrier));
                thread::spawn(move || {
                    let lock = git.lock_slot(SlotName::Batch).unwrap();
                    barrier.wait();
                    lock.is_some()
                })
            })
            .collect();
        let winners = racers
            .into_iter()
            .map(|r| r.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
    }

    #[test]
    fn preflight_takes_the_first_free_slot_and_numbers_a_new_one_when_all_are_held() {
        let dir = tempfile::tempdir().unwrap();
        let git = gitops(dir.path());

        let first = git.lock_preflight_slot().unwrap();
        let second = git.lock_preflight_slot().unwrap();
        let third = git.lock_preflight_slot().unwrap();
        assert_eq!(first.name(), SlotName::Preflight(1));
        assert_eq!(second.name(), SlotName::Preflight(2));
        assert_eq!(third.name(), SlotName::Preflight(3));

        drop(second);
        assert_eq!(
            git.lock_preflight_slot().unwrap().name(),
            SlotName::Preflight(2)
        );
    }

    #[test]
    fn racing_preflights_all_get_different_slots() {
        let dir = tempfile::tempdir().unwrap();
        let git = gitops(dir.path());
        let barrier = Arc::new(Barrier::new(6));
        let racers: Vec<_> = (0..6)
            .map(|_| {
                let (git, barrier) = (git.clone(), Arc::clone(&barrier));
                thread::spawn(move || {
                    let lock = git.lock_preflight_slot().unwrap();
                    barrier.wait();
                    lock.name()
                })
            })
            .collect();
        let names: BTreeSet<SlotName> = racers.into_iter().map(|r| r.join().unwrap()).collect();
        let expected: BTreeSet<SlotName> = (1..=6).map(SlotName::Preflight).collect();
        assert_eq!(names, expected);
    }
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
