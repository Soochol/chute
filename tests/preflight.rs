mod queue_kit;
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use chute::model::{ItemState, PrNumber, Sha, State};
use queue_kit::{World, item};
use serde_json::Value;

fn config(run: &str) -> String {
    format!(
        "approval = \"agent\"\n\n[[verify]]\nname = \"check\"\nrun = {run:?}\ntimeout = 60\nfailed = 'FAIL (\\S+)'\n"
    )
}

fn world(run: &str) -> World {
    World::with_config(Some(config(run)))
}

/// Publishes `branch` as PR `pr` on origin and queues it the way `submit` would have.
fn queue_pr(w: &World, pr: u64, file: &str, content: &str, state: ItemState) -> String {
    let branch = format!("feat/{pr}");
    let head = w.repo.pr_branch(&branch, file, content);
    w.repo.git(&[
        "push",
        "--quiet",
        "origin",
        &format!("{head}:refs/pull/{pr}/head"),
    ]);
    let mut queued = item(pr, state);
    queued.branch = branch;
    queued.head = Sha::try_from(head.clone()).unwrap();
    w.seed(vec![queued]);
    head
}

fn work_on(w: &World, branch: &str, file: &str, content: &str) -> String {
    w.repo
        .git(&["checkout", "--quiet", "-b", branch, "origin/main"]);
    w.repo
        .commit_file(file, content, &format!("feat: {branch}"))
}

fn preflight(w: &World, args: &[&str]) -> Output {
    let argv = [&["preflight"][..], args].concat();
    w.chute(w.repo.path(), &argv)
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "JSON이 아니다: {e}\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn read(path: &Value) -> String {
    fs::read_to_string(path.as_str().expect("경로")).unwrap()
}

fn origin_main(w: &World) -> String {
    w.repo.git(&[
        "ls-remote",
        &w.repo.origin().to_string_lossy(),
        "refs/heads/main",
    ])
}

const FILES_TOGETHER: &str =
    "! (test -f a.txt && test -f b.txt) || { echo 'FAIL together'; exit 1; }";

#[test]
fn an_empty_queue_passes_with_an_unpushed_commit() {
    let w = world("test -f mine.txt");
    let head = work_on(&w, "feat/mine", "mine.txt", "x\n");
    let out = preflight(&w, &[]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(text(&out).starts_with("통과: HEAD "));
    let out = preflight(&w, &["--json"]);
    let doc = json(&out);
    assert_eq!(doc["result"], "pass");
    assert_eq!(doc["head"], head);
    assert_eq!(doc["with"], serde_json::json!([]));
    assert!(
        w.repo
            .git(&["branch", "-r", "--contains", &head])
            .trim()
            .is_empty()
    );
}

#[test]
fn a_change_that_breaks_the_check_fails_alone_with_a_report() {
    let w = world("test ! -f bad.txt || { echo 'FAIL bad_case'; exit 1; }");
    let head = work_on(&w, "feat/bad", "bad.txt", "x\n");
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 1);
    let doc = json(&out);
    assert_eq!(doc["result"], "fail");
    assert_eq!(doc["kind"], "verify");
    assert_eq!(doc["mode"], "solo");
    assert_eq!(doc["head"], head);
    let report = read(&doc["report"]);
    assert!(report.contains("단독"), "{report}");
    assert!(report.contains("bad_case"), "{report}");
    let plain = preflight(&w, &[]);
    assert_eq!(code(&plain), 1);
    let shown = text(&plain);
    assert!(shown.contains("검증 실패(단독)"), "{shown}");
    assert!(shown.contains("보고서: "), "{shown}");
    let ref_name = "refs/chute/failed/preflight-feat_bad";
    assert!(
        !w.repo
            .git(&["rev-parse", "--verify", "--quiet", ref_name])
            .trim()
            .is_empty()
    );
}

#[test]
fn a_change_that_only_fails_with_a_queued_pr_is_a_combination() {
    let w = world(FILES_TOGETHER);
    queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    work_on(&w, "feat/mine", "b.txt", "b\n");
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 1, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json(&out);
    assert_eq!(doc["mode"], "combo");
    assert_eq!(doc["with"], serde_json::json!([7]));
    let report = read(&doc["report"]);
    assert!(report.contains("#7") || report.contains("PR 7"), "{report}");
    let report_doc: Value = serde_json::from_str(&read(&doc["report_json"])).unwrap();
    assert_eq!(report_doc["failure"]["mode"], "combo");
    assert_eq!(report_doc["failure"]["partners"][0]["pr"], 7);
}

#[test]
fn a_failure_that_also_happens_alone_is_solo_even_with_a_queue() {
    let w = world("test ! -f bad.txt || { echo 'FAIL bad_case'; exit 1; }");
    queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    work_on(&w, "feat/mine", "bad.txt", "x\n");
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 1);
    let doc = json(&out);
    assert_eq!(doc["mode"], "solo");
    assert_eq!(doc["with"], serde_json::json!([7]));
}

#[test]
fn a_conflict_with_a_queued_pr_names_the_file_and_the_pr() {
    let w = world("true");
    queue_pr(&w, 7, "shared.txt", "theirs\n", ItemState::InBatch);
    work_on(&w, "feat/mine", "shared.txt", "mine\n");
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 1, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json(&out);
    assert_eq!(doc["result"], "conflict");
    assert_eq!(doc["kind"], "conflict");
    let report = read(&doc["report"]);
    assert!(report.contains("shared.txt"), "{report}");
    assert!(report.contains("큐의 PR #7"), "{report}");
}

#[test]
fn a_conflict_with_main_names_the_main_commit() {
    let w = world("true");
    w.repo
        .commit_file("shared.txt", "main\n", "feat: main이 고친다");
    w.repo.git(&["push", "--quiet", "origin", "main"]);
    w.repo
        .git(&["checkout", "--quiet", "-b", "feat/mine", "HEAD~1"]);
    w.repo
        .commit_file("shared.txt", "mine\n", "feat: 내가 고친다");
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 1);
    let report = read(&json(&out)["report"]);
    assert!(report.contains("shared.txt"), "{report}");
    assert!(report.contains("main이 고친다"), "{report}");
    assert!(!report.contains("큐의 PR #"), "{report}");
}

#[test]
fn queued_prs_that_conflict_with_each_other_do_not_stop_the_run() {
    let w = world("test -f mine.txt");
    queue_pr(&w, 7, "shared.txt", "seven\n", ItemState::Queued(None));
    queue_pr(&w, 8, "shared.txt", "eight\n", ItemState::Queued(None));
    queue_pr(&w, 9, "nine.txt", "nine\n", ItemState::Queued(None));
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json(&out);
    assert_eq!(doc["with"], serde_json::json!([7, 9]));
    assert_eq!(doc["skipped"][0]["pr"], 8);
    assert_eq!(doc["skipped"][0]["why"], "conflict");
    assert!(text(&preflight(&w, &[])).contains("건너뛴 PR #8"));
}

#[test]
fn a_pr_whose_head_moved_is_left_out() {
    let w = world("true");
    queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    w.repo.git(&["checkout", "--quiet", "feat/7"]);
    let moved = w.repo.commit_file("a2.txt", "x\n", "feat: 더");
    w.repo.git(&[
        "push",
        "--quiet",
        "origin",
        &format!("{moved}:refs/pull/7/head"),
    ]);
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    let doc = json(&preflight(&w, &["--json"]));
    assert_eq!(doc["result"], "pass");
    assert_eq!(doc["with"], serde_json::json!([]));
    assert_eq!(doc["skipped"][0]["why"], "head_changed");
}

#[test]
fn only_queued_and_in_batch_items_are_verified_with() {
    let w = world("true");
    queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    queue_pr(&w, 8, "b.txt", "b\n", ItemState::InBatch);
    queue_pr(&w, 9, "c.txt", "c\n", ItemState::AwaitingApproval);
    queue_pr(
        &w,
        10,
        "d.txt",
        "d\n",
        ItemState::Bounced(chute::model::BounceReason::Verify),
    );
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    let doc = json(&preflight(&w, &["--json"]));
    assert_eq!(doc["with"], serde_json::json!([7, 8]));
}

#[test]
fn the_pr_of_the_calling_branch_is_left_out() {
    let w = world("true");
    let head = queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    w.repo.git(&["checkout", "--quiet", "feat/7"]);
    let doc = json(&preflight(&w, &["--json"]));
    assert_eq!(doc["with"], serde_json::json!([]));
    assert_eq!(doc["head"], head);
}

#[test]
fn uncommitted_changes_are_refused_without_running_the_check() {
    let marks = tempfile::tempdir().unwrap();
    let mark = marks.path().join("ran");
    let w = world(&format!("touch {}", mark.display()));
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    for dirty in [
        |w: &World| w.repo.write("mine.txt", "changed\n"),
        |w: &World| w.repo.write("new-file.txt", "untracked\n"),
    ] {
        dirty(&w);
        let out = preflight(&w, &[]);
        assert_eq!(code(&out), 2);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("커밋"), "{err}");
        assert!(!mark.exists(), "검증이 돌았다");
        w.repo.git(&["checkout", "--quiet", "--", "."]);
        w.repo.git(&["clean", "--quiet", "-fd"]);
    }
}

#[test]
fn the_result_is_recorded_and_shown_by_status() {
    let w = world("true");
    queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    let head = work_on(&w, "feat/mine", "mine.txt", "x\n");
    let mut mine = item(12, ItemState::AwaitingApproval);
    mine.branch = "feat/mine".into();
    mine.head = Sha::try_from(head.clone()).unwrap();
    w.seed(vec![mine]);
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 0);
    let state: State = w.state();
    let record = &state.preflights["feat/mine"];
    assert_eq!(record.head.as_str(), head);
    assert_eq!(record.with, vec![PrNumber(7)]);
    assert!(record.base.is_some());
    let entry = w.entry(12).unwrap();
    assert_eq!(entry["preflight"]["result"], "pass");
    assert_eq!(entry["preflight"]["head"], head);
}

#[test]
fn a_new_result_replaces_the_old_one_of_the_branch() {
    let w = world("test ! -f bad.txt");
    work_on(&w, "feat/mine", "ok.txt", "x\n");
    assert_eq!(code(&preflight(&w, &[])), 0);
    let second = w.repo.commit_file("bad.txt", "x\n", "feat: 깨뜨린다");
    assert_eq!(code(&preflight(&w, &[])), 1);
    let state = w.state();
    assert_eq!(state.preflights.len(), 1);
    assert_eq!(state.preflights["feat/mine"].head.as_str(), second);
    assert_eq!(
        serde_json::to_value(state.preflights["feat/mine"].result).unwrap(),
        "fail"
    );
}

#[test]
fn old_results_of_unqueued_branches_are_dropped() {
    let w = world("true");
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    w.store()
        .update(|state| {
            state.preflights.insert(
                "feat/stale".into(),
                chute::model::PreflightRecord {
                    head: Sha::try_from("abc1234".to_owned()).unwrap(),
                    result: chute::model::PreflightResult::Pass,
                    report: None,
                    at_ms: chute::model::EpochMs(1),
                    base: None,
                    with: Vec::new(),
                },
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(code(&preflight(&w, &[])), 0);
    let state = w.state();
    assert!(!state.preflights.contains_key("feat/stale"));
    assert!(state.preflights.contains_key("feat/mine"));
}

#[test]
fn queue_origin_main_and_the_worktree_are_untouched() {
    let w = world("test -f mine.txt");
    queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    let head = work_on(&w, "feat/mine", "mine.txt", "x\n");
    let queue_before = w.state().queue;
    let main_before = origin_main(&w);
    let status_before = w.repo.git(&["status", "--porcelain=v2", "--branch"]);
    let index_before = fs::read(w.repo.path().join(".git/index")).unwrap();
    let out = preflight(&w, &[]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(w.state().queue, queue_before);
    assert_eq!(origin_main(&w), main_before);
    assert_eq!(w.repo.head(), head);
    assert_eq!(
        w.repo.git(&["status", "--porcelain=v2", "--branch"]),
        status_before
    );
    assert_eq!(
        fs::read(w.repo.path().join(".git/index")).unwrap(),
        index_before
    );
    assert!(!w.repo.path().join("a.txt").exists());
    assert!(w.sb.fake("hc").calls().is_empty(), "hc가 불렸다");
    assert!(w.sb.fake("gh").calls().is_empty(), "gh가 불렸다");
}

#[test]
fn a_busy_batch_slot_does_not_stop_a_preflight_and_two_can_run_at_once() {
    let marks = tempfile::tempdir().unwrap();
    let dir = marks.path();
    let wait_for_the_other = format!(
        "pwd >> {d}/where; touch {d}/up-$$; i=0; while [ $(ls {d} | grep -c '^up-') -lt 2 ]; do i=$((i+1)); [ $i -gt 300 ] && exit 1; sleep 0.1; done",
        d = dir.display()
    );
    let w = world(&wait_for_the_other);
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    let slots = hold_batch_slot(&w);
    let both = w.run_parallel(&[
        vec!["preflight".to_owned(), "--json".to_owned()],
        vec!["preflight".to_owned(), "--json".to_owned()],
    ]);
    drop(slots);
    for out in &both {
        assert_eq!(code(out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    }
    let seen = fs::read_to_string(dir.join("where")).unwrap();
    let mut places: Vec<&str> = seen.lines().collect();
    places.sort();
    places.dedup();
    assert_eq!(places.len(), 2, "슬롯이 둘이어야 한다: {seen}");
    for place in places {
        assert!(
            place.contains("/preflight-"),
            "preflight 슬롯이 아니다: {place}"
        );
    }
}

fn hold_batch_slot(w: &World) -> chute::gitops::SlotLock {
    let home = chute::store::Home::at(w.sb.chute_home());
    let ext = chute::ext::Ext::from_vars(|key| w.sb.var(key));
    let info = home
        .repo(&"acme/widget".to_owned().try_into().unwrap())
        .info()
        .unwrap()
        .unwrap();
    chute::gitops::Gitops::new(&ext, &home, &info)
        .lock_slot(chute::gitops::SlotName::Batch)
        .unwrap()
        .expect("batch 슬롯을 잡지 못했다")
}

#[test]
fn a_flaky_step_passes_and_is_reported() {
    let marks = tempfile::tempdir().unwrap();
    let first = marks.path().join("first");
    let w = world(&format!(
        "if [ -e {m} ]; then exit 0; fi; touch {m}; echo 'FAIL wobbly'; exit 1",
        m = first.display()
    ));
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json(&out);
    assert_eq!(doc["result"], "pass");
    assert_eq!(doc["flaky"][0]["step"], "check");
    assert_eq!(doc["flaky"][0]["tests"], serde_json::json!(["wobbly"]));
}

#[test]
fn a_broken_base_is_announced_first() {
    let w = world("true");
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    let base = w.repo.git(&["rev-parse", "origin/main"]).trim().to_owned();
    w.store()
        .update(|state| {
            state.blocked = Some(chute::model::Blocked {
                reason: chute::model::BlockReason::BaseBroken,
                base: Sha::try_from(base.clone()).unwrap(),
                since_ms: chute::model::EpochMs(1),
            });
            Ok(())
        })
        .unwrap();
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("경고: 저장소가 base_broken"));
    assert_eq!(json(&out)["blocked"]["reason"], "base_broken");
}

#[test]
fn the_json_carries_head_base_partners_and_report() {
    let w = world(FILES_TOGETHER);
    queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    let head = work_on(&w, "feat/mine", "b.txt", "b\n");
    let base = w.repo.git(&["rev-parse", "origin/main"]).trim().to_owned();
    let doc = json(&preflight(&w, &["--json"]));
    for key in ["result", "head", "base", "with", "report", "at_ms"] {
        assert!(doc.get(key).is_some(), "{key}가 없다: {doc}");
    }
    assert_eq!(doc["head"], head);
    assert_eq!(doc["base"], base);
    assert!(Path::new(doc["report"].as_str().unwrap()).is_file());
    assert!(PathBuf::from(doc["report_json"].as_str().unwrap()).is_file());
}
