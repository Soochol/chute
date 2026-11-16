mod forward_kit;
mod queue_kit;
mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use chute::model::{BounceReason, HumanReason, ItemState};
use queue_kit::{World, item, stderr};
use serde_json::Value;
use support::Reply;

fn history(w: &World) -> Vec<Value> {
    fs::read_to_string(w.store().dir().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn giveup_sends_the_pr_to_a_human_and_records_the_reason() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::Bounced(BounceReason::Conflict))]);

    let stdout = w.ok(
        w.repo.path(),
        &["giveup", "12", "--reason", "충돌을 풀 수 없다"],
    );

    assert!(stdout.contains("사람 차례로 넘겼다: #12"), "{stdout}");
    assert!(stdout.contains("충돌을 풀 수 없다"), "{stdout}");
    let entry = w.entry(12).unwrap();
    assert_eq!(entry["state"], "needs_human");
    assert_eq!(entry["state_reason"], "gave_up");
    let events = history(&w);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["event"], "gave_up");
    assert_eq!(events[0]["pr"], 12);
    assert_eq!(events[0]["reason"], "충돌을 풀 수 없다");
    assert!(events[0]["at_ms"].as_u64().unwrap() > 0, "{events:?}");
}

#[test]
fn giveup_without_a_reason_fails_and_changes_nothing() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::Queued(None))]);

    let out = w.chute(w.repo.path(), &["giveup", "12"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--reason"), "{}", stderr(&out));

    let out = w.chute(w.repo.path(), &["giveup", "12", "--reason", "  "]);
    assert_eq!(out.status.code(), Some(1));

    assert_eq!(w.entry(12).unwrap()["state"], "queued");
    assert!(history(&w).is_empty());
}

#[test]
fn giveup_works_from_outside_the_repository_with_the_repo_flag() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::AwaitingApproval)]);
    let repo = w.repo.path().to_string_lossy().into_owned();

    w.ok(
        w.sb.root(),
        &["giveup", "12", "--reason", "필요 없어졌다", "--repo", &repo],
    );

    assert_eq!(w.entry(12).unwrap()["state_reason"], "gave_up");
}

#[test]
fn giveup_without_a_number_gives_up_the_pr_of_the_current_branch() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::Queued(None))]);
    w.sb.fake("gh")
        .respond("pr view --json number", Reply::ok(r#"{"number":12}"#));

    w.ok(w.repo.path(), &["giveup", "--reason", "막혔다"]);

    assert_eq!(w.entry(12).unwrap()["state_reason"], "gave_up");
    assert_eq!(
        w.sb.fake("gh").calls(),
        [vec!["pr", "view", "--json", "number"]]
    );
}

#[test]
fn giveup_refuses_a_pr_in_a_batch_or_already_with_a_human() {
    let w = World::new();
    w.seed(vec![
        item(12, ItemState::InBatch),
        item(13, ItemState::NeedsHuman(HumanReason::Budget)),
    ]);

    let out = w.chute(w.repo.path(), &["giveup", "12", "--reason", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("batch가 끝난 뒤"), "{}", stderr(&out));

    let out = w.chute(w.repo.path(), &["giveup", "13", "--reason", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("이미 사람 차례"), "{}", stderr(&out));

    assert_eq!(w.entry(12).unwrap()["state"], "in_batch");
    assert_eq!(w.entry(13).unwrap()["state_reason"], "budget");
    assert!(history(&w).is_empty());
}

#[test]
fn giveup_dropping_many_prs_at_once_loses_none_and_records_each_reason() {
    let w = World::new();
    w.seed((1..=10).map(|n| item(n, ItemState::Queued(None))).collect());
    let commands: Vec<Vec<String>> = (1..=10)
        .map(|n| {
            ["giveup", &n.to_string(), "--reason", &format!("이유 {n}")]
                .map(str::to_owned)
                .to_vec()
        })
        .collect();

    for out in w.run_parallel(&commands) {
        assert!(out.status.success(), "{}", stderr(&out));
    }

    assert!(
        w.state()
            .queue
            .iter()
            .all(|i| i.state == ItemState::NeedsHuman(HumanReason::GaveUp))
    );
    assert_eq!(history(&w).len(), 10);
}

#[test]
fn a_corrupt_state_file_stops_giveup_with_exit_code_2_and_writes_no_history() {
    let w = World::new();
    let garbage = w.corrupt_state();
    let out = w.chute(w.repo.path(), &["giveup", "12", "--reason", "x"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(w.state_file()).unwrap(), garbage);
    assert!(history(&w).is_empty());
}

#[test]
fn a_reason_that_starts_with_a_hyphen_needs_the_equals_form_and_never_swallows_a_flag() {
    let w = World::new();
    w.seed(vec![
        item(31, ItemState::Queued(None)),
        item(32, ItemState::Queued(None)),
        item(33, ItemState::Queued(None)),
    ]);

    w.ok(w.repo.path(), &["giveup", "31", "--reason=-1 retries left"]);
    w.ok(w.repo.path(), &["giveup", "32", "--reason=  --force  "]);
    for argv in [
        &["giveup", "33", "--reason", "--help"][..],
        &["giveup", "33", "--reason", "-1 retries left"][..],
    ] {
        let out = w.chute(w.repo.path(), argv);
        assert_ne!(out.status.code(), Some(1), "{argv:?}");
        assert_eq!(w.entry(33).unwrap()["state"], "queued", "{argv:?}");
    }

    let reasons: Vec<String> = history(&w)
        .iter()
        .map(|e| e["reason"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(reasons, ["-1 retries left", "--force"]);
}

#[test]
fn without_a_number_and_with_the_repo_flag_gh_runs_in_that_repository() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::Queued(None))]);
    let repo = w.repo.path().canonicalize().unwrap();
    w.sb.script(
        "gh",
        &format!(
            "[ \"$(pwd -P)\" = '{}' ] || {{ echo \"gh이 엉뚱한 폴더에서 돌았다: $(pwd -P)\" >&2; exit 1; }}\necho '{{\"number\":12}}'\n",
            repo.display()
        ),
    );

    let out = w.chute(
        w.sb.root(),
        &[
            "--repo",
            &repo.to_string_lossy(),
            "giveup",
            "--reason",
            "막혔다",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(w.entry(12).unwrap()["state_reason"], "gave_up");
}

#[test]
fn a_giveup_that_cannot_write_the_state_leaves_no_history_line() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::Queued(None))]);
    let dir = w.store().dir().to_owned();
    fs::write(dir.join("history.jsonl"), "").unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let blocked = fs::File::create(dir.join("probe")).is_err();

    let out = w.chute(w.repo.path(), &["giveup", "12", "--reason", "x"]);

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    if !blocked {
        return;
    }
    assert_eq!(out.status.code(), Some(2));
    assert!(history(&w).is_empty());
    assert_eq!(w.entry(12).unwrap()["state"], "queued");
}

#[test]
fn a_history_line_that_cannot_be_written_only_warns_after_the_state_moved() {
    let w = World::new();
    w.seed(vec![item(12, ItemState::Queued(None))]);
    fs::create_dir(w.store().dir().join("history.jsonl")).unwrap();

    let out = w.chute(w.repo.path(), &["giveup", "12", "--reason", "막혔다"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("history.jsonl에 남기지 못했다"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        w.state().queue[0].state,
        ItemState::NeedsHuman(HumanReason::GaveUp)
    );
}

#[test]
fn giveup_without_a_number_on_another_pc_finds_the_pr_here_and_sends_the_hub_item_to_a_human() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    forward_kit::seed(b, vec![item(12, ItemState::Queued(None))]);
    a.sb.fake("gh")
        .respond("pr view*", Reply::ok(r#"{"number": 12}"#));
    let a_before = forward_kit::state_file(a);

    let out = a.chute(
        forward_kit::BIN,
        &["giveup", "--reason", "충돌을 풀 수 없다"],
    );

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        forward_kit::out(&out),
        "사람 차례로 넘겼다: #12 (포기: 충돌을 풀 수 없다)\n"
    );
    assert_eq!(
        forward_kit::state(b).queue[0].state,
        ItemState::NeedsHuman(HumanReason::GaveUp)
    );
    let history = fs::read_to_string(forward_kit::store(b).dir().join("history.jsonl")).unwrap();
    assert!(history.contains("충돌을 풀 수 없다"), "{history}");
    assert_eq!(forward_kit::state_file(a), a_before);
}

#[test]
fn giveup_with_a_number_on_another_pc_does_not_ask_github() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    forward_kit::seed(b, vec![item(12, ItemState::Queued(None))]);

    let out = a.chute(forward_kit::BIN, &["giveup", "12", "--reason", "그만둔다"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(a.sb.fake("gh").calls().is_empty());
    assert_eq!(
        forward_kit::state(b).queue[0].state,
        ItemState::NeedsHuman(HumanReason::GaveUp)
    );
}

#[test]
fn giveup_on_another_pc_checks_the_reason_here_and_names_a_missing_pr() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    forward_kit::seed(b, vec![item(12, ItemState::Queued(None))]);

    let blank = a.chute(forward_kit::BIN, &["giveup", "12", "--reason", " "]);
    assert_eq!(blank.status.code(), Some(1));
    assert_eq!(
        forward_kit::state(b).queue[0].state,
        ItemState::Queued(None)
    );

    a.sb.fake("gh").respond(
        "pr view*",
        Reply::ok("").code(1).stderr("no pull requests found"),
    );
    let none = a.chute(forward_kit::BIN, &["giveup", "--reason", "그만둔다"]);
    assert_eq!(none.status.code(), Some(2), "{}", stderr(&none));
    assert!(
        stderr(&none).contains("PR을 찾지 못했다"),
        "{}",
        stderr(&none)
    );
    assert_eq!(
        forward_kit::state(b).queue[0].state,
        ItemState::Queued(None)
    );
}

#[test]
fn giveup_on_another_pc_says_so_when_the_hub_is_down_or_this_is_not_the_hub() {
    let net = forward_kit::net();
    forward_kit::assert_unreachable_hub(&net, &["giveup", "12", "--reason", "x"]);
    forward_kit::assert_not_hub_answer(&net, &["giveup", "12", "--reason", "x"]);
}

#[test]
fn giveup_on_another_pc_forwards_a_reason_that_starts_with_a_hyphen_as_a_value() {
    let net = forward_kit::net();
    let (a, b) = (net.node("a"), net.node("b"));
    forward_kit::seed(b, vec![item(12, ItemState::Queued(None))]);

    let out = a.chute(
        forward_kit::BIN,
        &["giveup", "12", "--reason=-1 재시도 남음"],
    );

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(forward_kit::out(&out).contains("(포기: -1 재시도 남음)"));
}
