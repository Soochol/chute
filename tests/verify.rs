mod support;

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use chute::config::{Config, VerifyStep};
use chute::model::{NonEmpty, Slug};
use chute::store::{Home, RepoStore};
use chute::verify::{Cancel, Cause, Detail, Failed, FlakyStep, Options, Outcome, log_path, run};
use support::Sandbox;

struct Rig {
    sb: Sandbox,
    store: RepoStore,
}

impl Rig {
    fn new() -> Rig {
        let sb = Sandbox::new();
        let store = Home::at(sb.chute_home()).repo(&Slug::new("acme", "widget").unwrap());
        for sub in ["work", "marks"] {
            fs::create_dir_all(sb.root().join(sub)).unwrap();
        }
        Rig { sb, store }
    }

    fn work(&self) -> PathBuf {
        self.sb.root().join("work")
    }

    fn logs(&self) -> PathBuf {
        self.sb.root().join("logs")
    }

    fn mark(&self, name: &str) -> PathBuf {
        self.sb.root().join("marks").join(name)
    }

    fn marks_dir(&self) -> String {
        self.sb.root().join("marks").display().to_string()
    }

    fn run(&self, config: &Config, opts: &Options) -> Outcome {
        run(config, &self.store, &self.work(), &self.logs(), opts).unwrap()
    }

    fn log(&self, step: &str, attempt: u32) -> String {
        fs::read_to_string(log_path(&self.logs(), step, attempt)).unwrap()
    }
}

fn step(name: &str, script: impl Into<String>) -> VerifyStep {
    VerifyStep {
        name: name.to_owned(),
        run: script.into(),
        timeout: Duration::from_secs(60),
        failed: None,
        probe: None,
        paths: None,
    }
}

fn with_failed(mut step: VerifyStep, pattern: &str) -> VerifyStep {
    step.failed = Some(regex::Regex::new(pattern).unwrap());
    step
}

const TEST_NAME: &str = r"test (\S+) \.\.\. FAILED";

fn config(steps: Vec<VerifyStep>, serialize: bool) -> Config {
    let mut config = Config::parse("[[verify]]\nname = \"x\"\nrun = \"true\"\n").unwrap();
    config.verify = NonEmpty::new(steps).unwrap();
    config.serialize = serialize;
    config
}

fn no_rerun() -> Options<'static> {
    Options {
        rerun_on_exit_failure: false,
        ..Options::default()
    }
}

fn failed(outcome: Outcome) -> Failed {
    match outcome {
        Outcome::Failed(failed) => failed,
        other => panic!("실패여야 한다: {other:?}"),
    }
}

fn passed(outcome: Outcome) -> Vec<FlakyStep> {
    match outcome {
        Outcome::Passed { flaky } => flaky,
        other => panic!("통과여야 한다: {other:?}"),
    }
}

fn count_lines(path: &Path) -> usize {
    fs::read_to_string(path).map_or(0, |text| text.lines().count())
}

fn wait_for(path: &Path) {
    let until = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < until, "{}가 생기지 않았다", path.display());
        thread::sleep(Duration::from_millis(10));
    }
}

fn pid_in(path: &Path) -> i32 {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .trim()
        .parse()
        .unwrap()
}

/// A zombie does not count as alive. An orphaned grandchild stays a zombie until init reaps it.
fn is_alive(pid: i32) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

#[test]
fn passing_steps_pass_and_each_leaves_a_log_with_both_streams() {
    let rig = Rig::new();
    let cfg = config(
        vec![
            step("a", "echo out-a; echo err-a >&2"),
            step("b", "echo out-b"),
        ],
        false,
    );
    assert_eq!(
        rig.run(&cfg, &Options::default()),
        Outcome::Passed { flaky: vec![] }
    );
    let a = rig.log("a", 1);
    assert!(a.contains("out-a") && a.contains("err-a"), "{a:?}");
    assert!(rig.log("b", 1).contains("out-b"));
    assert!(!log_path(&rig.logs(), "a", 2).exists());
}

#[test]
fn steps_run_in_the_given_folder_with_stdin_closed() {
    let rig = Rig::new();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(20),
            ..step(
                "here",
                format!(
                    "pwd > '{}/pwd'; [ /dev/stdin -ef /dev/null ]",
                    rig.marks_dir()
                ),
            )
        }],
        false,
    );
    passed(rig.run(&cfg, &Options::default()));
    let pwd = fs::read_to_string(rig.mark("pwd")).unwrap();
    assert_eq!(
        Path::new(pwd.trim()).canonicalize().unwrap(),
        rig.work().canonicalize().unwrap()
    );
}

#[test]
fn a_second_step_failing_twice_fails_the_run_and_the_third_never_runs() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![
            step("a", format!("echo a >> '{m}/a'")),
            step("b", format!("echo b >> '{m}/b'; echo boom >&2; exit 3")),
            step("c", format!("touch '{m}/c'")),
        ],
        false,
    );
    let f = failed(rig.run(&cfg, &Options::default()));
    assert_eq!(f.step, "b");
    assert_eq!(f.cause, Cause::Exited(3));
    assert_eq!(f.log, log_path(&rig.logs(), "b", 2));
    assert!(rig.log("b", 2).contains("boom"));
    assert_eq!(count_lines(&rig.mark("b")), 2);
    assert_eq!(count_lines(&rig.mark("a")), 1);
    assert!(!rig.mark("c").exists());
}

#[test]
fn a_step_that_passes_on_the_rerun_is_flaky_and_the_run_goes_on() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![
            with_failed(
                step(
                    "t",
                    format!(
                        "[ -e '{m}/seen' ] && exit 0; touch '{m}/seen'; \
                         echo 'test one ... FAILED'; echo 'test one ... FAILED'; \
                         echo 'test two ... FAILED'; exit 1"
                    ),
                ),
                TEST_NAME,
            ),
            step("after", format!("touch '{m}/after'")),
        ],
        false,
    );
    let flaky = passed(rig.run(&cfg, &Options::default()));
    assert_eq!(
        flaky,
        [FlakyStep {
            step: "t".into(),
            tests: vec!["one".into(), "two".into()]
        }]
    );
    assert!(rig.mark("after").exists());
    assert!(rig.log("t", 1).contains("test one"));
    assert!(!rig.log("t", 2).contains("FAILED"));
}

const FIRST_FAILS_ONCE: &str = "echo run >> '@M@/full'; [ -e '@M@/seen' ] && exit 0; touch '@M@/seen'; echo 'test one ... FAILED'; exit 1";

fn probed(rig: &Rig, run: &str, probe: &str) -> Config {
    let m = rig.marks_dir();
    let mut step = with_failed(step("t", run.replace("@M@", &m)), TEST_NAME);
    step.probe = Some(probe.replace("@M@", &m));
    config(vec![step], false)
}

fn with_probe_rerun() -> Options<'static> {
    Options {
        rerun_with_probe: true,
        ..Options::default()
    }
}

#[test]
fn without_the_probe_option_a_step_with_a_probe_is_rerun_whole() {
    let rig = Rig::new();
    let cfg = probed(
        &rig,
        FIRST_FAILS_ONCE,
        "echo probe >> '@M@/probe'; exit 1 # {tests}",
    );

    let flaky = passed(rig.run(&cfg, &Options::default()));

    assert_eq!(flaky[0].tests, ["one"]);
    assert_eq!(count_lines(&rig.mark("full")), 2);
    assert!(!rig.mark("probe").exists());
}

#[test]
fn with_the_probe_option_the_rerun_is_the_probe_on_the_failed_tests_only() {
    let rig = Rig::new();
    let cfg = probed(&rig, FIRST_FAILS_ONCE, "echo {tests} >> '@M@/probe'");

    let flaky = passed(rig.run(&cfg, &with_probe_rerun()));

    assert_eq!(
        flaky,
        [FlakyStep {
            step: "t".into(),
            tests: vec!["one".into()]
        }]
    );
    assert_eq!(count_lines(&rig.mark("full")), 1);
    assert_eq!(fs::read_to_string(rig.mark("probe")).unwrap(), "one\n");
}

#[test]
fn a_probe_that_fails_again_fails_the_step_with_its_own_log_and_tests() {
    let rig = Rig::new();
    let cfg = probed(
        &rig,
        FIRST_FAILS_ONCE,
        "echo 'test two ... FAILED'; echo {tests} > /dev/null; exit 1",
    );

    let f = failed(rig.run(&cfg, &with_probe_rerun()));

    assert_eq!(f.step, "t");
    assert_eq!(f.log, log_path(&rig.logs(), "t", 2));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["two".to_owned()]).unwrap())
    );
    assert_eq!(count_lines(&rig.mark("full")), 1);
}

#[test]
fn a_probe_log_without_test_names_keeps_the_names_of_the_first_attempt() {
    let rig = Rig::new();
    let cfg = probed(&rig, FIRST_FAILS_ONCE, "echo boom {tests}; exit 1");

    let f = failed(rig.run(&cfg, &with_probe_rerun()));

    assert_eq!(f.log, log_path(&rig.logs(), "t", 2));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["one".to_owned()]).unwrap())
    );
}

#[test]
fn a_failed_test_whose_name_does_not_fit_the_probe_makes_the_rerun_whole() {
    let rig = Rig::new();
    let run = "echo run >> '@M@/full'; [ -e '@M@/seen' ] && exit 0; touch '@M@/seen'; echo 'test one ... FAILED'; head -c 70000 /dev/zero | tr '\\0' x | sed 's/^/test /; s/$/ ... FAILED/'; echo; exit 1";
    let cfg = probed(
        &rig,
        run,
        "echo probe >> '@M@/probe'; echo {tests} > /dev/null",
    );

    let flaky = passed(rig.run(&cfg, &with_probe_rerun()));

    assert_eq!(flaky[0].tests.len(), 2);
    assert_eq!(count_lines(&rig.mark("full")), 2);
    assert!(!rig.mark("probe").exists());
}

#[test]
fn a_name_the_probe_command_cannot_carry_makes_the_rerun_whole() {
    let rig = Rig::new();
    let run = "echo run >> '@M@/full'; [ -e '@M@/seen' ] && exit 0; touch '@M@/seen'; printf 'test a\\000b ... FAILED\\n'; exit 1";
    let cfg = probed(
        &rig,
        run,
        "echo probe >> '@M@/probe'; echo {tests} > /dev/null",
    );

    let flaky = passed(rig.run(&cfg, &with_probe_rerun()));

    assert_eq!(flaky[0].tests, ["a\0b"]);
    assert_eq!(count_lines(&rig.mark("full")), 2);
    assert!(!rig.mark("probe").exists());
}

#[test]
fn a_probe_with_the_tests_twice_does_not_outgrow_the_argument_limit() {
    let rig = Rig::new();
    let run = "echo run >> '@M@/full'; [ -e '@M@/seen' ] && exit 0; touch '@M@/seen'; i=0; while [ $i -lt 1800 ]; do echo \"test name_padding_padding_$i ... FAILED\"; i=$((i+1)); done; exit 1";
    let cfg = probed(
        &rig,
        run,
        "echo probe >> '@M@/probe'; true {tests}; true {tests}",
    );

    let flaky = passed(rig.run(&cfg, &with_probe_rerun()));

    assert_eq!(flaky[0].tests.len(), 1800);
    assert_eq!(count_lines(&rig.mark("full")), 2);
}

#[test]
fn a_name_that_is_not_utf8_makes_the_rerun_whole() {
    let rig = Rig::new();
    let run = "echo run >> '@M@/full'; [ -e '@M@/seen' ] && exit 0; touch '@M@/seen'; printf 'test a\\377b ... FAILED\\n'; exit 1";
    let cfg = probed(
        &rig,
        run,
        "echo probe >> '@M@/probe'; echo {tests} > /dev/null",
    );

    passed(rig.run(&cfg, &with_probe_rerun()));

    assert_eq!(count_lines(&rig.mark("full")), 2);
    assert!(!rig.mark("probe").exists());
}

#[test]
fn a_probe_command_that_outgrows_the_argument_limit_makes_the_rerun_whole() {
    let rig = Rig::new();
    let run = "echo run >> '@M@/full'; [ -e '@M@/seen' ] && exit 0; touch '@M@/seen'; i=0; while [ $i -lt 900 ]; do printf 'test padding_padding_padding_padding_padding_padding_%05d ... FAILED\\n' $i; i=$((i+1)); done; exit 1";
    let probe = format!(
        "echo probe >> '@M@/probe'; : {{tests}} # {}",
        "x".repeat(90_000)
    );
    let cfg = probed(&rig, run, &probe);

    passed(rig.run(&cfg, &with_probe_rerun()));

    assert_eq!(count_lines(&rig.mark("full")), 2);
    assert!(!rig.mark("probe").exists());
}

#[test]
fn a_first_attempt_that_names_no_test_is_rerun_whole_even_with_a_probe() {
    let rig = Rig::new();
    let run =
        "echo run >> '@M@/full'; [ -e '@M@/seen' ] && exit 0; touch '@M@/seen'; echo boom; exit 1";
    let cfg = probed(&rig, run, "echo probe >> '@M@/probe'; echo {tests}");

    let flaky = passed(rig.run(&cfg, &with_probe_rerun()));

    assert!(flaky[0].tests.is_empty());
    assert_eq!(count_lines(&rig.mark("full")), 2);
    assert!(!rig.mark("probe").exists());
}

#[test]
fn with_the_rerun_off_the_first_failure_ends_the_run() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![
            step("b", format!("echo b >> '{m}/b'; exit 1")),
            step("c", format!("touch '{m}/c'")),
        ],
        false,
    );
    let f = failed(rig.run(&cfg, &no_rerun()));
    assert_eq!(f.log, log_path(&rig.logs(), "b", 1));
    assert_eq!(count_lines(&rig.mark("b")), 1);
    assert!(!log_path(&rig.logs(), "b", 2).exists());
    assert!(!rig.mark("c").exists());
}

#[test]
fn a_signal_death_is_told_apart_from_an_exit_code() {
    let rig = Rig::new();
    let cfg = config(vec![step("s", "kill -9 $$")], false);
    assert_eq!(failed(rig.run(&cfg, &no_rerun())).cause, Cause::Signaled(9));
}

#[test]
fn a_timed_out_step_is_not_rerun_and_leaves_no_grandchild() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![
            VerifyStep {
                timeout: Duration::from_secs(2),
                ..step(
                    "slow",
                    format!("echo run >> '{m}/runs'; sleep 300 & echo $! > '{m}/pid'; wait"),
                )
            },
            step("after", format!("touch '{m}/after'")),
        ],
        false,
    );
    let started = Instant::now();
    let f = failed(rig.run(&cfg, &Options::default()));
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_millis(3500),
        "{took:?}"
    );
    assert_eq!((f.step.as_str(), f.cause), ("slow", Cause::TimedOut));
    assert_eq!(f.log, log_path(&rig.logs(), "slow", 1));
    assert_eq!(count_lines(&rig.mark("runs")), 1);
    assert!(!rig.mark("after").exists());
    assert!(!is_alive(pid_in(&rig.mark("pid"))));
}

#[test]
fn a_group_that_ignores_sigterm_is_killed_once_the_grace_is_over() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(1),
            ..step(
                "stubborn",
                format!("trap '' TERM; sleep 300 & echo $! > '{m}/pid'; wait"),
            )
        }],
        false,
    );
    let opts = Options {
        kill_grace: Duration::from_millis(200),
        ..Options::default()
    };
    let started = Instant::now();
    let f = failed(rig.run(&cfg, &opts));
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(f.cause, Cause::TimedOut);
    assert!(!is_alive(pid_in(&rig.mark("pid"))));
}

#[test]
fn a_limit_too_large_to_represent_never_expires() {
    let rig = Rig::new();
    let toml = "[[verify]]\nname = \"a\"\nrun = \"true\"\ntimeout = 9223372036854775807\n";
    let cfg = Config::parse(toml).unwrap();
    passed(rig.run(&cfg, &Options::default()));
}

#[test]
fn a_timeout_on_the_rerun_ends_the_run_as_a_timeout() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(1),
            ..step(
                "t",
                format!("[ -e '{m}/seen' ] && sleep 300; touch '{m}/seen'; exit 1"),
            )
        }],
        false,
    );
    let f = failed(rig.run(&cfg, &Options::default()));
    assert_eq!(f.cause, Cause::TimedOut);
    assert_eq!(f.log, log_path(&rig.logs(), "t", 2));
}

#[test]
fn failed_tests_are_listed_once_in_the_order_they_appear() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step(
                "t",
                "echo 'test b ... FAILED'; echo 'test ok ... ok'; \
                 echo 'test a ... FAILED'; echo 'test b ... FAILED'; exit 1",
            ),
            TEST_NAME,
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &Options::default()));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["b".to_owned(), "a".to_owned()]).unwrap())
    );
}

#[test]
fn a_pattern_without_a_group_names_the_whole_match() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step("t", "echo 'FAILED x'; echo 'FAILED x'; exit 1"),
            "FAILED",
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &no_rerun()));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["FAILED".to_owned()]).unwrap())
    );
}

#[test]
fn without_a_pattern_the_end_of_the_log_is_reported() {
    let rig = Rig::new();
    let cfg = config(vec![step("t", "seq 1 100; exit 1")], false);
    let Detail::Tail(tail) = failed(rig.run(&cfg, &no_rerun())).detail else {
        panic!("로그 끝부분이어야 한다");
    };
    let expected: Vec<String> = (61..=100).map(|n| n.to_string()).collect();
    assert_eq!(tail, expected.join("\n"));
}

#[test]
fn a_pattern_that_matches_nothing_falls_back_to_the_end_of_the_log() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step("t", "echo 'error[E0425]: cannot find value'; exit 1"),
            TEST_NAME,
        )],
        false,
    );
    let Detail::Tail(tail) = failed(rig.run(&cfg, &no_rerun())).detail else {
        panic!("로그 끝부분이어야 한다");
    };
    assert!(tail.contains("E0425"), "{tail:?}");
}

#[test]
fn the_reported_tail_stays_small_and_whole_even_for_one_huge_line_of_wide_characters() {
    let rig = Rig::new();
    let cfg = config(
        vec![step("t", "printf '가%.0s' $(seq 1 10000); exit 1")],
        false,
    );
    let Detail::Tail(tail) = failed(rig.run(&cfg, &no_rerun())).detail else {
        panic!("로그 끝부분이어야 한다");
    };
    assert!(!tail.is_empty() && tail.len() <= 8 * 1024, "{}", tail.len());
    assert!(tail.chars().all(|c| c == '가'));
}

#[test]
fn only_the_end_of_an_enormous_log_is_searched() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step(
                "t",
                "echo 'test early ... FAILED'; \
                 yes \"$(head -c 999 /dev/zero | tr '\\0' y)\" | head -c 17000000; \
                 echo 'test late ... FAILED'; exit 1",
            ),
            TEST_NAME,
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &no_rerun()));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["late".to_owned()]).unwrap())
    );
}

#[test]
fn a_line_cut_by_the_window_is_not_mistaken_for_a_failure() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step(
                "t",
                "line='tests::very_long_name ... FAILED'; printf '%s\\n' \"$line\"; \
                 yes y | head -c $((16777216 + 5 - ${#line} - 1)); exit 1",
            ),
            r"(\S+) \.\.\. FAILED",
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &no_rerun()));
    assert!(matches!(f.detail, Detail::Tail(_)), "{:?}", f.detail);
}

#[test]
fn cancel_ends_the_running_group_and_the_result_is_cancelled() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![
            step("slow", format!("sleep 300 & echo $! > '{m}/pid'; wait")),
            step("after", format!("touch '{m}/after'")),
        ],
        false,
    );
    let cancel = Cancel::new();
    let (outcome, pid) = thread::scope(|s| {
        let run = s.spawn(|| {
            let opts = Options {
                cancel: cancel.clone(),
                ..Options::default()
            };
            rig.run(&cfg, &opts)
        });
        wait_for(&rig.mark("pid"));
        let pid = pid_in(&rig.mark("pid"));
        assert!(is_alive(pid), "시험의 생존 확인이 산 프로세스를 못 본다");
        let asked = Instant::now();
        cancel.cancel();
        let outcome = run.join().unwrap();
        assert!(
            asked.elapsed() < Duration::from_secs(3),
            "{:?}",
            asked.elapsed()
        );
        (outcome, pid)
    });
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(!is_alive(pid));
    assert!(!rig.mark("after").exists());
}

#[test]
fn a_cancel_before_the_start_runs_nothing() {
    let rig = Rig::new();
    let cfg = config(
        vec![step("a", format!("touch '{}/a'", rig.marks_dir()))],
        true,
    );
    let heard = RefCell::new(0);
    let record = |_: &str| *heard.borrow_mut() += 1;
    let opts = Options {
        on_step: Some(&record),
        ..Options::default()
    };
    opts.cancel.cancel();
    assert_eq!(rig.run(&cfg, &opts), Outcome::Cancelled);
    assert!(!rig.mark("a").exists());
    assert_eq!(*heard.borrow(), 0);
    assert!(!log_path(&rig.logs(), "a", 1).exists());
}

fn holder(rig: &Rig) -> Config {
    let m = rig.marks_dir();
    config(
        vec![
            step("holder-first", format!("touch '{m}/a-started'")),
            VerifyStep {
                timeout: Duration::from_secs(30),
                ..step(
                    "holder-second",
                    format!(
                        "while [ ! -e '{m}/release' ]; do sleep 0.02; done; touch '{m}/a-done'"
                    ),
                )
            },
        ],
        true,
    )
}

#[test]
fn serialize_makes_a_second_run_wait_for_the_first() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let first = holder(&rig);
    let second = config(
        vec![step(
            "second",
            format!("[ -e '{m}/a-done' ] && touch '{m}/b-after-a'; touch '{m}/b-ran'"),
        )],
        true,
    );
    let (a, b, b_ran_early) = thread::scope(|s| {
        let a = s.spawn(|| rig.run(&first, &Options::default()));
        wait_for(&rig.mark("a-started"));
        let b = s.spawn(|| rig.run(&second, &Options::default()));
        thread::sleep(Duration::from_millis(300));
        let early = rig.mark("b-ran").exists();
        fs::write(rig.mark("release"), "").unwrap();
        (a.join().unwrap(), b.join().unwrap(), early)
    });
    assert!(!b_ran_early, "첫 검증이 도는 동안 둘째가 돌았다");
    passed(a);
    passed(b);
    assert!(rig.mark("b-after-a").exists());
}

#[test]
fn a_cancel_ends_the_wait_for_the_lock_without_running_anything() {
    let rig = Rig::new();
    let first = holder(&rig);
    let second = config(
        vec![step("second", format!("touch '{}/b-ran'", rig.marks_dir()))],
        true,
    );
    let cancel = Cancel::new();
    let logs_b = rig.sb.root().join("logs-b");
    let (a, b) = thread::scope(|s| {
        let a = s.spawn(|| rig.run(&first, &Options::default()));
        wait_for(&rig.mark("a-started"));
        let b = s.spawn(|| {
            let opts = Options {
                cancel: cancel.clone(),
                ..Options::default()
            };
            run(&second, &rig.store, &rig.work(), &logs_b, &opts).unwrap()
        });
        wait_for(&rig.sb.root().join("logs-b"));
        thread::sleep(Duration::from_millis(200));
        cancel.cancel();
        let b = b.join().unwrap();
        fs::write(rig.mark("release"), "").unwrap();
        (a.join().unwrap(), b)
    });
    assert_eq!(b, Outcome::Cancelled);
    assert!(!rig.mark("b-ran").exists());
    passed(a);
}

#[test]
fn without_serialize_two_runs_overlap() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let meets = |me: &str, other: &str| {
        config(
            vec![step(
                me,
                format!(
                    "touch '{m}/{me}'; i=0; \
                     while [ ! -e '{m}/{other}' ]; do i=$((i+1)); [ $i -gt 200 ] && exit 1; sleep 0.05; done"
                ),
            )],
            false,
        )
    };
    let (first, second) = (meets("one", "two"), meets("two", "one"));
    let (a, b) = thread::scope(|s| {
        let a = s.spawn(|| rig.run(&first, &no_rerun()));
        let b = s.spawn(|| rig.run(&second, &no_rerun()));
        (a.join().unwrap(), b.join().unwrap())
    });
    passed(a);
    passed(b);
}

#[test]
fn every_run_has_its_own_run_id_and_its_steps_share_it() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![
            step("one", format!("printf %s \"$CHUTE_RUN_ID\" > '{m}/one'")),
            step("two", format!("printf %s \"$CHUTE_RUN_ID\" > '{m}/two'")),
        ],
        false,
    );
    let read = |name: &str| fs::read_to_string(rig.mark(name)).unwrap();
    passed(rig.run(&cfg, &Options::default()));
    let first = read("one");
    assert!(!first.is_empty());
    assert_eq!(read("two"), first);
    passed(rig.run(&cfg, &Options::default()));
    assert_ne!(read("one"), first);
    assert_eq!(read("two"), read("one"));
}

#[test]
fn runs_started_together_never_share_a_run_id() {
    const RUNS: usize = 8;
    let rig = Rig::new();
    let m = rig.marks_dir();
    let start = Barrier::new(RUNS);
    thread::scope(|s| {
        for i in 0..RUNS {
            let (rig, start, m) = (&rig, &start, &m);
            s.spawn(move || {
                let cfg = config(
                    vec![step(
                        &format!("id{i}"),
                        format!("printf %s \"$CHUTE_RUN_ID\" > '{m}/id{i}'"),
                    )],
                    false,
                );
                start.wait();
                passed(rig.run(&cfg, &Options::default()));
            });
        }
    });
    let ids: HashSet<String> = (0..RUNS)
        .map(|i| fs::read_to_string(rig.mark(&format!("id{i}"))).unwrap())
        .collect();
    assert_eq!(ids.len(), RUNS);
}

#[test]
fn on_step_hears_each_step_once_even_when_it_is_rerun() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![
            step("a", "true"),
            step(
                "b",
                format!("[ -e '{m}/seen' ] && exit 0; touch '{m}/seen'; exit 1"),
            ),
            step("c", "true"),
        ],
        false,
    );
    let heard = RefCell::new(Vec::new());
    let record = |name: &str| heard.borrow_mut().push(name.to_owned());
    let opts = Options {
        on_step: Some(&record),
        ..Options::default()
    };
    let flaky = passed(rig.run(&cfg, &opts));
    assert_eq!(*heard.borrow(), ["a", "b", "c"]);
    assert_eq!(flaky.len(), 1);
}

#[test]
fn a_missing_folder_is_an_error_and_not_a_failed_run() {
    let rig = Rig::new();
    let cfg = config(vec![step("a", "true")], false);
    let missing = rig.sb.root().join("gone");
    let result = run(&cfg, &rig.store, &missing, &rig.logs(), &Options::default());
    assert!(result.is_err());
}

#[test]
fn a_background_process_of_a_passed_step_does_not_outlive_the_run() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "bg",
            format!("sleep 300 & echo $! > '{m}/pid'; exit 0"),
        )],
        true,
    );
    passed(rig.run(&cfg, &Options::default()));
    assert!(!is_alive(pid_in(&rig.mark("pid"))));
}

#[test]
fn a_background_process_of_a_failed_attempt_is_gone_before_the_rerun() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "t",
            format!(
                "if [ -e '{m}/seen' ]; then \
                   kill -0 \"$(cat '{m}/pid')\" 2>/dev/null && touch '{m}/stray-seen'; exit 0; fi; \
                 touch '{m}/seen'; sleep 300 & echo $! > '{m}/pid'; exit 1"
            ),
        )],
        false,
    );
    assert_eq!(passed(rig.run(&cfg, &Options::default())).len(), 1);
    assert!(!rig.mark("stray-seen").exists());
    assert!(!is_alive(pid_in(&rig.mark("pid"))));
}

#[test]
fn a_step_that_truncates_the_log_by_reopening_dev_stderr_leaves_no_hole_in_it() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step(
                "t",
                "echo 'test early ... FAILED'; echo 'test mid ... FAILED' > /dev/stderr; \
                 echo 'test late ... FAILED'; exit 1",
            ),
            TEST_NAME,
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &no_rerun()));
    let log = rig.log("t", 1);
    assert!(!log.contains('\0'), "{log:?}");
    assert!(log.ends_with("test late ... FAILED\n"), "{log:?}");
    let Detail::Tests(names) = f.detail else {
        panic!("시험 이름이어야 한다");
    };
    assert!(names.contains(&"late".to_owned()));
}

#[test]
fn a_step_that_makes_its_output_nonblocking_does_not_break_the_writers_sharing_it() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "nb",
            format!(
                "perl -MFcntl -e 'fcntl(STDOUT, F_SETFL, fcntl(STDOUT, F_GETFL, 0) | O_NONBLOCK) or die'; \
                 head -c 5000000 /dev/zero | tr '\\0' x > '{m}/big'; \
                 for i in 1 2 3 4; do cat '{m}/big' & done; wait"
            ),
        )],
        false,
    );
    passed(rig.run(&cfg, &no_rerun()));
    assert_eq!(
        fs::metadata(log_path(&rig.logs(), "nb", 1)).unwrap().len(),
        20_000_000
    );
}

#[test]
fn a_process_an_earlier_run_left_behind_cannot_write_into_the_next_runs_log() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let leaves_a_writer = config(
        vec![step(
            "t",
            format!(
                "setsid sh -c \"touch '{m}/escaped'; sleep 1; echo 'test ghost ... FAILED'\" & \
                 while [ ! -e '{m}/escaped' ]; do sleep 0.01; done"
            ),
        )],
        false,
    );
    passed(rig.run(&leaves_a_writer, &no_rerun()));
    let next = config(
        vec![with_failed(
            step("t", "echo 'test real ... FAILED'; sleep 1.5; exit 1"),
            TEST_NAME,
        )],
        false,
    );
    let f = failed(rig.run(&next, &no_rerun()));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["real".to_owned()]).unwrap())
    );
}

#[test]
fn a_huge_kill_grace_does_not_panic() {
    let rig = Rig::new();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(1),
            ..step("slow", "sleep 300")
        }],
        false,
    );
    let opts = Options {
        kill_grace: Duration::MAX,
        ..Options::default()
    };
    assert_eq!(failed(rig.run(&cfg, &opts)).cause, Cause::TimedOut);
}

#[test]
fn a_log_left_by_an_earlier_run_does_not_outlive_a_rerun_that_was_not_needed() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "a",
            format!("[ -e '{m}/ok' ] && echo new-run && exit 0; echo old-run; exit 1"),
        )],
        false,
    );
    failed(rig.run(&cfg, &Options::default()));
    assert_eq!(rig.log("a", 2), "old-run\n");
    fs::write(rig.mark("ok"), "").unwrap();
    passed(rig.run(&cfg, &Options::default()));
    assert_eq!(rig.log("a", 1), "new-run\n");
    assert!(!log_path(&rig.logs(), "a", 2).exists());
}

#[test]
fn a_pattern_with_alternatives_names_the_group_that_took_part() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step("t", "echo 'FAIL alpha'; echo 'ERROR beta'; exit 1"),
            r"FAIL (\w+)|ERROR (\w+)",
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &no_rerun()));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["alpha".to_owned(), "beta".to_owned()]).unwrap())
    );
}

#[test]
fn every_flaky_step_is_listed_in_order() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let once = |name: &str| {
        step(
            name,
            format!("[ -e '{m}/{name}' ] && exit 0; touch '{m}/{name}'; exit 1"),
        )
    };
    let cfg = config(vec![once("a"), step("ok", "true"), once("b")], false);
    let names: Vec<String> = passed(rig.run(&cfg, &Options::default()))
        .into_iter()
        .map(|f| f.step)
        .collect();
    assert_eq!(names, ["a", "b"]);
}

#[test]
fn the_lock_lives_in_the_state_folder_and_does_not_block_state_updates() {
    let rig = Rig::new();
    let first = holder(&rig);
    let (updated_while_held, a) = thread::scope(|s| {
        let a = s.spawn(|| rig.run(&first, &Options::default()));
        wait_for(&rig.mark("a-started"));
        let (tx, rx) = mpsc::channel();
        let store = &rig.store;
        s.spawn(move || {
            store.update(|_| Ok(())).unwrap();
            tx.send(()).unwrap();
        });
        let updated = rx.recv_timeout(Duration::from_secs(3)).is_ok();
        fs::write(rig.mark("release"), "").unwrap();
        (updated, a.join().unwrap())
    });
    passed(a);
    assert!(
        updated_while_held,
        "RepoStore::update waited for the verification"
    );
    assert!(rig.store.dir().join("verify.lock").exists());
}

#[test]
fn verifications_of_different_repositories_do_not_wait_for_each_other() {
    let rig = Rig::new();
    let other = Home::at(rig.sb.chute_home()).repo(&Slug::new("acme", "gadget").unwrap());
    let first = holder(&rig);
    let quick = config(vec![step("quick", "true")], true);
    let (finished, a) = thread::scope(|s| {
        let a = s.spawn(|| rig.run(&first, &Options::default()));
        wait_for(&rig.mark("a-started"));
        let (tx, rx) = mpsc::channel();
        let (work, logs, other, quick) = (rig.work(), rig.sb.root().join("logs-b"), &other, &quick);
        s.spawn(move || {
            tx.send(run(quick, other, &work, &logs, &Options::default()).unwrap())
                .unwrap();
        });
        let outcome = rx.recv_timeout(Duration::from_secs(3));
        fs::write(rig.mark("release"), "").unwrap();
        (outcome, a.join().unwrap())
    });
    passed(a);
    passed(finished.expect("the other repository waited for the first one's lock"));
}

#[test]
fn kill_grace_is_the_time_a_group_gets_between_sigterm_and_sigkill() {
    let rig = Rig::new();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(1),
            ..step("stubborn", "trap '' TERM; sleep 300 & wait")
        }],
        false,
    );
    let opts = Options {
        kill_grace: Duration::from_secs(1),
        ..Options::default()
    };
    let started = Instant::now();
    failed(rig.run(&cfg, &opts));
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_secs(4),
        "{took:?}"
    );
}

#[test]
fn a_timed_out_step_still_reports_the_tests_that_failed_before_it() {
    let rig = Rig::new();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(1),
            ..with_failed(
                step("t", "echo 'test slow ... FAILED'; sleep 300"),
                TEST_NAME,
            )
        }],
        false,
    );
    let f = failed(rig.run(&cfg, &Options::default()));
    assert_eq!(f.cause, Cause::TimedOut);
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["slow".to_owned()]).unwrap())
    );
}

#[test]
fn a_grandchild_that_ignores_sigterm_dies_even_when_the_leader_exits_on_it() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(1),
            ..step(
                "mixed",
                format!("sh -c \"trap '' TERM; echo \\$\\$ > '{m}/pid'; exec sleep 300\" & wait"),
            )
        }],
        false,
    );
    let opts = Options {
        kill_grace: Duration::from_millis(300),
        ..Options::default()
    };
    assert_eq!(failed(rig.run(&cfg, &opts)).cause, Cause::TimedOut);
    assert!(!is_alive(pid_in(&rig.mark("pid"))));
}

#[test]
fn a_process_that_left_the_group_cannot_hold_the_run_open() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "escape",
            format!(
                "setsid sh -c \"touch '{m}/escaped'; exec sleep 4\" & \
                 while [ ! -e '{m}/escaped' ]; do sleep 0.01; done"
            ),
        )],
        false,
    );
    let started = Instant::now();
    passed(rig.run(&cfg, &Options::default()));
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_cancel_during_the_rerun_is_a_cancel_and_not_a_failure() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "t",
            format!(
                "if [ -e '{m}/seen' ]; then touch '{m}/second'; sleep 300; fi; touch '{m}/seen'; exit 1"
            ),
        )],
        false,
    );
    let cancel = Cancel::new();
    let outcome = thread::scope(|s| {
        let run = s.spawn(|| {
            let opts = Options {
                cancel: cancel.clone(),
                ..Options::default()
            };
            rig.run(&cfg, &opts)
        });
        wait_for(&rig.mark("second"));
        cancel.cancel();
        run.join().unwrap()
    });
    assert_eq!(outcome, Outcome::Cancelled);
}

#[test]
fn a_reused_log_folder_is_overwritten_and_not_appended() {
    let rig = Rig::new();
    let cfg = config(vec![step("a", "echo once")], false);
    passed(rig.run(&cfg, &Options::default()));
    passed(rig.run(&cfg, &Options::default()));
    assert_eq!(rig.log("a", 1), "once\n");
}

#[test]
fn on_step_is_called_before_the_step_runs() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(vec![step("a", format!("touch '{m}/ran-a'"))], false);
    let ran_already = RefCell::new(None);
    let record = |_: &str| *ran_already.borrow_mut() = Some(rig.mark("ran-a").exists());
    let opts = Options {
        on_step: Some(&record),
        ..Options::default()
    };
    passed(rig.run(&cfg, &opts));
    assert_eq!(*ran_already.borrow(), Some(false));
}

#[test]
fn a_rerun_runs_in_the_same_folder_with_the_same_run_id() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "t",
            format!(
                "pwd >> '{m}/pwds'; printf '%s\n' \"$CHUTE_RUN_ID\" >> '{m}/ids'; \
                 [ -e '{m}/seen' ] && exit 0; touch '{m}/seen'; exit 1"
            ),
        )],
        false,
    );
    assert_eq!(passed(rig.run(&cfg, &Options::default())).len(), 1);
    for file in ["pwds", "ids"] {
        let text = fs::read_to_string(rig.mark(file)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines.len() == 2 && lines[0] == lines[1] && !lines[0].is_empty(),
            "{file}: {lines:?}"
        );
    }
}

#[test]
fn a_step_killed_by_a_signal_is_rerun_like_any_other_failure() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "t",
            format!("[ -e '{m}/seen' ] && exit 0; touch '{m}/seen'; kill -9 $$"),
        )],
        false,
    );
    assert_eq!(passed(rig.run(&cfg, &Options::default())).len(), 1);
}

#[test]
fn empty_names_are_dropped_and_the_first_group_names_the_test() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step("t", "echo 'FAIL '; echo 'FAIL suite:case'; exit 1"),
            r"FAIL (\w*):?(\w*)",
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &no_rerun()));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["suite".to_owned()]).unwrap())
    );
}

#[test]
fn a_failure_within_the_searched_end_of_a_large_log_is_found() {
    let rig = Rig::new();
    let cfg = config(
        vec![with_failed(
            step(
                "t",
                "echo 'test mid ... FAILED'; head -c 3000000 /dev/zero | tr '\\0' y; echo; exit 1",
            ),
            TEST_NAME,
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &no_rerun()));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["mid".to_owned()]).unwrap())
    );
}

#[cfg(target_os = "linux")]
#[test]
fn zombies_this_process_adopts_do_not_make_the_group_look_alive() {
    nix::sys::prctl::set_child_subreaper(true).unwrap();
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![step(
            "bg",
            format!("sleep 300 & echo $! > '{m}/pid'; exit 0"),
        )],
        false,
    );
    let started = Instant::now();
    passed(rig.run(&cfg, &Options::default()));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(!is_alive(pid_in(&rig.mark("pid"))));
}

#[test]
fn the_failure_reports_the_names_found_in_the_log_it_points_at() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![with_failed(
            step(
                "t",
                format!(
                    "[ -e '{m}/seen' ] && {{ echo 'test second ... FAILED'; exit 1; }}; \
                     touch '{m}/seen'; echo 'test first ... FAILED'; exit 1"
                ),
            ),
            TEST_NAME,
        )],
        false,
    );
    let f = failed(rig.run(&cfg, &Options::default()));
    assert_eq!(f.log, log_path(&rig.logs(), "t", 2));
    assert_eq!(
        f.detail,
        Detail::Tests(NonEmpty::new(vec!["second".to_owned()]).unwrap())
    );
}

#[test]
fn a_lock_held_by_another_process_makes_the_run_wait() {
    let rig = Rig::new();
    fs::create_dir_all(rig.store.dir()).unwrap();
    let mut holder = Command::new("perl")
        .args([
            "-MFcntl=:flock",
            "-e",
            "$| = 1; open(F, '>>', shift) or die; flock(F, LOCK_EX) or die; print qq(held\\n); sleep 2",
        ])
        .arg(rig.store.dir().join("verify.lock"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(holder.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "held\n");
    let cfg = config(vec![step("a", "true")], true);
    let started = Instant::now();
    let outcome = rig.run(&cfg, &Options::default());
    let waited = started.elapsed();
    holder.wait().unwrap();
    passed(outcome);
    assert!(waited >= Duration::from_millis(1200), "{waited:?}");
}

#[test]
fn a_steps_time_limit_counts_from_its_own_start() {
    let rig = Rig::new();
    let cfg = config(
        vec![
            step("a", "sleep 1"),
            VerifyStep {
                timeout: Duration::from_millis(1800),
                ..step("b", "sleep 1")
            },
        ],
        false,
    );
    passed(rig.run(&cfg, &no_rerun()));
}

#[test]
fn a_rerun_gets_a_whole_time_limit_of_its_own() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(2),
            ..step(
                "t",
                format!(
                    "[ -e '{m}/seen' ] && {{ sleep 1.2; exit 0; }}; touch '{m}/seen'; sleep 1.2; exit 1"
                ),
            )
        }],
        false,
    );
    assert_eq!(passed(rig.run(&cfg, &Options::default())).len(), 1);
}

#[test]
fn the_default_grace_lets_a_sigterm_handler_finish() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(1),
            ..step(
                "t",
                format!("trap \"sleep 1; touch '{m}/cleaned'\" TERM; sleep 300 & wait"),
            )
        }],
        false,
    );
    assert_eq!(
        failed(rig.run(&cfg, &Options::default())).cause,
        Cause::TimedOut
    );
    assert!(rig.mark("cleaned").exists());
}

#[test]
fn a_leader_that_moved_to_another_group_is_still_ended_on_timeout() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![VerifyStep {
            timeout: Duration::from_secs(1),
            ..step(
                "t",
                format!(
                    "echo $$ > '{m}/pid'; exec python3 -I -c \
                     'import os, time; os.setpgid(0, os.getpgid(os.getppid())); time.sleep(300)'"
                ),
            )
        }],
        false,
    );
    assert_eq!(
        failed(rig.run(&cfg, &Options::default())).cause,
        Cause::TimedOut
    );
    assert!(!is_alive(pid_in(&rig.mark("pid"))));
}

#[test]
fn a_real_time_signal_death_is_an_outcome_and_not_an_error() {
    let rig = Rig::new();
    let cfg = config(vec![step("s", "kill -s 40 $$")], false);
    assert_eq!(
        failed(rig.run(&cfg, &no_rerun())).cause,
        Cause::Signaled(40)
    );
}

#[test]
fn non_utf8_output_is_a_failure_and_not_an_error() {
    let rig = Rig::new();
    let cfg = config(vec![step("t", "printf '\\377\\376\\n'; exit 1")], false);
    failed(rig.run(&cfg, &no_rerun()));
}

#[test]
fn steps_inherit_the_environment() {
    let rig = Rig::new();
    let cfg = config(
        vec![step("t", "[ -n \"$PATH\" ] && [ -n \"$HOME\" ]")],
        false,
    );
    passed(rig.run(&cfg, &no_rerun()));
}

#[test]
fn a_failing_command_in_the_middle_does_not_fail_the_step_by_itself() {
    let rig = Rig::new();
    let cfg = config(vec![step("t", "false; true")], false);
    passed(rig.run(&cfg, &no_rerun()));
}

#[test]
fn a_nested_log_folder_is_created() {
    let rig = Rig::new();
    let cfg = config(vec![step("t", "true")], false);
    let deep = rig.sb.root().join("deep/er/logs");
    let outcome = run(&cfg, &rig.store, &rig.work(), &deep, &Options::default()).unwrap();
    passed(outcome);
    assert!(log_path(&deep, "t", 1).exists());
}

#[test]
fn flaky_names_come_from_the_whole_first_log() {
    let rig = Rig::new();
    let m = rig.marks_dir();
    let cfg = config(
        vec![with_failed(
            step(
                "t",
                format!(
                    "[ -e '{m}/seen' ] && exit 0; touch '{m}/seen'; \
                     echo 'test early ... FAILED'; seq 1 200; exit 1"
                ),
            ),
            TEST_NAME,
        )],
        false,
    );
    let flaky = passed(rig.run(&cfg, &Options::default()));
    assert_eq!(flaky[0].tests, ["early"]);
}

#[test]
fn cache_dir_is_created_and_handed_to_every_step() {
    let rig = Rig::new();
    let cache = rig.sb.root().join("shared/cache");
    let mark = rig.mark("cache-seen");
    let script = format!(
        "test -d \"$CHUTE_CACHE_DIR\" && printf %s \"$CHUTE_CACHE_DIR\" >> {}",
        mark.display()
    );
    let config = config(
        vec![step("one", script.clone()), step("two", script)],
        false,
    );
    let opts = Options {
        cache_dir: Some(cache.clone()),
        ..Options::default()
    };

    assert!(passed(rig.run(&config, &opts)).is_empty());

    let seen = fs::read_to_string(mark).unwrap();
    assert_eq!(seen, format!("{0}{0}", cache.display()));
}

#[test]
fn cache_dir_is_not_set_for_a_run_without_one() {
    let rig = Rig::new();
    let config = config(vec![step("one", "test -z \"${CHUTE_CACHE_DIR+x}\"")], false);
    assert!(passed(rig.run(&config, &Options::default())).is_empty());
}

fn with_paths(mut step: VerifyStep, paths: &[&str]) -> VerifyStep {
    step.paths = NonEmpty::new(paths.iter().map(|p| (*p).to_owned()).collect());
    step
}

#[test]
fn a_step_whose_paths_match_none_of_the_changed_files_is_skipped_and_named() {
    let rig = Rig::new();
    let config = config(
        vec![
            with_paths(step("web", "touch ../marks/web"), &["web/"]),
            with_paths(step("lib", "touch ../marks/lib"), &["Cargo.toml", "src/"]),
            step("always", "touch ../marks/always"),
        ],
        false,
    );
    let changed = vec!["src/main.rs".to_owned(), "docs/web/x.md".to_owned()];
    let skipped = RefCell::new(Vec::new());
    let on_skip = |name: &str| skipped.borrow_mut().push(name.to_owned());
    let heard = RefCell::new(Vec::new());
    let on_step = |name: &str| heard.borrow_mut().push(name.to_owned());
    let opts = Options {
        changed: Some(&changed),
        on_skip: Some(&on_skip),
        on_step: Some(&on_step),
        ..Options::default()
    };

    passed(rig.run(&config, &opts));

    assert_eq!(*skipped.borrow(), ["web"]);
    assert_eq!(*heard.borrow(), ["lib", "always"]);
    assert!(!rig.mark("web").exists());
    assert!(rig.mark("lib").exists());
    assert!(rig.mark("always").exists());
}

#[test]
fn without_a_list_of_changed_files_every_step_runs() {
    let rig = Rig::new();
    let config = config(
        vec![with_paths(step("web", "touch ../marks/web"), &["web/"])],
        false,
    );
    let skipped = RefCell::new(Vec::new());
    let on_skip = |name: &str| skipped.borrow_mut().push(name.to_owned());
    let opts = Options {
        on_skip: Some(&on_skip),
        ..Options::default()
    };

    passed(rig.run(&config, &opts));

    assert!(skipped.borrow().is_empty());
    assert!(rig.mark("web").exists());
}
