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

#[test]
fn a_commit_that_fixes_an_unrelated_failure_is_not_blamed_for_the_real_one() {
    let lab = post_lab("", "", POST_STEP);
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script).unwrap().replace(
        "\nexit 0\n",
        "\nif [ -e svc/OTHER ]; then echo 'FAILED other_test'; exit 1; fi\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi\nexit 0\n",
    );
    fs::write(&script, text).unwrap();
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    let heads = [
        lab.push_pr_branch(1, &[("svc/BREAK", "x\n")]),
        lab.push_pr_branch(2, &[("svc/OTHER", "y\n")]),
    ];
    for (n, head) in [1, 2].into_iter().zip(&heads) {
        lab.queue_pr(n, head, Some(lab.agent()), chute::model::Approval::Agent);
    }
    merged(&lab);
    let third = lab.push_pr_branch_removing(3, "svc/OTHER");
    lab.queue_pr(3, &third, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);
    let fourth = lab.push_pr_branch(4, &[("d.txt", "d\n")]);
    lab.queue_pr(4, &fourth, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let red = lab.state().post.red.unwrap();
    assert_eq!(red.tests, ["broken_test"]);
    assert_eq!(red.culprit.unwrap().pr, Some(PrNumber(1)));
}

#[test]
fn a_post_verify_step_pushed_straight_to_the_remote_starts_the_check_once_the_branch_is_fetched() {
    let lab = Lab::new("");
    let post = lab.post();
    assert_eq!(post.run_once().unwrap(), PostPass::Idle);

    let other = lab.sb.root().join("other-clone");
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = lab
            .sb
            .command("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(
        lab.sb.root(),
        &[
            "clone",
            "--quiet",
            &lab.repo.origin().to_string_lossy(),
            "other-clone",
        ],
    );
    let mut config = fs::read_to_string(other.join("chute.toml")).unwrap();
    config.push_str("\n[[post_verify]]\nname = \"slow\"\nrun = \"true\"\n");
    fs::write(other.join("chute.toml"), config).unwrap();
    git(
        &other,
        &["commit", "--quiet", "-am", "chore: 머지 뒤 검사를 켠다"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);

    assert_eq!(
        post.run_once().unwrap(),
        PostPass::Idle,
        "가져오기 전에는 켜진 줄 모른다"
    );
    lab.repo.git(&["fetch", "--quiet", "origin"]);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
}

#[test]
fn a_failure_that_includes_the_tips_failure_counts_and_an_undecidable_range_names_nobody() {
    let lab = post_lab("", "", POST_STEP);
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script).unwrap().replace(
        "\nexit 0\n",
        "\nif [ -e svc/OTHER ]; then echo 'FAILED other_test'; fi\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; fi\nif [ -e svc/OTHER ] || [ -e svc/BREAK ]; then exit 1; fi\nexit 0\n",
    );
    fs::write(&script, text).unwrap();
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    let first = lab.push_pr_branch(1, &[("svc/BREAK", "x\n"), ("svc/OTHER", "y\n")]);
    lab.queue_pr(1, &first, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);
    let second = lab.push_pr_branch_removing(2, "svc/OTHER");
    lab.queue_pr(2, &second, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);
    let third = lab.push_pr_branch(3, &[("d.txt", "d\n")]);
    lab.queue_pr(3, &third, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let red = lab.state().post.red.unwrap();
    assert_eq!(red.tests, ["broken_test"]);
    assert_eq!(red.culprit.unwrap().pr, Some(PrNumber(1)));
}

#[test]
fn a_range_where_nothing_fails_like_the_tip_names_no_culprit_and_checks_each_commit_once() {
    let lab = post_lab("", "", POST_STEP);
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script).unwrap().replace(
        "\nexit 0\n",
        "\nif [ -e svc/OTHER ]; then echo 'FAILED other_test'; exit 1; fi\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi\nexit 0\n",
    );
    fs::write(&script, text).unwrap();
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    let first = lab.push_pr_branch(1, &[("svc/BREAK", "x\n"), ("svc/OTHER", "y\n")]);
    lab.queue_pr(1, &first, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);
    let second = lab.push_pr_branch_removing(2, "svc/OTHER");
    lab.queue_pr(2, &second, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    assert!(lab.state().post.red.unwrap().culprit.is_none());
    let checked = runs(&lab);
    let mut distinct = checked.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        checked.len(),
        distinct.len(),
        "같은 커밋을 두 번 검사하지 않는다"
    );
}

const PROBE_STEP: &str = "[[post_verify]]\nname = \"slow\"\nrun = \"sh @ROOT@/post.sh\"\ntimeout = 60\nfailed = 'FAILED (\\S+)'\nprobe = \"sh @ROOT@/probe.sh {tests}\"\n";

/// `post_lab` with a probe whose script is `probe_body`. It logs its arguments first.
fn probe_lab(body: &str, probe_body: &str, top: &str) -> Lab {
    let lab = post_lab(body, top, PROBE_STEP);
    let root = lab.sb.root().to_string_lossy().into_owned();
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script)
        .unwrap()
        .replace("@ROOT@", &root);
    fs::write(&script, text).unwrap();
    fs::write(
        lab.sb.root().join("probe.sh"),
        format!("echo \"$@\" >> '{root}/probe-args.log'\n{probe_body}\n"),
    )
    .unwrap();
    lab
}

fn lines_of(lab: &Lab, file: &str) -> Vec<String> {
    fs::read_to_string(lab.sb.root().join(file))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

const FAIL_ONCE: &str = "m='@ROOT@/post-marker'\nif [ ! -e \"$m\" ]; then touch \"$m\"; echo 'FAILED wobbly_test'; exit 1; fi";

#[test]
fn a_failed_step_is_rerun_as_its_probe_on_the_failed_tests_and_a_passing_probe_is_a_flake() {
    let lab = probe_lab(FAIL_ONCE, "exit 0", "");
    let tip = lab.origin_main();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);

    assert_eq!(lines_of(&lab, "probe-args.log"), ["wobbly_test"]);
    assert_eq!(
        lines_of(&lab, "post-runs.log"),
        std::slice::from_ref(&tip),
        "단계 전체는 한 번만 돈다"
    );
    let flaky: Vec<_> = events_of(&lab, |e| matches!(e, EventKind::Flaky { .. }))
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
}

#[test]
fn a_probe_that_fails_too_is_a_real_failure_and_the_search_reruns_by_probe_as_well() {
    let lab = probe_lab(
        "if [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi",
        "echo 'FAILED broken_test'; exit 1",
        "",
    );
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    for (n, file) in [(1, "a.txt"), (2, "svc/BREAK"), (3, "c.txt")] {
        let head = lab.push_pr_branch(n, &[(file, "x\n")]);
        lab.queue_pr(n, &head, Some(lab.agent()), chute::model::Approval::Agent);
    }
    merged(&lab);
    let tip = lab.origin_main();
    let merges = lab.merge_commits_on_main();
    let green_run = lines_of(&lab, "post-runs.log").len();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    assert_eq!(
        lines_of(&lab, "post-runs.log")[green_run..],
        [tip, merges[0].clone(), merges[1].clone()],
        "단계 전체는 커밋마다 한 번씩만 돈다"
    );
    assert_eq!(
        lines_of(&lab, "probe-args.log"),
        ["broken_test", "broken_test"],
        "tip과 실패한 가운데 커밋에서 probe가 돌았다"
    );
    let red = lab.state().post.red.expect("실패가 남아야 한다");
    assert_eq!(red.step, "slow");
    assert_eq!(red.tests, ["broken_test"]);
    assert!(red.log.to_string_lossy().ends_with("slow.2.log"));
    assert_eq!(red.culprit.unwrap().pr, Some(PrNumber(2)));
    assert!(events_of(&lab, |e| matches!(e, EventKind::Flaky { .. })).is_empty());
}

#[test]
fn a_failure_that_names_no_test_is_rerun_whole_even_when_the_step_has_a_probe() {
    let lab = probe_lab(
        "m='@ROOT@/post-marker'\nif [ ! -e \"$m\" ]; then touch \"$m\"; echo boom; exit 1; fi",
        "exit 1",
        "",
    );
    let tip = lab.origin_main();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);

    assert_eq!(lines_of(&lab, "post-runs.log"), [tip.clone(), tip]);
    assert!(lines_of(&lab, "probe-args.log").is_empty());
    let flaky = events_of(&lab, |e| matches!(e, EventKind::Flaky { .. }));
    assert_eq!(flaky.len(), 1);
}

#[test]
fn flakes_in_the_check_after_the_merge_pile_up_into_a_chronic_warning_without_telling_anyone() {
    let lab = probe_lab(
        "h=$(git rev-parse HEAD)\nm=\"@ROOT@/seen-$h\"\nif [ ! -e \"$m\" ]; then touch \"$m\"; echo 'FAILED wobbly_test'; exit 1; fi",
        "exit 0",
        "[flaky]\nchronic = 2\n",
    );

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    assert!(lab.state().warnings.is_empty(), "한 번은 만성이 아니다");

    merge(&lab, 1, &[("a.txt", "a\n")]);
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);

    let warnings = lab.state().warnings;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].kind, chute::model::WarningKind::FlakyChronic);
    assert!(warnings[0].message.contains("wobbly_test"), "{warnings:?}");
    assert!(lab.hc_calls("send").is_empty(), "알림은 보내지 않는다");
}

fn plain_lab(body: &str) -> Lab {
    let lab = post_lab(body, "", POST_STEP);
    let root = lab.sb.root().to_string_lossy().into_owned();
    let script = lab.sb.root().join("post.sh");
    let text = fs::read_to_string(&script)
        .unwrap()
        .replace("@ROOT@", &root);
    fs::write(&script, text).unwrap();
    lab
}

fn queue_all(lab: &Lab, prs: &[(u64, &str)]) {
    for (n, file) in prs {
        let head = lab.push_pr_branch(*n, &[(*file, "x\n")]);
        lab.queue_pr(*n, &head, Some(lab.agent()), chute::model::Approval::Agent);
    }
    merged(lab);
}

const OLD_AND_NEW: &str = "if [ -e svc/OLD ]; then echo 'FAILED old_test'; f=1; fi\nif [ -e svc/NEW ]; then echo 'FAILED new_test'; f=1; fi\n[ -z \"$f\" ] || exit 1";
const OLD_AND_NEW_PROBE: &str = "for t in \"$@\"; do\n  if [ \"$t\" = old_test ] && [ -e svc/OLD ]; then echo 'FAILED old_test'; f=1; fi\n  if [ \"$t\" = new_test ] && [ -e svc/NEW ]; then echo 'FAILED new_test'; f=1; fi\ndone\n[ -z \"$f\" ] || exit 1";

#[test]
fn the_search_rechecks_the_tips_tests_and_a_commit_where_they_pass_alone_counts_as_passing() {
    let lab = probe_lab(OLD_AND_NEW, OLD_AND_NEW_PROBE, "");
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    queue_all(&lab, &[(1, "svc/OLD"), (2, "c.txt"), (3, "svc/NEW")]);
    let fix = lab.push_pr_branch_removing(4, "svc/OLD");
    lab.queue_pr(4, &fix, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);
    queue_all(&lab, &[(5, "e.txt")]);
    let tip = lab.origin_main();
    let merges = lab.merge_commits_on_main();
    assert_eq!(merges.len(), 5);
    let green_run = lines_of(&lab, "post-runs.log").len();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    assert_eq!(
        lines_of(&lab, "post-runs.log")[green_run..],
        [tip, merges[1].clone(), merges[2].clone()],
        "단계 전체는 tip과 후보 둘에서 한 번씩 돈다"
    );
    assert_eq!(
        lines_of(&lab, "probe-args.log"),
        ["new_test", "new_test", "new_test"],
        "옛 실패만 있는 후보도 tip의 시험으로 다시 돈다"
    );
    let red = lab.state().post.red.expect("실패가 남아야 한다");
    assert_eq!(red.tests, ["new_test"]);
    assert_eq!(red.culprit.unwrap().pr, Some(PrNumber(3)));
    assert!(
        events_of(&lab, |e| matches!(e, EventKind::Flaky { .. })).is_empty(),
        "옛 실패는 흔들림이 아니다"
    );
}

#[test]
fn a_whole_rerun_keeps_only_the_tests_that_failed_both_times() {
    let lab = plain_lab(
        "h=$(git rev-parse HEAD)\nn=$(grep -c \"^$h\\$\" '@ROOT@/post-runs.log')\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; f=1; fi\nif [ -e svc/TIP ] && [ \"$n\" = 2 ]; then echo 'FAILED wobbly_test'; f=1; fi\n[ -z \"$f\" ] || exit 1",
    );
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    queue_all(&lab, &[(1, "a.txt"), (2, "svc/BREAK"), (3, "svc/TIP")]);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let red = lab.state().post.red.expect("실패가 남아야 한다");
    assert_eq!(red.tests, ["broken_test"], "한 번만 실패한 시험은 뺀다");
    assert_eq!(red.culprit.unwrap().pr, Some(PrNumber(2)));
    let line = lab
        .lines_sent_to("t1rs")
        .into_iter()
        .find(|l| l.contains("post_failed"))
        .expect("알림이 가야 한다");
    assert!(line.contains("tests=broken_test"), "{line}");
}

#[test]
fn a_probe_that_fails_without_naming_a_test_during_the_search_is_not_blamed() {
    let lab = probe_lab(
        "if [ -e svc/BREAK ]; then echo 'FAILED broken_test'; f=1; fi\nif [ -e svc/WOBBLE ]; then m=\"@ROOT@/seen-$(git rev-parse HEAD)\"; if [ ! -e \"$m\" ]; then touch \"$m\"; echo 'FAILED broken_test'; f=1; fi; fi\n[ -z \"$f\" ] || exit 1",
        "if [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi\nif [ -e svc/WOBBLE ]; then echo 'probe: unsupported'; exit 1; fi",
        "",
    );
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    queue_all(&lab, &[(1, "svc/WOBBLE"), (2, "svc/BREAK"), (3, "c.txt")]);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let red = lab.state().post.red.expect("실패가 남아야 한다");
    assert_eq!(red.tests, ["broken_test"]);
    assert!(
        red.culprit.is_none(),
        "이름 없이 실패한 probe로는 범인을 가리지 못한다: {:?}",
        red.culprit
    );
}

#[test]
fn a_probe_set_on_the_default_branch_after_the_check_started_is_used_for_the_next_commit() {
    let lab = plain_lab(
        "while [ -e '@ROOT@/gate' ]; do sleep 0.05; done\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi",
    );
    let root = lab.sb.root().to_string_lossy().into_owned();
    fs::write(
        lab.sb.root().join("probe.sh"),
        format!("echo \"$@\" >> '{root}/probe-args.log'\nif [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi\n"),
    )
    .unwrap();
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    queue_all(&lab, &[(1, "a.txt"), (2, "svc/BREAK"), (3, "c.txt")]);
    fs::write(lab.sb.root().join("gate"), "").unwrap();
    let green_run = lines_of(&lab, "post-runs.log").len();
    let post = lab.post();

    let tip = thread::scope(|scope| {
        let run = scope.spawn(|| post.run_once().unwrap());
        wait_until("the tip's step starts", || {
            lines_of(&lab, "post-runs.log").len() == green_run + 1
        });
        let mut config = fs::read_to_string(lab.repo.path().join("chute.toml")).unwrap();
        config.push_str(&format!("probe = \"sh {root}/probe.sh {{tests}}\"\n"));
        lab.main_commit(&[("chute.toml", &config)], "chore: probe를 더한다");
        fs::remove_file(lab.sb.root().join("gate")).unwrap();
        assert_eq!(run.join().unwrap(), PostPass::Failed);
        lines_of(&lab, "post-runs.log")[green_run].clone()
    });

    let runs = &lines_of(&lab, "post-runs.log")[green_run..];
    assert_eq!(
        runs.iter().filter(|c| **c == tip).count(),
        2,
        "검사를 시작할 때는 probe가 없어 tip은 단계 전체를 두 번 돈다: {runs:?}"
    );
    assert_eq!(
        lines_of(&lab, "probe-args.log"),
        ["broken_test"],
        "그 뒤의 후보 커밋에서는 새 probe가 돈다"
    );
    assert_eq!(
        lab.state().post.red.unwrap().culprit.unwrap().pr,
        Some(PrNumber(2))
    );
}

#[test]
fn a_test_the_search_reran_and_passed_is_flaky_but_a_failure_it_did_not_rerun_is_not() {
    let lab = probe_lab(
        "if [ -e svc/BREAK ]; then echo 'FAILED broken_test'; f=1; fi\nif [ -e svc/WOBBLE ]; then m=\"@ROOT@/seen-$(git rev-parse HEAD)\"; if [ ! -e \"$m\" ]; then touch \"$m\"; echo 'FAILED broken_test'; echo 'FAILED other_test'; f=1; fi; fi\n[ -z \"$f\" ] || exit 1",
        "if [ -e svc/BREAK ]; then echo 'FAILED broken_test'; exit 1; fi",
        "",
    );
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    queue_all(&lab, &[(1, "svc/WOBBLE"), (2, "svc/BREAK"), (3, "c.txt")]);

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    let red = lab.state().post.red.unwrap();
    assert_eq!(red.culprit.unwrap().pr, Some(PrNumber(2)));
    let wobbles: Vec<Vec<String>> = events_of(&lab, |e| matches!(e, EventKind::Flaky { .. }))
        .into_iter()
        .filter_map(|e| match e {
            EventKind::Flaky { tests, .. } => Some(tests),
            _ => None,
        })
        .collect();
    assert_eq!(wobbles, [["broken_test"]], "other_test는 다시 돌지 않았다");
}

#[test]
fn the_search_of_fourteen_merges_runs_the_whole_step_five_times_and_the_probe_five_times() {
    let lab = probe_lab(
        "if [ -e svc/LIST ]; then echo 'FAILED list_format_test'; f=1; fi\nif [ -e svc/STALE ]; then echo 'FAILED stale_test'; f=1; fi\n[ -z \"$f\" ] || exit 1",
        "for t in \"$@\"; do\n  if [ \"$t\" = list_format_test ] && [ -e svc/LIST ]; then echo 'FAILED list_format_test'; f=1; fi\n  if [ \"$t\" = stale_test ] && [ -e svc/STALE ]; then echo 'FAILED stale_test'; f=1; fi\ndone\n[ -z \"$f\" ] || exit 1",
        "",
    );
    assert_eq!(lab.post().run_once().unwrap(), PostPass::Passed);
    queue_all(
        &lab,
        &[
            (1, "svc/LIST"),
            (2, "f2.txt"),
            (3, "f3.txt"),
            (4, "f4.txt"),
            (5, "svc/STALE"),
            (6, "f6.txt"),
            (7, "f7.txt"),
        ],
    );
    let fix = lab.push_pr_branch_removing(8, "svc/LIST");
    lab.queue_pr(8, &fix, Some(lab.agent()), chute::model::Approval::Agent);
    merged(&lab);
    queue_all(
        &lab,
        &[
            (9, "f9.txt"),
            (10, "f10.txt"),
            (11, "f11.txt"),
            (12, "f12.txt"),
            (13, "f13.txt"),
            (14, "f14.txt"),
        ],
    );
    let tip = lab.origin_main();
    let merges = lab.merge_commits_on_main();
    assert_eq!(merges.len(), 14);
    let green_run = lines_of(&lab, "post-runs.log").len();

    assert_eq!(lab.post().run_once().unwrap(), PostPass::Failed);

    assert_eq!(
        lines_of(&lab, "post-runs.log")[green_run..],
        [
            tip,
            merges[6].clone(),
            merges[2].clone(),
            merges[4].clone(),
            merges[3].clone()
        ],
        "단계 전체는 tip과 후보 넷에서 한 번씩 돈다"
    );
    assert_eq!(
        lines_of(&lab, "probe-args.log"),
        ["stale_test"; 5],
        "probe는 모두 tip의 시험 하나만 다시 돈다"
    );
    let red = lab.state().post.red.unwrap();
    assert_eq!(red.tests, ["stale_test"]);
    assert_eq!(red.culprit.unwrap().pr, Some(PrNumber(5)));
}
