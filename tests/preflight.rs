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

fn env_config(run: &str, env_check: &str) -> String {
    format!("env_check = {env_check}\n{}", config(run))
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
    let refs = failed_refs(&w);
    assert_eq!(refs.len(), 1, "{refs:?}");
    let tree = report_doc_tree(&doc);
    assert!(refs[0].ends_with(&tree), "{refs:?} {tree}");
    let plain = preflight(&w, &[]);
    assert_eq!(code(&plain), 1);
    let shown = text(&plain);
    assert!(shown.contains("검증 실패(단독)"), "{shown}");
    assert!(shown.contains("보고서: "), "{shown}");
}

fn failed_refs(w: &World) -> Vec<String> {
    w.repo
        .git(&["for-each-ref", "--format=%(refname)", "refs/chute/failed/"])
        .lines()
        .map(str::to_owned)
        .collect()
}

fn report_doc_tree(doc: &Value) -> String {
    let report: Value = serde_json::from_str(&read(&doc["report_json"])).unwrap();
    report["step"]["tree"].as_str().unwrap().to_owned()
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
                    env: None,
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
    assert_eq!(
        w.sb.fake("hc").calls(),
        [["server", "status", "--json"]],
        "hc는 이 PC의 코어 id를 묻는 데만 불려야 한다"
    );
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

#[test]
fn preflight_waits_for_a_free_slot_when_max_parallel_are_held_and_says_so_once() {
    let w = World::with_config(Some(format!(
        "max_parallel = 1\n{}",
        config("test -d \"$CHUTE_CACHE_DIR\"")
    )));
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    let held = hold_slot(&w, chute::gitops::SlotName::Preflight(1));

    let mut command = w.sb.command(env!("CARGO_BIN_EXE_chute"));
    let mut child = command
        .current_dir(w.repo.path())
        .arg("preflight")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let notice = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
    assert_eq!(notice, "다른 preflight 1개가 돌고 있어 기다린다");
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert!(child.try_wait().unwrap().is_none(), "슬롯이 없는데 끝났다");

    drop(held);
    let status = child.wait().unwrap();
    reader.join().unwrap();
    let rest: Vec<String> = rx.try_iter().collect();
    assert!(status.success(), "{rest:?}");
    let repeats = rest.iter().filter(|line| line.contains("기다린다")).count();
    assert_eq!(repeats, 0, "기다림 메시지가 두 번 나왔다: {rest:?}");
}

fn hold_slot(w: &World, name: chute::gitops::SlotName) -> chute::gitops::SlotLock {
    let home = chute::store::Home::at(w.sb.chute_home());
    let ext = chute::ext::Ext::from_vars(|key| w.sb.var(key));
    let info = home
        .repo(&"acme/widget".to_owned().try_into().unwrap())
        .info()
        .unwrap()
        .unwrap();
    chute::gitops::Gitops::new(&ext, &home, &info)
        .lock_slot(name)
        .unwrap()
        .expect("슬롯을 잡지 못했다")
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
                step: None,
                log: None,
                pushed_by_chute: false,
                rechecked: false,
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

#[test]
fn failures_of_branches_with_similar_names_keep_their_own_refs() {
    let w = world("test ! -f bad.txt || { echo 'FAIL bad_case'; exit 1; }");
    let mut trees = Vec::new();
    for (branch, file) in [("feat/x", "bad.txt"), ("feat.x", "bad.txt")] {
        w.repo
            .git(&["checkout", "--quiet", "-b", branch, "origin/main"]);
        w.repo.commit_file(file, branch, &format!("feat: {branch}"));
        let out = preflight(&w, &["--json"]);
        assert_eq!(code(&out), 1);
        trees.push(report_doc_tree(&json(&out)));
    }
    assert_ne!(trees[0], trees[1]);
    let refs = failed_refs(&w);
    assert_eq!(refs.len(), 2, "{refs:?}");
    for tree in &trees {
        assert!(refs.iter().any(|r| r.ends_with(tree.as_str())), "{refs:?}");
    }
    w.repo.git(&["rm", "--quiet", "bad.txt"]);
    w.repo.git(&["commit", "--quiet", "-m", "fix: 고친다"]);
    assert_eq!(code(&preflight(&w, &[])), 0);
    assert_eq!(
        failed_refs(&w).len(),
        2,
        "통과해도 다른 보고서의 참조는 남는다"
    );
}

#[test]
fn failed_refs_older_than_a_week_are_removed_and_fresh_ones_stay() {
    let w = world("true");
    let head = work_on(&w, "feat/mine", "mine.txt", "x\n");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let old = format!("refs/chute/failed/preflight-1-{head}");
    let fresh = format!("refs/chute/failed/preflight-{now_ms}-{head}");
    let foreign = "refs/chute/failed/pr-12-abc";
    for name in [&old, &fresh, &foreign.to_owned()] {
        w.repo.git(&["update-ref", name, &head]);
    }
    assert_eq!(code(&preflight(&w, &[])), 0);
    let refs = failed_refs(&w);
    assert!(!refs.contains(&old), "{refs:?}");
    assert!(refs.contains(&fresh), "{refs:?}");
    assert!(refs.contains(&foreign.to_owned()), "{refs:?}");
}

#[test]
fn a_tag_named_like_the_branch_does_not_change_which_pr_is_the_callers() {
    let w = world("true");
    let head = queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    w.repo.git(&["tag", "feat/7", &head]);
    w.repo.git(&["checkout", "--quiet", "feat/7"]);
    let doc = json(&preflight(&w, &["--json"]));
    assert_eq!(doc["with"], serde_json::json!([]));
    assert!(w.state().preflights.contains_key("feat/7"));
    assert_eq!(w.entry(7).unwrap()["preflight"]["result"], "pass");
}

#[test]
fn a_head_that_cannot_be_laid_over_main_alone_is_reported_as_a_conflict() {
    let w = world("test ! -f bad.txt || { echo 'FAIL bad_case'; exit 1; }");
    w.repo.commit_file("f.txt", "orig\n", "chore: 원본");
    w.repo.git(&["push", "--quiet", "origin", "main"]);
    w.repo
        .git(&["checkout", "--quiet", "-b", "feat/mine", "main"]);
    w.repo.commit_file("f.txt", "same\n", "feat: 같은 내용");
    w.repo.commit_file("bad.txt", "x\n", "feat: 깨뜨린다");
    w.repo.git(&["checkout", "--quiet", "main"]);
    w.repo.commit_file("f.txt", "main\n", "feat: main이 고친다");
    w.repo.git(&["push", "--quiet", "origin", "main"]);
    queue_pr(&w, 7, "f.txt", "same\n", ItemState::Queued(None));
    w.repo.git(&["checkout", "--quiet", "feat/mine"]);
    let out = preflight(&w, &["--json"]);
    assert_eq!(code(&out), 1, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json(&out);
    assert_eq!(doc["result"], "conflict");
    assert_eq!(doc["with"], serde_json::json!([]));
    let report = read(&doc["report"]);
    assert!(report.contains("f.txt"), "{report}");
    assert!(report.contains("main이 고친다"), "{report}");
    assert!(!report.contains("통과하지만"), "{report}");
    assert_eq!(
        serde_json::to_value(w.state().preflights["feat/mine"].result).unwrap(),
        "conflict"
    );
}

#[test]
fn a_branch_name_ending_in_a_unicode_space_is_not_taken_for_another_branch() {
    let w = world("true");
    queue_pr(&w, 7, "a.txt", "a\n", ItemState::Queued(None));
    let odd = "feat/7\u{a0}";
    w.repo
        .git(&["checkout", "--quiet", "-b", odd, "origin/main"]);
    w.repo.commit_file("mine.txt", "x\n", "feat: 내 것");
    let doc = json(&preflight(&w, &["--json"]));
    assert_eq!(doc["with"], serde_json::json!([7]));
    let keys: Vec<String> = w.state().preflights.keys().cloned().collect();
    assert_eq!(keys, vec![odd.to_owned()]);
}

#[test]
fn untracked_content_inside_a_submodule_does_not_block_a_preflight() {
    let w = world("true");
    let sub = w.sb.root().join("sub-origin");
    fs::create_dir_all(&sub).unwrap();
    let git_in = |dir: &Path, args: &[&str]| {
        let out =
            w.sb.command("git")
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
    git_in(&sub, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(sub.join("s.txt"), "s\n").unwrap();
    git_in(&sub, &["add", "."]);
    git_in(&sub, &["commit", "--quiet", "-m", "init"]);
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    git_in(
        w.repo.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            &sub.to_string_lossy(),
            "sub",
        ],
    );
    git_in(
        w.repo.path(),
        &["commit", "--quiet", "-m", "feat: 서브모듈"],
    );
    fs::write(w.repo.path().join("sub/build.o"), "junk\n").unwrap();
    let out = preflight(&w, &[]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    fs::write(w.repo.path().join("sub/s.txt"), "changed\n").unwrap();
    assert_eq!(
        code(&preflight(&w, &[])),
        2,
        "추적 파일을 고친 서브모듈은 거절한다"
    );
}

#[test]
fn untracked_files_count_even_when_git_is_set_to_hide_them() {
    let marks = tempfile::tempdir().unwrap();
    let mark = marks.path().join("ran");
    let w = world(&format!("touch {}", mark.display()));
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    w.repo.git(&["config", "status.showUntrackedFiles", "no"]);
    w.repo.write("new-file.txt", "untracked\n");
    let out = preflight(&w, &[]);
    assert_eq!(code(&out), 2);
    assert!(!mark.exists(), "검증이 돌았다");
}

#[test]
fn preflight_env_is_in_the_json_the_report_and_the_record() {
    let w = World::with_config(Some(env_config(
        "test ! -f bad.txt || { echo 'FAIL bad_case'; exit 1; }",
        "[\"echo tool 1.2\", \"echo broken >&2; exit 4\"]",
    )));
    let head = work_on(&w, "feat/bad", "bad.txt", "x\n");

    let out = preflight(&w, &["--json"]);

    assert_eq!(code(&out), 1, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json(&out);
    let env = &doc["env"];
    assert_eq!(env["machine_id"], support::SANDBOX_MACHINE_ID);
    assert!(!env["host"].as_str().unwrap().is_empty());
    assert_eq!(env["checks"][0]["run"], "echo tool 1.2");
    assert_eq!(env["checks"][0]["output"], "tool 1.2");
    assert_eq!(env["checks"][0]["ok"], true);
    assert_eq!(env["checks"][1]["output"], "broken");
    assert_eq!(env["checks"][1]["ok"], false);
    let report = read(&doc["report"]);
    assert!(report.contains("## 검사 환경"), "{report}");
    assert!(report.contains("`echo tool 1.2`: `tool 1.2`"), "{report}");
    assert!(!report.contains("preflight를 돌린 PC"), "{report}");
    let report_json: Value = serde_json::from_str(&read(&doc["report_json"])).unwrap();
    assert_eq!(report_json["env"]["env"]["checks"][0]["output"], "tool 1.2");
    let recorded = w.state().preflights["feat/bad"].env.clone().unwrap();
    assert_eq!(recorded.checks.len(), 2);
    assert_eq!(recorded.checks[0].output, "tool 1.2");
    assert_eq!(recorded.host, env["host"]);
    assert!(w.state().preflights["feat/bad"].head.as_str() == head);
}

#[test]
fn preflight_env_is_printed_for_a_pass_and_kept_without_env_check() {
    let w = World::with_config(Some(env_config("true", "[\"echo tool 1.2\"]")));
    work_on(&w, "feat/mine", "mine.txt", "x\n");
    let out = preflight(&w, &[]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let shown = text(&out);
    assert!(shown.contains("검사 환경: "), "{shown}");
    assert!(shown.contains("echo tool 1.2: tool 1.2"), "{shown}");

    let plain = world("true");
    work_on(&plain, "feat/mine", "mine.txt", "x\n");
    let doc = json(&preflight(&plain, &["--json"]));
    assert_eq!(doc["env"]["checks"], serde_json::json!([]));
    assert_eq!(doc["env"]["machine_id"], support::SANDBOX_MACHINE_ID);
    assert!(
        plain.state().preflights["feat/mine"]
            .env
            .as_ref()
            .is_some_and(|env| env.checks.is_empty())
    );
}

mod remote {
    use super::*;
    use chute::model::{QueueItem, Slug};
    use chute::store::Home;
    use support::{Net, Node};

    const BIN: &str = env!("CARGO_BIN_EXE_chute");

    fn slug() -> Slug {
        Slug::new("acme", "widget").unwrap()
    }

    fn hub_state(node: &Node) -> State {
        Home::at(node.sb.chute_home()).repo(&slug()).load().unwrap()
    }

    fn mesh(run: &str) -> Net {
        mesh_with(config(run))
    }

    fn mesh_with(config: String) -> Net {
        let net = Net::new("acme/widget", &["a", "b"], BIN);
        for node in &net.nodes {
            let out = node.chute(BIN, &["init"]);
            let _ = fs::remove_file(node.repo.path().join("chute.toml"));
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        net.connect_all();
        let a = net.node("a");
        a.repo
            .commit_file("chute.toml", &config, "chore: chute 설정");
        a.repo.git(&["push", "--quiet", "origin", "main"]);
        let out = net.node("b").chute(BIN, &["hub", "set", "a"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        net
    }

    fn hub_queue(net: &Net, pr: u64, file: &str, content: &str, state: ItemState) -> String {
        let a = net.node("a");
        let branch = format!("feat/{pr}");
        let head = a.repo.pr_branch(&branch, file, content);
        a.repo.git(&[
            "push",
            "--quiet",
            "origin",
            &format!("{head}:refs/pull/{pr}/head"),
        ]);
        let mut queued: QueueItem = queue_kit::item(pr, state);
        queued.branch = branch;
        queued.head = Sha::try_from(head.clone()).unwrap();
        Home::at(a.sb.chute_home())
            .repo(&slug())
            .update(|s| {
                s.queue.push(queued);
                Ok(())
            })
            .unwrap();
        head
    }

    fn work_on_b(net: &Net, branch: &str, file: &str, content: &str) -> String {
        let b = net.node("b");
        b.repo.git(&["fetch", "--quiet", "origin"]);
        b.repo
            .git(&["checkout", "--quiet", "-b", branch, "origin/main"]);
        b.repo
            .commit_file(file, content, &format!("feat: {branch}"))
    }

    fn run_b(net: &Net, args: &[&str]) -> Output {
        let argv = [&["preflight"][..], args].concat();
        net.node("b").chute(BIN, &argv)
    }

    fn stderr(out: &Output) -> String {
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    #[test]
    fn a_pc_that_is_not_the_hub_verifies_with_the_hubs_queue() {
        let net = mesh("true");
        hub_queue(&net, 7, "a.txt", "a\n", ItemState::Queued(None));
        hub_queue(&net, 8, "b.txt", "b\n", ItemState::InBatch);
        hub_queue(&net, 9, "c.txt", "c\n", ItemState::AwaitingApproval);
        work_on_b(&net, "feat/mine", "mine.txt", "x\n");
        assert!(hub_state(net.node("b")).queue.is_empty());

        let out = run_b(&net, &[]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        assert!(
            text(&out).contains("함께 검증한 PR #7, #8"),
            "{}",
            text(&out)
        );
        assert!(hub_state(net.node("b")).queue.is_empty());
    }

    #[test]
    fn the_queue_of_the_hub_is_laid_over_the_head_before_the_check_runs() {
        let net = mesh("test -f a.txt && test -f mine.txt");
        hub_queue(&net, 7, "a.txt", "a\n", ItemState::Queued(None));
        work_on_b(&net, "feat/mine", "mine.txt", "x\n");
        let out = run_b(&net, &["--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        assert_eq!(json(&out)["with"], serde_json::json!([7]));
    }

    #[test]
    fn a_conflict_with_the_hubs_queue_is_reported_on_this_pc() {
        let net = mesh("true");
        hub_queue(&net, 7, "shared.txt", "theirs\n", ItemState::Queued(None));
        work_on_b(&net, "feat/mine", "shared.txt", "mine\n");
        let out = run_b(&net, &["--json"]);
        assert_eq!(code(&out), 1, "{}", stderr(&out));
        let doc = json(&out);
        assert_eq!(doc["result"], "conflict");
        let path = PathBuf::from(doc["report"].as_str().unwrap());
        let here = net.node("b").sb.chute_home();
        assert!(path.starts_with(&here), "{path:?} {here:?}");
        let report = read(&doc["report"]);
        assert!(report.contains("shared.txt"), "{report}");
        assert!(report.contains("큐의 PR #7"), "{report}");
    }

    #[test]
    fn a_passing_result_is_recorded_on_the_hub_and_shown_by_its_status() {
        let net = mesh("true");
        hub_queue(&net, 7, "a.txt", "a\n", ItemState::Queued(None));
        hub_queue(&net, 2, "two.txt", "two\n", ItemState::AwaitingApproval);
        let b = net.node("b");
        b.repo.git(&["fetch", "--quiet", "origin"]);
        b.repo
            .git(&["checkout", "--quiet", "-b", "feat/2", "origin/feat/2"]);
        let head = b.repo.commit_file("more.txt", "x\n", "feat: 더");

        let out = run_b(&net, &[]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));

        let hub = hub_state(net.node("a"));
        let record = hub.preflights.get("feat/2").expect("허브에 기록됐다");
        assert_eq!(record.result, chute::model::PreflightResult::Pass);
        assert_eq!(record.head.as_str(), head);
        assert_eq!(record.with, vec![PrNumber(7)]);
        assert!(hub_state(net.node("b")).preflights.is_empty());

        let status: Value =
            serde_json::from_slice(&net.node("a").chute(BIN, &["status", "--json"]).stdout)
                .unwrap();
        let item = status["queue"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["pr"] == 2)
            .unwrap();
        assert_eq!(item["preflight"]["result"], "pass");
        assert_eq!(item["preflight"]["head"], head);
    }

    #[test]
    fn preflight_env_of_a_pc_that_is_not_the_hub_is_kept_on_the_hub() {
        let net = mesh_with(env_config("true", "[\"echo tool 1.2\"]"));
        hub_queue(&net, 2, "two.txt", "two\n", ItemState::AwaitingApproval);
        let b = net.node("b");
        b.repo.git(&["fetch", "--quiet", "origin"]);
        b.repo
            .git(&["checkout", "--quiet", "-b", "feat/2", "origin/feat/2"]);
        b.repo.commit_file("more.txt", "x\n", "feat: 더");

        let out = run_b(&net, &["--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));

        let hub = hub_state(net.node("a"));
        let env = hub.preflights["feat/2"]
            .env
            .clone()
            .expect("허브에 env가 남았다");
        assert_eq!(env.checks.len(), 1);
        assert_eq!(env.checks[0].output, "tool 1.2");
        assert_eq!(json(&out)["env"]["checks"][0]["output"], "tool 1.2");
        assert_eq!(
            env.machine_id.as_ref().map(|id| id.as_str()),
            json(&out)["env"]["machine_id"].as_str()
        );
        assert!(hub_state(net.node("b")).preflights.is_empty());
    }

    #[test]
    fn a_hub_that_cannot_be_reached_stops_with_exit_2_and_uses_no_slot() {
        let net = mesh("true");
        hub_queue(&net, 7, "a.txt", "a\n", ItemState::Queued(None));
        work_on_b(&net, "feat/mine", "mine.txt", "x\n");
        net.set_state("b", "a", "unreachable");

        let out = run_b(&net, &[]);
        assert_eq!(code(&out), 2, "{}", stderr(&out));
        assert!(
            stderr(&out).contains("허브에 닿지 않아 큐를 알 수 없다"),
            "{}",
            stderr(&out)
        );
        assert!(text(&out).is_empty(), "{}", text(&out));
        let slots = Home::at(net.node("b").sb.chute_home()).slots_dir(&slug());
        assert!(!slots.exists(), "{slots:?}");
    }

    #[test]
    fn a_failed_recording_only_warns_and_keeps_the_result_and_exit_code() {
        let peers = |net: &Net| net.node("b").sb.root().join("net").join("peers");
        let cut = |net: &Net| format!("sed -i 's/connected/unreachable/' {}", peers(net).display());

        let net = mesh("true");
        let run = cut(&net);
        net.node("a")
            .repo
            .commit_file("chute.toml", &config(&run), "chore: 검증 중 허브가 끊긴다");
        net.node("a")
            .repo
            .git(&["push", "--quiet", "origin", "main"]);
        hub_queue(&net, 7, "a.txt", "a\n", ItemState::Queued(None));
        work_on_b(&net, "feat/mine", "mine.txt", "x\n");

        let out = run_b(&net, &["--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        assert_eq!(json(&out)["result"], "pass");
        assert!(
            stderr(&out).contains("경고: 결과를 상태에 기록하지 못했다"),
            "{}",
            stderr(&out)
        );
        assert!(hub_state(net.node("a")).preflights.is_empty());
    }

    #[test]
    fn a_failing_result_keeps_exit_code_1_when_the_recording_fails_too() {
        let net = mesh("true");
        let run = format!(
            "sed -i 's/connected/unreachable/' {}; echo 'FAIL boom'; exit 1",
            net.node("b").sb.root().join("net").join("peers").display()
        );
        net.node("a")
            .repo
            .commit_file("chute.toml", &config(&run), "chore: 검증이 실패한다");
        net.node("a")
            .repo
            .git(&["push", "--quiet", "origin", "main"]);
        work_on_b(&net, "feat/mine", "mine.txt", "x\n");
        let out = run_b(&net, &[]);
        assert_eq!(code(&out), 1, "{}", stderr(&out));
        assert!(stderr(&out).contains("경고: 결과를 상태에 기록하지 못했다"));
    }

    #[test]
    fn a_broken_base_on_the_hub_is_announced_and_reported_here() {
        let net = mesh("true");
        let base = net.node("a").repo.git(&["rev-parse", "origin/main"]);
        Home::at(net.node("a").sb.chute_home())
            .repo(&slug())
            .update(|state| {
                state.blocked = Some(chute::model::Blocked {
                    reason: chute::model::BlockReason::BaseBroken,
                    base: Sha::try_from(base.trim().to_owned()).unwrap(),
                    since_ms: chute::model::EpochMs(1),
                    step: None,
                    log: None,
                    pushed_by_chute: false,
                    rechecked: false,
                });
                Ok(())
            })
            .unwrap();
        work_on_b(&net, "feat/mine", "mine.txt", "x\n");
        let out = run_b(&net, &["--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        assert!(stderr(&out).starts_with("경고: 저장소가 base_broken"));
        assert_eq!(json(&out)["blocked"]["reason"], "base_broken");
    }

    #[test]
    fn a_hub_that_refuses_the_record_leaves_a_warning_and_the_result() {
        let net = mesh("true");
        let hub_state = Home::at(net.node("a").sb.chute_home())
            .repo(&slug())
            .dir()
            .join("state.json");
        let run = format!("printf '{{' > {}", hub_state.display());
        net.node("a").repo.commit_file(
            "chute.toml",
            &config(&run),
            "chore: 검증 중 허브 상태가 깨진다",
        );
        net.node("a")
            .repo
            .git(&["push", "--quiet", "origin", "main"]);
        work_on_b(&net, "feat/mine", "mine.txt", "x\n");
        let out = run_b(&net, &["--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        assert_eq!(json(&out)["result"], "pass");
        assert!(
            stderr(&out).contains("경고: 결과를 상태에 기록하지 못했다"),
            "{}",
            stderr(&out)
        );
    }

    #[test]
    fn the_hub_prunes_by_its_own_clock_not_the_senders() {
        let net = mesh("true");
        let now = chute::model::EpochMs::now().0;
        let record = |head: &str, at: u64| {
            serde_json::json!({
                "branch": "old/keep",
                "record": {"head": head, "result": "pass", "report": null, "at_ms": at}
            })
            .to_string()
        };
        let head = net.node("a").repo.head();
        let sent = |text: &str| {
            let out = net.node("b").chute(BIN, &["preflight", "--record", text]);
            assert_eq!(code(&out), 0, "{}", stderr(&out));
        };
        sent(&record(&head, now - 1000));
        let skewed = record(&head, now + 365 * 24 * 3600 * 1000).replace("old/keep", "feat/skew");
        sent(&skewed);
        let kept: Vec<String> = hub_state(net.node("a")).preflights.into_keys().collect();
        assert_eq!(kept, ["feat/skew", "old/keep"]);
    }
}
