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
    use std::fs;
    use std::path::Path;
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    use chute::ext::Ext;
    use chute::gitops::{Gitops, SlotName};
    use chute::model::{RepoInfo, Slug};
    use chute::store::Home;

    fn gitops(root: &Path) -> Gitops {
        let info = RepoInfo {
            slug: Slug::new("acme", "widget").unwrap(),
            path: root.join("work"),
            remote: "unused".into(),
            remote_name: "origin".into(),
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
    fn preflight_takes_the_first_free_slot_and_waits_for_one_when_max_parallel_are_held() {
        let dir = tempfile::tempdir().unwrap();
        let git = gitops(dir.path());

        let first = git.lock_preflight_slot(2).unwrap();
        let second = git.lock_preflight_slot(2).unwrap();
        assert_eq!(first.name(), SlotName::Preflight(1));
        assert_eq!(second.name(), SlotName::Preflight(2));

        let (tx, rx) = mpsc::channel();
        let waiter = {
            let git = git.clone();
            thread::spawn(move || tx.send(git.lock_preflight_slot(2).unwrap().name()))
        };
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
        assert!(
            !dir.path()
                .join("slots/acme__widget/preflight-3.lock")
                .exists()
        );

        drop(second);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            SlotName::Preflight(2)
        );
        waiter.join().unwrap().unwrap();
    }

    #[test]
    fn a_slot_is_free_when_nobody_holds_it_and_its_name_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let git = gitops(dir.path());
        let held = git.lock_slot(SlotName::Preflight(3)).unwrap().unwrap();
        assert!(!git.slot_is_free(SlotName::Preflight(3)).unwrap());
        assert!(git.slot_is_free(SlotName::Preflight(4)).unwrap());
        drop(held);
        assert!(git.slot_is_free(SlotName::Preflight(3)).unwrap());

        for name in [SlotName::Batch, SlotName::Revert, SlotName::Preflight(12)] {
            assert_eq!(SlotName::parse(&name.to_string()), Some(name));
        }
        for text in [
            "preflight-0",
            "preflight-01",
            "preflight-",
            "preflight-x",
            "cache",
        ] {
            assert_eq!(SlotName::parse(text), None, "{text}");
        }
    }

    #[test]
    fn gc_offers_only_surplus_preflight_slots_nobody_holds_and_keeps_them_locked() {
        let dir = tempfile::tempdir().unwrap();
        let git = gitops(dir.path());
        for name in [
            SlotName::Preflight(1),
            SlotName::Preflight(2),
            SlotName::Preflight(3),
            SlotName::Preflight(4),
            SlotName::Preflight(5),
            SlotName::Batch,
        ] {
            fs::create_dir_all(git.slot_path(name)).unwrap();
        }
        let held = git.lock_slot(SlotName::Preflight(4)).unwrap().unwrap();

        let offered = git.lock_surplus_preflight_slots(2).unwrap();

        let names: Vec<_> = offered.iter().map(|lock| lock.name()).collect();
        assert_eq!(names, [SlotName::Preflight(3), SlotName::Preflight(5)]);
        assert!(
            git.lock_slot(SlotName::Preflight(3)).unwrap().is_none(),
            "a preflight cannot take a slot gc is about to remove"
        );
        assert!(git.lock_slot(SlotName::Preflight(2)).unwrap().is_some());
        drop(offered);
        assert!(git.lock_slot(SlotName::Preflight(3)).unwrap().is_some());
        drop(held);
        let all = git.lock_surplus_preflight_slots(0).unwrap();
        assert_eq!(all.len(), 5, "with no limit every slot folder is surplus");
        assert!(
            git.lock_surplus_preflight_slots(5).unwrap().is_empty(),
            "slots at or under max_parallel stay"
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
                    let lock = git.lock_preflight_slot(6).unwrap();
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
