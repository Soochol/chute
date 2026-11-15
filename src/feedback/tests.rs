use super::*;
use crate::model::{CommitRef, FailedCheck, ItemState};
use crate::repo;
use crate::store::Home;
use crate::testkit::{FakeTool, Reply, Sandbox, TestRepo};
use std::collections::BTreeSet;
use std::sync::Mutex;

const CONFIG: &str = r#"
respawn = ["claude", "--dangerously-skip-permissions"]
[[verify]]
name = "test"
run = "cargo test --quiet"
timeout = 1800
failed = 'test (\S+) \.\.\. FAILED'
"#;
const RECORDED_MS: u64 = 1_794_722_483_775;
const OTHER_MS: u64 = 1_794_800_000_000;

fn sha(s: &str) -> Sha {
    Sha::try_from(s.to_owned()).unwrap()
}

fn one<T>(item: T) -> NonEmpty<T> {
    NonEmpty::new(vec![item]).unwrap()
}

fn terminal(address: &str, state: &str, created_ms: u64) -> Reply {
    Reply::ok(format!(
        "{{\"address\":\"{address}\",\"state\":\"{state}\",\"created_ms\":{created_ms},\"labels\":{{}}}}"
    ))
}

struct Lab {
    sb: Sandbox,
    repo: TestRepo,
    ext: Ext,
    info: RepoInfo,
    store: RepoStore,
    config: Config,
    worktrees: PathBuf,
}

impl Lab {
    fn new() -> Lab {
        Lab::with_config(CONFIG)
    }

    fn with_config(config: &str) -> Lab {
        let sb = Sandbox::new();
        let gitconfig = sb.var("GIT_CONFIG_GLOBAL").unwrap();
        sb.script(
            "git",
            &format!(
                "export HOME='{}' GIT_CONFIG_GLOBAL='{}' GIT_CONFIG_NOSYSTEM=1\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE\nexec git \"$@\"\n",
                sb.home().display(),
                PathBuf::from(gitconfig).display()
            ),
        );
        let repo = TestRepo::new(&sb, "acme/widget");
        let ext = Ext::from_vars(|k| sb.var(k));
        let info = repo::discover(&ext, repo.path()).unwrap();
        let store = Home::at(sb.chute_home()).repo(&info.slug);
        let worktrees = sb.root().join("herdr-worktrees");
        Lab {
            sb,
            repo,
            ext,
            info,
            store,
            config: Config::parse(config).unwrap(),
            worktrees,
        }
    }

    fn fb(&self) -> Feedback<'_> {
        Feedback {
            ext: &self.ext,
            repo: &self.info,
            store: &self.store,
            config: &self.config,
            worktrees: &self.worktrees,
        }
    }

    fn hc(&self) -> FakeTool {
        self.sb.fake("hc")
    }

    fn hc_calls(&self) -> Vec<Vec<String>> {
        self.hc().calls()
    }

    fn item(&self, pr: u64, branch: &str) -> QueueItem {
        QueueItem {
            pr: PrNumber(pr),
            title: format!("제목 {pr}"),
            branch: branch.to_owned(),
            head: sha(&self.repo.head()),
            state: ItemState::Queued(None),
            agent: None,
            attempts: 0,
            submitted_ms: EpochMs(1),
            touches_config: false,
            last_failure: None,
        }
    }

    fn recorded_agent(&self, workdir: &Path) -> Agent {
        Agent {
            terminal: TerminalId("t1rs".into()),
            created_ms: EpochMs(RECORDED_MS),
            name: Some("impl-a".into()),
            workdir: workdir.to_owned(),
            alive: true,
        }
    }

    fn log(&self, text: &str) -> PathBuf {
        let path = self.sb.root().join("verify-logs").join("test.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn step(&self, mode: VerifyMode, tests: &[&str], log: PathBuf) -> StepFailure {
        StepFailure {
            step: "test".into(),
            mode,
            failed_tests: tests.iter().map(|t| (*t).to_owned()).collect(),
            log,
            tree: sha(&self.repo.head()),
        }
    }

    fn solo(&self, tests: &[&str]) -> Failure {
        Failure::Verify(self.step(VerifyMode::Solo, tests, self.log("ok\nFAILED\n")))
    }

    /// The fake `hc` can start an agent `t1new` that is live.
    fn spawnable(&self) {
        self.hc()
            .respond(
                "run *",
                Reply::ok("{\"address\":\"t1new\",\"first_message\":{\"sent\":true}}"),
            )
            .respond("get t1new*", terminal("t1new", "live", OTHER_MS));
    }

    fn bounced(&self, item: &QueueItem) -> Written {
        self.fb()
            .write_report(&Subject::from(item), &self.solo(&["a::b"]), None)
            .unwrap()
    }

    fn run_call(&self) -> Vec<String> {
        let calls = self.hc_calls();
        let runs: Vec<&Vec<String>> = calls.iter().filter(|c| c[0] == "run").collect();
        assert_eq!(runs.len(), 1, "{calls:?}");
        runs[0].clone()
    }

    fn run_cwd(&self) -> PathBuf {
        let run = self.run_call();
        let at = run.iter().position(|a| a == "--cwd").unwrap();
        PathBuf::from(&run[at + 1])
    }
}

fn verbs(calls: &[Vec<String>]) -> Vec<&str> {
    calls.iter().map(|c| c[0].as_str()).collect()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn a_solo_verify_report_has_what_an_agent_needs_to_fix_it() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/a");
    let log = lab.log("running\ntest a::b ... FAILED\n");
    let failure = Failure::Verify(lab.step(VerifyMode::Solo, &["a::b", "c::d"], log.clone()));

    let written = lab
        .fb()
        .write_report(
            &Subject::from(&item),
            &failure,
            Some("refs/chute/failed/pr-12-1"),
        )
        .unwrap();

    assert_eq!(written.md, lab.store.report_dir(PrNumber(12)).join("1.md"));
    let md = read(&written.md);
    for needed in [
        "PR #12",
        "feat/a",
        "`test`",
        "cargo test --quiet",
        "`a::b`",
        "`c::d`",
        "(2개)",
        "단독",
        &log.display().to_string(),
        "refs/chute/failed/pr-12-1",
        "git worktree add --detach \"${TMPDIR:-/tmp}/chute-repro-pr12-1\" refs/chute/failed/pr-12-1",
        "sh -c 'cargo test --quiet'",
        "git worktree remove --force",
        "`chute submit`",
        "`chute giveup --reason \"<이유>\"`",
    ] {
        assert!(md.contains(needed), "{needed}\n{md}");
    }
    assert!(!md.contains("로그 끝부분"), "{md}");

    let json: ReportDoc = serde_json::from_str(&read(&written.json)).unwrap();
    assert_eq!(json, written.doc);
    assert_eq!(written.json, written.md.with_extension("json"));
    let Failure::Verify(step) = &json.failure else {
        panic!("{:?}", json.failure)
    };
    assert_eq!(step.failed_tests, ["a::b", "c::d"]);
    assert_eq!(step.log, log);
    assert_eq!(json.step.as_ref().unwrap().command, "cargo test --quiet");

    let record = written.record();
    assert_eq!(record.report, written.md);
    assert_eq!(record.failure, failure);
    assert_eq!(
        record.summary,
        "검증 실패(단독): test 단계, 실패한 시험 2개"
    );
}

#[test]
fn without_test_names_the_end_of_the_log_is_quoted() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/a");
    let mut text = String::from("first line\n");
    for i in 0..100 {
        text.push_str(&format!("\u{1b}[31mline {i}\u{1b}[0m ```\n"));
    }
    let failure = Failure::Verify(lab.step(VerifyMode::Solo, &[], lab.log(&text)));

    let written = lab
        .fb()
        .write_report(&Subject::from(&item), &failure, None)
        .unwrap();

    let md = read(&written.md);
    assert!(md.contains("line 99 ```"), "{md}");
    assert!(
        !md.contains("first line") && !md.contains("line 10 "),
        "{md}"
    );
    assert!(!md.contains('\u{1b}'));
    assert!(
        md.contains("````text"),
        "a log with fences needs a longer fence\n{md}"
    );
    assert!(md.contains("실패한 시험 이름을 뽑지 못했다"));
    let tail = written.doc.step.unwrap().log_tail.unwrap();
    assert_eq!(tail.lines().count(), LOG_TAIL_LINES);
}

#[test]
fn an_unreadable_log_does_not_stop_the_report() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/a");
    let failure =
        Failure::Verify(lab.step(VerifyMode::Solo, &[], lab.sb.root().join("no-such.log")));

    let written = lab
        .fb()
        .write_report(&Subject::from(&item), &failure, None)
        .unwrap();

    assert!(read(&written.md).contains("로그를 읽지 못했다"));
}

/// `main` plus merges for PR 9, PR 90 and PR 12, as `gitops` writes them.
fn batch_tree(lab: &Lab) -> Sha {
    let repo = &lab.repo;
    repo.pr_branch("feat/a", "a.txt", "a change\n");
    repo.pr_branch("feat/c", "c.txt", "c change\n");
    repo.pr_branch("feat/b", "b.txt", "b change\n");
    repo.git(&["checkout", "--quiet", "-b", "batch", "main"]);
    for (n, branch) in [(9, "feat/a"), (90, "feat/c"), (12, "feat/b")] {
        let message = format!("Merge pull request #{n} from acme/{branch}");
        repo.git(&["merge", "--quiet", "--no-ff", "-m", &message, branch]);
    }
    let tree = sha(&repo.head());
    repo.git(&["checkout", "--quiet", "main"]);
    tree
}

#[test]
fn a_combo_report_lists_each_partner_with_its_files_and_diff() {
    let lab = Lab::new();
    let tree = batch_tree(&lab);
    let item = lab.item(12, "feat/b");
    let partner = Partner {
        pr: PrNumber(9),
        title: "앞 PR 제목".into(),
        files: vec!["ignored".into()],
        diff: "/ignored".into(),
    };
    let failure = Failure::Verify(StepFailure {
        tree,
        ..lab.step(
            VerifyMode::Combo {
                partners: one(partner),
            },
            &["t::x"],
            lab.log("x\n"),
        )
    });

    let written = lab
        .fb()
        .write_report(&Subject::from(&item), &failure, None)
        .unwrap();

    let Failure::Verify(StepFailure {
        mode: VerifyMode::Combo { partners },
        ..
    }) = &written.doc.failure
    else {
        panic!("{:?}", written.doc.failure)
    };
    let found = partners.first();
    assert_eq!(found.files, ["a.txt"]);
    assert_eq!(found.diff.parent().unwrap(), written.md.parent().unwrap());
    let diff = read(&found.diff);
    assert!(
        diff.contains("+++ b/a.txt") && diff.contains("+a change"),
        "{diff}"
    );
    assert!(!diff.contains("c.txt") && !diff.contains("b.txt"), "{diff}");

    let md = read(&written.md);
    for needed in [
        "조합",
        "#9",
        "앞 PR 제목",
        "`a.txt`",
        &found.diff.display().to_string(),
    ] {
        assert!(md.contains(needed), "{needed}\n{md}");
    }
    assert!(!md.contains("ignored"), "{md}");
    let json: ReportDoc = serde_json::from_str(&read(&written.json)).unwrap();
    assert_eq!(json, written.doc);
}

#[test]
fn a_partner_that_cannot_be_found_is_said_so_in_the_report() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/b");
    let partner = Partner {
        pr: PrNumber(77),
        title: "없는 PR".into(),
        files: vec![],
        diff: "/x".into(),
    };
    let failure = Failure::Timeout(lab.step(
        VerifyMode::Combo {
            partners: one(partner),
        },
        &[],
        lab.log("hang\n"),
    ));

    let written = lab
        .fb()
        .write_report(&Subject::from(&item), &failure, None)
        .unwrap();

    let Failure::Timeout(StepFailure {
        mode: VerifyMode::Combo { partners },
        ..
    }) = &written.doc.failure
    else {
        panic!()
    };
    assert!(partners.first().files.is_empty());
    assert!(read(&partners.first().diff).contains("diff를 만들지 못했다"));
    assert!(read(&written.md).contains("#77"));
}

#[test]
fn a_conflict_report_names_the_files_and_the_main_commits() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/a");
    let failure = Failure::Conflict {
        files: NonEmpty::new(vec![
            ConflictFile {
                path: "src/x.rs".into(),
                main_commits: vec![CommitRef {
                    sha: sha("abc1234def"),
                    title: "feat: x를 바꾼다".into(),
                }],
            },
            ConflictFile {
                path: "src/y.rs".into(),
                main_commits: vec![],
            },
        ])
        .unwrap(),
    };

    let written = lab
        .fb()
        .write_report(&Subject::from(&item), &failure, None)
        .unwrap();

    let md = read(&written.md);
    for needed in [
        "`src/x.rs`",
        "`src/y.rs`",
        "`abc1234def`",
        "feat: x를 바꾼다",
        "git fetch origin && git merge origin/main",
        "`chute submit`",
    ] {
        assert!(md.contains(needed), "{needed}\n{md}");
    }
    assert!(written.doc.step.is_none());
    assert_eq!(written.doc.summary, "충돌: src/x.rs, src/y.rs");
    let json: ReportDoc = serde_json::from_str(&read(&written.json)).unwrap();
    assert_eq!(json, written.doc);
}

#[test]
fn a_timeout_report_says_it_was_a_timeout() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/a");
    let failure = Failure::Timeout(lab.step(VerifyMode::Solo, &[], lab.log("stuck here\n")));

    let written = lab
        .fb()
        .write_report(&Subject::from(&item), &failure, None)
        .unwrap();

    let md = read(&written.md);
    assert!(
        md.contains("**시간 초과**") && md.contains("1800초"),
        "{md}"
    );
    assert!(md.contains("stuck here"));
    assert!(written.doc.summary.starts_with("시간 초과(단독)"));
    assert_eq!(written.doc.step.unwrap().timeout_s, 1800);
}

#[test]
fn a_second_bounce_of_the_same_pr_gets_the_next_number_and_keeps_the_first() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/a");

    let first = lab.bounced(&item);
    let (md1, json1) = (read(&first.md), read(&first.json));
    let second = lab.bounced(&item);
    let other = lab.bounced(&lab.item(13, "feat/b"));

    assert_eq!((first.doc.seq, second.doc.seq), (1, 2));
    assert_eq!(second.md.file_name().unwrap(), "2.md");
    assert_eq!(read(&first.md), md1);
    assert_eq!(read(&first.json), json1);
    assert!(read(&second.md).contains("보고서 번호: 2"));
    assert_eq!(other.doc.seq, 1, "numbers are per PR");
}

#[test]
fn numbers_keep_rising_after_older_reports_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("7.json"), "{}").unwrap();
    fs::write(dir.path().join("notes.md"), "").unwrap();
    fs::write(dir.path().join("3.diff"), "").unwrap();

    let (seq, md) = claim_sequence(dir.path()).unwrap();

    assert_eq!(
        (seq, md.file_name().unwrap().to_str().unwrap()),
        (8, "8.md")
    );
}

#[test]
fn two_writers_never_claim_the_same_number() {
    let dir = tempfile::tempdir().unwrap();
    let claimed = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..5 {
                    let (seq, _) = claim_sequence(dir.path()).unwrap();
                    claimed.lock().unwrap().push(seq);
                }
            });
        }
    });
    let claimed = claimed.into_inner().unwrap();
    assert_eq!(claimed.iter().collect::<BTreeSet<_>>().len(), 40);
    assert_eq!(highest_sequence(dir.path()).unwrap(), 40);
}

#[test]
fn a_commit_without_a_pr_gets_a_report_of_its_own() {
    let lab = Lab::new();
    let commit = sha(&lab.repo.head());
    let subject = Subject::Commit {
        commit: commit.clone(),
    };

    let written = lab
        .fb()
        .write_report(&subject, &lab.solo(&["a::b"]), None)
        .unwrap();

    assert_eq!(
        written.md,
        lab.store.commit_report_dir(&commit).join("1.md")
    );
    let md = read(&written.md);
    assert!(
        md.contains("preflight") && md.contains("`chute preflight`"),
        "{md}"
    );
    assert!(!md.contains("chute giveup"), "{md}");
    let json: ReportDoc = serde_json::from_str(&read(&written.json)).unwrap();
    assert_eq!(json.subject, subject);
    let again = lab
        .fb()
        .write_report(&subject, &lab.solo(&["a::b"]), None)
        .unwrap();
    assert_eq!(again.doc.seq, 2);
}

#[test]
fn remote_ci_failures_have_no_report_yet_and_leave_nothing_behind() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/a");
    let failure = Failure::Ci {
        checks: one(FailedCheck {
            name: "build".into(),
            link: "https://example.invalid/1".into(),
        }),
    };

    let err = lab
        .fb()
        .write_report(&Subject::from(&item), &failure, None)
        .unwrap_err();

    assert!(err.to_string().contains("원격 CI"), "{err}");
    assert!(!lab.store.report_dir(PrNumber(12)).exists());
}

#[test]
fn a_report_for_a_step_missing_from_the_config_is_an_error_not_a_guess() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/a");
    let failure = Failure::Verify(StepFailure {
        step: "lint".into(),
        ..lab.step(VerifyMode::Solo, &[], lab.log("x"))
    });

    let err = lab
        .fb()
        .write_report(&Subject::from(&item), &failure, None)
        .unwrap_err();

    assert!(err.to_string().contains("lint"), "{err}");
}

#[test]
fn a_live_agent_with_the_same_created_ms_gets_one_send_with_the_report_path() {
    let lab = Lab::new();
    let mut item = lab.item(12, "feat/a");
    item.agent = Some(lab.recorded_agent(lab.repo.path()));
    let written = lab.bounced(&item);
    lab.hc()
        .respond("get t1rs*", terminal("t1rs", "live", RECORDED_MS))
        .respond("send t1rs *", Reply::ok("{}"));

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    assert_eq!(
        delivery,
        Delivery::Sent {
            terminal: TerminalId("t1rs".into())
        }
    );
    let calls = lab.hc_calls();
    assert_eq!(verbs(&calls), ["get", "send"]);
    assert_eq!(calls[1][..2], ["send", "t1rs"]);
    assert_eq!(
        calls[1].len(),
        3,
        "no --force or other flags: {:?}",
        calls[1]
    );
    let line = &calls[1][2];
    assert!(!line.contains('\n'), "{line}");
    for needed in [
        "PR #12",
        "검증 실패(단독)",
        "시험 1개",
        &written.md.display().to_string(),
        "chute submit",
        "chute giveup",
    ] {
        assert!(line.contains(needed), "{needed}\n{line}");
    }
}

#[test]
fn a_head_change_notice_goes_the_same_way_without_a_report() {
    let lab = Lab::new();
    let mut item = lab.item(12, "feat/a");
    item.agent = Some(lab.recorded_agent(lab.repo.path()));
    lab.hc()
        .respond("get t1rs*", terminal("t1rs", "live", RECORDED_MS))
        .respond("send t1rs *", Reply::ok("{}"));
    let (registered, current) = (sha("abc1234def5678"), sha("0123456789abcdef"));

    let delivery = lab.fb().deliver(
        &item,
        &Notice::HeadChanged {
            registered: &registered,
            current: &current,
        },
    );

    assert!(matches!(delivery, Delivery::Sent { .. }));
    let calls = lab.hc_calls();
    let line = &calls[1][2];
    for needed in ["PR #12", "abc1234def56", "0123456789ab", "chute submit"] {
        assert!(line.contains(needed), "{needed}\n{line}");
    }
    assert!(!line.contains("보고서"), "{line}");
}

#[test]
fn a_new_agent_replaces_one_that_is_not_the_recorded_one() {
    let cases: [(&str, Option<Reply>); 4] = [
        (
            "different created_ms",
            Some(terminal("t1rs", "live", OTHER_MS)),
        ),
        ("finished", Some(terminal("t1rs", "finished", RECORDED_MS))),
        (
            "not found",
            Some(
                Reply::ok("")
                    .code(3)
                    .stderr("{\"error\":{\"kind\":\"not_found\"}}"),
            ),
        ),
        ("no record", None),
    ];
    for (why, get) in cases {
        let lab = Lab::new();
        lab.repo.pr_branch("feat/a", "a.txt", "a\n");
        let mut item = lab.item(12, "feat/a");
        if let Some(reply) = get {
            item.agent = Some(lab.recorded_agent(&lab.sb.root().join("gone")));
            lab.hc().respond("get t1rs*", reply);
        }
        lab.spawnable();
        let written = lab.bounced(&item);

        let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

        let Delivery::Spawned(agent) = delivery else {
            panic!("{why}: {delivery:?}")
        };
        assert_eq!(agent.terminal, TerminalId("t1new".into()), "{why}");
        assert_eq!(agent.created_ms, EpochMs(OTHER_MS), "{why}");
        let calls = lab.hc_calls();
        assert!(!verbs(&calls).contains(&"send"), "{why}: {calls:?}");
        assert_eq!(verbs(&calls).iter().filter(|v| **v == "run").count(), 1);
    }
}

#[test]
fn a_send_that_finds_the_terminal_gone_falls_back_to_a_new_agent() {
    let lab = Lab::new();
    lab.repo.pr_branch("feat/a", "a.txt", "a\n");
    let mut item = lab.item(12, "feat/a");
    item.agent = Some(lab.recorded_agent(&lab.sb.root().join("gone")));
    lab.hc()
        .respond("get t1rs*", terminal("t1rs", "live", RECORDED_MS))
        .respond("send t1rs *", Reply::ok("").code(11));
    lab.spawnable();
    let written = lab.bounced(&item);

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    assert!(matches!(delivery, Delivery::Spawned(_)), "{delivery:?}");
    assert_eq!(verbs(&lab.hc_calls()), ["get", "send", "run", "get"]);
}

#[test]
fn a_blocked_agent_is_unreachable_and_nothing_is_forced_or_started() {
    let lab = Lab::new();
    let mut item = lab.item(12, "feat/a");
    item.agent = Some(lab.recorded_agent(lab.repo.path()));
    lab.hc()
        .respond("get t1rs*", terminal("t1rs", "live", RECORDED_MS))
        .respond(
            "send t1rs *",
            Reply::ok("")
                .code(9)
                .stderr(include_str!("../../tests/fixtures/hc/send-blocked.json")),
        );
    let written = lab.bounced(&item);

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    assert_eq!(
        delivery,
        Delivery::Unreachable(Unreachable::Blocked {
            terminal: TerminalId("t1rs".into())
        })
    );
    let calls = lab.hc_calls();
    assert_eq!(verbs(&calls), ["get", "send"]);
    assert!(calls.iter().flatten().all(|a| a != "--force"), "{calls:?}");
}

#[test]
fn an_empty_respawn_is_unreachable_and_starts_nothing() {
    let lab = Lab::with_config(&format!(
        "respawn = []\n{}",
        &CONFIG[CONFIG.find("[[verify]]").unwrap()..]
    ));
    let item = lab.item(12, "feat/a");
    let written = lab.bounced(&item);

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    assert_eq!(delivery, Delivery::Unreachable(Unreachable::NoRespawn));
    assert!(lab.hc_calls().is_empty());
    assert!(!lab.worktrees.exists());
}

#[test]
fn hc_that_cannot_answer_is_unreachable_and_does_not_start_a_second_agent() {
    let lab = Lab::new();
    let mut item = lab.item(12, "feat/a");
    item.agent = Some(lab.recorded_agent(lab.repo.path()));
    lab.hc()
        .respond("get t1rs*", Reply::ok("").code(1).stderr("core down"));
    lab.spawnable();
    let written = lab.bounced(&item);

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    let Delivery::Unreachable(Unreachable::Failed(why)) = delivery else {
        panic!("{delivery:?}")
    };
    assert!(why.contains("core down"), "{why}");
    assert_eq!(verbs(&lab.hc_calls()), ["get"]);
}

#[test]
fn a_terminal_state_chute_does_not_know_is_not_taken_for_gone() {
    let lab = Lab::new();
    let mut item = lab.item(12, "feat/a");
    item.agent = Some(lab.recorded_agent(lab.repo.path()));
    lab.hc()
        .respond("get t1rs*", terminal("t1rs", "starting", RECORDED_MS));
    lab.spawnable();
    let written = lab.bounced(&item);

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    assert!(matches!(
        delivery,
        Delivery::Unreachable(Unreachable::Failed(_))
    ));
    assert_eq!(verbs(&lab.hc_calls()), ["get"]);
}

#[test]
fn the_new_agent_is_started_in_the_background_without_a_parent_or_herdr_labels() {
    let lab = Lab::new();
    lab.repo.pr_branch("feat/a", "a.txt", "a\n");
    let item = lab.item(12, "feat/a");
    lab.spawnable();
    let written = lab.bounced(&item);

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    let run = lab.run_call();
    for flag in ["--bg", "--no-parent", "--cwd"] {
        assert!(run.iter().any(|a| a == flag), "{flag}: {run:?}");
    }
    assert!(
        run.iter().all(|a| !a.contains("herdr.") && a != "--label"),
        "{run:?}"
    );
    let dashes = run.iter().position(|a| a == "--").unwrap();
    assert_eq!(
        run[dashes + 1..dashes + 3],
        ["claude", "--dangerously-skip-permissions"]
    );
    assert_eq!(run.len(), dashes + 4, "the first task is the last argument");
    let task = &run[dashes + 3];
    for needed in [
        "PR #12",
        "feat/a",
        &written.md.display().to_string(),
        "chute --skill",
    ] {
        assert!(task.contains(needed), "{needed}\n{task}");
    }
    assert!(!task.contains('\n'));
    let Delivery::Spawned(agent) = delivery else {
        panic!("{delivery:?}")
    };
    assert_eq!(agent.name.as_deref(), Some("chute-pr12"));
    assert_eq!(agent.workdir, lab.run_cwd());
    assert!(agent.alive);
}

#[test]
fn respawn_arguments_are_passed_through_in_order() {
    let lab = Lab::with_config(&format!(
        "respawn = [\"codex\", \"--no-daemon\", \"-m\", \"x y\"]\n{}",
        &CONFIG[CONFIG.find("[[verify]]").unwrap()..]
    ));
    lab.repo.pr_branch("feat/a", "a.txt", "a\n");
    let item = lab.item(12, "feat/a");
    lab.spawnable();
    let written = lab.bounced(&item);

    lab.fb().deliver(&item, &Notice::Bounced(&written));

    let run = lab.run_call();
    let dashes = run.iter().position(|a| a == "--").unwrap();
    assert_eq!(
        run[dashes + 1..dashes + 5],
        ["codex", "--no-daemon", "-m", "x y"]
    );
}

#[test]
fn a_first_task_that_was_not_sent_is_unreachable() {
    let lab = Lab::new();
    lab.repo.pr_branch("feat/a", "a.txt", "a\n");
    let item = lab.item(12, "feat/a");
    lab.hc().respond(
        "run *",
        Reply::ok(
            "{\"address\":\"t1new\",\"first_message\":{\"sent\":false,\"reason\":\"대화상자가 떠 있다\"}}",
        ),
    );
    let written = lab.bounced(&item);

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    assert_eq!(
        delivery,
        Delivery::Unreachable(Unreachable::FirstMessageNotSent {
            terminal: TerminalId("t1new".into()),
            reason: "대화상자가 떠 있다".into()
        })
    );
    assert_eq!(verbs(&lab.hc_calls()), ["run"]);
}

#[test]
fn a_failed_launch_is_unreachable_with_the_reason() {
    let lab = Lab::new();
    lab.repo.pr_branch("feat/a", "a.txt", "a\n");
    let item = lab.item(12, "feat/a");
    lab.hc()
        .respond("run *", Reply::ok("").code(1).stderr("이름이 이미 쓰인다"));
    let written = lab.bounced(&item);

    let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

    let Delivery::Unreachable(Unreachable::Failed(why)) = delivery else {
        panic!("{delivery:?}")
    };
    assert!(why.contains("이름이 이미 쓰인다"), "{why}");
}

fn worktree_add(lab: &Lab, name: &str, branch: &str) -> PathBuf {
    let path = lab.sb.root().join(name);
    lab.repo.git(&[
        "worktree",
        "add",
        "--quiet",
        &path.to_string_lossy(),
        branch,
    ]);
    path
}

#[test]
fn the_recorded_folder_is_used_when_it_is_still_on_the_pr_branch() {
    let lab = Lab::new();
    lab.repo.pr_branch("feat/a", "a.txt", "a\n");
    let recorded = worktree_add(&lab, "recorded", "feat/a");
    let mut item = lab.item(12, "feat/a");
    item.agent = Some(lab.recorded_agent(&recorded));
    lab.hc()
        .respond("get t1rs*", terminal("t1rs", "finished", RECORDED_MS));
    lab.spawnable();
    let written = lab.bounced(&item);

    lab.fb().deliver(&item, &Notice::Bounced(&written));

    assert_eq!(lab.run_cwd(), recorded);
    assert!(!lab.worktrees.exists());
}

#[test]
fn a_worktree_that_has_the_branch_is_used_when_the_recorded_folder_is_not_on_it() {
    for recorded_exists in [false, true] {
        let lab = Lab::new();
        lab.repo.pr_branch("feat/a", "a.txt", "a\n");
        lab.repo.pr_branch("feat/other", "o.txt", "o\n");
        let elsewhere = worktree_add(&lab, "elsewhere", "feat/a");
        let stale = if recorded_exists {
            worktree_add(&lab, "switched", "feat/other")
        } else {
            lab.sb.root().join("deleted")
        };
        let mut item = lab.item(12, "feat/a");
        item.agent = Some(lab.recorded_agent(&stale));
        lab.hc()
            .respond("get t1rs*", terminal("t1rs", "finished", RECORDED_MS));
        lab.spawnable();
        let written = lab.bounced(&item);

        lab.fb().deliver(&item, &Notice::Bounced(&written));

        assert_eq!(
            lab.run_cwd(),
            elsewhere,
            "recorded_exists={recorded_exists}"
        );
        assert!(!lab.worktrees.exists());
    }
}

#[test]
fn a_new_worktree_is_made_under_the_root_when_no_checkout_has_the_branch() {
    let lab = Lab::new();
    lab.repo.pr_branch("feat/a", "a.txt", "a\n");
    let item = lab.item(12, "feat/a");
    lab.spawnable();
    let written = lab.bounced(&item);

    lab.fb().deliver(&item, &Notice::Bounced(&written));

    let expected = lab.worktrees.join("widget").join("feat/a");
    assert_eq!(lab.run_cwd(), expected);
    let git = |args: &[&str]| {
        lab.ext
            .git(&expected, args.iter().copied())
            .unwrap()
            .ok()
            .unwrap()
    };
    assert_eq!(git(&["branch", "--show-current"]).trim(), "feat/a");
    assert!(expected.join("a.txt").is_file());
}

#[test]
fn a_branch_that_exists_only_on_the_remote_is_made_at_the_registered_head() {
    let lab = Lab::new();
    let item = lab.item(12, "feat/ghost");
    lab.spawnable();
    let written = lab.bounced(&item);

    lab.fb().deliver(&item, &Notice::Bounced(&written));

    let dest = lab.worktrees.join("widget").join("feat/ghost");
    assert_eq!(lab.run_cwd(), dest);
    assert_eq!(
        lab.repo.git(&["rev-parse", "feat/ghost"]).trim(),
        item.head.as_str()
    );
}

#[test]
fn a_branch_left_at_a_deleted_worktree_is_freed_and_checked_out_again() {
    let lab = Lab::new();
    lab.repo.pr_branch("feat/a", "a.txt", "a\n");
    let gone = worktree_add(&lab, "gone", "feat/a");
    fs::remove_dir_all(&gone).unwrap();
    let item = lab.item(12, "feat/a");
    lab.spawnable();
    let written = lab.bounced(&item);

    lab.fb().deliver(&item, &Notice::Bounced(&written));

    assert_eq!(lab.run_cwd(), lab.worktrees.join("widget").join("feat/a"));
    assert!(!lab.repo.git(&["worktree", "list"]).contains("gone"));
}

#[test]
fn a_branch_name_that_would_leave_the_root_is_refused() {
    for branch in ["../escape", "-flag", "a/../../b", "/abs"] {
        let lab = Lab::new();
        let item = lab.item(12, branch);
        lab.spawnable();
        let written = lab.bounced(&item);

        let delivery = lab.fb().deliver(&item, &Notice::Bounced(&written));

        assert!(
            matches!(delivery, Delivery::Unreachable(Unreachable::Failed(_))),
            "{branch}: {delivery:?}"
        );
        assert!(lab.hc_calls().is_empty(), "{branch}");
        assert!(!lab.worktrees.exists(), "{branch}");
    }
}

#[test]
fn the_worktree_root_is_under_home() {
    let root = worktrees_root(|k| (k == "HOME").then(|| "/h".into())).unwrap();
    assert_eq!(root, Path::new("/h/.herdr/worktrees"));
    assert!(worktrees_root(|_| None).is_err());
    assert!(worktrees_root(|_| Some("".into())).is_err());
}

#[test]
fn shell_words_are_quoted_only_when_needed() {
    assert_eq!(
        shell_word("refs/chute/failed/pr-12-1"),
        "refs/chute/failed/pr-12-1"
    );
    assert_eq!(shell_word("cargo test"), "'cargo test'");
    assert_eq!(shell_word("echo 'a'"), "'echo '\\''a'\\'''");
    assert_eq!(shell_word(""), "''");
}
