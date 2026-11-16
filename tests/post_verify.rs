mod engine_kit;
mod support;

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use chute::engine::Pass;
use chute::model::{Culprit, Delivered, EventKind, NotifyKind, PostPhase, PrNumber};
use chute::post::PostPass;
use chute::verify::Cancel;
use engine_kit::Lab;

const POST_STEP: &str = "[[post_verify]]\nname = \"slow\"\nrun = \"sh @ROOT@/post.sh\"\ntimeout = 60\nfailed = 'FAILED (\\S+)'\n";

fn post_lab(body: &str, top: &str, step: &str) -> Lab {
    let lab = Lab::build_with_step("", top, 60, step);
    let script = format!(
        "unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE\necho \"$(git rev-parse HEAD)\" >> '{root}/post-runs.log'\n{body}\nexit 0\n",
        root = lab.sb.root().display()
    );
    fs::write(lab.sb.root().join("post.sh"), script).unwrap();
    lab
}

fn runs(lab: &Lab) -> Vec<String> {
    let mut commits: Vec<String> = fs::read_to_string(lab.sb.root().join("post-runs.log"))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    commits.dedup();
    commits
}

fn merge(lab: &Lab, n: u64, files: &[(&str, &str)]) -> String {
    lab.open_pr_for(n, files, Some(lab.agent()), chute::model::Approval::Agent);
    merged(lab)
}

fn merged(lab: &Lab) -> String {
    match lab.engine().run_once().unwrap() {
        Pass::Done(_) => lab.origin_main(),
        other => panic!("batch가 끝나야 한다: {other:?}"),
    }
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "기다리던 일이 일어나지 않았다: {what}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn events_of(lab: &Lab, pick: impl Fn(&EventKind) -> bool) -> Vec<EventKind> {
    lab.events().into_iter().filter(|e| pick(e)).collect()
}

#[test]
fn without_post_verify_steps_nothing_runs_and_nothing_is_recorded() {
    let lab = Lab::new("");
    merge(&lab, 1, &[("a.txt", "a\n")]);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Idle);

    assert_eq!(lab.state().post, Default::default());
    assert!(events_of(&lab, |e| matches!(e, EventKind::PostGreen { .. })).is_empty());
    assert_eq!(lab.merged_prs(), [1]);
}

#[test]
fn a_passing_check_moves_last_green_to_the_tip_once() {
    let lab = post_lab("", "", POST_STEP);
    let tip = lab.origin_main();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);

    let state = lab.state().post;
    assert_eq!(state.last_green.unwrap().commit.as_str(), tip);
    assert!(state.run.is_none() && state.red.is_none());
    assert_eq!(runs(&lab), std::slice::from_ref(&tip));
    let green = events_of(&lab, |e| matches!(e, EventKind::PostGreen { .. }));
    assert_eq!(green.len(), 1);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Idle);
    assert_eq!(runs(&lab), [tip]);
}

#[test]
fn the_check_does_not_hold_the_queue_and_the_next_run_takes_the_newest_tip() {
    let lab = post_lab(
        "while [ -e '@ROOT@/gate' ]; do sleep 0.05; done",
        "",
        POST_STEP,
    );
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script)
        .unwrap()
        .replace("@ROOT@", &lab.sb.root().to_string_lossy());
    fs::write(&script, text).unwrap();
    fs::write(lab.sb.root().join("gate"), "").unwrap();
    let first_tip = lab.origin_main();
    let post = lab.post();

    let second_tip = thread::scope(|scope| {
        let first = scope.spawn(|| post.run_once().unwrap());
        wait_until("the check shows its step", || {
            lab.state()
                .post
                .run
                .is_some_and(|run| run.step.as_deref() == Some("slow"))
        });
        let run = lab.state().post.run.unwrap();
        assert_eq!(run.tip.as_str(), first_tip);
        assert_eq!(run.phase, PostPhase::Verifying);

        merge(&lab, 1, &[("a.txt", "a\n")]);
        let second = merge(&lab, 2, &[("b.txt", "b\n")]);
        assert_eq!(
            lab.merged_prs(),
            [1, 2],
            "검사가 도는 동안에도 큐가 머지한다"
        );
        assert!(lab.state().post.run.is_some(), "검사는 아직 돈다");

        fs::remove_file(lab.sb.root().join("gate")).unwrap();
        assert_eq!(first.join().unwrap(), PostPass::Passed);
        second
    });

    assert_eq!(
        lab.state().post.last_green.unwrap().commit.as_str(),
        first_tip
    );
    assert_eq!(post.run_once().unwrap(), PostPass::Passed);
    assert_eq!(
        runs(&lab),
        [first_tip, second_tip.clone()],
        "사이 tip은 검사하지 않는다"
    );
    assert_eq!(
        lab.state().post.last_green.unwrap().commit.as_str(),
        second_tip
    );
}

#[test]
fn a_step_that_fails_once_and_passes_again_is_flaky_and_the_tip_is_green() {
    let lab = post_lab(
        "m='@ROOT@/post-marker'\nif [ ! -e \"$m\" ]; then touch \"$m\"; echo 'FAILED wobbly_test'; exit 1; fi",
        "",
        POST_STEP,
    );
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script)
        .unwrap()
        .replace("@ROOT@", &lab.sb.root().to_string_lossy());
    fs::write(&script, text).unwrap();
    let tip = lab.origin_main();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);

    let flaky: Vec<_> = lab
        .events()
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Flaky {
                batch, step, tests, ..
            } => Some((batch.0, step, tests)),
            _ => None,
        })
        .collect();
    assert_eq!(
        flaky,
        [(
            format!("post-{}", &tip[..12]),
            "slow".to_owned(),
            vec!["wobbly_test".to_owned()]
        )]
    );
    let state = lab.state().post;
    assert_eq!(state.last_green.unwrap().commit.as_str(), tip);
    assert!(state.red.is_none());
    let out = lab
        .sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .current_dir(lab.repo.path())
        .args(["flaky", "--json"])
        .output()
        .unwrap();
    let ledger = String::from_utf8_lossy(&out.stdout);
    assert!(ledger.contains("wobbly_test"), "{ledger}");
    assert!(
        lab.hc_calls("send").is_empty(),
        "흔들림은 아무에게도 알리지 않는다"
    );
}

fn breaking(lab: &Lab) {
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script).unwrap().replace(
        "\nexit 0\n",
        "\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi\nexit 0\n",
    );
    fs::write(&script, text).unwrap();
}

#[test]
fn a_real_failure_is_bisected_to_the_merge_that_broke_it_and_its_workers_are_told() {
    let lab = post_lab("", "", POST_STEP);
    breaking(&lab);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    let green = lab.origin_main();
    let (agent, orchestrator) = (lab.agent(), lab.orchestrator());
    for (n, files) in [
        (1, [("a.txt", "a\n")]),
        (2, [("svc/BREAK", "x\n")]),
        (3, [("c.txt", "c\n")]),
    ] {
        let head = lab.push_pr_branch(n, &files);
        lab.queue_pr_by(
            n,
            &head,
            Some(agent.clone()),
            Some(orchestrator.clone()),
            chute::model::Approval::Agent,
        );
    }
    assert!(matches!(lab.engine().run_once().unwrap(), Pass::Done(_)));
    let tip = lab.origin_main();
    let merges = lab.merge_commits_on_main();
    assert_eq!(merges.len(), 3);
    let before = runs(&lab).len();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let searched: Vec<String> = runs(&lab)[before..].to_vec();
    assert_eq!(
        searched,
        [tip.clone(), merges[0].clone(), merges[1].clone()],
        "tip 한 번, 그다음 이분 탐색으로 두 번"
    );
    let red = lab.state().post.red.expect("실패가 남아야 한다");
    assert_eq!(red.commit.as_str(), tip);
    assert_eq!(red.step, "slow");
    assert_eq!(red.tests, ["broken_test"]);
    assert_eq!(red.last_green.as_ref().unwrap().as_str(), green);
    let culprit: Culprit = red.culprit.expect("범인을 찾아야 한다");
    assert_eq!(culprit.commit.as_str(), merges[1]);
    assert_eq!(culprit.pr, Some(PrNumber(2)));
    assert!(culprit.batch.is_some());
    assert!(red.report.as_ref().unwrap().is_file());
    assert_eq!(lab.state().post.last_green.unwrap().commit.as_str(), green);

    let failed = events_of(&lab, |e| matches!(e, EventKind::PostFailed { .. }));
    assert_eq!(failed.len(), 1);
    for who in ["t1rs", "t1orch"] {
        let lines: Vec<String> = lab
            .lines_sent_to(who)
            .into_iter()
            .filter(|l| l.contains("post_failed"))
            .collect();
        assert_eq!(lines.len(), 1, "{who}: {lines:?}");
        for needed in [
            "pr=2".to_owned(),
            "step=slow".to_owned(),
            "tests=broken_test".to_owned(),
            format!("last_green={}", &green[..12]),
            format!("batch={}", culprit.batch.as_ref().unwrap().0),
            "chute revert".to_owned(),
        ] {
            assert!(lines[0].contains(&needed), "{needed}: {}", lines[0]);
        }
    }
    assert!(
        lab.notified()
            .iter()
            .all(|(kind, delivered)| *kind == NotifyKind::PostFailed
                && *delivered == Delivered::Sent
                || *kind == NotifyKind::Merged)
    );

    assert_eq!(
        lab.post().run_once().unwrap(),
        PostPass::Idle,
        "같은 tip은 다시 검사하지 않는다"
    );

    let head = lab.push_pr_branch_removing(4, "svc/BREAK");
    lab.queue_pr_by(
        4,
        &head,
        Some(agent),
        Some(orchestrator),
        chute::model::Approval::Agent,
    );
    merged(&lab);
    let fixed_tip = lab.origin_main();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);

    let state = lab.state().post;
    assert!(state.red.is_none());
    assert_eq!(state.last_green.unwrap().commit.as_str(), fixed_tip);
    let fixed = events_of(&lab, |e| matches!(e, EventKind::PostFixed { .. }));
    assert_eq!(fixed.len(), 1);
    let line = lab
        .lines_sent_to("t1rs")
        .into_iter()
        .find(|l| l.contains("post_fixed"))
        .expect("풀렸다는 알림이 가야 한다");
    assert!(
        line.contains("pr=2") && line.contains(&format!("was={}", &tip[..12])),
        "{line}"
    );
}

#[test]
fn a_failure_that_stays_is_not_searched_or_told_again_and_a_new_one_is() {
    let lab = post_lab("", "", POST_STEP);
    breaking(&lab);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    merge(&lab, 1, &[("svc/BREAK", "x\n")]);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);
    let told = |lab: &Lab| {
        lab.lines_sent_to("t1rs")
            .iter()
            .filter(|l| l.contains("post_failed"))
            .count()
    };
    assert_eq!(told(&lab), 1);
    let first = lab.state().post.red.unwrap();

    let tip = merge(&lab, 2, &[("b.txt", "b\n")]);
    let before = runs(&lab).len();
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    assert_eq!(runs(&lab).len(), before + 1, "범인을 다시 찾지 않는다");
    let red = lab.state().post.red.unwrap();
    assert_eq!(red.commit.as_str(), tip);
    assert_eq!(red.culprit, first.culprit);
    assert_eq!(red.since_ms, first.since_ms);
    assert_eq!(told(&lab), 1, "같은 실패는 다시 알리지 않는다");
    assert_eq!(
        events_of(&lab, |e| matches!(e, EventKind::PostFailed { .. })).len(),
        1
    );
}

#[test]
fn a_failure_with_no_passing_commit_behind_it_has_no_culprit_and_reaches_nobody() {
    let lab = post_lab("", "", POST_STEP);
    breaking(&lab);
    lab.main_commit(&[("svc/BREAK", "x\n")], "chore: 깨진 채로 둔다");

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let red = lab.state().post.red.unwrap();
    assert!(red.culprit.is_none() && red.last_green.is_none());
    assert!(lab.hc_calls("send").is_empty());
    assert!(
        lab.notified()
            .contains(&(NotifyKind::PostFailed, Delivered::NoTerminal))
    );
}

#[test]
fn the_interval_keeps_a_new_tip_waiting_until_it_is_over() {
    let lab = post_lab("", "post_verify_interval = 30", POST_STEP);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    merge(&lab, 1, &[("a.txt", "a\n")]);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Idle);
    assert_eq!(runs(&lab).len(), 1);

    lab.store
        .update(|s| {
            let started = s.post.last_started_ms.unwrap();
            s.post.last_started_ms = Some(chute::model::EpochMs(started.0 - 31 * 60 * 1000));
            Ok(())
        })
        .unwrap();
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    assert_eq!(runs(&lab).len(), 2);
}

#[test]
fn a_step_with_paths_runs_only_when_a_file_under_them_changed_since_the_last_green() {
    let step = "[[post_verify]]\nname = \"slow\"\nrun = \"sh @ROOT@/post.sh\"\ntimeout = 60\npaths = [\"svc/\"]\n";
    let lab = post_lab("", "", step);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    assert_eq!(runs(&lab).len(), 1, "처음에는 바뀐 파일을 모르니 돈다");

    let docs = merge(&lab, 1, &[("docs/a.md", "a\n")]);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    assert_eq!(runs(&lab).len(), 1, "paths 밖만 바뀌면 단계를 건너뛴다");
    assert_eq!(lab.state().post.last_green.unwrap().commit.as_str(), docs);

    merge(&lab, 2, &[("svc/a.rs", "a\n")]);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    assert_eq!(runs(&lab).len(), 2);
}

#[test]
fn a_stop_in_the_middle_of_a_run_leaves_no_run_and_no_verdict() {
    let lab = post_lab("sleep 30", "", POST_STEP);
    let cancel = Cancel::new();
    let post = lab.post().with_cancel(cancel.clone());

    let pass = thread::scope(|scope| {
        let run = scope.spawn(|| post.run_once().unwrap());
        wait_until("the step starts", || runs(&lab).len() == 1);
        cancel.cancel();
        run.join().unwrap()
    });

    assert_eq!(pass, PostPass::Cancelled);
    let state = lab.state().post;
    assert!(state.run.is_none() && state.red.is_none() && state.last_green.is_none());
}

#[test]
fn with_hold_on_a_failure_keeps_prs_on_the_failed_steps_paths_in_the_queue_until_a_fix_passes() {
    let step = "[[post_verify]]\nname = \"slow\"\nrun = \"sh @ROOT@/post.sh\"\ntimeout = 60\npaths = [\"svc/\"]\nfailed = 'FAILED (\\S+)'\n";
    let lab = post_lab("", "post_verify_hold = true", step);
    breaking(&lab);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    merge(&lab, 1, &[("svc/BREAK", "x\n")]);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);
    assert!(lab.state().post.holding());

    lab.open_pr(2, &[("svc/b.rs", "b\n")]);
    lab.open_pr(3, &[("docs/c.md", "c\n")]);
    assert!(matches!(lab.engine().run_once().unwrap(), Pass::Done(_)));
    assert_eq!(lab.merged_prs(), [1, 3], "paths 밖의 PR만 머지된다");
    assert!(lab.item(2).is_some());
    assert_eq!(lab.engine().run_once().unwrap(), Pass::Held);
    assert!(lab.item(2).is_some());

    let head = lab.push_pr_branch_removing(4, "svc/BREAK");
    lab.queue_repair_by(4, &head, Some(lab.agent()), None);
    assert!(matches!(lab.engine().run_once().unwrap(), Pass::Done(_)));
    assert!(lab.merged_prs().contains(&4), "고치는 PR은 잡히지 않는다");
    assert_eq!(
        lab.engine().run_once().unwrap(),
        Pass::Held,
        "고침이 통과하기 전에는 계속 잡는다"
    );

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    assert!(!lab.state().post.holding());
    assert!(matches!(lab.engine().run_once().unwrap(), Pass::Done(_)));
    assert!(lab.merged_prs().contains(&2));
}

#[test]
fn without_hold_a_failure_does_not_touch_the_queue() {
    let lab = post_lab("", "", POST_STEP);
    breaking(&lab);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    merge(&lab, 1, &[("svc/BREAK", "x\n")]);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);
    assert!(!lab.state().post.holding());

    lab.open_pr(2, &[("svc/b.rs", "b\n")]);
    assert!(matches!(lab.engine().run_once().unwrap(), Pass::Done(_)));
    assert!(lab.merged_prs().contains(&2));
}

#[test]
fn a_new_failure_after_a_fix_nobody_checked_is_blamed_on_the_merge_that_caused_it() {
    let lab = post_lab("", "", POST_STEP);
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script).unwrap().replace(
        "\nexit 0\n",
        "\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi\nif [ -e svc/BREAK2 ]; then echo 'FAILED other_test'; exit 1; fi\nexit 0\n",
    );
    fs::write(&script, text).unwrap();
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    merge(&lab, 1, &[("svc/BREAK", "x\n")]);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let fix = lab.push_pr_branch_removing(2, "svc/BREAK");
    let new_break = lab.push_pr_branch(3, &[("svc/BREAK2", "y\n")]);
    lab.queue_pr(2, &fix, Some(lab.agent()), chute::model::Approval::Agent);
    lab.queue_pr(
        3,
        &new_break,
        Some(lab.agent()),
        chute::model::Approval::Agent,
    );
    merged(&lab);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let red = lab.state().post.red.unwrap();
    assert_eq!(red.tests, ["other_test"]);
    assert_eq!(red.culprit.unwrap().pr, Some(PrNumber(3)));
}

#[test]
fn a_step_that_wobbles_while_the_culprit_is_searched_for_still_reaches_the_flaky_ledger() {
    let lab = post_lab("", "", POST_STEP);
    let marker = lab.sb.root().join("wobble-marker");
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script).unwrap().replace(
        "\nexit 0\n",
        &format!(
            "\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi\nif [ -e a.txt ] && [ ! -e '{m}' ]; then touch '{m}'; echo 'FAILED wobbly_test'; exit 1; fi\nexit 0\n",
            m = marker.display()
        ),
    );
    fs::write(&script, text).unwrap();
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    for (n, file) in [(1, "a.txt"), (2, "svc/BREAK"), (3, "c.txt")] {
        let head = lab.push_pr_branch(n, &[(file, "x\n")]);
        lab.queue_pr(n, &head, Some(lab.agent()), chute::model::Approval::Agent);
    }
    merged(&lab);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    assert_eq!(
        lab.state().post.red.unwrap().culprit.unwrap().pr,
        Some(PrNumber(2))
    );
    let wobbles = events_of(
        &lab,
        |e| matches!(e, EventKind::Flaky { tests, .. } if tests == &["wobbly_test"]),
    );
    assert_eq!(wobbles.len(), 1);
}

#[test]
fn serialize_does_not_make_the_queue_wait_for_the_check() {
    let lab = post_lab("", "serialize = true", POST_STEP);
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script).unwrap().replace(
        "\nexit 0\n",
        &format!(
            "\nwhile [ -e '{}/gate' ]; do sleep 0.05; done\nexit 0\n",
            lab.sb.root().display()
        ),
    );
    fs::write(&script, text).unwrap();
    fs::write(lab.sb.root().join("gate"), "").unwrap();
    let post = lab.post();

    thread::scope(|scope| {
        let run = scope.spawn(|| post.run_once().unwrap());
        wait_until("the step runs", || {
            lab.state().post.run.is_some_and(|r| r.step.is_some())
        });
        merge(&lab, 1, &[("a.txt", "a\n")]);
        assert!(lab.state().post.run.is_some());
        fs::remove_file(lab.sb.root().join("gate")).unwrap();
        assert_eq!(run.join().unwrap(), PostPass::Passed);
    });
}

#[test]
fn a_repo_without_post_verify_is_not_fetched_for_it_and_gets_no_warning() {
    let lab = Lab::new("");
    lab.repo
        .git(&["remote", "set-url", "origin", "/nonexistent/origin.git"]);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Idle);
}

#[test]
fn an_unreadable_batch_record_does_not_keep_a_failure_from_being_recorded() {
    let lab = post_lab("", "", POST_STEP);
    breaking(&lab);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    merge(&lab, 1, &[("a.txt", "a\n")]);
    let first = lab.batch_ids().remove(0);
    fs::write(lab.store.batch_dir(&first).join("batch.json"), "{not json").unwrap();
    lab.main_commit(&[("svc/BREAK", "x\n")], "chore: 직접 push로 깨뜨린다");

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let red = lab.state().post.red.unwrap();
    assert_eq!(red.culprit.as_ref().unwrap().pr, None);
}
