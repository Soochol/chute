mod engine_kit;
mod support;

use std::fs;
use std::process::Output;

use chute::engine::Pass;
use chute::model::BatchId;
use engine_kit::Lab;

const NEEDS_A: &str = "if [ -e gate ] && [ ! -e a.txt ]; then fail \"FAILED needs_a\"; fi";

fn chute(lab: &Lab, args: &[&str]) -> Output {
    lab.sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .current_dir(lab.repo.path())
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn merge_batch(lab: &Lab, prs: &[(u64, &str, &str)]) -> BatchId {
    for (n, path, content) in prs {
        lab.open_pr(*n, &[(path, content)]);
    }
    match lab.engine().run_once().unwrap() {
        Pass::Done(summary) => {
            assert_eq!(summary.merged.len(), prs.len(), "{summary:?}");
            summary.batch
        }
        other => panic!("batch가 끝나야 한다: {other:?}"),
    }
}

fn tree(lab: &Lab, rev: &str) -> String {
    lab.origin_git(&["rev-parse", &format!("{rev}^{{tree}}")])
}

fn on_main(lab: &Lab, path: &str) -> bool {
    lab.sb
        .command("git")
        .arg("--git-dir")
        .arg(lab.repo.origin())
        .args(["cat-file", "-e", &format!("main:{path}")])
        .status()
        .unwrap()
        .success()
}

fn hc_calls(lab: &Lab) -> usize {
    lab.sb.fake("hc").calls().len()
}

#[test]
fn reverting_a_two_pr_batch_restores_the_tree_before_it_and_keeps_history() {
    let lab = Lab::new("");
    let id = merge_batch(&lab, &[(1, "a.txt", "a\n"), (2, "b.txt", "b\n")]);
    let record = lab.store.read_batch(&id).unwrap().unwrap();
    let before_tree = tree(&lab, record.base.as_str());
    let main_before = lab.origin_main();
    let hc_before = hc_calls(&lab);

    let out = chute(&lab, &["revert", &id.0]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(tree(&lab, "main"), before_tree);
    lab.origin_git(&["merge-base", "--is-ancestor", &main_before, "main"]);
    assert_ne!(lab.origin_main(), main_before);
    let text = stdout(&out);
    assert!(text.contains("#1") && text.contains("#2"), "{text}");
    let newest = lab.origin_main();
    let older = lab.origin_git(&["rev-parse", "main~1"]);
    for revert_commit in [&newest, &older] {
        assert!(
            text.contains(revert_commit.as_str()),
            "{text}\n{revert_commit}"
        );
    }
    assert!(text.contains("git revert"), "{text}");
    assert_eq!(hc_calls(&lab), hc_before, "hc를 불렀다");
    assert!(
        lab.state()
            .verified_bases
            .iter()
            .any(|b| b.as_str() == newest)
    );
    assert!(lab.state().queue.is_empty());
}

#[test]
fn a_later_batch_that_does_not_overlap_survives_the_revert() {
    let lab = Lab::new("");
    let first = merge_batch(&lab, &[(1, "a.txt", "a\n"), (2, "b.txt", "b\n")]);
    merge_batch(&lab, &[(3, "c.txt", "c\n")]);

    let out = chute(&lab, &["revert", &first.0]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!on_main(&lab, "a.txt") && !on_main(&lab, "b.txt"));
    assert!(on_main(&lab, "c.txt"));
}

#[test]
fn a_conflict_with_a_later_batch_pushes_nothing_and_names_that_batch() {
    let lab = Lab::new("");
    let first = merge_batch(&lab, &[(1, "a.txt", "one\n")]);
    let second = merge_batch(&lab, &[(2, "a.txt", "two\n")]);
    let main_before = lab.origin_main();

    let out = chute(&lab, &["revert", &first.0]);

    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(lab.origin_main(), main_before);
    let err = stderr(&out);
    assert!(err.contains(&second.0) && err.contains("a.txt"), "{err}");
    let again = chute(&lab, &["batches", "--json"]);
    let list: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|b| b["reverted"] == false)
    );
}

#[test]
fn a_tree_that_fails_verification_is_not_pushed_unless_no_verify() {
    let lab = Lab::new(NEEDS_A);
    let id = merge_batch(&lab, &[(1, "a.txt", "a\n")]);
    lab.main_commit(&[("gate", "on\n")], "chore: gate");
    let main_before = lab.origin_main();

    let out = chute(&lab, &["revert", &id.0]);

    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(lab.origin_main(), main_before);
    let err = stderr(&out);
    assert!(err.contains("check") && err.contains(".log"), "{err}");
    assert!(on_main(&lab, "a.txt"));

    let out = chute(&lab, &["revert", &id.0, "--no-verify"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!on_main(&lab, "a.txt"));
    let pushed = lab.origin_main();
    assert!(
        !lab.state()
            .verified_bases
            .iter()
            .any(|b| b.as_str() == pushed)
    );
}

#[test]
fn a_batch_cannot_be_reverted_twice_and_an_unknown_batch_is_refused() {
    let lab = Lab::new("");
    let id = merge_batch(&lab, &[(1, "a.txt", "a\n")]);
    assert_eq!(chute(&lab, &["revert", &id.0]).status.code(), Some(0));
    let main_after = lab.origin_main();

    let twice = chute(&lab, &["revert", &id.0]);
    let unknown = chute(&lab, &["revert", "b-no-such-batch"]);

    assert_eq!(twice.status.code(), Some(1));
    assert!(stderr(&twice).contains("이미 되돌린"), "{}", stderr(&twice));
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        stderr(&unknown).contains("batch가 없다"),
        "{}",
        stderr(&unknown)
    );
    assert_eq!(lab.origin_main(), main_after);
}

#[test]
fn batches_json_lists_the_newest_first_and_marks_the_reverted_one() {
    let lab = Lab::new("");
    let first = merge_batch(&lab, &[(1, "a.txt", "a\n"), (2, "b.txt", "b\n")]);
    let second = merge_batch(&lab, &[(3, "c.txt", "c\n")]);
    assert_eq!(chute(&lab, &["revert", &first.0]).status.code(), Some(0));

    let out = chute(&lab, &["batches", "--json"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"], second.0.as_str());
    assert_eq!(list[0]["reverted"], false);
    assert_eq!(list[0]["merged"], serde_json::json!([3]));
    assert_eq!(list[1]["id"], first.0.as_str());
    assert_eq!(list[1]["reverted"], true);
    assert_eq!(list[1]["merged"], serde_json::json!([1, 2]));
    assert_eq!(list[1]["revert_commit"], lab.origin_main().as_str());

    let text = stdout(&chute(&lab, &["batches"]));
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with(&second.0) && !lines[0].contains("되돌림"),
        "{text}"
    );
    assert!(
        lines[1].starts_with(&first.0) && lines[1].contains("되돌림"),
        "{text}"
    );
}

#[test]
fn a_batch_record_that_is_incomplete_is_refused_without_guessing() {
    let lab = Lab::new("");
    let unfinished = merge_batch(&lab, &[(1, "a.txt", "a\n")]);
    let without_commits = merge_batch(&lab, &[(2, "b.txt", "b\n")]);
    let mut record = lab.store.read_batch(&unfinished).unwrap().unwrap();
    record.finished_ms = None;
    lab.store.write_batch(&record).unwrap();
    let mut record = lab.store.read_batch(&without_commits).unwrap().unwrap();
    record.merges.clear();
    lab.store.write_batch(&record).unwrap();
    let main_before = lab.origin_main();

    let first = chute(&lab, &["revert", &unfinished.0]);
    let second = chute(&lab, &["revert", &without_commits.0]);

    assert_eq!(first.status.code(), Some(1));
    assert!(
        stderr(&first).contains("끝나지 않았다"),
        "{}",
        stderr(&first)
    );
    assert_eq!(second.status.code(), Some(1));
    assert!(stderr(&second).contains("#2"), "{}", stderr(&second));
    assert_eq!(lab.origin_main(), main_before);
}

#[test]
fn a_batch_that_merged_nothing_is_refused() {
    let lab = Lab::new("");
    let id = merge_batch(&lab, &[(1, "a.txt", "a\n")]);
    let mut record = lab.store.read_batch(&id).unwrap().unwrap();
    record.merges.clear();
    lab.store.write_batch(&record).unwrap();
    let history = lab.store.dir().join("history.jsonl");
    let kept: Vec<String> = fs::read_to_string(&history)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("\"batch_finished\"") && !l.contains("\"merged\""))
        .map(str::to_owned)
        .collect();
    fs::write(&history, kept.join("\n") + "\n").unwrap();

    let out = chute(&lab, &["revert", &id.0]);

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("머지한 PR이 없"), "{}", stderr(&out));
}

#[test]
fn when_main_moves_during_the_revert_it_starts_again_on_the_new_main() {
    let body = format!(
        "{NEEDS_A}\nbump='@ROOT@/bump'\nif [ -e \"$bump\" ]; then rm \"$bump\"; c=$(git --git-dir='@ORIGIN@' commit-tree -p main -m bump 'main^{{tree}}') && git --git-dir='@ORIGIN@' update-ref refs/heads/main \"$c\"; fi"
    );
    let lab = Lab::new(&body);
    let id = merge_batch(&lab, &[(1, "a.txt", "a\n")]);
    let runs_before = lab.verify_runs();
    fs::write(lab.sb.root().join("bump"), "").unwrap();

    let out = chute(&lab, &["revert", &id.0]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(lab.verify_runs() - runs_before, 2);
    assert!(!on_main(&lab, "a.txt"));
    let subjects = lab.origin_git(&["log", "--first-parent", "--format=%s", "-3", "main"]);
    assert!(subjects.contains("bump"), "{subjects}");
    assert!(subjects.starts_with("Revert"), "{subjects}");
}
