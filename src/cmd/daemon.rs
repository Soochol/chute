use std::fs;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::sys::signal::{SigHandler, Signal, signal};

use super::Ctx;
use crate::engine::{Engine, Pass};
use crate::feedback;
use crate::model::{DaemonInfo, EpochMs, PrNumber};
use crate::store::RepoStore;
use crate::verify::Cancel;

const STOP_FILE: &str = "stop";
const WATCH_POLL: Duration = Duration::from_millis(100);
const IDLE_POLL: Duration = Duration::from_secs(1);
const BLOCKED_POLL: Duration = Duration::from_secs(15);
const ERROR_POLL: Duration = Duration::from_secs(10);

#[derive(clap::Args, Debug)]
pub struct Args {}

/// How `chute down` asks the daemon to stop. The request is a file in the state folder,
/// like everything else the CLI and the daemon say to each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    AfterBatch,
    Now,
}

pub fn request_stop(store: &RepoStore, stop: Stop) -> Result<()> {
    fs::create_dir_all(store.dir())?;
    let path = store.dir().join(STOP_FILE);
    let text = match stop {
        Stop::AfterBatch => "after",
        Stop::Now => "now",
    };
    fs::write(&path, text).with_context(|| format!("쓰지 못했다: {}", path.display()))
}

fn requested_stop(store: &RepoStore) -> Option<Stop> {
    match fs::read_to_string(store.dir().join(STOP_FILE)).ok()?.trim() {
        "after" => Some(Stop::AfterBatch),
        "now" => Some(Stop::Now),
        _ => None,
    }
}

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_: i32) {
    TERMINATED.store(true, Ordering::SeqCst);
}

#[derive(Default)]
struct Flags {
    after_batch: AtomicBool,
    now: AtomicBool,
    done: AtomicBool,
}

impl Flags {
    fn stopping(&self) -> bool {
        self.after_batch.load(Ordering::SeqCst) || self.now.load(Ordering::SeqCst)
    }
}

pub fn run(_args: Args, ctx: &Ctx) -> Result<ExitCode> {
    let (info, store) = ctx.registered_repo()?;
    let Some(_lock) = store.try_lock_daemon()? else {
        eprintln!("chute: {}의 데몬이 이미 돌고 있다", info.slug);
        return Ok(ExitCode::SUCCESS);
    };
    let _ = fs::remove_file(store.dir().join(STOP_FILE));
    super::up::clear_launch_marker(&store);
    let worktrees_root = feedback::worktrees_root(|key| std::env::var_os(key))?;
    let cancel = Cancel::new();
    let engine = Engine::new(
        ctx.ext.clone(),
        ctx.home.clone(),
        info.clone(),
        worktrees_root,
    )
    .with_cancel(cancel.clone());
    let mut me = DaemonInfo {
        pid: std::process::id(),
        terminal: ctx
            .terminal()
            .map(|t| crate::model::TerminalId(t.to_owned())),
        doing: None,
        started_ms: EpochMs::now(),
    };
    // SAFETY: the handler only stores into an atomic.
    unsafe {
        signal(Signal::SIGTERM, SigHandler::Handler(on_terminate))?;
        signal(Signal::SIGINT, SigHandler::Handler(on_terminate))?;
    }
    let flags = Arc::new(Flags::default());
    let watcher = {
        let (store, flags, cancel) = (store.clone(), flags.clone(), cancel.clone());
        thread::spawn(move || watch(&store, &flags, &cancel))
    };
    println!("chute 데몬을 시작한다: {} (pid {})", info.slug, me.pid);
    let served = serve(&engine, &store, &mut me, &flags);
    flags.done.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    let _ = store.clear_daemon();
    let _ = fs::remove_file(store.dir().join(STOP_FILE));
    super::up::clear_launch_marker(&store);
    served?;
    println!("chute 데몬을 멈췄다");
    Ok(ExitCode::SUCCESS)
}

fn watch(store: &RepoStore, flags: &Flags, cancel: &Cancel) {
    while !flags.done.load(Ordering::SeqCst) {
        let stop = if TERMINATED.load(Ordering::SeqCst) {
            Some(Stop::Now)
        } else {
            requested_stop(store)
        };
        match stop {
            Some(Stop::Now) => {
                flags.now.store(true, Ordering::SeqCst);
                cancel.cancel();
            }
            Some(Stop::AfterBatch) => flags.after_batch.store(true, Ordering::SeqCst),
            None => {}
        }
        thread::sleep(WATCH_POLL);
    }
}

fn doing(store: &RepoStore, me: &mut DaemonInfo, what: &str) {
    if me.doing.as_deref() != Some(what) {
        me.doing = Some(what.to_owned());
        if let Err(e) = store.write_daemon(me) {
            eprintln!("chute: daemon.json을 쓰지 못했다: {e:#}");
        }
    }
}

fn nap(flags: &Flags, how_long: Duration) {
    let until = Instant::now() + how_long;
    while !flags.stopping() && Instant::now() < until {
        thread::sleep(WATCH_POLL.min(how_long));
    }
}

fn prs(list: &[PrNumber]) -> String {
    let shown: Vec<String> = list.iter().map(ToString::to_string).collect();
    if shown.is_empty() {
        "없음".to_owned()
    } else {
        shown.join(" ")
    }
}

fn serve(engine: &Engine, store: &RepoStore, me: &mut DaemonInfo, flags: &Flags) -> Result<()> {
    let mut recovered = false;
    while !flags.stopping() {
        if !recovered {
            doing(store, me, "지난 실행이 남긴 것을 정리하는 중");
            match engine.recover() {
                Ok(()) => recovered = true,
                Err(e) => {
                    eprintln!("chute: 정리하지 못했다: {e:#}");
                    nap(flags, ERROR_POLL);
                    continue;
                }
            }
        }
        doing(store, me, "큐를 보는 중");
        match engine.run_once() {
            Ok(Pass::Idle) => nap(flags, IDLE_POLL),
            Ok(Pass::Done(summary)) => println!(
                "batch {} 끝: 머지 {} · 반송 {}",
                summary.batch.0,
                prs(&summary.merged),
                prs(&summary.bounced)
            ),
            Ok(Pass::Blocked) => {
                doing(store, me, "기준 커밋이 깨져 멈춤");
                nap(flags, BLOCKED_POLL);
            }
            Ok(Pass::Cancelled) => break,
            Err(e) => {
                eprintln!("chute: batch를 처리하지 못했다: {e:#}");
                doing(store, me, "오류 뒤 기다리는 중");
                nap(flags, ERROR_POLL);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Slug;
    use crate::store::Home;

    #[test]
    fn a_stop_request_is_read_back_and_anything_else_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Home::at(tmp.path()).repo(&Slug::new("acme", "widget").unwrap());
        assert_eq!(requested_stop(&store), None);

        request_stop(&store, Stop::AfterBatch).unwrap();
        assert_eq!(requested_stop(&store), Some(Stop::AfterBatch));
        request_stop(&store, Stop::Now).unwrap();
        assert_eq!(requested_stop(&store), Some(Stop::Now));

        fs::write(store.dir().join(STOP_FILE), "so").unwrap();
        assert_eq!(requested_stop(&store), None);
    }
}
