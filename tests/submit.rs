mod queue_kit;
mod support;

use std::process::Output;

use chute::model::{
    Agent, BounceReason, EpochMs, EventKind, HumanReason, ItemState, MachineId, PrNumber,
    PreflightRecord, PreflightResult, Sha, Slug, State, TerminalId,
};
use chute::store::Home;
use queue_kit::{World, item, stderr};
use serde_json::{Value, json};
use support::{Net, Node, Reply};

const HEAD: &str = "12e5678";
const OLD_HEAD: &str = "0d0d0d0";
const PR_FIELDS: &str =
    "number,title,headRefName,headRefOid,baseRefName,state,isDraft,files,changedFiles";

fn sha(s: &str) -> Sha {
    Sha::try_from(s.to_owned()).unwrap()
}

fn pr(number: u64, head: &str) -> Value {
    json!({
        "number": number,
        "title": format!("PR {number}"),
        "headRefName": format!("feat/{number}"),
        "headRefOid": head,
        "baseRefName": "main",
        "state": "OPEN",
        "isDraft": false,
        "files": [{"path": "src/a.rs", "additions": 1, "deletions": 0}],
        "changedFiles": 1,
    })
}

fn with(mut pr: Value, key: &str, value: Value) -> Value {
    pr[key] = value;
    pr
}

fn terminal(address: &str, name: &str, created_ms: u64, workdir: &str) -> Reply {
    Reply::ok(
        json!({
            "address": address,
            "name": name,
            "state": "live",
            "created_ms": created_ms,
            "cwd": "/w/launch",
            "foreground": {"argv": ["claude"], "cwd": workdir, "name": "claude", "pid": 1},
        })
        .to_string(),
    )
}

fn gh_pr(w: &World, number: Option<u64>, reply: Value) {
    let pattern = match number {
        Some(n) => format!("pr view {n} --json *"),
        None => "pr view --json *".to_owned(),
    };
    w.sb.fake("gh")
        .respond(&pattern, Reply::ok(reply.to_string()));
}

fn hc_get(w: &World, address: &str, reply: Reply) {
    w.sb.fake("hc")
        .respond(&format!("get --json {address}"), reply);
}

fn hc_knows_impl_a(w: &World, address: &str) {
    let mut reply: Value = serde_json::from_str(include_str!("fixtures/hc/get.json")).unwrap();
    reply["address"] = json!(address);
    hc_get(w, address, Reply::ok(reply.to_string()));
}

fn submit_as(w: &World, terminal: Option<&str>, args: &[&str]) -> Output {
    let argv = [&["submit"][..], args].concat();
    match terminal {
        Some(t) => w.chute_with_env(w.repo.path(), &argv, &[("HC_TERMINAL_ID", t)]),
        None => w.chute(w.repo.path(), &argv),
    }
}

fn submit(w: &World, args: &[&str]) -> Output {
    submit_as(w, None, args)
}

fn hc_lookups(w: &World) -> Vec<Vec<String>> {
    w.sb.fake("hc")
        .calls()
        .into_iter()
        .filter(|call| call.first().is_some_and(|verb| verb == "get"))
        .collect()
}

fn code(out: &Output) -> Option<i32> {
    out.status.code()
}

fn seed_at(w: &World, pr: u64, head: &str, state: ItemState) {
    let mut queued = item(pr, state);
    queued.head = sha(head);
    w.seed(vec![queued]);
}

#[test]
fn submit_json_reports_pr_head_state_and_place_and_status_shows_the_agent() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1rs");

    let stdout = w.ok(
        w.repo.path(),
        &["submit", "12", "--agent", "t1rs", "--json"],
    );

    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        report,
        json!({
            "pr": 12, "head": "12e5678", "state": "queued", "state_reason": null,
            "position": 1, "change": "added",
        })
    );
    let entry = w.entry(12).unwrap();
    assert_eq!(entry["head"], "12e5678");
    assert_eq!(entry["state"], "queued");
    assert_eq!(
        entry["agent"],
        json!({"terminal": "t1rs", "name": "impl-a", "alive": true,
               "machine_id": support::SANDBOX_MACHINE_ID, "host": "sandbox-pc", "machine": null})
    );
}

#[test]
fn trust_mode_queues_the_pr_and_records_its_head_and_agent() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_get(
        &w,
        "t1rs",
        terminal("t1rs", "impl-a", 1763188200000, "/w/impl-a"),
    );

    let out = submit_as(&w, Some("t1rs"), &["12"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let state = w.state();
    let [item] = state.queue.as_slice() else {
        panic!("{:?}", state.queue)
    };
    assert_eq!(item.pr, PrNumber(12));
    assert_eq!(item.state, ItemState::Queued(None));
    assert_eq!(item.head, sha(HEAD));
    assert_eq!(
        (item.title.as_str(), item.branch.as_str()),
        ("PR 12", "feat/12")
    );
    assert_eq!(
        item.agent,
        Some(Agent {
            terminal: TerminalId("t1rs".into()),
            created_ms: EpochMs(1763188200000),
            name: Some("impl-a".into()),
            workdir: "/w/impl-a".into(),
            alive: true,
            machine: Some(MachineId::try_from(support::SANDBOX_MACHINE_ID.to_owned()).unwrap()),
        })
    );
    assert_eq!((item.attempts, item.touches_config), (0, false));
    assert!(item.last_failure.is_none());
    assert_eq!(
        w.sb.fake("gh").calls(),
        [vec!["pr", "view", "12", "--json", PR_FIELDS]]
    );
}

#[test]
fn a_hash_in_front_of_the_number_is_accepted() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    assert_eq!(code(&submit(&w, &["#12"])), Some(0));
    assert_eq!(w.state().queue[0].pr, PrNumber(12));
}

#[test]
fn the_terminal_launch_folder_is_used_when_it_has_no_current_folder() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    let mut reply: Value = serde_json::from_str(include_str!("fixtures/hc/get.json")).unwrap();
    reply.as_object_mut().unwrap().remove("foreground");
    hc_get(&w, "t1rs", Reply::ok(reply.to_string()));

    assert_eq!(code(&submit_as(&w, Some("t1rs"), &["12"])), Some(0));

    let agent = w.state().queue[0].agent.clone().unwrap();
    assert_eq!(agent.workdir, std::path::PathBuf::from("/w/launch"));
}

#[test]
fn the_recorded_agent_follows_the_shape_of_a_real_hc_get() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    w.sb.fake("hc")
        .respond("get --json t1rs", Reply::file("tests/fixtures/hc/get.json"));

    assert_eq!(code(&submit_as(&w, Some("t1rs"), &["12"])), Some(0));

    let agent = w.state().queue[0].agent.clone().unwrap();
    assert_eq!(agent.terminal, TerminalId("t1rs".into()));
    assert_eq!(agent.workdir, std::path::PathBuf::from("/w/impl-a"));
}

#[test]
fn submit_reads_the_agent_from_hc_terminal_id_and_the_flag_wins() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1env");

    let out = submit_as(&w, Some("t1env"), &["12"]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(w.entry(12).unwrap()["agent"]["terminal"], "t1env");
    assert_eq!(hc_lookups(&w), [vec!["get", "--json", "t1env"]]);

    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1flag");
    hc_knows_impl_a(&w, "t1env");
    let out = submit_as(&w, Some("t1env"), &["12", "--agent", "t1flag"]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(w.entry(12).unwrap()["agent"]["terminal"], "t1flag");
    assert_eq!(w.entry(12).unwrap()["submitter"]["terminal"], "t1env");
    assert_eq!(
        hc_lookups(&w),
        [
            vec!["get", "--json", "t1flag"],
            vec!["get", "--json", "t1env"]
        ]
    );
}

#[test]
fn submit_outside_hc_registers_without_an_agent_and_never_calls_hc() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("에이전트 없이 등록한다"),
        "{}",
        stderr(&out)
    );
    assert_eq!(w.entry(12).unwrap()["agent"], Value::Null);
    assert!(hc_lookups(&w).is_empty());
}

#[test]
fn submit_warns_when_hc_cannot_confirm_the_terminal_and_still_registers() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_get(
        &w,
        "t1gone",
        Reply::ok("")
            .code(1)
            .stderr(r#"{"error":{"kind":"not_found","message":"그런 터미널이 없다"}}"#),
    );

    let out = submit(&w, &["12", "--agent", "t1gone"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let warning = stderr(&out);
    assert!(
        warning.contains("경고: 에이전트를 확인하지 못했다(t1gone)"),
        "{warning}"
    );
    assert!(warning.contains("에이전트 없이 등록한다"), "{warning}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("등록했다: #12"), "{stdout}");
    assert_eq!(w.entry(12).unwrap()["agent"], Value::Null);
}

#[test]
fn a_terminal_that_is_not_live_or_unreadable_is_left_out_and_the_pr_still_registers() {
    let finished = json!({
        "address": "t1rs", "name": "impl-a", "state": "finished", "created_ms": 1, "cwd": "/w",
    });
    for (name, reply) in [
        ("터미널이 끝났다", Reply::ok(finished.to_string())),
        ("출력을 읽을 수 없다", Reply::ok("not json")),
        ("오류 본문", Reply::ok(r#"{"error":{"kind":"x"}}"#)),
    ] {
        let w = World::new();
        gh_pr(&w, Some(12), pr(12, HEAD));
        hc_get(&w, "t1rs", reply);

        let out = submit_as(&w, Some("t1rs"), &["12"]);

        assert_eq!(code(&out), Some(0), "{name}: {}", stderr(&out));
        let item = &w.state().queue[0];
        assert_eq!(item.agent, None, "{name}");
        assert_eq!(item.state, ItemState::Queued(None), "{name}");
    }
}

#[test]
fn human_approval_mode_waits_for_approval() {
    let w = World::human();
    gh_pr(&w, Some(12), pr(12, HEAD));
    assert_eq!(code(&submit(&w, &["12"])), Some(0));
    assert_eq!(w.state().queue[0].state, ItemState::AwaitingApproval);
}

#[test]
fn a_pr_that_edits_chute_toml_waits_for_approval_even_in_trust_mode() {
    let w = World::new();
    let touching = with(
        pr(12, HEAD),
        "files",
        json!([{"path": "src/a.rs"}, {"path": "chute.toml"}]),
    );
    gh_pr(&w, Some(12), with(touching, "changedFiles", json!(2)));

    let stdout = w.ok(w.repo.path(), &["submit", "12"]);

    assert!(stdout.contains("등록했다: #12"), "{stdout}");
    assert!(stdout.contains("승인 대기"), "{stdout}");
    assert!(stdout.contains("`chute approve 12`"), "{stdout}");
    let item = &w.state().queue[0];
    assert_eq!(item.state, ItemState::AwaitingApproval);
    assert!(item.touches_config);
    assert_eq!(w.entry(12).unwrap()["touches_config"], true);

    gh_pr(
        &w,
        Some(13),
        with(pr(13, HEAD), "files", json!([{"path": "docs/chute.toml"}])),
    );
    assert_eq!(code(&submit(&w, &["13"])), Some(0));
    assert_eq!(w.state().queue[1].state, ItemState::Queued(None));
}

#[test]
fn a_file_list_cut_short_by_gh_waits_for_approval() {
    let w = World::new();
    gh_pr(&w, Some(12), with(pr(12, HEAD), "changedFiles", json!(101)));

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("101개 가운데 1개만"),
        "{}",
        stderr(&out)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("승인 대기"), "{stdout}");
    assert!(!stdout.contains("chute.toml을 고치는 PR이라"), "{stdout}");
    let item = &w.state().queue[0];
    assert_eq!(item.state, ItemState::AwaitingApproval);
    assert!(item.touches_config);
}

#[test]
fn a_complete_file_list_without_chute_toml_goes_straight_to_the_queue() {
    let w = World::new();
    let two = with(pr(12, HEAD), "files", json!([{"path": "a"}, {"path": "b"}]));
    gh_pr(&w, Some(12), with(two, "changedFiles", json!(2)));
    let out = submit(&w, &["12"]);
    assert!(
        stderr(&out).is_empty() || !stderr(&out).contains("읽었다"),
        "{}",
        stderr(&out)
    );
    assert_eq!(w.state().queue[0].state, ItemState::Queued(None));
}

#[test]
fn prs_the_queue_cannot_take_are_refused_before_looking_for_an_agent() {
    let cases = [
        (
            "닫힌 PR",
            with(pr(12, HEAD), "state", json!("CLOSED")),
            "닫힌 PR",
        ),
        (
            "머지된 PR",
            with(pr(12, HEAD), "state", json!("MERGED")),
            "이미 머지된 PR",
        ),
        (
            "draft",
            with(pr(12, HEAD), "isDraft", json!(true)),
            "draft PR이다",
        ),
        (
            "다른 브랜치",
            with(pr(12, HEAD), "baseRefName", json!("release")),
            "`release` 브랜치를 향한 PR",
        ),
    ];
    for (name, reply, reason) in cases {
        let w = World::new();
        gh_pr(&w, Some(12), reply);

        let out = submit_as(&w, Some("t1rs"), &["12", "--json"]);

        assert_eq!(code(&out), Some(1), "{name}");
        assert!(out.stdout.is_empty(), "{name}");
        assert!(stderr(&out).contains(reason), "{name}: {}", stderr(&out));
        assert_eq!(w.state(), State::default(), "{name}");
        assert!(w.sb.fake("hc").calls().is_empty(), "{name}");
    }
}

#[test]
fn a_repository_whose_default_branch_has_no_chute_toml_is_refused() {
    let w = World::with_config(None);
    gh_pr(&w, Some(12), pr(12, HEAD));

    let out = submit(&w, &["12"]);
    assert_eq!(code(&out), Some(1));
    assert!(
        stderr(&out).contains("chute.toml을 origin/main에서 읽지 못했다"),
        "{}",
        stderr(&out)
    );

    w.repo
        .commit_file("chute.toml", &chute::config::draft("true"), "local only");
    assert_eq!(code(&submit(&w, &["12"])), Some(1));

    assert_eq!(w.state(), State::default());
    assert!(w.sb.fake("gh").calls().is_empty());
}

#[test]
fn a_local_branch_or_tag_named_like_the_remote_default_branch_cannot_replace_its_config() {
    for shadow in ["branch", "tag"] {
        let w = World::human();
        w.repo.git(&["checkout", "--quiet", "-b", "scratch"]);
        let commit = w.repo.commit_file(
            "chute.toml",
            &chute::config::draft("true"),
            "local config that trusts agents",
        );
        match shadow {
            "branch" => w.repo.git(&["branch", "origin/main", &commit]),
            _ => w.repo.git(&["tag", "origin/main", &commit]),
        };
        w.repo.git(&["checkout", "--quiet", "main"]);
        gh_pr(&w, Some(12), pr(12, HEAD));

        assert_eq!(code(&submit(&w, &["12"])), Some(0), "{shadow}");

        assert_eq!(
            w.state().queue[0].state,
            ItemState::AwaitingApproval,
            "{shadow}"
        );
    }
}

#[test]
fn a_local_branch_or_tag_named_like_the_full_remote_ref_cannot_stand_in_for_a_missing_one() {
    for shadow in ["branch", "tag"] {
        let w = World::human();
        w.repo.git(&["checkout", "--quiet", "-b", "scratch"]);
        let commit = w.repo.commit_file(
            "chute.toml",
            &chute::config::draft("true"),
            "local config that trusts agents",
        );
        w.repo
            .git(&["update-ref", "-d", "refs/remotes/origin/main"]);
        match shadow {
            "branch" => w.repo.git(&["branch", "refs/remotes/origin/main", &commit]),
            _ => w.repo.git(&["tag", "refs/remotes/origin/main", &commit]),
        };
        w.repo.git(&["checkout", "--quiet", "main"]);
        gh_pr(&w, Some(12), pr(12, HEAD));

        let out = submit(&w, &["12"]);

        assert_eq!(code(&out), Some(1), "{shadow}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("chute.toml을 origin/main에서 읽지 못했다"),
            "{shadow}"
        );
        assert_eq!(w.state(), State::default(), "{shadow}");
    }
}

#[test]
fn a_local_branch_named_like_the_remote_default_branch_does_not_supply_a_missing_config() {
    let w = World::with_config(None);
    w.repo.git(&["checkout", "--quiet", "-b", "origin/main"]);
    w.repo
        .commit_file("chute.toml", &chute::config::draft("true"), "local config");
    w.repo.git(&["checkout", "--quiet", "main"]);
    gh_pr(&w, Some(12), pr(12, HEAD));

    assert_eq!(code(&submit(&w, &["12"])), Some(1));
    assert_eq!(w.state(), State::default());
}

#[test]
fn without_a_number_the_pr_of_the_current_branch_is_used() {
    let w = World::new();
    gh_pr(&w, None, pr(31, HEAD));

    assert_eq!(code(&submit(&w, &[])), Some(0));

    assert_eq!(w.state().queue[0].pr, PrNumber(31));
    assert_eq!(
        w.sb.fake("gh").calls(),
        [vec!["pr", "view", "--json", PR_FIELDS]]
    );
}

#[test]
fn without_a_number_and_with_the_repo_flag_gh_runs_in_that_repository() {
    let w = World::new();
    let repo = w.repo.path().canonicalize().unwrap();
    let reply = pr(12, HEAD).to_string();
    w.sb.script(
        "gh",
        &format!(
            "[ \"$(pwd -P)\" = '{}' ] || {{ echo \"gh이 엉뚱한 폴더에서 돌았다: $(pwd -P)\" >&2; exit 1; }}\ncat <<'JSON'\n{reply}\nJSON\n",
            repo.display()
        ),
    );

    let out = w.chute(w.sb.root(), &["--repo", &repo.to_string_lossy(), "submit"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(w.state().queue[0].pr, PrNumber(12));
}

#[test]
fn submit_records_an_absolute_work_folder_even_for_a_relative_repo_flag() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_get(
        &w,
        "t1rs",
        Reply::ok(r#"{"address":"t1rs","name":"a","state":"live","created_ms":1}"#),
    );
    let parent = w.repo.path().parent().unwrap();
    let relative = w
        .repo
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    w.ok(
        parent,
        &["--repo", &relative, "submit", "12", "--agent", "t1rs"],
    );

    let state = w.state();
    let workdir = &state.queue[0].agent.as_ref().unwrap().workdir;
    assert_eq!(workdir, w.repo.path());
}

#[test]
fn submitting_the_same_head_again_leaves_the_state_as_it_was() {
    for before in [ItemState::Queued(None), ItemState::AwaitingApproval] {
        let w = World::human();
        seed_at(&w, 12, HEAD, before);
        gh_pr(&w, Some(12), pr(12, HEAD));
        hc_get(&w, "t1rs", terminal("t1rs", "impl-a", 1, "/w/impl-a"));
        let state_before = w.state();

        let out = submit_as(&w, Some("t1rs"), &["12"]);

        assert_eq!(code(&out), Some(0), "{before:?}: {}", stderr(&out));
        assert!(String::from_utf8_lossy(&out.stdout).contains("바뀐 것이 없다"));
        let mut after = w.state();
        let submitter = after.queue[0].submitter.take();
        assert_eq!(
            submitter.map(|a| a.terminal.0),
            Some("t1rs".to_owned()),
            "{before:?}"
        );
        assert_eq!(after, state_before, "{before:?}");
    }
}

#[test]
fn a_new_head_needs_approval_again_in_human_mode_and_not_in_trust_mode() {
    for (w, expected) in [
        (World::human(), ItemState::AwaitingApproval),
        (World::new(), ItemState::Queued(None)),
    ] {
        seed_at(&w, 12, OLD_HEAD, ItemState::Queued(None));
        gh_pr(&w, Some(12), pr(12, HEAD));
        assert_eq!(code(&submit(&w, &["12"])), Some(0));
        let item = &w.state().queue[0];
        assert_eq!((item.state, &item.head), (expected, &sha(HEAD)));
    }
}

#[test]
fn an_approved_pr_resubmitted_with_a_new_head_waits_for_approval_again() {
    let w = World::human();
    gh_pr(&w, Some(12), pr(12, OLD_HEAD));
    gh_pr(&w, None, pr(12, HEAD));

    assert_eq!(code(&submit(&w, &["12"])), Some(0));
    assert_eq!(w.state().queue[0].state, ItemState::AwaitingApproval);
    w.ok(w.repo.path(), &["approve", "12"]);
    assert_eq!(w.state().queue[0].state, ItemState::Queued(None));

    let out = submit(&w, &[]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let item = &w.state().queue[0];
    assert_eq!(
        (item.state, &item.head),
        (ItemState::AwaitingApproval, &sha(HEAD))
    );
}

#[test]
fn a_bounced_pr_goes_back_to_waiting_with_its_count_and_the_new_agent() {
    let w = World::new();
    let mut bounced = item(12, ItemState::Bounced(BounceReason::Verify));
    bounced.head = sha(OLD_HEAD);
    bounced.attempts = 2;
    w.seed(vec![bounced]);
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_get(&w, "t1new", terminal("t1new", "respawned", 9, "/w/new"));

    let out = submit_as(&w, Some("t1new"), &["12"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("다시 큐에 올렸다: #12"));
    let item = &w.state().queue[0];
    assert_eq!((item.state, item.attempts), (ItemState::Queued(None), 2));
    assert_eq!(item.head, sha(HEAD));
    assert_eq!(
        item.agent.as_ref().unwrap().terminal,
        TerminalId("t1new".into())
    );
}

#[test]
fn a_bounced_pr_in_human_mode_says_it_waits_for_approval_and_not_that_it_is_queued() {
    let w = World::human();
    let mut bounced = item(12, ItemState::Bounced(BounceReason::Verify));
    bounced.head = sha(OLD_HEAD);
    w.seed(vec![bounced]);
    gh_pr(&w, Some(12), pr(12, HEAD));

    let stdout = w.ok(w.repo.path(), &["submit", "12"]);

    assert!(
        stdout.starts_with("HEAD가 바뀌어 다시 승인을 기다린다: #12"),
        "{stdout}"
    );
    assert_eq!(w.state().queue[0].state, ItemState::AwaitingApproval);
}

#[test]
fn a_pr_in_a_batch_or_on_a_humans_turn_is_refused_and_left_alone() {
    for from in [
        ItemState::InBatch,
        ItemState::NeedsHuman(HumanReason::GaveUp),
    ] {
        let w = World::new();
        seed_at(&w, 12, OLD_HEAD, from);
        gh_pr(&w, Some(12), pr(12, HEAD));
        let before = w.state();

        let out = submit_as(&w, Some("t1rs"), &["12"]);

        assert_eq!(code(&out), Some(1), "{from:?}");
        assert_eq!(w.state(), before, "{from:?}");
    }
}

fn preflight_at(w: &World, head: &str) {
    w.store()
        .update(|s| {
            s.preflights.insert(
                "feat/12".into(),
                PreflightRecord {
                    head: sha(head),
                    result: PreflightResult::Pass,
                    report: None,
                    at_ms: EpochMs(5),
                    base: None,
                    with: Vec::new(),
                    env: None,
                },
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_preflight_result_for_the_head_shows_with_the_item() {
    let w = World::new();
    preflight_at(&w, HEAD);
    gh_pr(&w, Some(12), pr(12, HEAD));

    assert_eq!(code(&submit(&w, &["12"])), Some(0));

    assert_eq!(
        w.entry(12).unwrap()["preflight"],
        json!({"result": "pass", "head": HEAD})
    );
}

#[test]
fn a_preflight_result_for_an_older_head_is_not_shown_with_the_new_head() {
    let w = World::human();
    preflight_at(&w, OLD_HEAD);
    gh_pr(&w, Some(12), pr(12, HEAD));

    assert_eq!(code(&submit(&w, &["12"])), Some(0));

    let entry = w.entry(12).unwrap();
    assert_eq!(entry["state"], "awaiting_approval");
    assert_eq!(entry["preflight"], Value::Null);
}

#[test]
fn a_refused_pr_writes_nothing_and_a_repository_never_initialised_cannot_run() {
    let w = World::new();
    gh_pr(&w, Some(12), with(pr(12, HEAD), "isDraft", json!(true)));
    let out = submit(&w, &["12"]);
    assert_eq!(code(&out), Some(1));
    assert!(w.entry(12).is_none());

    let sb = support::Sandbox::new();
    let repo = support::TestRepo::new(&sb, "acme/widget");
    let out = sb
        .command(env!("CARGO_BIN_EXE_chute"))
        .current_dir(repo.path())
        .args(["submit", "12"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("chute init"), "{}", stderr(&out));
}

#[test]
fn a_corrupt_state_file_stops_submit_with_exit_code_2_and_is_left_alone() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    let garbage = w.corrupt_state();

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(2));
    assert_eq!(std::fs::read_to_string(w.state_file()).unwrap(), garbage);
}

#[test]
fn submitting_many_prs_at_once_loses_none() {
    let w = World::new();
    let numbers: Vec<u64> = (1..=8).collect();
    for n in &numbers {
        gh_pr(&w, Some(*n), pr(*n, HEAD));
    }
    let commands: Vec<Vec<String>> = numbers
        .iter()
        .map(|n| vec!["submit".to_owned(), n.to_string()])
        .collect();

    let outs = w.run_parallel(&commands);

    for out in &outs {
        assert_eq!(code(out), Some(0), "{}", stderr(out));
    }
    let mut queued: Vec<u64> = w.state().queue.iter().map(|i| i.pr.0).collect();
    queued.sort();
    assert_eq!(queued, numbers);
}

#[test]
fn submit_twice_with_the_same_head_prints_that_nothing_changed() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    w.ok(w.repo.path(), &["submit", "12"]);
    let before = w.status(w.repo.path())["queue"].clone();

    let stdout = w.ok(w.repo.path(), &["submit", "12"]);

    assert!(stdout.contains("바뀐 것이 없다"), "{stdout}");
    assert_eq!(w.status(w.repo.path())["queue"], before);
}

#[test]
fn an_unpushed_local_default_branch_cannot_set_the_approval_mode() {
    let w = World::human();
    w.repo.commit_file(
        "chute.toml",
        &chute::config::draft("true"),
        "local only, trusts agents",
    );
    gh_pr(&w, Some(12), pr(12, HEAD));

    assert_eq!(code(&submit(&w, &["12"])), Some(0));

    assert_eq!(w.state().queue[0].state, ItemState::AwaitingApproval);
}

#[test]
fn a_failing_gh_cannot_run_submit_and_writes_nothing() {
    let w = World::new();

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("PR을 읽지 못했다"),
        "{}",
        stderr(&out)
    );
    assert_eq!(w.state(), State::default());
}

#[test]
fn each_new_pr_is_reported_one_place_further_back() {
    let w = World::new();
    for n in 1..=3 {
        gh_pr(&w, Some(n), pr(n, HEAD));
    }
    for (n, place) in [(1, 1), (2, 2), (3, 3)] {
        let stdout = w.ok(w.repo.path(), &["submit", &n.to_string(), "--json"]);
        let report: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(report["position"], place, "#{n}");
    }
}

#[test]
fn an_agent_named_instead_of_addressed_is_recorded_by_the_address_hc_reports() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    w.sb.fake("hc").respond(
        "get --json impl-a",
        Reply::file("tests/fixtures/hc/get.json"),
    );

    assert_eq!(code(&submit(&w, &["12", "--agent", "impl-a"])), Some(0));

    assert_eq!(w.entry(12).unwrap()["agent"]["terminal"], "t1rs");
}

#[test]
fn empty_folder_name_and_address_from_hc_are_not_recorded_as_values() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_get(
        &w,
        "t1rs",
        Reply::ok(
            r#"{"address":"t1rs","name":"","state":"live","created_ms":1,"cwd":"","foreground":{"cwd":""}}"#,
        ),
    );
    for (terminal_id, address) in [("t1blank", ""), ("t1space", "  ")] {
        hc_get(
            &w,
            terminal_id,
            Reply::ok(format!(
                r#"{{"address":"{address}","name":"x","state":"live","created_ms":1}}"#
            )),
        );
    }

    let out = submit_as(&w, Some("t1rs"), &["12"]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let state = w.state();
    let agent = state.queue[0].agent.as_ref().unwrap();
    assert_eq!(agent.name, None);
    assert_eq!(agent.workdir, w.repo.path());

    gh_pr(&w, Some(13), pr(13, HEAD));
    let out = submit_as(&w, Some("t1blank"), &["13"]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(w.state().queue[1].agent, None);
    assert!(
        stderr(&out).contains("응답에 주소가 없다"),
        "{}",
        stderr(&out)
    );

    gh_pr(&w, Some(14), pr(14, HEAD));
    let out = submit_as(&w, Some("t1space"), &["14"]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(w.state().queue[2].agent, None);
    assert!(
        stderr(&out).contains("응답에 주소가 없다"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_empty_agent_flag_or_terminal_id_is_not_an_address() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1env");

    let out = submit_as(&w, Some("t1env"), &["12", "--agent", ""]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(w.entry(12).unwrap()["agent"]["terminal"], "t1env");

    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    let out = submit_as(&w, Some(""), &["12"]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(w.entry(12).unwrap()["agent"], Value::Null);
    assert!(hc_lookups(&w).is_empty());
}

#[test]
fn status_shows_the_approval_mode_submit_uses_when_a_local_branch_has_the_remote_ref_name() {
    let w = World::human();
    w.repo.git(&["checkout", "--quiet", "-b", "scratch"]);
    let commit = w.repo.commit_file(
        "chute.toml",
        &chute::config::draft("true"),
        "local config that trusts agents",
    );
    w.repo.git(&["branch", "origin/main", &commit]);
    w.repo.git(&["checkout", "--quiet", "main"]);
    gh_pr(&w, Some(12), pr(12, HEAD));

    assert_eq!(code(&submit(&w, &["12"])), Some(0));

    let status = w.status(w.repo.path());
    assert_eq!(status["approval"], "human");
    assert_eq!(status["queue"][0]["state"], "awaiting_approval");
    assert_eq!(status["warnings"], json!([]));
}

#[test]
fn a_config_that_cannot_be_read_names_the_remote_branch_it_came_from() {
    let w = World::with_config(Some("budget = \"many\"\n".to_owned()));
    gh_pr(&w, Some(12), pr(12, HEAD));

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("origin/main의 설정을 읽지 못했다"),
        "{}",
        stderr(&out)
    );
    assert_eq!(w.state(), State::default());
}

#[test]
fn a_git_replace_ref_cannot_swap_the_config_of_the_remote_default_branch() {
    let w = World::human();
    let remote_commit = w
        .repo
        .git(&["rev-parse", "refs/remotes/origin/main"])
        .trim()
        .to_owned();
    w.repo.git(&["checkout", "--quiet", "-b", "scratch"]);
    let forged = w.repo.commit_file(
        "chute.toml",
        &chute::config::draft("true"),
        "local config that trusts agents",
    );
    w.repo.git(&["replace", &remote_commit, &forged]);
    w.repo.git(&["checkout", "--quiet", "main"]);
    gh_pr(&w, Some(12), pr(12, HEAD));

    assert_eq!(code(&submit(&w, &["12"])), Some(0));

    assert_eq!(w.state().queue[0].state, ItemState::AwaitingApproval);
    assert_eq!(w.status(w.repo.path())["approval"], "human");
}

fn hc_run_calls(w: &World) -> Vec<Vec<String>> {
    w.sb.fake("hc")
        .calls()
        .into_iter()
        .filter(|call| call.first().is_some_and(|verb| verb == "run"))
        .collect()
}

#[test]
fn a_successful_submit_starts_the_daemon_with_hc_run_in_the_background_without_a_parent() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    w.sb.fake("hc")
        .respond("run *", Reply::ok(r#"{"address":"t1dd"}"#));

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let runs = hc_run_calls(&w);
    assert_eq!(runs.len(), 1, "{runs:?}");
    let run = &runs[0];
    for flag in ["--bg", "--no-parent"] {
        assert!(run.iter().any(|a| a == flag), "{flag}: {run:?}");
    }
    assert_eq!(&run[run.len() - 3..], ["--", "chute", "daemon"]);
}

#[test]
fn a_second_submit_right_after_does_not_start_another_daemon() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    gh_pr(&w, Some(13), pr(13, "13e5678"));
    w.sb.fake("hc")
        .respond("run *", Reply::ok(r#"{"address":"t1dd"}"#));

    assert_eq!(code(&submit(&w, &["12"])), Some(0));
    assert_eq!(code(&submit(&w, &["13"])), Some(0));

    assert_eq!(hc_run_calls(&w).len(), 1);
}

#[test]
fn a_daemon_that_cannot_be_started_only_warns_and_the_submit_succeeds() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    w.sb.fake("hc")
        .respond("run *", Reply::ok("").code(5).stderr("hc가 없다"));

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("데몬을 띄우지 못했다"),
        "{}",
        stderr(&out)
    );
    assert!(w.entry(12).is_some());
}

#[test]
fn the_hub_can_pin_the_head_it_submits_and_takes_a_short_sha() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));

    let wrong = submit(&w, &["12", "--head", OLD_HEAD]);
    assert_eq!(code(&wrong), Some(1));
    let message = stderr(&wrong);
    assert!(
        message.contains("push하지 않은 커밋") && message.contains(HEAD),
        "{message}"
    );
    assert!(w.state().queue.is_empty());

    let too_short = submit(&w, &["12", "--head", "12e567"]);
    assert_eq!(code(&too_short), Some(2), "{}", stderr(&too_short));
    assert!(w.state().queue.is_empty());

    let full = format!("{HEAD}{}", "0".repeat(33));
    assert_eq!(code(&submit(&w, &["12", "--head", &full])), Some(1));
    assert!(w.state().queue.is_empty());

    assert_eq!(code(&submit(&w, &["12", "--head", HEAD])), Some(0));
    assert_eq!(w.state().queue[0].head, sha(HEAD));
}

#[test]
fn a_short_head_matches_the_full_sha_github_returns() {
    let w = World::new();
    let full = format!("{HEAD}{}", "a".repeat(33));
    gh_pr(&w, Some(12), pr(12, &full));

    assert_eq!(code(&submit(&w, &["12", "--head", HEAD])), Some(0));
    assert_eq!(w.state().queue[0].head, sha(&full));
}

#[test]
fn a_hub_never_asks_hc_again_when_the_other_pc_found_no_agent() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1rs");

    let out = w.chute_with_env(
        w.repo.path(),
        &["submit", "12", "--relayed-agent", "null"],
        &[("HC_TERMINAL_ID", "t1rs")],
    );

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(w.state().queue[0].agent, None);
    assert!(hc_lookups(&w).is_empty());
}

#[test]
fn no_agent_means_no_machine_id_lookup_and_no_machine_warning() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    w.sb.fake("hc").clear_rules().respond(
        "server status --json",
        Reply::ok("").code(1).stderr("core down"),
    );

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(!stderr(&out).contains("머신 id"), "{}", stderr(&out));
}

#[test]
fn a_hub_that_cannot_name_this_pc_still_submits_and_leaves_the_machine_empty() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    w.sb.fake("hc").clear_rules().respond(
        "server status --json",
        Reply::ok("").code(1).stderr("core down"),
    );
    hc_knows_impl_a(&w, "t1rs");

    let out = submit(&w, &["12", "--agent", "t1rs"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("머신 id를 읽지 못했다"),
        "{}",
        stderr(&out)
    );
    let agent = w.state().queue[0].agent.clone().unwrap();
    assert_eq!((agent.terminal.0.as_str(), agent.machine), ("t1rs", None));
}

const SLUG: &str = "acme/widget";
const BIN: &str = env!("CARGO_BIN_EXE_chute");
const AGENT_NAME: &str = "구현 에이전트 a";
const AGENT_DIR: &str = "/w/내 작업 폴더/impl a";

struct Pair {
    net: Net,
    local_head: String,
}

impl Pair {
    fn hub(&self) -> &Node {
        self.net.node("hub")
    }

    fn pc(&self) -> &Node {
        self.net.node("pc")
    }
}

fn state_of(node: &Node) -> State {
    let slug = Slug::try_from(SLUG.to_owned()).unwrap();
    Home::at(node.sb.chute_home()).repo(&slug).load().unwrap()
}

fn state_bytes(node: &Node) -> Option<Vec<u8>> {
    let dir = node.sb.chute_home().join("repos/acme__widget");
    std::fs::read(dir.join("state.json")).ok()
}

fn gh_view(node: &Node, number: u64, reply: Value) {
    node.sb.fake("gh").respond(
        &format!("pr view {number} --json *"),
        Reply::ok(reply.to_string()),
    );
}

fn pair() -> Pair {
    pair_with_config(&chute::config::draft("true"))
}

fn pair_with_config(config: &str) -> Pair {
    let net = Net::new(SLUG, &["hub", "pc"], BIN);
    let (hub, pc) = (net.node("hub"), net.node("pc"));
    hub.repo
        .commit_file(chute::config::FILE_NAME, config, "chore: chute 설정");
    hub.repo.git(&["push", "--quiet", "origin", "main"]);
    pc.repo.git(&["pull", "--quiet", "origin", "main"]);
    for node in &net.nodes {
        let out = node.chute(BIN, &["init"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    net.connect_all();
    let hub_id = MachineId::try_from(hub.id.clone()).unwrap();
    let slug = Slug::try_from(SLUG.to_owned()).unwrap();
    Home::at(pc.sb.chute_home())
        .repo(&slug)
        .set_hub(hub_id)
        .unwrap();
    let local_head =
        pc.repo
            .commit_file("src/a.rs", "fn a() {}\n", "feat: 아직 push하지 않은 커밋");
    pc.sb.fake("hc").respond(
        "get --json t1rs",
        terminal("t1rs", AGENT_NAME, 1763188200000, AGENT_DIR),
    );
    Pair { net, local_head }
}

fn both_see_pr(pair: &Pair, head: &str) {
    for node in &pair.net.nodes {
        gh_view(node, 12, pr(12, head));
    }
}

fn pc_submit(pair: &Pair, args: &[&str]) -> Output {
    let argv = [&["submit", "12"][..], args].concat();
    pair.pc().chute(BIN, &argv)
}

fn hub_hc_lookups(pair: &Pair) -> usize {
    pair.hub()
        .sb
        .fake("hc")
        .calls()
        .iter()
        .filter(|call| call.first().is_some_and(|verb| verb == "get"))
        .count()
}

#[test]
fn a_submit_from_another_pc_lands_in_the_hubs_queue_with_the_agent_read_on_that_pc() {
    let p = pair();
    both_see_pr(&p, &p.local_head);
    let before = state_bytes(p.pc());

    let out = pc_submit(&p, &["--agent", "t1rs", "--json"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["pr"], 12);
    assert_eq!(report["head"], p.local_head);
    assert_eq!(report["change"], "added");

    let queue = state_of(p.hub()).queue;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].head, sha(&p.local_head));
    assert_eq!(
        queue[0].agent,
        Some(Agent {
            terminal: TerminalId("t1rs".into()),
            created_ms: EpochMs(1763188200000),
            name: Some(AGENT_NAME.into()),
            workdir: AGENT_DIR.into(),
            alive: true,
            machine: Some(MachineId::try_from(p.pc().id.clone()).unwrap()),
        })
    );
    assert_eq!(state_bytes(p.pc()), before);
    assert_eq!(hub_hc_lookups(&p), 0, "허브는 hc get으로 다시 묻지 않는다");
}

#[test]
fn a_submit_from_another_pc_without_an_agent_records_none_and_warns_on_that_pc() {
    let p = pair();
    both_see_pr(&p, &p.local_head);

    let out = pc_submit(&p, &[]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("에이전트를 알 수 없다"),
        "{}",
        stderr(&out)
    );
    assert_eq!(state_of(p.hub()).queue[0].agent, None);
    assert_eq!(hub_hc_lookups(&p), 0);
}

#[test]
fn the_hub_refuses_a_pc_whose_head_is_not_the_one_on_github() {
    let p = pair();
    both_see_pr(&p, OLD_HEAD);

    let out = pc_submit(&p, &["--agent", "t1rs"]);

    assert_eq!(code(&out), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("push하지 않은 커밋"),
        "{}",
        stderr(&out)
    );
    assert!(state_of(p.hub()).queue.is_empty());
    assert!(state_of(p.pc()).queue.is_empty());
}

#[test]
fn an_explicit_head_replaces_the_local_one_when_submitting_from_another_pc() {
    let p = pair();
    both_see_pr(&p, OLD_HEAD);

    let wrong = pc_submit(&p, &["--agent", "t1rs", "--head", &p.local_head[..9]]);
    assert_eq!(code(&wrong), Some(1), "{}", stderr(&wrong));
    assert!(state_of(p.hub()).queue.is_empty());

    let right = pc_submit(&p, &["--agent", "t1rs", "--head", OLD_HEAD]);
    assert_eq!(code(&right), Some(0), "{}", stderr(&right));
    assert_eq!(state_of(p.hub()).queue[0].head, sha(OLD_HEAD));
}

#[test]
fn the_hub_refuses_an_agent_on_a_pc_it_does_not_know_and_says_how_to_add_it() {
    let p = pair();
    both_see_pr(&p, &p.local_head);
    std::fs::write(p.hub().sb.root().join("net/peers"), "").unwrap();

    let out = pc_submit(&p, &["--agent", "t1rs"]);

    assert_eq!(code(&out), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("hc machine add"), "{}", stderr(&out));
    assert!(state_of(p.hub()).queue.is_empty());
}

#[test]
fn a_hub_that_is_down_ends_with_exit_code_2_and_no_queue_anywhere_has_the_item() {
    let p = pair();
    both_see_pr(&p, &p.local_head);
    p.net.set_state("pc", "hub", "unreachable");
    let before = state_bytes(p.pc());

    let out = pc_submit(&p, &["--agent", "t1rs"]);

    assert_eq!(code(&out), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("기록되지 않았다"), "{}", stderr(&out));
    assert!(state_of(p.hub()).queue.is_empty());
    assert_eq!(state_bytes(p.pc()), before);
    assert!(state_of(p.pc()).queue.is_empty());
}

#[test]
fn the_hidden_agent_argument_keeps_quotes_spaces_and_korean_when_given_straight_to_the_hub() {
    let p = pair();
    both_see_pr(&p, &p.local_head);
    let agent = Agent {
        terminal: TerminalId("t9".into()),
        created_ms: EpochMs(7),
        name: Some("한글 이름 \"따옴표\"".into()),
        workdir: "/w/공백 있는 폴더/x".into(),
        alive: true,
        machine: Some(MachineId::try_from(p.pc().id.clone()).unwrap()),
    };
    let text = serde_json::to_string(&agent).unwrap();

    let out = p.hub().chute(
        BIN,
        &[
            "submit",
            "12",
            "--head",
            &p.local_head,
            "--relayed-agent",
            &text,
        ],
    );

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(state_of(p.hub()).queue[0].agent, Some(agent));
}

#[test]
fn an_agent_addressed_through_another_machine_keeps_that_machine_not_the_hubs() {
    let p = pair();
    both_see_pr(&p, &p.local_head);
    p.hub().sb.fake("hc").respond(
        "get --json pc/t1rs",
        terminal("pc/t1rs", AGENT_NAME, 1763188200000, AGENT_DIR),
    );

    let out = p.hub().chute(
        BIN,
        &[
            "submit",
            "12",
            "--head",
            &p.local_head,
            "--agent",
            "pc/t1rs",
        ],
    );

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let agent = state_of(p.hub()).queue[0].agent.clone().unwrap();
    assert_eq!(
        agent.machine,
        Some(MachineId::try_from(p.pc().id.clone()).unwrap())
    );
}

fn hc_knows_orchestrator(w: &World) {
    hc_get(
        w,
        "t1orch",
        terminal("t1orch", "orch", 1763188300000, "/w/orch"),
    );
}

#[test]
fn submitter_and_agent_are_recorded_apart_when_the_orchestrator_submits_for_the_worker() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1rs");
    hc_knows_orchestrator(&w);

    let out = submit_as(
        &w,
        Some("t1orch"),
        &["12", "--head", HEAD, "--agent", "t1rs"],
    );

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let queued = &w.state().queue[0];
    let agent = queued.agent.clone().unwrap();
    let submitter = queued.submitter.clone().unwrap();
    assert_eq!(agent.terminal.0, "t1rs");
    assert_eq!(submitter.terminal.0, "t1orch");
    assert_eq!(submitter.created_ms, EpochMs(1763188300000));
    assert_eq!(submitter.name.as_deref(), Some("orch"));
    let shown = w.entry(12).unwrap();
    assert_eq!(shown["agent"]["terminal"], "t1rs");
    assert_eq!(shown["submitter"]["terminal"], "t1orch");
}

#[test]
fn a_worker_that_submits_for_itself_is_both_submitter_and_agent_with_one_hc_lookup() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1rs");

    let out = submit_as(&w, Some("t1rs"), &["12"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let queued = &w.state().queue[0];
    assert!(queued.submitter.is_some());
    assert_eq!(queued.submitter, queued.agent);
    assert_eq!(hc_lookups(&w).len(), 1);
}

#[test]
fn a_submit_from_a_plain_shell_has_no_submitter_even_with_an_agent_flag() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1rs");

    let out = submit(&w, &["12", "--agent", "t1rs"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let queued = &w.state().queue[0];
    assert_eq!(queued.agent.as_ref().unwrap().terminal.0, "t1rs");
    assert_eq!(queued.submitter, None);
    assert_eq!(w.entry(12).unwrap()["submitter"], Value::Null);
}

#[test]
fn a_resubmit_changes_the_submitter_even_for_the_same_head_and_keeps_it_when_none_is_known() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    hc_knows_impl_a(&w, "t1rs");
    hc_knows_orchestrator(&w);
    assert_eq!(code(&submit_as(&w, Some("t1rs"), &["12"])), Some(0));
    let first = w.state().queue[0].submitter.clone().unwrap();
    assert_eq!(first.terminal.0, "t1rs");

    let again = submit_as(&w, Some("t1orch"), &["12", "--agent", "t1rs"]);
    assert_eq!(code(&again), Some(0), "{}", stderr(&again));
    let second = w.state().queue[0].submitter.clone().unwrap();
    assert_eq!(second.terminal.0, "t1orch");

    let plain = submit(&w, &["12"]);
    assert_eq!(code(&plain), Some(0), "{}", stderr(&plain));
    let kept = w.state().queue[0].submitter.clone().unwrap();
    assert_eq!(kept.terminal.0, "t1orch");
}

#[test]
fn a_submit_from_another_pc_leaves_its_submitter_on_the_hub_with_that_pcs_machine() {
    let p = pair();
    both_see_pr(&p, &p.local_head);
    p.pc().sb.fake("hc").respond(
        "get --json t1orch",
        terminal("t1orch", "orch", 1763188300000, "/w/orch"),
    );

    let out = p
        .pc()
        .sb
        .command(BIN)
        .current_dir(p.pc().repo.path())
        .env("HC_TERMINAL_ID", "t1orch")
        .args(["submit", "12", "--agent", "t1rs"])
        .output()
        .unwrap();

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let queued = state_of(p.hub()).queue.remove(0);
    let pc_id = MachineId::try_from(p.pc().id.clone()).unwrap();
    let submitter = queued.submitter.unwrap();
    assert_eq!(submitter.terminal.0, "t1orch");
    assert_eq!(submitter.machine, Some(pc_id.clone()));
    assert_eq!(queued.agent.unwrap().terminal.0, "t1rs");
    assert_eq!(hub_hc_lookups(&p), 0, "허브는 hc get으로 다시 묻지 않는다");
}

#[test]
fn the_hidden_submitter_argument_keeps_quotes_spaces_and_korean_when_given_straight_to_the_hub() {
    let p = pair();
    both_see_pr(&p, &p.local_head);
    let submitter = Agent {
        terminal: TerminalId("t9".into()),
        created_ms: EpochMs(7),
        name: Some("한글 이름 \"따옴표\"".into()),
        workdir: "/w/공백 있는 폴더/x".into(),
        alive: true,
        machine: Some(MachineId::try_from(p.pc().id.clone()).unwrap()),
    };
    let text = serde_json::to_string(&submitter).unwrap();

    let out = p.hub().chute(
        BIN,
        &[
            "submit",
            "12",
            "--head",
            &p.local_head,
            "--relayed-agent",
            "null",
            "--relayed-submitter",
            &text,
        ],
    );

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(state_of(p.hub()).queue[0].submitter, Some(submitter));
}

#[test]
fn a_submit_from_another_pc_without_an_agent_flag_looks_the_terminal_up_once_for_agent_and_submitter()
 {
    let p = pair();
    both_see_pr(&p, &p.local_head);

    let out = p
        .pc()
        .sb
        .command(BIN)
        .current_dir(p.pc().repo.path())
        .env("HC_TERMINAL_ID", "t1rs")
        .args(["submit", "12"])
        .output()
        .unwrap();

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let lookups = p
        .pc()
        .sb
        .fake("hc")
        .calls()
        .into_iter()
        .filter(|c| c.first().is_some_and(|v| v == "get"))
        .count();
    assert_eq!(lookups, 1);
    let queued = state_of(p.hub()).queue.remove(0);
    assert!(queued.submitter.is_some());
    assert_eq!(queued.submitter, queued.agent);
}

fn requiring_head() -> String {
    let draft = chute::config::draft("true");
    assert!(draft.contains("require_head = false"));
    draft.replace("require_head = false", "require_head = true")
}

#[test]
fn require_head_refuses_a_submit_without_head_and_takes_one_with_it() {
    let w = World::with_config(Some(requiring_head()));
    gh_pr(&w, Some(12), pr(12, HEAD));

    let out = submit(&w, &["12"]);

    assert_eq!(code(&out), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("확인한 HEAD(`--head`) 없이 submit을 받지 않는다"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("PR과 HEAD를 보고한다"),
        "{}",
        stderr(&out)
    );
    assert!(w.state().queue.is_empty());

    assert_eq!(code(&submit(&w, &["12", "--head", HEAD])), Some(0));
    assert_eq!(w.state().queue[0].head, sha(HEAD));
}

#[test]
fn require_head_off_still_takes_a_submit_without_head() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));

    assert_eq!(code(&submit(&w, &["12"])), Some(0));
    assert_eq!(w.state().queue.len(), 1);
}

#[test]
fn require_head_refuses_a_submit_from_another_pc_that_gave_no_head() {
    let p = pair_with_config(&requiring_head());
    both_see_pr(&p, &p.local_head);

    let out = pc_submit(&p, &["--agent", "t1rs"]);

    assert_eq!(code(&out), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("확인한 HEAD(`--head`) 없이 submit을 받지 않는다"),
        "{}",
        stderr(&out)
    );
    assert!(state_of(p.hub()).queue.is_empty());

    let given = pc_submit(&p, &["--agent", "t1rs", "--head", &p.local_head]);
    assert_eq!(code(&given), Some(0), "{}", stderr(&given));
    assert_eq!(state_of(p.hub()).queue.len(), 1);
}

#[test]
fn submit_priority_marks_the_item_and_a_second_submit_can_clear_it() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));

    let first = w.ok(w.repo.path(), &["submit", "12", "--priority", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&first).unwrap()["change"],
        "added"
    );
    assert!(w.state().queue[0].priority);

    let same = w.ok(w.repo.path(), &["submit", "12", "--priority", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&same).unwrap()["change"],
        "unchanged"
    );

    let cleared = w.ok(w.repo.path(), &["submit", "12", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&cleared).unwrap()["change"],
        "options_updated"
    );
    assert!(!w.state().queue[0].priority);
}

#[test]
fn submit_records_a_submitted_event_only_when_it_changes_the_queue() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));

    submit(&w, &["12", "--priority"]);
    submit(&w, &["12", "--priority"]);
    submit(&w, &["12"]);

    let events: Vec<_> = w
        .store()
        .history()
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.kind)
        .collect();
    let priorities: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::Submitted {
                pr: PrNumber(12),
                priority,
                after,
                repair: false,
                ..
            } if after.is_empty() => Some(*priority),
            _ => None,
        })
        .collect();
    assert_eq!(priorities, [true, false]);
}

#[test]
fn submit_priority_from_another_pc_reaches_the_hub() {
    let p = pair();
    both_see_pr(&p, &p.local_head);

    let out = pc_submit(&p, &["--agent", "t1rs", "--priority"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(state_of(p.hub()).queue[0].priority);
}

fn queued_with_after(w: &World, pr: u64, after: &[u64]) {
    let mut queued = item(pr, ItemState::Queued(None));
    queued.after = after.iter().map(|n| PrNumber(*n)).collect();
    w.seed(vec![queued]);
}

#[test]
fn submit_after_a_queued_pr_holds_the_item_behind_it() {
    let w = World::new();
    w.seed(vec![item(1, ItemState::Queued(None))]);
    gh_pr(&w, Some(12), pr(12, HEAD));

    let out = submit(&w, &["12", "--after", "1", "--after", "#1"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    let queue = w.state().queue;
    assert_eq!(queue[1].pr, PrNumber(12));
    assert_eq!(queue[1].after, [PrNumber(1)]);
    assert_eq!(queue[1].state, ItemState::Queued(None));
}

#[test]
fn submit_after_a_pr_already_merged_on_github_is_taken_without_a_mark() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    gh_pr(&w, Some(7), json!({"state": "MERGED"}));

    let out = submit(&w, &["12", "--after", "7"]);

    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(w.state().queue[0].after.is_empty());
}

#[test]
fn submit_after_a_pr_that_is_neither_queued_nor_merged_is_refused() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));
    gh_pr(&w, Some(9), json!({"state": "OPEN"}));
    w.sb.fake("gh").respond(
        "pr view 8 --json *",
        Reply::ok("").code(1).stderr("not found"),
    );

    for n in ["9", "8"] {
        let out = submit(&w, &["12", "--after", n]);
        assert_eq!(code(&out), Some(1), "{n}: {}", stderr(&out));
        assert!(stderr(&out).contains("--after"), "{}", stderr(&out));
    }
    assert!(w.state().queue.is_empty());
}

#[test]
fn submit_after_itself_or_a_pr_that_waits_for_it_is_refused() {
    let w = World::new();
    gh_pr(&w, Some(12), pr(12, HEAD));

    let itself = submit(&w, &["12", "--after", "12"]);
    assert_eq!(code(&itself), Some(1), "{}", stderr(&itself));
    assert!(stderr(&itself).contains("자기 자신"), "{}", stderr(&itself));

    queued_with_after(&w, 1, &[12]);
    let cycle = submit(&w, &["12", "--after", "1"]);
    assert_eq!(code(&cycle), Some(1), "{}", stderr(&cycle));
    assert!(
        stderr(&cycle).contains("서로 기다리게"),
        "{}",
        stderr(&cycle)
    );
    assert_eq!(w.state().queue.len(), 1);
}

#[test]
fn submit_after_changes_on_the_same_head_and_empties_without_the_flag() {
    let w = World::new();
    w.seed(vec![
        item(1, ItemState::Queued(None)),
        item(2, ItemState::Queued(None)),
    ]);
    gh_pr(&w, Some(12), pr(12, HEAD));

    submit(&w, &["12", "--after", "1"]);
    let moved = w.ok(w.repo.path(), &["submit", "12", "--after", "2", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&moved).unwrap()["change"],
        "options_updated"
    );
    assert_eq!(w.state().queue[2].after, [PrNumber(2)]);

    submit(&w, &["12"]);
    assert!(w.state().queue[2].after.is_empty());
}
