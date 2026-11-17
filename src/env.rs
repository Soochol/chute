use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use nix::unistd::gethostname;
use serde::{Deserialize, Serialize};

use crate::ext::Ext;
use crate::machine;
use crate::model::{EnvCheck, EnvRecord};
use crate::verify::{self, Cancel, End};

pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_CHARS: usize = 200;
const READ_BYTES: u64 = 16 * 1024;
const LOG_LIMIT_BLOCKS: u32 = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSection {
    pub env: EnvRecord,
    #[serde(default)]
    pub preflight: Option<EnvRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    pub what: String,
    pub preflight: Option<String>,
    pub hub: Option<String>,
}

pub fn record(
    ext: &Ext,
    dir: &Path,
    log_dir: &Path,
    commands: &[String],
    cancel: &Cancel,
) -> EnvRecord {
    record_within(ext, dir, log_dir, commands, CHECK_TIMEOUT, cancel)
}

pub fn record_within(
    ext: &Ext,
    dir: &Path,
    log_dir: &Path,
    commands: &[String],
    timeout: Duration,
    cancel: &Cancel,
) -> EnvRecord {
    let _ = fs::create_dir_all(log_dir);
    EnvRecord {
        machine_id: machine::core(ext).ok().map(|core| core.id),
        host: gethostname()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        checks: commands
            .iter()
            .enumerate()
            .take_while(|_| !cancel.is_cancelled())
            .map(|(n, run)| {
                let log = log_dir.join(format!("env-{}.log", n + 1));
                check(ext, dir, run, &log, timeout, cancel)
            })
            .collect(),
    }
}

fn check(
    ext: &Ext,
    dir: &Path,
    run: &str,
    log: &Path,
    timeout: Duration,
    cancel: &Cancel,
) -> EnvCheck {
    let ran = verify::run_command(dir, &capped(run), log, timeout, ext.clock(), cancel);
    let (ok, note) = match ran {
        Ok(End::Passed) => (true, None),
        Ok(End::Failed(verify::Cause::TimedOut)) => (
            false,
            Some(format!("{}초 안에 끝나지 않아 멈췄다", timeout.as_secs())),
        ),
        Ok(End::Failed(_) | End::Cancelled) => (false, None),
        Err(e) => (false, Some(format!("{e:#}"))),
    };
    let first = read_head(log).and_then(|text| text.lines().map(clean).find(|l| !l.is_empty()));
    let output = match (first, note) {
        (Some(line), Some(note)) => format!("{line} ({note})"),
        (Some(line), None) => line,
        (None, Some(note)) => note,
        (None, None) => String::new(),
    };
    EnvCheck {
        run: run.to_owned(),
        output,
        ok,
    }
}

/// A command that prints without end stops at the file size limit instead of filling the disk.
fn capped(run: &str) -> String {
    format!("ulimit -f {LOG_LIMIT_BLOCKS} 2>/dev/null\n{run}")
}

fn read_head(log: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(log)
        .ok()?
        .take(READ_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}'
                | '\u{61c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e007f}'
        )
}

fn clean(line: &str) -> String {
    let plain: String = line
        .chars()
        .map(|c| if is_unsafe(c) { ' ' } else { c })
        .collect();
    plain.trim().chars().take(OUTPUT_CHARS).collect()
}

pub fn differences(preflight: &EnvRecord, hub: &EnvRecord) -> Vec<Difference> {
    let mut rows = Vec::new();
    if !same_machine(preflight, hub) {
        rows.push(Difference {
            what: "머신".to_owned(),
            preflight: Some(machine_line(preflight)),
            hub: Some(machine_line(hub)),
        });
    }
    let mut runs: Vec<&str> = Vec::new();
    for check in preflight.checks.iter().chain(&hub.checks) {
        if !runs.contains(&check.run.as_str()) {
            runs.push(&check.run);
        }
    }
    for run in runs {
        let (before, now) = (result_of(preflight, run), result_of(hub, run));
        if before != now {
            rows.push(Difference {
                what: run.to_owned(),
                preflight: before,
                hub: now,
            });
        }
    }
    rows
}

fn same_machine(a: &EnvRecord, b: &EnvRecord) -> bool {
    a.host == b.host
        && match (&a.machine_id, &b.machine_id) {
            (Some(x), Some(y)) => x == y,
            _ => true,
        }
}

pub fn machine_line(env: &EnvRecord) -> String {
    match &env.machine_id {
        Some(id) => format!("{} ({id})", env.host),
        None => env.host.clone(),
    }
}

pub fn result_text(check: &EnvCheck) -> String {
    if check.ok {
        check.output.clone()
    } else {
        format!("실패: {}", check.output)
    }
}

fn result_of(env: &EnvRecord, run: &str) -> Option<String> {
    env.checks
        .iter()
        .find(|check| check.run == run)
        .map(result_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;
    use crate::testkit::Sandbox;
    use std::time::Instant;

    /// Only catches a hang: a wait that gives up fails the test instead of deciding its outcome.
    const STUCK: Duration = Duration::from_secs(30);

    fn wait_until(ready: impl Fn() -> bool) {
        let give_up = Instant::now() + STUCK;
        while !ready() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn commands(list: &[&str]) -> Vec<String> {
        list.iter().map(|c| (*c).to_owned()).collect()
    }

    struct Fixture {
        sb: Sandbox,
        ext: Ext,
        dir: std::path::PathBuf,
        logs: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let sb = Sandbox::new();
        let ext = Ext::from_vars(|k| sb.var(k));
        let dir = sb.root().join("slot");
        let logs = sb.root().join("logs");
        fs::create_dir_all(&dir).unwrap();
        Fixture { sb, ext, dir, logs }
    }

    fn env_of(f: &Fixture, list: &[&str], timeout: Duration) -> EnvRecord {
        record_within(
            &f.ext,
            &f.dir,
            &f.logs,
            &commands(list),
            timeout,
            &Cancel::new(),
        )
    }

    #[test]
    fn the_machine_is_kept_when_env_check_is_empty() {
        let f = fixture();
        let env = env_of(&f, &[], CHECK_TIMEOUT);
        assert!(env.checks.is_empty());
        assert_eq!(
            env.machine_id.unwrap().as_str(),
            crate::testkit::SANDBOX_MACHINE_ID
        );
        assert!(!env.host.is_empty());
    }

    #[test]
    fn an_unreadable_core_leaves_the_machine_id_empty() {
        let f = fixture();
        f.sb.fake("hc").clear_rules().respond(
            "server status --json",
            crate::testkit::Reply::ok("{}").code(1),
        );
        let env = env_of(&f, &[], CHECK_TIMEOUT);
        assert_eq!(env.machine_id, None);
        assert!(!env.host.is_empty());
    }

    #[test]
    fn a_check_keeps_the_first_line_cut_to_200_chars() {
        let f = fixture();
        let long = "가".repeat(300);
        let env = env_of(
            &f,
            &[
                "printf 'first\\nsecond\\n'",
                &format!("echo {long}"),
                "echo oops >&2; echo late",
            ],
            CHECK_TIMEOUT,
        );
        assert_eq!(env.checks[0].output, "first");
        assert_eq!(env.checks[1].output.chars().count(), 200);
        assert_eq!(env.checks[2].output, "oops");
        assert!(env.checks.iter().all(|c| c.ok));
    }

    #[test]
    fn a_failing_check_is_recorded_and_the_next_one_still_runs() {
        let f = fixture();
        let env = env_of(&f, &["echo broken; exit 3", "echo fine"], CHECK_TIMEOUT);
        assert_eq!(
            env.checks,
            [
                EnvCheck {
                    run: "echo broken; exit 3".into(),
                    output: "broken".into(),
                    ok: false
                },
                EnvCheck {
                    run: "echo fine".into(),
                    output: "fine".into(),
                    ok: true
                },
            ]
        );
    }

    #[test]
    fn a_check_that_outlasts_the_limit_is_cut_and_the_next_one_still_runs() {
        let f = fixture();
        let clock = Clock::frozen();
        let ext = f.ext.clone().with_clock(clock.clone());
        let first_log = f.logs.join("env-1.log");
        let env = std::thread::scope(|s| {
            let run = s.spawn(|| {
                record_within(
                    &ext,
                    &f.dir,
                    &f.logs,
                    &commands(&["echo start; sleep 30", "echo after"]),
                    Duration::from_secs(3600),
                    &Cancel::new(),
                )
            });
            // The limit is read before the check starts, so once `start` shows, one jump past it
            // cuts the first check and the second check's limit is read from the moved clock.
            wait_until(|| fs::read_to_string(&first_log).is_ok_and(|log| log.contains("start")));
            clock.advance(Duration::from_secs(2 * 3600));
            run.join().unwrap()
        });
        assert!(!env.checks[0].ok, "{:?}", env.checks);
        assert!(
            env.checks[0].output.starts_with("start"),
            "{:?}",
            env.checks
        );
        assert_eq!(env.checks[1].output, "after", "{:?}", env.checks);
    }

    #[test]
    fn an_endless_check_is_bounded_in_time_and_log_size() {
        let f = fixture();
        let started = Instant::now();
        let env = env_of(&f, &["yes", "yes | tr -d '\\n'"], Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(env.checks.iter().all(|c| !c.ok));
        assert!(env.checks[0].output.starts_with('y'));
        for log in fs::read_dir(&f.logs).unwrap() {
            assert!(log.unwrap().metadata().unwrap().len() <= 256 * 512 + 4096);
        }
    }

    #[test]
    fn control_characters_in_the_output_are_replaced() {
        let f = fixture();
        let env = env_of(&f, &["printf 'a\\rb\\033[31mred\\0z\\n'"], CHECK_TIMEOUT);
        assert!(!env.checks[0].output.contains(|c: char| c.is_control()));
        assert!(env.checks[0].output.starts_with("a b"));
        let env = env_of(
            &f,
            &["printf '\\342\\200\\256evil\\342\\200\\250x\\n'"],
            CHECK_TIMEOUT,
        );
        assert_eq!(env.checks[0].output, "evil x");
        let env = env_of(
            &f,
            &["printf 'a\\330\\234b\\363\\240\\201\\201c\\n'"],
            CHECK_TIMEOUT,
        );
        assert_eq!(env.checks[0].output, "a b c");
    }

    #[test]
    fn cancelling_ends_the_running_check_and_skips_the_rest() {
        let f = fixture();
        let cancel = Cancel::new();
        let stopper = cancel.clone();
        let mark = f.dir.join("running");
        let thread = std::thread::spawn(move || {
            wait_until(|| mark.exists());
            stopper.cancel();
        });
        let started = Instant::now();
        let env = record_within(
            &f.ext,
            &f.dir,
            &f.logs,
            &commands(&["touch running; sleep 20", "echo never"]),
            CHECK_TIMEOUT,
            &cancel,
        );
        thread.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(env.checks.len(), 1, "{:?}", env.checks);
        assert!(!env.checks[0].ok);
    }

    #[test]
    fn checks_run_in_the_given_folder() {
        let f = fixture();
        fs::write(f.dir.join("marker"), "here").unwrap();
        let env = env_of(&f, &["cat marker"], CHECK_TIMEOUT);
        assert_eq!(env.checks[0].output, "here");
    }

    fn env(host: &str, checks: &[(&str, &str, bool)]) -> EnvRecord {
        EnvRecord {
            machine_id: None,
            host: host.into(),
            checks: checks
                .iter()
                .map(|(run, output, ok)| EnvCheck {
                    run: (*run).into(),
                    output: (*output).into(),
                    ok: *ok,
                })
                .collect(),
        }
    }

    #[test]
    fn differences_list_only_rows_that_disagree() {
        let pre = env(
            "laptop",
            &[("a", "1", true), ("b", "x", true), ("c", "p", true)],
        );
        let hub = env(
            "hub",
            &[("a", "1", true), ("b", "y", true), ("d", "q", false)],
        );
        let rows = differences(&pre, &hub);
        let whats: Vec<&str> = rows.iter().map(|r| r.what.as_str()).collect();
        assert_eq!(whats, ["머신", "b", "c", "d"]);
        assert_eq!(rows[2].hub, None);
        assert_eq!(rows[3].preflight, None);
        assert!(differences(&hub, &hub).is_empty());
        let mut named = hub.clone();
        named.machine_id = Some(crate::model::MachineId::try_from("9a0c".to_owned()).unwrap());
        assert!(differences(&named, &hub).is_empty());
    }
}
