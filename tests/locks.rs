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

mod pc_slots {
    use std::fs::{self, File, OpenOptions};
    use std::path::Path;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use chute::config::Config;
    use chute::model::Slug;
    use chute::pc_limit::{self, Slot, Wait};
    use chute::store::{Home, RepoStore};
    use chute::verify::{self, Cancel, CommandEnd, Options, Outcome};

    fn home_with(machine_toml: Option<&str>) -> (tempfile::TempDir, Home) {
        let dir = tempfile::tempdir().unwrap();
        if let Some(text) = machine_toml {
            fs::write(dir.path().join("machine.toml"), text).unwrap();
        }
        let home = Home::at(dir.path());
        (dir, home)
    }

    fn take(home: &Home) -> File {
        match pc_limit::acquire(home.root(), &Cancel::new(), None).unwrap() {
            Slot::Held(file) => file,
            Slot::Cancelled => panic!("cancelled"),
        }
    }

    fn is_locked(path: &Path) -> bool {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .unwrap();
        matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock))
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < end, "{what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn max_verify_defaults_to_three_without_a_file() {
        let (dir, _) = home_with(None);
        assert_eq!(pc_limit::max_verify(dir.path()).unwrap(), 3);
    }

    #[test]
    fn max_verify_is_read_from_machine_toml() {
        let (dir, _) = home_with(Some("max_verify = 5\n"));
        assert_eq!(pc_limit::max_verify(dir.path()).unwrap(), 5);
    }

    #[test]
    fn a_bad_machine_toml_is_an_error_naming_the_file() {
        for text in [
            "max_verify = 0\n",
            "max_verify = -1\n",
            "max_verify = \"many\"\n",
            "max_verfy = 2\n",
            "max_verify = [\n",
        ] {
            let (dir, _) = home_with(Some(text));
            let path = dir.path().join("machine.toml");
            let err = pc_limit::max_verify(dir.path()).unwrap_err();
            assert!(
                format!("{err:#}").contains(&path.display().to_string()),
                "{text:?}: {err:#}"
            );
            assert!(pc_limit::acquire(dir.path(), &Cancel::new(), None).is_err());
        }
    }

    #[test]
    fn the_next_verification_waits_while_max_verify_slots_are_held_and_enters_when_one_is_released()
    {
        let (dir, home) = home_with(Some("max_verify = 2\n"));
        let first = take(&home);
        let second = take(&home);
        assert!(dir.path().join("machine/verify-1.lock").exists());
        assert!(dir.path().join("machine/verify-2.lock").exists());

        let events = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel();
        let waiter = {
            let (root, events) = (dir.path().to_owned(), Arc::clone(&events));
            thread::spawn(move || {
                let note = |wait: Wait| events.lock().unwrap().push(wait);
                let slot = pc_limit::acquire(&root, &Cancel::new(), Some(&note)).unwrap();
                tx.send(()).unwrap();
                slot
            })
        };
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
        assert!(!dir.path().join("machine/verify-3.lock").exists());
        assert_eq!(*events.lock().unwrap(), [Wait::Begin { running: 2 }]);

        drop(second);
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let slot = waiter.join().unwrap();
        assert!(matches!(slot, Slot::Held(_)));
        assert_eq!(
            *events.lock().unwrap(),
            [Wait::Begin { running: 2 }, Wait::End]
        );
        drop(first);
    }

    #[test]
    fn a_wait_that_fails_still_reports_its_end() {
        let (dir, home) = home_with(Some("max_verify = 1\n"));
        let _held = take(&home);
        let events = Arc::new(Mutex::new(Vec::new()));
        let waiter = {
            let (root, events) = (dir.path().to_owned(), Arc::clone(&events));
            thread::spawn(move || {
                let note = |wait: Wait| events.lock().unwrap().push(wait);
                pc_limit::acquire(&root, &Cancel::new(), Some(&note))
            })
        };
        wait_until("the wait never began", || {
            !events.lock().unwrap().is_empty()
        });
        fs::remove_dir_all(dir.path().join("machine")).unwrap();
        assert!(waiter.join().unwrap().is_err());
        assert_eq!(
            *events.lock().unwrap(),
            [Wait::Begin { running: 1 }, Wait::End]
        );
    }

    #[test]
    fn a_free_slot_is_taken_without_any_wait_notice() {
        let (dir, home) = home_with(Some("max_verify = 2\n"));
        let _held = take(&home);
        let events = Mutex::new(Vec::new());
        let note = |wait: Wait| events.lock().unwrap().push(wait);
        let slot = pc_limit::acquire(dir.path(), &Cancel::new(), Some(&note)).unwrap();
        assert!(matches!(slot, Slot::Held(_)));
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn a_cancel_during_the_wait_ends_it() {
        let (dir, home) = home_with(Some("max_verify = 1\n"));
        let _held = take(&home);
        let cancel = Cancel::new();
        let waiter = {
            let (root, cancel) = (dir.path().to_owned(), cancel.clone());
            thread::spawn(move || pc_limit::acquire(&root, &cancel, None).unwrap())
        };
        thread::sleep(Duration::from_millis(200));
        assert!(!waiter.is_finished());
        cancel.cancel();
        assert!(matches!(waiter.join().unwrap(), Slot::Cancelled));
    }

    const SERIAL: &str = "approval = \"agent\"\nserialize = true\n\n[[verify]]\nname = \"check\"\nrun = \"true\"\ntimeout = 60\n";

    fn serial_store(home: &Home) -> RepoStore {
        let store = home.repo(&Slug::new("acme", "widget").unwrap());
        fs::create_dir_all(store.dir()).unwrap();
        store
    }

    fn run_in_thread(
        config: &'static str,
        store: RepoStore,
        work: &Path,
        cancel: &Cancel,
    ) -> thread::JoinHandle<Outcome> {
        let (work, cancel) = (work.to_owned(), cancel.clone());
        thread::spawn(move || {
            let opts = Options {
                cancel,
                ..Options::default()
            };
            let config = Config::parse(config).unwrap();
            verify::run(&config, &store, &work, &work.join("logs"), &opts).unwrap()
        })
    }

    #[test]
    fn a_serialized_run_holds_the_repo_lock_while_it_waits_for_a_pc_slot() {
        let (dir, home) = home_with(Some("max_verify = 1\n"));
        let store = serial_store(&home);
        let repo_lock = store.dir().join("verify.lock");
        let work = dir.path().join("work");
        fs::create_dir_all(&work).unwrap();
        let held = take(&home);

        let cancel = Cancel::new();
        let run = run_in_thread(SERIAL, store.clone(), &work, &cancel);
        wait_until("the run never took the repo lock", || is_locked(&repo_lock));
        assert!(!run.is_finished());

        cancel.cancel();
        assert_eq!(run.join().unwrap(), Outcome::Cancelled);
        assert!(!is_locked(&repo_lock));
        drop(held);
    }

    #[test]
    fn a_serialized_run_waiting_for_the_repo_lock_holds_no_pc_slot() {
        let (dir, home) = home_with(Some("max_verify = 1\n"));
        let store = serial_store(&home);
        let repo_lock = store.dir().join("verify.lock");
        let work = dir.path().join("work");
        fs::create_dir_all(&work).unwrap();
        let other_run = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&repo_lock)
            .unwrap();
        other_run.try_lock().unwrap();

        let cancel = Cancel::new();
        let run = run_in_thread(SERIAL, store, &work, &cancel);
        thread::sleep(Duration::from_millis(300));
        assert!(!run.is_finished());
        drop(take(&home));

        cancel.cancel();
        assert_eq!(run.join().unwrap(), Outcome::Cancelled);
    }

    #[test]
    fn a_probe_waits_for_a_pc_slot_too() {
        let (dir, home) = home_with(Some("max_verify = 1\n"));
        let store = serial_store(&home);
        let work = dir.path().join("work");
        fs::create_dir_all(&work).unwrap();
        let held = take(&home);

        let cancel = Cancel::new();
        let probe = {
            let (store, work, cancel) = (store.clone(), work.clone(), cancel.clone());
            thread::spawn(move || {
                let config = Config::parse(
                    SERIAL
                        .replace("serialize = true", "serialize = false")
                        .as_str(),
                )
                .unwrap();
                let opts = Options {
                    cancel,
                    ..Options::default()
                };
                verify::run_probe(
                    &config,
                    &store,
                    &work,
                    &work.join("probe.log"),
                    "true",
                    Duration::from_secs(5),
                    &opts,
                )
                .unwrap()
            })
        };
        thread::sleep(Duration::from_millis(300));
        assert!(!probe.is_finished());
        cancel.cancel();
        assert_eq!(probe.join().unwrap(), CommandEnd::Cancelled);
        drop(held);
    }
}
