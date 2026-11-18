import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  LOCAL_POLL_MS,
  REMOTE_POLL_MS,
  actionArgs,
  badgeText,
  causeText,
  changeSummary,
  diffNotices,
  formatAgo,
  formatDuration,
  interpret,
  RECENT_DEFAULT,
  clockExact,
  clockText,
  gradeOf,
  needsClock,
  nextFold,
  overview,
  overviewText,
  pollInterval,
  recentWindow,
  toggleFold,
  pullUrl,
  tint,
  toneToken,
  usualSeconds,
  MIN_USUAL_SECONDS,
} from "./board.js";

import { NARROW_WIDTH, isNarrow } from "./board.js";

const fixture = JSON.parse(readFileSync(new URL("../../tests/fixtures/status-full.json", import.meta.url), "utf8"));
const NOW = fixture.batch.started_ms + 312_000;

const unreachable = JSON.parse(
  readFileSync(new URL("../../tests/fixtures/status-hub-unreachable.json", import.meta.url), "utf8"),
);

const clone = (value) => structuredClone(value);
const ok = (data) => ({ code: 0, stdout: JSON.stringify(data), stderr: "" });
const board = (data, now = NOW) => interpret(ok(data), now);
const only = (data) => board(data).repos[0];
const section = (repo, id) => repo.sections.find((s) => s.id === id);
const prs = (s) => s.items.map((item) => item.pr);
const withQueue = (...items) => ({ ...clone(fixture), queue: items.map(clone) });
const queueOnly = (...items) => ({ ...withQueue(...items), blocked: null, warnings: [], post_verify: {}, batch: null });
const itemOf = (pr) => clone(fixture.queue.find((item) => item.pr === pr));

test("every fixture item lands in one of the six sections", () => {
  const result = board([fixture]);
  assert.equal(result.kind, "board");
  const repo = result.repos[0];
  assert.deepEqual(repo.sections.map((s) => s.id), ["alerts", "human", "batch", "waiting", "bounced"]);
  assert.deepEqual(repo.sections.map((s) => s.title), ["Alerts", "Needs you", "Current batch", "Queued", "Bounced"]);
  assert.deepEqual(prs(section(repo, "human")), [21]);
  assert.deepEqual(section(repo, "batch").items[0].prs.map((p) => p.pr), [12, 13]);
  assert.deepEqual(prs(section(repo, "waiting")), [19, 20, 22, 23, 24]);
  assert.deepEqual(prs(section(repo, "bounced")), [9, 10, 11, 15, 14, 16, 18, 17]);

  const placed = ["human", "waiting", "bounced"].flatMap((id) => prs(section(repo, id))).length + 2;
  assert.equal(placed, fixture.queue.length);
});

test("every state and reason in the fixture is covered", () => {
  const pairs = new Set(fixture.queue.map((item) => `${item.state}/${item.state_reason}`));
  assert.deepEqual(
    [...pairs].sort(),
    [
      "awaiting_approval/null",
      "bounced/ci",
      "bounced/conflict",
      "bounced/head_changed",
      "bounced/timeout",
      "bounced/verify",
      "in_batch/null",
      "queued/ci_pending",
      "queued/null",
    ],
  );
  const repo = only([fixture]);
  const reasons = Object.fromEntries(
    ["human", "bounced"].flatMap((id) => section(repo, id).items.map((item) => [item.pr, item.reason])),
  );
  assert.deepEqual(reasons, {
    21: "Waiting under an old chute rule. Approve, or resubmit to queue.",
    9: "Verification failed",
    10: "Verification failed",
    11: "Conflict",
    15: "Verification failed",
    14: "Conflict",
    16: "Timed out",
    18: "CI failed",
    17: "HEAD changed",
  });
});

test("buttons: approve and drop on awaiting approval, none elsewhere", () => {
  const repo = only([fixture]);
  const buttons = (id) =>
    Object.fromEntries(section(repo, id).items.map((item) => [item.pr, item.actions.map((a) => a.label)]));
  assert.deepEqual(buttons("human"), { 21: ["Approve", "Drop"] });
  for (const id of ["bounced", "waiting"]) {
    for (const item of section(repo, id).items) assert.equal(item.actions?.length ?? 0, 0, `#${item.pr}`);
  }
});

test("approve carries the HEAD the card showed; drop is [verb, pr, --repo, path]", () => {
  const repo = only([fixture]);
  const human = Object.fromEntries(section(repo, "human").items.map((item) => [item.pr, item.actions]));
  const path = fixture.repo.path;
  assert.deepEqual(human[21].map((a) => a.args), [
    ["approve", "21", "--head", itemOf(21).head, "--repo", path],
    ["drop", "21", "--repo", path],
  ]);
  assert.deepEqual(actionArgs("approve", 7, "/r", "abc1234"), ["approve", "7", "--head", "abc1234", "--repo", "/r"]);
  assert.deepEqual(actionArgs("drop", 7, "/r", "abc1234"), ["drop", "7", "--repo", "/r"]);
});

test("an awaiting item whose head is missing or unsafe gets no approve button, only drop", () => {
  const unsafe = [undefined, null, "", "abc", "ABC1234", "abc123g", "--repo", "abc 1234", " abc1234", "a".repeat(65), 1234, ["abc1234"]];
  for (const head of unsafe) {
    const item = section(only(withQueue({ ...itemOf(21), head })), "human").items[0];
    assert.deepEqual(item.actions.map((a) => a.verb), ["drop"], JSON.stringify(head));
    assert.equal(item.approvalBlocked, "Can't approve: HEAD unknown", JSON.stringify(head));
  }
  for (const head of ["abcd", "a".repeat(64), "09e5678"]) {
    const item = section(only(withQueue({ ...itemOf(21), head })), "human").items[0];
    assert.deepEqual(item.actions[0].args.slice(0, 4), ["approve", "21", "--head", head], head);
    assert.equal(item.approvalBlocked, null, head);
  }
});

test("a valid head with no buttons for another reason does not claim the head is unknown", () => {
  for (const patch of [{ pr: "21" }, { pr: 21.5 }]) {
    const item = section(only(withQueue({ ...itemOf(21), ...patch })), "human").items[0];
    assert.deepEqual(item.actions, []);
    assert.equal(item.approvalBlocked, null);
  }
  const data = clone(fixture);
  data.repo.path = "";
  assert.equal(section(only(data), "human").items.find((i) => i.pr === 21).approvalBlocked, null);
});

test("only awaiting_approval is the human's turn, and nothing on the board offers a retry", () => {
  const repo = only([fixture]);
  assert.deepEqual(section(repo, "human").items.map((i) => i.state), ["awaiting_approval"]);
  const verbs = repo.sections.flatMap((s) => s.items.flatMap((i) => (i.actions ?? []).map((a) => a.verb)));
  assert.deepEqual([...new Set(verbs)].sort(), ["approve", "drop"]);
});

test("a legacy needs_human item is an unknown state, not a human card", () => {
  const repo = only(withQueue({ ...itemOf(9), state: "needs_human", state_reason: "budget" }, itemOf(21)));
  assert.deepEqual(prs(section(repo, "human")), [21]);
  assert.deepEqual(section(repo, "unknown").items.map((i) => [i.pr, i.state, i.reason]), [[9, "needs_human", "budget"]]);
});

test("a repo without a path gets no buttons rather than a command aimed at the current folder", () => {
  const data = clone(fixture);
  delete data.repo.path;
  const repo = only(data);
  for (const item of section(repo, "human").items) assert.deepEqual(item.actions, []);
});

test("blocked queue and warnings come first", () => {
  const repo = only([fixture]);
  const alerts = section(repo, "alerts");
  assert.equal(repo.sections[0].id, "alerts");
  assert.equal(alerts.items[0].kind, "blocked");
  assert.match(alerts.items[0].text, /Queue stopped/);
  assert.equal(alerts.items[0].detail, "base 0a1b2c3 · for 7m 12s");
  assert.deepEqual(
    alerts.items.filter((a) => a.kind === "warning").map((a) => a.detail),
    fixture.warnings.map((w) => w.message),
  );
});

test("a stopped daemon is announced in the first section", () => {
  const data = clone(fixture);
  data.blocked = null;
  data.warnings = [];
  data.post_verify = {};
  data.daemon = { running: false, terminal: null, pid: null };
  const repo = only(data);
  assert.equal(repo.sections[0].id, "alerts");
  assert.equal(repo.sections[0].items.length, 1);
  assert.equal(repo.sections[0].items[0].kind, "daemon");
  assert.match(repo.sections[0].items[0].text, /Daemon is off/);
});

test("a healthy repo has no alerts section", () => {
  const data = clone(fixture);
  data.blocked = null;
  data.warnings = [];
  data.post_verify = {};
  assert.equal(section(only(data), "alerts"), undefined);
});

test("ci_pending is waiting for CI, a ci bounce is a CI failure", () => {
  const repo = only([fixture]);
  const waiting = section(repo, "waiting").items;
  assert.deepEqual(waiting.map((item) => [item.pr, item.label, item.ciPending]), [
    [19, "Waiting on CI", true],
    [20, "Queued", false],
    [22, "Queued", false],
    [23, "Queued", false],
    [24, "Queued", false],
  ]);
  const ci = section(repo, "bounced").items.find((item) => item.pr === 18);
  assert.equal(ci.reason, "CI failed");
  assert.equal(ci.failure.summary, "GitHub check build 실패");
});

test("waiting items are numbered in submission order", () => {
  const first = itemOf(20);
  const second = itemOf(19);
  const waiting = section(only(withQueue(first, second)), "waiting").items;
  assert.deepEqual(waiting.map((item) => [item.position, item.pr]), [[1, 19], [2, 20]]);
});

test("batch shows members, phase, step, round and elapsed time", () => {
  const [batch] = section(only([fixture]), "batch").items;
  assert.deepEqual(batch.prs, [
    { pr: 12, url: "https://github.com/Soochol/chute/pull/12", title: "큐 상태 전이 추가", marks: [], submitter: null },
    { pr: 13, url: "https://github.com/Soochol/chute/pull/13", title: "검증 실행기", marks: [], submitter: null },
  ]);
  assert.equal(batch.phase, "Verifying");
  assert.equal(batch.step, "test");
  assert.equal(batch.round, "Round 1");
  assert.equal(batch.elapsed, "5m 12s");
  assert.equal(batch.waitingMachine, false);
});

test("batch shows that it waits for a free slot on this PC", () => {
  const data = { ...fixture, batch: { ...fixture.batch, waiting_machine: true } };
  const [batch] = section(only([data]), "batch").items;
  assert.equal(batch.waitingMachine, true);
});

test("batch says which verification's pass it took over, and nothing when it ran its own", () => {
  const [reused] = section(only([fixture]), "batch").items;
  assert.equal(reused.reused, "Reused preflight pass");
  const own = { ...fixture, batch: { ...fixture.batch, reused: null } };
  assert.equal(section(only([own]), "batch").items[0].reused, null);
  const unknown = { ...fixture, batch: { ...fixture.batch, reused: "elsewhere" } };
  assert.equal(section(only([unknown]), "batch").items[0].reused, null);
});

test("an in_batch item with no batch record is still shown", () => {
  const data = withQueue(itemOf(12));
  data.batch = null;
  const [batch] = section(only(data), "batch").items;
  assert.deepEqual(batch.prs.map((p) => p.pr), [12]);
  assert.equal(batch.phase, null);
  assert.equal(batch.elapsed, null);
});

test("awaiting approval keeps the preflight result in the first info line", () => {
  const item = section(only([fixture]), "human").items.find((i) => i.pr === 21);
  assert.deepEqual([item.preflight.text, item.preflight.tone], ["Preflight passed", "ok"]);
  assert.deepEqual(item.facts, ["Preflight passed", "Retries 0/3", "waiting 8m"]);
  for (const card of [item, ...section(only([fixture]), "human").items]) {
    assert.ok(!JSON.stringify(card).includes("Awaiting approval"));
    assert.ok(!JSON.stringify(card).includes("chute.toml changed"));
    assert.ok(!JSON.stringify(card).includes("chute.toml unchanged"));
  }

  const plain = section(only(withQueue({ ...itemOf(21), touches_config: false, preflight: { result: "fail", head: "21e5678" } })), "human").items[0];
  assert.deepEqual([plain.preflight.text, plain.preflight.tone], ["Preflight failed", "error"]);

  const stale = section(only(withQueue({ ...itemOf(21), preflight: { result: "pass", head: "old0000" } })), "human").items[0];
  assert.equal(stale.preflight.text, "Preflight passed (old HEAD)");
  assert.equal(stale.preflight.stale, true);
  assert.equal(stale.preflight.tone, "muted");

  const none = section(only(withQueue({ ...itemOf(21), preflight: null })), "human").items[0];
  assert.equal(none.preflight.text, "No preflight");
});

const change = (kind, changes = [], more = 0) => ({ kind, changes, more });
const op = (kind, path, before = null, after = null) => ({ op: kind, path, before, after });
const humanCard = (patch, approval = "agent", pr = 21) => {
  const data = { ...withQueue({ ...itemOf(21), pr, ...patch }), approval };
  return section(only(data), "human").items[0];
};

test("question and reason follow the repo approval, touches_config and the change kind", () => {
  const OLD = "Waiting under an old chute rule. Approve, or resubmit to queue.";
  const HUMAN = "This repo asks a person to approve every PR.";
  const INVALID = "chute can't read the chute.toml in this PR, so the queue stops after merge.";
  const REMOVED = "This PR removes chute.toml, so the queue stops after merge.";
  const CONFIG_Q = "Approve config change?";
  const QUEUE_Q = "Approve to queue?";
  const ask = (patch, approval) => {
    const card = humanCard(patch, approval);
    return [card.question, card.reason];
  };
  for (const [approval, rest] of [["agent", OLD], ["human", HUMAN]]) {
    const at = (name) => `${name} ${approval}`;
    assert.deepEqual(ask({ touches_config: true, config_change: change("edited") }, approval), [CONFIG_Q, rest], at("edited"));
    assert.deepEqual(ask({ touches_config: true, config_change: change("added") }, approval), [CONFIG_Q, rest], at("added"));
    assert.deepEqual(ask({ touches_config: true, config_change: change("invalid") }, approval), [CONFIG_Q, INVALID], at("invalid"));
    assert.deepEqual(ask({ touches_config: true, config_change: change("removed") }, approval), [CONFIG_Q, REMOVED], at("removed"));
    assert.deepEqual(ask({ touches_config: true, config_change: change("unreadable") }, approval), [CONFIG_Q, rest], at("unreadable"));
    assert.deepEqual(ask({ touches_config: true, config_change: undefined }, approval), [CONFIG_Q, rest], at("undefined"));
    assert.deepEqual(ask({ touches_config: true, config_change: change("unchanged") }, approval), [QUEUE_Q, rest], at("unchanged"));
    assert.deepEqual(ask({ touches_config: false, config_change: null }, approval), [QUEUE_Q, rest], at("plain"));
  }
  assert.deepEqual(ask({ touches_config: false, config_change: change("invalid") }, "agent"), [QUEUE_Q, OLD]);
});

test("a chute.toml mark goes on cards that are not awaiting approval, and never for an unchanged file", () => {
  const marked = (patch, id = "waiting") => {
    const data = { ...withQueue({ ...itemOf(19), ...patch }), approval: "agent" };
    return section(only(data), id).items[0].marks.map((m) => [m.kind, m.text, m.tone]);
  };
  const MARK = [["config", "chute.toml", "warn"]];
  assert.deepEqual(marked({ touches_config: true, config_change: change("edited") }), MARK);
  assert.deepEqual(marked({ touches_config: true, config_change: change("invalid") }), MARK);
  assert.deepEqual(marked({ touches_config: true, config_change: undefined }), MARK);
  assert.deepEqual(marked({ touches_config: true, config_change: change("unchanged") }), []);
  assert.deepEqual(marked({ touches_config: false, config_change: change("edited") }), []);
  assert.deepEqual(
    marked({ ...itemOf(21), touches_config: true, config_change: change("edited") }, "human"),
    [],
  );
});

test("change lines pair a mark with a tone and show short values inline", () => {
  const lines = (changes, more = 0) => humanCard({ touches_config: true, config_change: change("edited", changes, more) }).summary;
  const got = lines([
    op("added", ["verify", "account-server-test"], null, "{ name = ... }"),
    op("removed", ["post_verify", "e2e"], "{ name = ... }"),
    op("added", ["post_verify", "smoke"], null, "{}"),
    op("removed", ["verify", "lint"], "{}"),
    op("changed", ["verify", "clippy", "run"], '"cargo clippy"', '"cargo clippy --all-targets -- -D warnings"'),
    op("changed", ["post_verify", "e2e", "run"], '"a"', '"b"'),
    op("changed", ["budget"], "3", "5"),
    op("changed", ["flaky", "chronic"], '"a-very-long-before"', '"a-very-long-after"'),
    op("added", ["env_check"], null, '["A"]'),
    op("removed", ["cache", "dir"], '"x"'),
    op("reordered", ["verify"]),
    op("reordered", ["post_verify"]),
  ], 3);
  assert.deepEqual(
    got.map((l) => [l.mark, l.text, l.tone]),
    [
      ["+", "step account-server-test", "ok"],
      ["−", "post-merge step e2e", "error"],
      ["+", "post-merge step smoke", "ok"],
      ["−", "step lint", "error"],
      ["~", "clippy: run changed", "warn"],
      ["~", "post-merge e2e: run: \"a\" → \"b\"", "warn"],
      ["~", "budget: 3 → 5", "warn"],
      ["~", "flaky.chronic changed", "warn"],
      ["+", 'env_check = ["A"]', "ok"],
      ["−", "cache.dir", "error"],
      ["~", "steps reordered", "warn"],
      ["~", "post-merge steps reordered", "warn"],
      ["+", "3 more changes", "waiting"],
    ],
  );
  const tips = got.map((l) => l.tip);
  assert.equal(tips[4], '"cargo clippy" → "cargo clippy --all-targets -- -D warnings"');
  assert.equal(tips[6], null, "short values are on the line");
  assert.equal(tips[7], '"a-very-long-before" → "a-very-long-after"');
  assert.equal(lines([op("changed", ["budget"], "3", "5")], 1).at(-1).text, "1 more change");
  assert.equal(lines([op("changed", ["budget"], "x".repeat(12), "y".repeat(12))])[0].text, "budget: " + "x".repeat(12) + " → " + "y".repeat(12));
  assert.equal(lines([op("changed", ["budget"], "x".repeat(12), "y".repeat(13))])[0].text, "budget changed");
});

test("a note stands in for the change lines by kind, and for items from before the field existed", () => {
  const note = (patch) => humanCard({ touches_config: true, ...patch }).summary.map((l) => [l.text, l.tone]);
  assert.deepEqual(note({ config_change: change("added") }), [["Adds chute.toml", "waiting"]]);
  assert.deepEqual(note({ config_change: change("removed") }), [["Removes chute.toml", "error"]]);
  assert.deepEqual(note({ config_change: change("invalid") }), [["chute.toml in this PR is invalid", "error"]]);
  assert.deepEqual(note({ config_change: change("edited") }), [["Only comments or formatting changed", "waiting"]]);
  const none = [["No change summary. Check the diff.", "waiting"]];
  assert.deepEqual(note({ config_change: change("unreadable") }), none);
  assert.deepEqual(note({ config_change: change("unchanged") }), none);
  assert.deepEqual(note({ config_change: undefined }), none);
  assert.deepEqual(note({ config_change: "garbage" }), none);
  assert.deepEqual(humanCard({ touches_config: false, config_change: null }).summary, []);
});

test("View diff opens the files tab of the PR, and is absent without a slug", () => {
  assert.equal(humanCard({}).diffUrl, "https://github.com/Soochol/chute/pull/21/files");
  assert.equal(pullUrl("Soochol/chute", 21) + "/files", humanCard({}).diffUrl);
  const data = clone(fixture);
  data.repo.slug = "";
  assert.equal(section(only(data), "human").items.find((i) => i.pr === 21).diffUrl, null);
});

test("the submitter line says who submitted, once when the agent is the submitter", () => {
  const orch = { terminal: "t1zz", name: "orch", alive: true };
  const differs = humanCard({ submitter: orch });
  assert.deepEqual([differs.submittedBy, differs.sameSubmitter], ["submitted by orch (t1zz)", false]);
  assert.equal(differs.agent.label, "impl-config (t1s4)");

  const same = humanCard({ submitter: { terminal: "t1s4", name: "impl-config", alive: true } });
  assert.deepEqual([same.submittedBy, same.sameSubmitter], ["submitted by impl-config (t1s4)", true]);

  const neither = humanCard({ agent: null, submitter: null });
  assert.deepEqual([neither.submittedBy, neither.sameSubmitter], ["submitted by unknown terminal", false]);

  const agentOnly = humanCard({ submitter: null });
  assert.equal(agentOnly.submittedBy, null);

  const submitterOnly = humanCard({ agent: null, submitter: orch });
  assert.equal(submitterOnly.submittedBy, "submitted by orch (t1zz)");
});

test("an overlap notice names the other PRs that change the same target", () => {
  const edit = (pr, ...paths) => ({
    ...itemOf(21),
    pr,
    head: `${pr}e5678`.padStart(7, "0"),
    touches_config: true,
    config_change: change("edited", paths.map((path) => op("changed", path, "1", "2"))),
  });
  const cards = (...items) =>
    Object.fromEntries(section(only(withQueue(...items)), "human").items.map((card) => [card.pr, card.overlap]));

  assert.deepEqual(cards(edit(21, ["verify", "clippy", "run"]), edit(22, ["verify", "clippy", "timeout"])), {
    21: "#22 also changes clippy. These may conflict.",
    22: "#21 also changes clippy. These may conflict.",
  });
  assert.deepEqual(cards(edit(21, ["verify", "clippy", "run"]), edit(22, ["verify", "test", "run"])), { 21: null, 22: null });
  assert.deepEqual(cards(edit(21, ["budget"]), edit(22, ["post_verify", "budget", "run"])), { 21: null, 22: null });
  assert.equal(cards(edit(21, ["flaky", "chronic"]), edit(22, ["flaky", "chronic"]))[21], "#22 also changes flaky.chronic. These may conflict.");

  const three = cards(edit(21, ["verify", "clippy", "run"]), edit(22, ["verify", "clippy", "run"]), edit(23, ["verify", "clippy", "run"]));
  assert.equal(three[21], "#22, #23 also change clippy. These may conflict.");
  assert.equal(three[23], "#21, #22 also change clippy. These may conflict.");

  const first = cards(edit(21, ["budget"], ["verify", "clippy", "run"]), edit(22, ["verify", "clippy", "run"]));
  assert.equal(first[21], "#22 also changes clippy. These may conflict.");

  const bare = { ...itemOf(21), touches_config: true, config_change: undefined };
  assert.deepEqual(cards(bare, { ...bare, pr: 22 }), { 21: null, 22: null });
  assert.deepEqual(cards(edit(21, ["verify", "clippy", "run"]), { ...bare, pr: 22 }), { 21: null, 22: null });
  const queued = { ...edit(22, ["verify", "clippy", "run"]), state: "queued" };
  assert.equal(cards(edit(21, ["verify", "clippy", "run"]), queued)[21], null);
});

test("human cards carry question, agent and retries", () => {
  const [card] = section(only([fixture]), "human").items;
  assert.equal(card.state, "awaiting_approval");
  assert.equal(card.question, "Approve config change?");
  assert.equal(card.retries, "0/3");
  assert.deepEqual(card.agent, { terminal: "t1s4", name: "impl-config", alive: true, remote: false, host: null, label: "impl-config (t1s4)" });
});

test("bounced cards show a live, a dead and a missing agent", () => {
  const items = section(only([fixture]), "bounced").items;
  const retried = items.find((i) => i.pr === 9);
  assert.equal(retried.retries, "3/3");
  assert.deepEqual(retried.agent, { terminal: "t1rp", name: "impl-h", alive: true, remote: false, host: null, label: "impl-h (t1rp)" });
  assert.equal(retried.failure.summary, "test 단계 실패: skill::tests::prints_manual");
  assert.equal(items.find((i) => i.pr === 10).agent.alive, false);
  assert.equal(items.find((i) => i.pr === 11).agent, null);
});

test("bounced cards carry mode, summary, agent, retries and report path", () => {
  const items = section(only([fixture]), "bounced").items;
  const verify = items.find((i) => i.pr === 15);
  assert.equal(verify.failure.mode, "Solo");
  assert.equal(verify.failure.summary, "test 단계 실패: feedback::tests::report_lists_partners");
  assert.equal(verify.failure.report, "/home/dev/.local/state/chute/repos/Soochol__chute/reports/pr-15/2.md");
  assert.equal(verify.retries, "2/3");
  assert.equal(verify.agent.label, "impl-d (t1rv)");
  assert.equal(items.find((i) => i.pr === 16).failure.mode, "Combined");
  assert.equal(items.find((i) => i.pr === 14).failure.mode, null);
  const headChanged = items.find((i) => i.pr === 17);
  assert.equal(headChanged.failure, null);
  assert.equal(headChanged.reason, "HEAD changed");
});

const bouncedCard = (patch) => {
  const item = { ...itemOf(15), ...patch };
  return section(only(queueOnly(item)), "bounced").items[0];
};

test("a bounced card's summary line is the first non-empty line of last_failure.summary", () => {
  const line = (summary) => bouncedCard({ last_failure: { kind: "verify", summary } }).summaryLine;
  assert.equal(line("e2e failed: a.spec.ts:1\nsee the report"), "e2e failed: a.spec.ts:1");
  assert.equal(line("\n  \n  conflict: src/a.rs  \nmore"), "conflict: src/a.rs");
  assert.equal(line(""), null);
  assert.equal(line("  \n "), null);
  assert.equal(line(42), null);
  assert.equal(line(undefined), null);
  assert.equal(bouncedCard({ last_failure: null }).summaryLine, null);
});

test("retry dots: one per budget try, filled by attempts, warn on the last retry", () => {
  const dots = (attempts, budget) => bouncedCard({ attempts, budget }).retryDots;
  assert.deepEqual(dots(1, 3), { total: 3, used: 1, tone: "dim", tip: "Bounced 1 of 3 · 3 retries left" });
  assert.deepEqual(dots(3, 3), { total: 3, used: 3, tone: "warn", tip: "Bounced 3 of 3 · 1 retry left" });
  assert.equal(dots(5, 3).used, 3);
});

test("retry dots are absent for a budget outside 1 to 10 or a broken attempts count", () => {
  for (const [budget, text] of [[0, "0"], [11, "11"], [1.5, "1.5"], ["3", "—"], [undefined, "—"]]) {
    const card = bouncedCard({ attempts: 1, budget });
    assert.equal(card.retryDots, null, `budget ${String(budget)}`);
    assert.equal(card.retries, `1/${text}`);
  }
  for (const attempts of [-1, null]) {
    const card = bouncedCard({ attempts, budget: 3 });
    assert.equal(card.retryDots, null, `attempts ${String(attempts)}`);
    assert.equal(card.retries, `${attempts === null ? "—" : attempts}/3`);
  }
});

test("today is one line, and the expanded rows call the verify time a sum", () => {
  const today = only([fixture]).today;
  assert.deepEqual(today.parts, ["7 merged", "2 bounced", "median 6m 52s"]);
  assert.deepEqual(
    today.details.map((d) => [d.key, d.label, d.value, d.hint ?? null]),
    [
      ["flaky", "Flaky", "1", null],
      ["batches", "Batches", "3", null],
      ["verify", "Total verify time", "21m", "(sum of 3 batches)"],
    ],
  );
  const data = clone(fixture);
  data.today.median_lead_s = null;
  assert.deepEqual(only(data).today.parts, ["7 merged", "2 bounced", "median —"]);
  delete data.today;
  assert.equal(only(data).today, null);
});

test("empty alerts are not drawn", () => {
  const data = { ...clone(fixture), queue: [], batch: null, blocked: null, warnings: [], post_verify: {} };
  assert.ok(!only(data).sections.some((s) => s.id === "alerts"));
});

test("several repos from --all, and a single object", () => {
  const second = clone(fixture);
  second.repo = { ...second.repo, slug: "Soochol/other", path: "/home/dev/code/other" };
  const result = board([fixture, second]);
  assert.deepEqual(result.repos.map((r) => r.slug), ["Soochol/chute", "Soochol/other"]);
  assert.equal(section(result.repos[1], "human").items[0].actions[0].args.at(-1), "/home/dev/code/other");
  assert.equal(board(fixture).repos.length, 1);
  const keys = result.repos.map((r) => section(r, "human").items[0].key);
  assert.equal(new Set(keys).size, 2, keys.join());
  assert.equal(result.repos[0].stateDir, fixture.repo.state_dir);
  assert.equal(result.repos[0].approval, "Agent trusted");
});

test("no registered repos", () => {
  const result = interpret({ code: 0, stdout: "[]\n", stderr: "" }, NOW);
  assert.equal(result.kind, "empty");
  assert.match(result.hint, /chute init/);
});

test("broken or unexpected output does not throw", () => {
  for (const stdout of ["", "{", "not json at all\nsecond line", "null", "42", '"text"', "[1, 2]"]) {
    const result = interpret({ code: 0, stdout, stderr: "" }, NOW);
    assert.equal(result.kind, "failed", JSON.stringify(stdout));
    assert.ok(result.detail.length > 0);
  }
  assert.match(interpret({ code: 0, stdout: "not json\nmore" }, NOW).detail, /not json/);
  assert.equal(interpret(undefined, NOW).kind, "failed");
  assert.equal(interpret(null, NOW).kind, "failed");
});

test("a failing chute shows the first line of its error", () => {
  const result = interpret({ code: 1, stdout: "", stderr: "\n상태 파일이 깨졌다: state.json\n  원인: eof\n" }, NOW);
  assert.equal(result.kind, "failed");
  assert.equal(result.detail, "상태 파일이 깨졌다: state.json");
  assert.equal(interpret({ code: 3, stdout: "", stderr: "" }, NOW).detail, "Exit code 3");
});

test("chute that cannot start shows that fact and install guidance", () => {
  const result = interpret({ spawnError: "cannot start chute: program not found" }, NOW);
  assert.equal(result.kind, "missing");
  assert.equal(result.detail, "cannot start chute: program not found");
  assert.match(result.title, /Can't run chute/);
  assert.match(result.hint, /cargo install/);
  assert.match(result.hint, /bin/);
});

test("unknown values are shown as they are", () => {
  const odd = {
    ...itemOf(20),
    pr: 30,
    title: "새 상태",
    state: "frobnicated",
    state_reason: "because",
  };
  const bounced = { ...itemOf(15), pr: 31, state_reason: "cosmic_rays" };
  const waiting = { ...itemOf(20), pr: 33, state_reason: "later" };
  const data = withQueue(odd, bounced, waiting, itemOf(21));
  data.batch = { ...data.batch, phase: "teleporting", prs: [] };
  data.blocked = { reason: "new_reason", base: "abc", since_ms: 1 };
  data.approval = "robot";
  data.queue[0].agent = null;
  const repo = only(data);

  assert.deepEqual(section(repo, "unknown").items.map((i) => [i.pr, i.state, i.reason]), [[30, "frobnicated", "because"]]);
  assert.equal(section(repo, "bounced").items[0].reason, "cosmic_rays");
  assert.equal(section(repo, "waiting").items[0].note, "later");
  assert.equal(section(repo, "alerts").items[0].text, "Queue stopped: new_reason");
  assert.equal(repo.approval, "robot");
  assert.equal(
    repo.sections.reduce((n, s) => n + (s.id === "alerts" || s.id === "today" || s.id === "batch" ? 0 : s.items.length), 0),
    4,
  );
});

test("an unknown batch phase and preflight result are shown as they are", () => {
  const data = withQueue(itemOf(12), { ...itemOf(21), preflight: { result: "vanished", head: "21e5678" } });
  data.batch.phase = "teleporting";
  const repo = only(data);
  assert.equal(section(repo, "batch").items[0].phase, "teleporting");
  assert.equal(section(repo, "human").items[0].preflight.text, "Preflight vanished");
});

test("missing fields do not break the board", () => {
  const sparse = { schema: 1, queue: [{ pr: 1, state: "queued" }, { pr: 2, state: "awaiting_approval" }, "junk", null] };
  const repo = only(sparse);
  assert.equal(repo.slug, "(unnamed repo)");
  assert.deepEqual(prs(section(repo, "waiting")), [1]);
  assert.deepEqual(prs(section(repo, "human")), [2]);
  assert.equal(section(repo, "human").items[0].retries, "—/—");
});

test("formatDuration", () => {
  assert.equal(formatDuration(0), "0s");
  assert.equal(formatDuration(59), "59s");
  assert.equal(formatDuration(60), "1m");
  assert.equal(formatDuration(125), "2m 5s");
  assert.equal(formatDuration(3600), "1h");
  assert.equal(formatDuration(3660), "1h 1m");
  assert.equal(formatDuration(59.6), "1m");
  assert.equal(formatDuration(null), "—");
  assert.equal(formatDuration(-1), "—");
});

test("values that are names of built-in object keys are shown as they are", () => {
  for (const word of ["constructor", "toString", "__proto__", "hasOwnProperty", "valueOf"]) {
    const data = withQueue(
      { ...itemOf(20), pr: 30, state: word },
      { ...itemOf(15), pr: 31, state_reason: word },
      { ...itemOf(20), pr: 33, state_reason: word },
      { ...itemOf(21), pr: 34, preflight: { result: word, head: "21e5678" } },
      { ...itemOf(15), pr: 35, last_failure: { ...itemOf(15).last_failure, mode: word } },
    );
    data.batch = { ...data.batch, phase: word };
    data.blocked = { reason: word, base: "abc", since_ms: 1 };
    data.approval = word;
    const repo = only(data);
    const find = (id, pr) => section(repo, id).items.find((item) => item.pr === pr);

    assert.deepEqual(section(repo, "unknown").items.map((i) => [i.pr, i.state]), [[30, word]], word);
    assert.equal(find("bounced", 31).reason, word, word);
    assert.equal(find("bounced", 35).failure.mode, word, word);
    assert.equal(find("waiting", 33).note, word, word);
    assert.equal(find("human", 34).preflight.text, `Preflight ${word}`, word);
    assert.equal(section(repo, "batch").items[0].phase, word, word);
    assert.equal(section(repo, "alerts").items[0].text, `Queue stopped: ${word}`, word);
    assert.equal(repo.approval, word, word);
  }
});

test("an object where a state or reason should be is shown as JSON text", () => {
  const data = withQueue(
    { ...itemOf(20), pr: 30, state: { a: 1 }, state_reason: ["x"] },
    { ...itemOf(20), pr: 31, state_reason: { b: 2 } },
  );
  const repo = only(data);
  assert.deepEqual(section(repo, "unknown").items.map((i) => [i.state, i.reason]), [['{"a":1}', '["x"]']]);
  assert.equal(section(repo, "waiting").items[0].note, '{"b":2}');
});

test("an item without a PR number gets no buttons", () => {
  const noPr = { ...itemOf(21), pr: undefined };
  const items = section(only(withQueue(noPr, { ...itemOf(21), pr: 22 })), "human").items;
  assert.equal(items.length, 2);
  for (const item of items) {
    if (item.pr === "?") assert.deepEqual(item.actions, []);
    else assert.equal(item.actions.length, 2);
  }
  assert.ok(items.some((item) => item.pr === "?"));
});

test("an exit code that is not a number is reported without printing null", () => {
  const result = interpret({ code: null, stdout: "", stderr: "" }, NOW);
  assert.equal(result.kind, "failed");
  assert.equal(result.detail, "Exited abnormally");
});

test("warnings alone still make the alerts section", () => {
  const data = { ...clone(fixture), blocked: null };
  assert.deepEqual(section(only(data), "alerts").items.map((a) => a.kind), ["post_verify", "warning", "warning", "warning"]);
});

test("a blocked queue and a stopped daemon are both announced, blocked first", () => {
  const data = { ...clone(fixture), daemon: { running: false, terminal: null, pid: null } };
  assert.deepEqual(section(only(data), "alerts").items.map((a) => a.kind), ["blocked", "daemon", "post_verify", "warning", "warning", "warning"]);
  data.warnings = [];
  data.post_verify = {};
  assert.deepEqual(section(only(data), "alerts").items.map((a) => a.kind), ["blocked", "daemon"]);
});

test("an unknown failure mode is shown as it is", () => {
  const data = withQueue({ ...itemOf(15), last_failure: { ...itemOf(15).last_failure, mode: "triple" } });
  assert.equal(section(only(data), "bounced").items[0].failure.mode, "triple");
});

test("objects where numbers or strings belong never reach the screen code as objects", () => {
  const hostile = JSON.parse('{"toString": 1}');
  const data = withQueue(
    { ...itemOf(21), pr: hostile },
    { ...itemOf(9), pr: 40, state_reason: hostile },
    { ...itemOf(20), pr: 41, state: hostile },
    { ...itemOf(15), pr: 42, submitted_ms: hostile, last_failure: { ...itemOf(15).last_failure, mode: hostile } },
    { ...itemOf(20), pr: 43, state_reason: hostile },
  );
  data.batch = { ...data.batch, phase: hostile, prs: [hostile, 12], round: hostile };
  data.blocked = { reason: hostile, base: hostile, since_ms: hostile };
  data.approval = hostile;
  const leaves = [];
  const walk = (value) => {
    if (value !== null && typeof value === "object") Object.values(value).forEach(walk);
    else leaves.push(value);
  };
  const result = board(data);
  assert.equal(result.kind, "board");
  walk(result);
  assert.ok(leaves.length > 50);
  for (const leaf of leaves) assert.doesNotThrow(() => `${leaf}`);
  assert.deepEqual(
    result.repos[0].sections.find((s) => s.id === "unknown").items.map((i) => i.state),
    ['{"toString":1}'],
  );
});

test("every batch phase and approval label is pinned", () => {
  const phases = { building: "Building", verifying: "Verifying", bisecting: "Bisecting", pushing: "Pushing" };
  for (const [code, label] of Object.entries(phases)) {
    const data = clone(fixture);
    data.batch.phase = code;
    assert.equal(section(only(data), "batch").items[0].phase, label, code);
  }
  for (const [code, label] of Object.entries({ agent: "Agent trusted", human: "Human review" })) {
    assert.equal(only({ ...clone(fixture), approval: code }).approval, label, code);
  }
});

test("every preflight result has its text and tone", () => {
  const cases = [
    ["pass", "Preflight passed", "ok"],
    ["fail", "Preflight failed", "error"],
    ["conflict", "Preflight conflict", "error"],
    ["skipped", "Preflight skipped", "muted"],
  ];
  for (const [result, text, tone] of cases) {
    const item = section(only(withQueue({ ...itemOf(21), preflight: { result, head: "21e5678" } })), "human").items[0];
    assert.deepEqual([item.preflight.text, item.preflight.tone], [text, tone], result);
  }
  const empty = section(only(withQueue({ ...itemOf(21), preflight: {} })), "human").items[0];
  assert.equal(empty.preflight.text, "Preflight ? (old HEAD)");
});

test("alerts keep their text, kind and tone", () => {
  const alerts = section(only([fixture]), "alerts").items;
  assert.deepEqual(alerts.map((a) => [a.kind, a.tone]), [
    ["blocked", "error"],
    ["post_verify", "error"],
    ["warning", "warn"],
    ["warning", "warn"],
    ["warning", "warn"],
  ]);
  assert.equal(alerts[0].text, "Queue stopped: base commit failed verification");

  const data = {
    ...clone(fixture),
    blocked: null,
    post_verify: {},
    daemon: { running: false, terminal: null, pid: null },
    warnings: ["문자열 경고", { kind: "config" }, { message: "" }, {}],
  };
  assert.deepEqual(section(only(data), "alerts").items.map((a) => [a.kind, a.tone, a.text]), [
    ["daemon", "error", "Daemon is off"],
    ["warning", "warn", "Warning"],
    ["warning", "warn", "Can't read config"],
    ["warning", "warn", "Warning"],
    ["warning", "warn", "Warning"],
  ]);
  assert.deepEqual(
    section(only(data), "alerts").items.filter((a) => a.kind === "warning").map((a) => a.detail),
    ["문자열 경고", "", "", ""],
  );
});

test("a blocked reason that is not a known word is shown as it is, and a future time adds no age", () => {
  const data = clone(fixture);
  data.blocked = { reason: 5, base: "abc", since_ms: NOW + 10_000 };
  const [alert] = section(only(data), "alerts").items;
  assert.equal(alert.text, "Queue stopped: 5");
  assert.equal(alert.detail, "base abc");
  data.blocked = { reason: { x: 1 } };
  assert.equal(section(only(data), "alerts").items[0].text, 'Queue stopped: {"x":1}');
  data.blocked = {};
  assert.equal(section(only(data), "alerts").items[0].text, "Queue stopped: unknown reason");
  data.blocked = "base_broken";
  assert.equal(section(only(data), "alerts").items[0].text, "Queue stopped: base commit failed verification");
});

test("a daemon record that does not say running:false raises no alert", () => {
  for (const daemon of [{}, null, { running: true }, { running: null }]) {
    const data = { ...clone(fixture), blocked: null, warnings: [], post_verify: {}, daemon };
    assert.equal(section(only(data), "alerts"), undefined, JSON.stringify(daemon));
  }
});

test("a batch that has not started its first round shows no round, and one that starts in the future shows no negative time", () => {
  const data = clone(fixture);
  data.batch.round = 0;
  data.batch.started_ms = NOW + 60_000;
  const [batch] = section(only(data), "batch").items;
  assert.equal(batch.round, null);
  assert.equal(batch.elapsed, "0s");
});

test("a clock that is not a number falls back to the real clock", () => {
  const real = Date.now;
  Date.now = () => NOW;
  try {
    for (const clock of [NaN, Infinity, null, "x"]) {
      assert.equal(section(board([fixture], clock).repos[0], "alerts").items[0].detail, "base 0a1b2c3 · for 7m 12s", String(clock));
    }
  } finally {
    Date.now = real;
  }
});

test("a waiting item that waits for CI shows no raw reason", () => {
  assert.equal(section(only([fixture]), "waiting").items.find((i) => i.pr === 19).note, null);
});

test("the unknown section comes after bounced, in submission order, with title and raw state", () => {
  const data = queueOnly(
    { ...itemOf(20), pr: 31, title: "둘째", state: "zeta", submitted_ms: 2 },
    { ...itemOf(20), pr: 30, title: "첫째", state: "", submitted_ms: 1 },
    itemOf(15),
  );
  const repo = only(data);
  assert.deepEqual(repo.sections.map((s) => s.id), ["human", "batch", "waiting", "bounced", "unknown"]);
  assert.equal(section(repo, "unknown").title, "Unknown state");
  assert.deepEqual(section(repo, "unknown").items.map((i) => [i.pr, i.title, i.state, i.reason]), [
    [30, "첫째", "(none)", null],
    [31, "둘째", "zeta", null],
  ]);
});

test("array-wrapped states are unknown, not routed as the word inside", () => {
  const data = queueOnly(
    { ...itemOf(9), pr: 40, state: ["needs_human"], state_reason: ["budget"] },
    { ...itemOf(21), pr: 41, state: ["awaiting_approval"] },
    { ...itemOf(20), pr: 42, state: [["queued"]] },
  );
  const repo = only(data);
  assert.deepEqual(repo.sections.filter((s) => s.items.length > 0).map((s) => s.id), ["unknown"]);
  assert.deepEqual(
    section(repo, "unknown").items.map((i) => i.state).sort(),
    ['["awaiting_approval"]', '["needs_human"]', '[["queued"]]'],
  );
});

test("items are ordered by submission time, then PR number, and unknown times go last", () => {
  const data = withQueue(
    { ...itemOf(20), pr: 9, submitted_ms: 5 },
    { ...itemOf(20), pr: 3, submitted_ms: 5 },
    { ...itemOf(20), pr: 8, submitted_ms: "x" },
    { ...itemOf(20), pr: 7, submitted_ms: undefined },
    { ...itemOf(20), pr: 4, submitted_ms: 1 },
  );
  assert.deepEqual(prs(section(only(data), "waiting")), [4, 3, 9, 7, 8]);
});

test("a bounced item without a reason falls back to the failure kind, then to a plain note", () => {
  const failure = itemOf(15).last_failure;
  const data = withQueue(
    { ...itemOf(15), pr: 97, state_reason: null, last_failure: { ...failure, kind: "ci" } },
    { ...itemOf(15), pr: 98, state_reason: "", last_failure: { ...failure, kind: "" } },
    { ...itemOf(15), pr: 99, state_reason: null, last_failure: null },
  );
  const reasons = Object.fromEntries(section(only(data), "bounced").items.map((i) => [i.pr, i.reason]));
  assert.deepEqual(reasons, { 97: "CI failed", 98: "No reason", 99: "No reason" });
});

test("failure text prefers stderr, then stdout, then the exit code", () => {
  assert.equal(interpret({ code: 2, stdout: "out line\nmore", stderr: "" }, NOW).detail, "out line");
  assert.equal(interpret({ code: 2, stdout: "out", stderr: "err\n" }, NOW).detail, "err");
  assert.equal(interpret({ code: 2, stdout: "", stderr: "" }, NOW).detail, "Exit code 2");
  assert.equal(interpret({ code: 2, stdout: "", stderr: "\n   spaced out  \n" }, NOW).detail, "spaced out");
  assert.equal(interpret({ code: 0, stdout: `${"x".repeat(300)}\nmore` }, NOW).detail.length < 200, true);
  assert.equal(interpret({ spawnError: "" }, NOW).kind, "missing");
  assert.equal(interpret({ code: 1, stdout: { a: 1 }, stderr: { b: 2 } }, NOW).detail, "Exit code 1");
  assert.equal(interpret({ code: null, stdout: {}, stderr: [] }, NOW).detail, "Exited abnormally");
});

test("batch members come from the batch record even when the queue lacks them", () => {
  const data = { ...clone(fixture), queue: [] };
  assert.deepEqual(section(only(data), "batch").items[0].prs, [
    { pr: 12, url: "https://github.com/Soochol/chute/pull/12", title: "", marks: [], submitter: null },
    { pr: 13, url: "https://github.com/Soochol/chute/pull/13", title: "", marks: [], submitter: null },
  ]);
});

test("in_batch items are all shown, also when they have no usable PR number", () => {
  const data = withQueue({ ...itemOf(12), pr: undefined, title: "A" }, { ...itemOf(13), pr: -3, title: "B" }, { ...itemOf(12), pr: 12, title: "C" });
  data.batch.prs = [];
  const shown = section(only(data), "batch").items[0].prs.map((p) => [p.pr, p.title]);
  assert.deepEqual(shown.sort((a, b) => a[1].localeCompare(b[1])), [["?", "A"], ["?", "B"], [12, "C"]]);
});

test("a PR number that is not a positive safe integer gets no buttons and shows as ?", () => {
  for (const pr of [-3, 0, 1.5, 1e21, 9007199254740993, "7", null]) {
    const item = section(only(withQueue({ ...itemOf(21), pr })), "human").items[0];
    assert.equal(item.pr, "?", String(pr));
    assert.deepEqual(item.actions, [], String(pr));
  }
  assert.equal(section(only(withQueue({ ...itemOf(21), pr: 9007199254740991 })), "human").items[0].actions.length, 2);
});

test("a very deeply nested value is shown without throwing", () => {
  const depth = 20000;
  const text = JSON.stringify({ ...clone(fixture), queue: [{ ...itemOf(20), pr: 30, state: "@@DEEP@@" }] }).replace(
    '"@@DEEP@@"',
    `${"[".repeat(depth)}"x"${"]".repeat(depth)}`,
  );
  const result = interpret({ code: 0, stdout: text, stderr: "" }, NOW);
  assert.equal(result.kind, "board");
  assert.equal(section(result.repos[0], "unknown").items[0].state, "(can't display)");
});

test("a batch record with garbage in its PR list does not leak objects", () => {
  const data = clone(fixture);
  data.batch.prs = [12, { a: 1 }, null, "13"];
  assert.deepEqual(section(only(data), "batch").items[0].prs.map((p) => p.pr), [12, "?", "?", "?", 13]);
});

test("a rejection from exec becomes its message, or nothing", () => {
  assert.equal(causeText(new Error("cannot run chute: not found\nmore")), "cannot run chute: not found");
  assert.equal(causeText("spawn failed"), "spawn failed");
  for (const odd of [undefined, null, {}, { code: "ENOENT" }, { message: 5 }, 7]) assert.equal(causeText(odd), "", String(odd));
});

test("random malformed status never breaks the board", () => {
  let seed = 20261115;
  const rnd = () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const pick = (list) => list[Math.floor(rnd() * list.length)];
  const WORDS = ["", " ", "x", "queued", "in_batch", "bounced", "ci_pending", "ci", "budget", "pass", "solo", "base_broken", "__proto__", "constructor", "toString", "?", "5", "-1", "a\nb", "t1"];
  const NUMS = [0, 1, 2, 3, 5, 12, -1, 1.5, 1e21, -0, 9007199254740993, 1794724200000, 1e-7];
  const garbage = (depth = 0) => {
    const kind = Math.floor(rnd() * (depth > 2 ? 5 : 7));
    if (kind === 0) return null;
    if (kind === 1) return rnd() < 0.5;
    if (kind === 2) return pick(NUMS);
    if (kind < 5) return pick(WORDS);
    if (kind === 5) return Array.from({ length: Math.floor(rnd() * 3) }, () => garbage(depth + 1));
    const object = {};
    for (let n = Math.floor(rnd() * 3); n > 0; n--) object[pick([...WORDS, "pr", "state", "message", "kind", "terminal", "result", "head", "path"])] = garbage(depth + 1);
    return object;
  };
  const or = (good, odds = 0.25) => (rnd() < odds ? garbage() : good());
  const maybe = (object, key, make, omitted = 0.1) => {
    if (rnd() >= omitted) object[key] = make();
  };
  const STATES = ["awaiting_approval", "queued", "in_batch", "bounced"];
  const queueItem = () => {
    const o = {};
    maybe(o, "pr", () => or(() => pick([1, 2, 3, 5, 12, 99])));
    maybe(o, "title", () => or(() => pick(["t", "제목", ""])));
    maybe(o, "head", () => or(() => "abc1234"));
    maybe(o, "state", () => or(() => pick(STATES), 0.2));
    maybe(o, "state_reason", () => or(() => pick([null, "ci_pending", "ci", "conflict", "verify", "timeout", "head_changed"])), 0.3);
    maybe(o, "agent", () => or(() => ({ terminal: or(() => "t1"), name: or(() => "impl"), alive: or(() => true) })));
    maybe(o, "attempts", () => or(() => 1));
    maybe(o, "budget", () => or(() => 3));
    maybe(o, "submitted_ms", () => or(() => pick(NUMS)));
    maybe(o, "touches_config", () => or(() => rnd() < 0.5));
    maybe(o, "preflight", () => or(() => ({ head: or(() => "abc1234"), result: or(() => pick(["pass", "fail", "conflict", "skipped"])) })));
    maybe(o, "last_failure", () => or(() => ({ kind: or(() => "verify"), mode: or(() => "solo"), summary: or(() => "s"), report: or(() => "/r") })));
    maybe(o, "submitter", () => or(() => ({ terminal: or(() => pick(["t1", "t2"])), name: or(() => "orch"), alive: or(() => true) })));
    maybe(o, "priority", () => or(() => rnd() < 0.5));
    maybe(o, "repair", () => or(() => rnd() < 0.5));
    maybe(o, "after", () => or(() => Array.from({ length: Math.floor(rnd() * 3) }, () => or(() => pick([1, 2, 12])))));
    return o;
  };
  const statusView = () => {
    const o = {};
    maybe(o, "repo", () => or(() => ({ slug: or(() => "a/b"), path: or(() => "/p"), state_dir: or(() => "/s") }), 0.15));
    maybe(o, "approval", () => or(() => pick(["agent", "human"])));
    maybe(o, "daemon", () => or(() => ({ running: or(() => rnd() < 0.5) })));
    maybe(o, "blocked", () => or(() => pick([null, { reason: or(() => "base_broken"), base: or(() => "0a1"), since_ms: or(() => 1794724080000), step: or(() => "test"), log: or(() => "/l"), pushed_by_chute: or(() => rnd() < 0.5), rechecked: or(() => rnd() < 0.5) }])));
    maybe(o, "batch", () => or(() => pick([null, { prs: or(() => Array.from({ length: Math.floor(rnd() * 4) }, () => or(() => pick([1, 2, 3, 5, 12])))), phase: or(() => "verifying"), step: or(() => "test"), steps: or(() => ["fmt", "test"]), round: or(() => 1), started_ms: or(() => 1794724200000), waiting_machine: or(() => rnd() < 0.5) }])));
    maybe(o, "queue", () => or(() => Array.from({ length: Math.floor(rnd() * 6) }, () => or(queueItem, 0.1)), 0.1));
    maybe(o, "today", () => or(() => ({ merged: or(() => 1), bounced: or(() => 0), flaky: or(() => 0), batches: or(() => 1), median_lead_s: or(() => 60), verify_s: or(() => 5) })));
    maybe(o, "recent", () => or(() => Array.from({ length: Math.floor(rnd() * 5) }, () => or(() => ({ merged: or(() => [1, 2]), seconds: or(() => 300), finished_ms: or(() => 1794724000000) })))));
    maybe(o, "recent_merged", () => or(() => Array.from({ length: Math.floor(rnd() * 7) }, () => or(() => ({ pr: or(() => 5), title: or(() => "t"), bounces: or(() => 1), merged_ms: or(() => 1794724000000) })))));
    maybe(o, "post_verify", () => or(() => ({ last_green: or(() => ({ commit: or(() => "abc1234"), at_ms: or(() => 1794724000000) })), run: or(() => ({ commit: or(() => "def5678"), phase: or(() => "searching"), step: or(() => "e2e"), started_ms: or(() => 1794724100000) })) })));
    maybe(o, "warnings", () => or(() => Array.from({ length: Math.floor(rnd() * 3) }, () => or(() => ({ kind: or(() => pick(["config", "flaky_chronic", "gc"])), message: or(() => "m") })))));
    return o;
  };

  const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
  const SECTION_OF = new Map(Object.entries({ awaiting_approval: "human", queued: "waiting", bounced: "bounced", in_batch: "batch" }));
  const validPr = (pr) => Number.isSafeInteger(pr) && pr > 0;
  for (let run = 0; run < 20000; run++) {
    const text = JSON.stringify(rnd() < 0.1 ? garbage() : rnd() < 0.5 ? [statusView(), statusView()] : [statusView()]);
    const data = JSON.parse(text);
    const ctx = `input ${text.slice(0, 500)}`;
    const result = interpret({ code: 0, stdout: text, stderr: "" }, NOW);

    const CONTAINERS = new Set(["repos", "sections", "items", "actions", "args", "prs", "agent", "failure", "preflight", "hub", "marks", "submitter", "facts", "notices", "status", "today", "parts", "problems", "details", "postVerify", "green", "run", "recent", "files", "file", "report", "summary", "todo", "item", "needs", "flaky", "messages", "stepBar", "retryDots"]);
    CONTAINERS.add("daemon");
    const leaves = [];
    const walk = (value, path, key) => {
      if (value !== null && typeof value === "object") {
        assert.ok(key === "" || /^\d+$/.test(key) || CONTAINERS.has(key), `${path} holds an object where text belongs :: ${ctx}`);
        Object.entries(value).forEach(([k, v]) => walk(v, `${path}.${k}`, k));
      } else leaves.push([path, value]);
    };
    walk(result, "", "");
    for (const [path, leaf] of leaves) {
      assert.ok(leaf === null || ["string", "boolean"].includes(typeof leaf) || Number.isFinite(leaf), `${path} is ${leaf} :: ${ctx}`);
      if (typeof leaf === "string") {
        for (const bad of ["undefined", "NaN", "[object", "null", "Infinity"]) {
          assert.ok(!leaf.includes(bad) || text.includes(bad), `${path} shows ${bad} :: ${ctx}`);
        }
      }
    }
    if (result.kind !== "board") continue;

    const views = (Array.isArray(data) ? data : [data])
      .filter(isObject)
      .map((view) => [view, gradeOf(interpret({ code: 0, stdout: JSON.stringify(view) }, NOW).repos?.[0]?.status.tone)])
      .sort((a, b) => a[1] - b[1])
      .map(([view]) => view);
    assert.equal(result.repos.length, views.length, ctx);
    result.repos.forEach((repo, index) => {
      const view = views[index];
      const path = isObject(view.repo) && typeof view.repo.path === "string" ? view.repo.path : "";
      const queue = (Array.isArray(view.queue) ? view.queue : []).filter(isObject);
      const shown = Object.fromEntries(repo.sections.map((s) => [s.id, s.items]));
      const expected = { human: 0, waiting: 0, bounced: 0, unknown: 0, batch: 0 };
      for (const item of queue) expected[SECTION_OF.get(item.state) ?? "unknown"]++;
      for (const id of ["human", "waiting", "bounced", "unknown"]) {
        assert.equal(shown[id]?.length ?? 0, expected[id], `${id} count :: ${ctx}`);
      }
      assert.ok((shown.batch?.[0]?.prs.length ?? 0) >= expected.batch, `batch entries :: ${ctx}`);

      for (const [id, items] of Object.entries(shown)) {
        for (const item of items) {
          for (const action of item.actions ?? []) {
            assert.equal(id, "human", `buttons outside human :: ${ctx}`);
            const [verb, pr] = action.args;
            const flag = action.args.at(-2);
            const repoPath = action.args.at(-1);
            if (verb === "approve") assert.ok(action.args[2] === "--head" && /^[0-9a-f]{4,64}$/.test(action.args[3]) && action.args.length === 6, `approve head :: ${ctx}`);
            else assert.equal(action.args.length, 4, ctx);
            assert.ok(action.verb === verb && ["approve", "drop"].includes(verb), `verb :: ${ctx}`);
            assert.ok(/^[1-9][0-9]*$/.test(pr) && flag === "--repo" && repoPath === path && path !== "", `args ${action.args} :: ${ctx}`);
            assert.equal(pr, String(item.pr), ctx);
          }
        }
      }
      for (const card of shown.human ?? []) {
        const want = ["approve", "drop"];
        const got = card.actions.map((a) => a.verb);
        if (got.length === 1 && validPr(card.pr) && path) {
          assert.deepEqual(got, ["drop"], `awaiting without a safe head :: ${ctx}`);
          continue;
        }
        assert.deepEqual(got, validPr(card.pr) && path ? want : [], `human buttons :: ${ctx}`);
      }

      const kinds = [];
      if (view.blocked) kinds.push("blocked");
      if (isObject(view.daemon) && view.daemon.running === false && queue.length > 0) kinds.push("daemon");
      for (const _ of Array.isArray(view.warnings) ? view.warnings : []) kinds.push("warning");
      assert.deepEqual((shown.alerts ?? []).map((a) => a.kind), kinds, `alerts :: ${ctx}`);
      if (kinds.length) assert.equal(repo.sections[0].id, "alerts", ctx);
      for (const row of shown.waiting ?? []) assert.equal(row.ciPending, row.label === "Waiting on CI", ctx);
    });
  }
});

test("batch members follow submission order when the batch record lists none", () => {
  const data = clone(fixture);
  data.batch.prs = [];
  assert.deepEqual(section(only(data), "batch").items[0].prs.map((p) => p.pr), [12, 13]);
});

test("an agent without a name is shown by its terminal", () => {
  const data = withQueue({ ...itemOf(21), pr: 9, agent: { terminal: "t1rs", name: null, alive: true } }, { ...itemOf(21), agent: { terminal: "t9", name: "", alive: false } });
  const agents = Object.fromEntries(section(only(data), "human").items.map((i) => [i.pr, i.agent]));
  assert.deepEqual(agents[9], { terminal: "t1rs", name: null, alive: true, remote: false, host: null, label: "t1rs" });
  assert.deepEqual(agents[21], { terminal: "t9", name: null, alive: false, remote: false, host: null, label: "t9" });
});

test("an awaiting item without a preflight record says so in a muted tone", () => {
  const item = section(only(withQueue({ ...itemOf(21), preflight: null })), "human").items[0];
  assert.deepEqual([item.preflight.text, item.preflight.tone, item.preflight.stale], ["No preflight", "muted", false]);
});

test("a run error is cut to its first line and an empty output is not quoted", () => {
  assert.equal(interpret({ spawnError: "first\r\nsecond" }, NOW).detail, "first");
  assert.equal(interpret({ code: 0, stdout: "", stderr: "" }, NOW).detail, "chute status printed non-JSON output");
  assert.equal(interpret({ code: 2, stdout: "", stderr: "line one\r\nline two" }, NOW).detail, "line one");
});

test("numbers that overflow to infinity are not printed as numbers", () => {
  const text = '[{"today":{"merged":1e999,"bounced":2,"median_lead_s":1e999,"verify_s":5400}}]';
  const today = interpret({ code: 0, stdout: text }, NOW).repos[0].today;
  assert.deepEqual(today.parts, ["— merged", "2 bounced", "median —"]);
  assert.equal(today.details.find((d) => d.key === "verify").value, "1h 30m");
});

test("the waiting order does not depend on the input order", () => {
  const [a, b, c] = [1, 2, "x"].map((pr) => ({ ...itemOf(20), pr, submitted_ms: 5 }));
  const orders = new Set();
  for (const perm of [[a, b, c], [a, c, b], [b, a, c], [b, c, a], [c, a, b], [c, b, a]]) {
    orders.add(JSON.stringify(prs(section(only(queueOnly(...perm)), "waiting"))));
  }
  assert.deepEqual([...orders], ['[1,2,"?"]']);
});

const agentOfPr = (repo, pr) => {
  const cards = repo.sections.flatMap((s) => s.items).filter((item) => item.pr === pr);
  return cards[0]?.agent;
};

test("the repo card names the hub host, or this PC when the hub is local", () => {
  assert.equal(only(fixture).hub.label, "desk-host");
  const local = clone(fixture);
  local.hub = { ...local.hub, local: true, host: "this-host", machine: null };
  assert.equal(only(local).hub.label, "this PC");
  const v1 = clone(fixture);
  delete v1.hub;
  assert.deepEqual(
    { local: only(v1).hub.local, reachable: only(v1).hub.reachable, label: only(v1).hub.label },
    { local: true, reachable: true, label: "this PC" },
  );
});

test("an unreachable hub shows its reason instead of queue, batch and buttons", () => {
  const repo = only(unreachable);
  assert.equal(repo.hub.reachable, false);
  assert.equal(repo.hub.label, "desk-host");
  assert.equal(repo.hub.error, unreachable.hub.error);
  assert.deepEqual(repo.sections, []);

  const stale = clone(fixture);
  stale.hub = clone(unreachable.hub);
  const hidden = only(stale);
  assert.deepEqual(hidden.sections, []);
  assert.equal(hidden.approval, null);
});

test("an agent on another machine shows its host and can be peeked", () => {
  const repo = only(fixture);
  const remote = agentOfPr(repo, 14);
  assert.equal(remote.remote, true);
  assert.equal(remote.host, "desk-host");
  assert.equal(remote.terminal, "desk/t1ru");
  assert.equal(remote.alive, true);

  const dead = agentOfPr(repo, 10);
  assert.equal(dead.host, "desk-host");
  assert.equal(dead.alive, false);
});

test("an agent on this PC shows no host", () => {
  const repo = only(fixture);
  for (const pr of [9, 15, 17, 21]) {
    const agent = agentOfPr(repo, pr);
    assert.equal(agent.remote, false, `#${pr}`);
    assert.equal(agent.host, null, `#${pr}`);
  }
  assert.equal(agentOfPr(repo, 11), null);
});

test("a remote agent without a host falls back to its machine name", () => {
  const data = queueOnly(itemOf(14));
  data.queue[0].agent.host = null;
  assert.equal(agentOfPr(only(data), 14).host, "desk");
});

test("polling is fast when any hub is remote and the v1 pace when all are local", () => {
  assert.equal(pollInterval(board([fixture])), REMOTE_POLL_MS);
  assert.equal(pollInterval(board([unreachable])), REMOTE_POLL_MS);
  const local = clone(fixture);
  local.hub = { ...local.hub, local: true, machine: null };
  assert.equal(pollInterval(board([local])), LOCAL_POLL_MS);
  assert.equal(pollInterval(board([local, fixture])), REMOTE_POLL_MS);
  const v1 = clone(fixture);
  delete v1.hub;
  assert.equal(pollInterval(board([v1])), LOCAL_POLL_MS);
  assert.equal(pollInterval(null), LOCAL_POLL_MS);
  assert.equal(pollInterval({ kind: "failed" }), LOCAL_POLL_MS);
  assert.ok(REMOTE_POLL_MS < LOCAL_POLL_MS);
});

const marksOfItem = (item) => item.marks.map((m) => m.text);
const waitingItem = (data, pr) => section(only(data), "waiting").items.find((item) => item.pr === pr);

test("v3: a priority item is marked urgent, with or without an agent", () => {
  const data = withQueue(itemOf(20), itemOf(23));
  assert.deepEqual(marksOfItem(waitingItem(data, 20)), ["Urgent"]);
  assert.deepEqual(marksOfItem(waitingItem(data, 23)), ["Urgent"]);
  assert.deepEqual(marksOfItem(waitingItem(withQueue(itemOf(19)), 19)), []);
});

test("v3: an item held behind other PRs lists every one of them", () => {
  const data = withQueue(itemOf(24), itemOf(12));
  assert.deepEqual(marksOfItem(waitingItem(data, 24)), ["after #12, #22"]);
  const one = itemOf(24);
  one.after = [7];
  assert.deepEqual(marksOfItem(waitingItem(withQueue(one), 24)), ["after #7"]);
});

test("v3: a repair item is marked, and marks combine", () => {
  assert.deepEqual(marksOfItem(waitingItem(withQueue(itemOf(22)), 22)), ["Repair"]);
  const both = itemOf(22);
  both.priority = true;
  both.after = [3];
  assert.deepEqual(marksOfItem(waitingItem(withQueue(both), 22)), ["Urgent", "Repair", "after #3"]);
});

test("v3: cards in the human and bounced sections carry the marks too", () => {
  const human = itemOf(21);
  human.priority = true;
  human.after = [5, 6];
  const bounced = itemOf(14);
  bounced.repair = true;
  const repo = only(withQueue(human, bounced));
  assert.deepEqual(marksOfItem(section(repo, "human").items[0]), ["Urgent", "after #5, #6"]);
  assert.deepEqual(marksOfItem(section(repo, "bounced").items[0]), ["Repair"]);
});

test("v3: the submitter is shown only when it differs from the agent", () => {
  assert.equal(waitingItem(withQueue(itemOf(22)), 22).submitter.text, "submit orch (t1s0)");
  assert.equal(waitingItem(withQueue(itemOf(24)), 24).submitter.label, "orch (t1s0)");
  assert.equal(waitingItem(withQueue(itemOf(23)), 23).submitter, null, "same terminal as the agent");
  assert.equal(waitingItem(withQueue(itemOf(20)), 20).submitter, null, "no submitter");
  const bounced = itemOf(14);
  bounced.submitter = itemOf(22).submitter;
  assert.equal(section(only(withQueue(bounced)), "bounced").items[0].submitter.text, "submit orch (t1s0)");
  const nameless = itemOf(22);
  nameless.submitter = { terminal: "desk/t9", alive: true };
  assert.equal(waitingItem(withQueue(nameless), 22).submitter.text, "submit desk/t9");
  const noAgent = itemOf(20);
  noAgent.submitter = itemOf(22).submitter;
  assert.equal(waitingItem(withQueue(noAgent), 20).submitter.label, "orch (t1s0)");
});

test("v3: a halt shows step, who pushed, the recheck, the log, and the repair PR waiting", () => {
  const [alert] = section(only([fixture]), "alerts").items;
  assert.deepEqual(alert.facts, [
    "Failed step test",
    "Commit pushed by chute",
    "Rechecked",
    "Repair PR #22 queued",
  ]);
  assert.equal(alert.log, fixture.blocked.log);
  assert.equal(alert.detail, "base 0a1b2c3 · for 7m 12s");
});

test("v3: a halt not pushed by chute and not rechecked says so, and a repair in a batch is being verified", () => {
  const data = clone(fixture);
  data.blocked.pushed_by_chute = false;
  data.blocked.rechecked = false;
  data.blocked.log = null;
  data.queue.find((item) => item.pr === 22).state = "in_batch";
  const [alert] = section(only(data), "alerts").items;
  assert.deepEqual(alert.facts, ["Failed step test", "Commit not pushed by chute", "Repair PR #22 verifying"]);
  assert.equal(alert.log, null);
});

test("v3: a repair item that is no longer queued is not called waiting for verification", () => {
  const data = clone(fixture);
  data.queue.find((item) => item.pr === 22).state = "bounced";
  const [alert] = section(only(data), "alerts").items;
  assert.ok(alert.facts.includes("Repair PR #22 needs attention"));
});

test("v3: no repair item means no repair line", () => {
  const data = clone(fixture);
  data.queue = data.queue.filter((item) => !item.repair);
  const [alert] = section(only(data), "alerts").items;
  assert.ok(!alert.facts.some((fact) => fact.includes("Repair")));
});

test("v3: a repair item is not announced when the queue is not halted", () => {
  const data = { ...clone(fixture), blocked: null, post_verify: {} };
  assert.ok(section(only(data), "alerts").items.every((a) => a.facts.length === 0));
});

test("v3: flaky_chronic and gc warnings come out like other warnings; chronic also marks the flaky number", () => {
  const data = clone(fixture);
  data.warnings.push(
    { kind: "flaky_chronic", message: "build::slow가 30일 안에 3번 흔들렸다", pr: null, at_ms: 1 },
    { kind: "gc", message: "정리하지 못한 폴더가 있다", pr: null, at_ms: 2 },
  );
  const repo = only(data);
  const warnings = section(repo, "alerts").items.filter((a) => a.kind === "warning");
  assert.deepEqual(warnings.slice(-2).map((a) => [a.tone, a.text, a.detail]), [
    ["warn", "Chronic flaky test", "build::slow가 30일 안에 3번 흔들렸다"],
    ["warn", "Cleanup incomplete", "정리하지 못한 폴더가 있다"],
  ]);
  const stats = repo.today.details;
  assert.equal(stats.find((s) => s.key === "flaky").note, "chronic");
  assert.ok(stats.filter((s) => s.key !== "flaky").every((s) => !s.note));
  assert.equal(only([fixture]).today.details.find((s) => s.key === "flaky").note, null);
});

test("v3: a gc warning alone does not mark the flaky number", () => {
  const data = { ...clone(fixture), warnings: [{ kind: "gc", message: "정리 경고" }] };
  assert.equal(only(data).today.details.find((s) => s.key === "flaky").note, null);
});

test("v3: JSON without any v3 field draws as before", () => {
  const data = clone(fixture);
  for (const item of data.queue) {
    delete item.submitter;
    delete item.priority;
    delete item.after;
    delete item.repair;
  }
  data.blocked = { reason: "base_broken", base: "0a1b2c3", since_ms: fixture.blocked.since_ms };
  const repo = only(data);
  assert.deepEqual(prs(section(repo, "waiting")), [19, 20, 22, 23, 24]);
  for (const id of ["human", "waiting", "bounced"]) {
    for (const item of section(repo, id).items) {
      assert.deepEqual(item.marks, [], `${id} #${item.pr}`);
      assert.equal(item.submitter, null);
    }
  }
  const [alert] = section(repo, "alerts").items;
  assert.deepEqual(alert.facts, []);
  assert.equal(alert.log, null);
  assert.equal(alert.detail, "base 0a1b2c3 · for 7m 12s");
});

test("v3: malformed v3 fields do not break the board", () => {
  const item = itemOf(24);
  item.after = [0, -1, "x", null, 1.5, 9];
  item.priority = "yes";
  item.repair = 1;
  item.submitter = "orch";
  const queued = waitingItem(withQueue(item), 24);
  assert.deepEqual(marksOfItem(queued), ["after #9"]);
  assert.equal(queued.submitter, null);
  item.after = "12";
  assert.deepEqual(marksOfItem(waitingItem(withQueue(item), 24)), []);
  const data = clone(fixture);
  data.blocked = { reason: "base_broken", step: 5, pushed_by_chute: "yes", rechecked: "yes", log: 7 };
  const [alert] = section(only(data), "alerts").items;
  assert.equal(alert.log, null);
  assert.deepEqual(alert.facts, ["Repair PR #22 queued"]);
});

test("v3: members of a running batch keep their marks and submitter", () => {
  const repair = itemOf(22);
  repair.state = "in_batch";
  const data = { ...clone(fixture), queue: [repair], batch: { ...clone(fixture.batch), prs: [22] } };
  const [entry] = section(only(data), "batch").items[0].prs;
  assert.deepEqual(entry.marks.map((m) => m.text), ["Repair"]);
  assert.equal(entry.submitter.text, "submit orch (t1s0)");
});

test("v3: priority is a warning color and repair an error color", () => {
  const both = itemOf(22);
  both.priority = true;
  const { marks } = waitingItem(withQueue(both), 22);
  assert.deepEqual(marks.map((m) => [m.text, m.tone]), [["Urgent", "warn"], ["Repair", "error"]]);
});

test("v3: a PR listed twice in after is shown once", () => {
  const item = itemOf(24);
  item.after = [12, 12, 22];
  assert.deepEqual(marksOfItem(waitingItem(withQueue(item), 24)), ["after #12, #22"]);
});

test("a failed check after the merge is an alert with the culprit, the last green commit and the revert hint", () => {
  const alert = section(only([fixture]), "alerts").items.find((a) => a.kind === "post_verify");
  assert.equal(alert.tone, "error");
  assert.equal(alert.text, "Post-merge check failed · e2e");
  assert.match(alert.detail, /^9f8e7d6 · /);
  assert.deepEqual(alert.facts, [
    "Culprit #12",
    "Failed tests login flow",
    "Last green 1e2d3c4",
    "To undo: chute revert b20261117-091500",
  ]);
  assert.match(alert.log, /e2e\.1\.log$/);
  assert.deepEqual(alert.files.map((f) => f.label), ["Report", "Log"]);

  const data = clone(fixture);
  data.post_verify.red.culprit = null;
  data.post_verify.red.last_green = null;
  data.post_verify.red.holding = true;
  const lost = section(only(data), "alerts").items.find((a) => a.kind === "post_verify");
  assert.deepEqual(lost.facts, ["Culprit merge not found", "Failed tests login flow", "Related PRs wait until it's fixed"]);
});

test("the culprit's title comes from the recently merged list when it has one", () => {
  const data = clone(fixture);
  data.recent_merged.push({ pr: 12, title: "feat: 큐 상태 전이", batch: "b", merged_ms: 1, bounces: 0 });
  const alert = section(only(data), "alerts").items.find((a) => a.kind === "post_verify");
  assert.equal(alert.facts[0], "Culprit #12 feat: 큐 상태 전이");
});

test("the check after the merge: last green and the running check are two rows, none when both are missing", () => {
  const post = (data) => only(data).postVerify;
  const now = fixture.post_verify.run.started_ms + 190_000;
  const at = (data) => interpret(ok(data), now).repos[0].postVerify;
  assert.equal(at(fixture).green.commit, "1e2d3c4");
  assert.match(at(fixture).green.ago, /ago$/);
  assert.deepEqual(
    [at(fixture).run.commit, at(fixture).run.label, at(fixture).run.step, at(fixture).run.elapsed],
    ["5a4b3c2", "Finding culprit", "e2e", "3m 10s"],
  );
  const data = clone(fixture);
  data.post_verify.run.phase = "verifying";
  data.post_verify.run.waiting_machine = true;
  assert.equal(at(data).run.label, "Checking");
  assert.equal(at(data).run.waitingMachine, true);
  data.post_verify.run = null;
  assert.equal(post(data).run, null);
  data.post_verify.last_green = null;
  assert.equal(post(data), null);
  delete data.post_verify;
  assert.equal(post(data), null);
});

const statusOf = (data, now) => interpret(ok(data), now ?? NOW).repos[0].status;
const quiet = () => ({ ...clone(fixture), blocked: null, post_verify: {}, batch: null, queue: [], warnings: [], daemon: { running: true } });

test("the status line takes the first matching row of the table, and only one", () => {
  const data = clone(fixture);
  assert.deepEqual(statusOf(data), { tone: "error", text: "Stopped · base failed verification" });
  data.blocked = null;
  assert.deepEqual(statusOf(data), { tone: "error", text: "Post-merge check failed · e2e" });
  data.post_verify.red = null;
  data.daemon.running = false;
  assert.deepEqual(statusOf(data), { tone: "error", text: "Daemon is off" });
  data.daemon.running = true;
  assert.deepEqual(statusOf(data), { tone: "human", text: "Needs you · 4" });
  data.queue = data.queue.filter((item) => item.state !== "awaiting_approval" && item.state !== "bounced");
  assert.deepEqual(statusOf(data), { tone: "working", text: "Verifying · test 3/3" });
  data.batch.step = "fmt";
  assert.equal(statusOf(data).text, "Verifying · fmt 1/3");
  data.batch.waiting_machine = true;
  assert.deepEqual(statusOf(data), { tone: "working", text: "Waiting for a slot" });
  data.batch = null;
  data.queue = data.queue.filter((item) => item.state === "queued");
  assert.deepEqual(statusOf(data), { tone: "waiting", text: `Queued · ${data.queue.length}` });
  assert.deepEqual(statusOf(quiet()), { tone: "ok", text: "All clear" });
});

test("an unreachable hub is the first row of the status line", () => {
  const repo = interpret(ok(unreachable), NOW).repos[0];
  assert.deepEqual(repo.status, { tone: "error", text: "Hub unreachable" });
});

test("a batch without a step list shows the step name only, and one without a step says just that it verifies", () => {
  const data = quiet();
  data.batch = { prs: [12], phase: "verifying", step: "test", started_ms: NOW - 1000 };
  assert.equal(statusOf(data).text, "Verifying · test");
  data.batch.steps = [];
  assert.equal(statusOf(data).text, "Verifying · test");
  data.batch.steps = ["fmt", "clippy"];
  assert.equal(statusOf(data).text, "Verifying · test");
  data.batch.step = null;
  assert.equal(statusOf(data).text, "Verifying");
});

test("the usual time is the median of recent seconds, and absent below three samples", () => {
  assert.equal(usualSeconds([{ seconds: 100 }, { seconds: 300 }, { seconds: 200 }]), 200);
  assert.equal(usualSeconds([{ seconds: 100 }, { seconds: 300 }, { seconds: 200 }, { seconds: 400 }]), 250);
  assert.equal(usualSeconds([{ seconds: 100 }, { seconds: 300 }]), null);
  assert.equal(usualSeconds([{ seconds: 100 }, { seconds: null }, { seconds: "x" }, { seconds: 300 }]), null);
  assert.equal(usualSeconds(undefined), null);
});

test("the usual time leaves out batches under a minute", () => {
  const of = (...seconds) => usualSeconds(seconds.map((value) => ({ seconds: value })));
  assert.equal(of(20, 22, 25, 250, 283, 300), 283);
  assert.equal(of(20, 30, 250, 300), null);
  assert.equal(of(59, 60, 61, 62), 61);
  assert.equal(MIN_USUAL_SECONDS, 60);
});

test("the running batch shows its step n/m, elapsed time, and a progress bar only when the usual time is known", () => {
  const data = quiet();
  data.batch = clone(fixture.batch);
  const batch = (now) => interpret(ok(data), now).repos[0].sections.find((s) => s.id === "batch").items[0];
  assert.equal(batch(NOW).stepText, "test 3/3");
  assert.equal(batch(NOW).elapsed, "5m 12s");
  assert.equal(batch(NOW).progress, null);
  assert.equal(batch(NOW).usual, null);
  data.recent = [{ seconds: 200 }, { seconds: 600 }, { seconds: 400 }];
  assert.equal(batch(NOW).usual, "usually 6m 40s");
  assert.equal(Math.round(batch(NOW).progress * 100), 78);
  assert.equal(batch(NOW).longer, false);
  const late = batch(fixture.batch.started_ms + 500_000);
  assert.equal(late.progress, 1);
  assert.equal(late.longer, true);
  delete data.batch.steps;
  assert.equal(batch(NOW).stepText, "test");
});

const stepBarOf = (batch) => {
  const data = quiet();
  data.batch = { prs: [12], started_ms: NOW - 1000, ...batch };
  return interpret(ok(data), NOW).repos[0].sections.find((s) => s.id === "batch").items[0];
};
const states = (batch) => stepBarOf(batch).stepBar.map((s) => s.state);

test("the step bar marks done, current and later steps from the step list", () => {
  const steps = ["fmt", "clippy", "test"];
  for (const phase of ["verifying", "bisecting"]) {
    const bar = stepBarOf({ phase, steps, step: "clippy" }).stepBar;
    assert.deepEqual(bar, [
      { name: "fmt", state: "done" },
      { name: "clippy", state: "current" },
      { name: "test", state: "todo" },
    ]);
  }
  assert.deepEqual(states({ phase: "verifying", steps, step: "other" }), ["todo", "todo", "todo"]);
  assert.deepEqual(states({ phase: "verifying", steps }), ["todo", "todo", "todo"]);
  assert.deepEqual(states({ steps, step: "test" }), ["done", "done", "current"]);
  assert.deepEqual(states({ phase: "nonsense", steps, step: "fmt" }), ["current", "todo", "todo"]);
  const odd = stepBarOf({ phase: "verifying", steps: ["fmt", 1, null, "", "test"], step: "test" }).stepBar;
  assert.deepEqual(odd.map((s) => s.name), ["fmt", "test"]);
  assert.deepEqual(odd.map((s) => s.state), ["done", "current"]);
});

test("the step bar is all done while pushing and all later while building", () => {
  const steps = ["fmt", "clippy", "test"];
  assert.deepEqual(states({ phase: "pushing", steps, step: "clippy" }), ["done", "done", "done"]);
  assert.deepEqual(states({ phase: "building", steps, step: "clippy" }), ["todo", "todo", "todo"]);
});

test("a batch without a step list has no step bar", () => {
  assert.deepEqual(stepBarOf({ phase: "verifying", step: "test" }).stepBar, []);
  assert.deepEqual(stepBarOf({ phase: "verifying", steps: [], step: "test" }).stepBar, []);
  assert.equal(stepBarOf({ phase: "verifying", step: "test" }).stepText, "test");
  assert.equal(stepBarOf({ phase: "verifying", steps: [], step: "test" }).stepText, "test");
  assert.equal(stepBarOf({ phase: "verifying" }).stepText, null);
});

test("the phase line joins the phase, the round and the reused pass", () => {
  const data = quiet();
  data.batch = clone(fixture.batch);
  const line = () => interpret(ok(data), NOW).repos[0].sections.find((s) => s.id === "batch").items[0].phaseLine;
  assert.equal(line(), "Verifying · Round 1 · Reused preflight pass");
  delete data.batch.phase;
  assert.equal(line(), "Verifying · Round 1 · Reused preflight pass");
  delete data.batch.round;
  delete data.batch.reused;
  assert.equal(line(), "Verifying");
  data.batch.phase = "pushing";
  assert.equal(line(), "Pushing");
});

test("a batch without started_ms shows no usual time, elapsed time or progress even when the usual time is known", () => {
  const data = quiet();
  data.batch = clone(fixture.batch);
  data.recent = [{ seconds: 200 }, { seconds: 600 }, { seconds: 400 }];
  const batch = () => interpret(ok(data), NOW).repos[0].sections.find((s) => s.id === "batch").items[0];
  const started = data.batch.started_ms;
  delete data.batch.started_ms;
  const blind = batch();
  assert.deepEqual([blind.usual, blind.elapsed, blind.progress, blind.longer], [null, null, null, false]);
  data.batch.started_ms = started;
  assert.equal(batch().usual, "usually 6m 40s");
});

test("recent merges: five from recent_merged, with titles, bounce counts and a GitHub address", () => {
  const data = clone(fixture);
  data.recent_merged = Array.from({ length: 7 }, (_, i) => ({ pr: 20 - i, title: `제목 ${i}`, batch: "b", merged_ms: NOW - 60_000 * (i + 1), bounces: i === 1 ? 2 : 0 }));
  const recent = only(data).recent;
  assert.equal(recent.length, 5);
  assert.deepEqual({ ...recent[1], tip: undefined }, { pr: 19, title: "제목 1", bounces: 2, ago: "2m ago", took: "", tip: undefined, url: "https://github.com/Soochol/chute/pull/19" });
  assert.deepEqual(only([fixture]).recent.map((m) => [m.pr, m.title, m.bounces]), [
    [8, "feat: 큐 상태 전이 추가", 1],
    [7, "fix: 잠금 해제 순서를 고친다", 0],
    [6, "", 0],
  ]);
});

test("an old chute without recent_merged gives the numbers of recent[].merged, without titles", () => {
  const data = clone(fixture);
  delete data.recent_merged;
  assert.deepEqual(only(data).recent.map((m) => [m.pr, m.title, m.bounces]), [[7, "", 0], [8, "", 0], [5, "", 0], [6, "", 0]]);
  delete data.recent;
  assert.deepEqual(only(data).recent, []);
});

test("the GitHub address comes only out of a slug shaped owner/name", () => {
  assert.equal(pullUrl("a/b", 3), "https://github.com/a/b/pull/3");
  assert.equal(pullUrl("Soochol/chute.rs", 3), "https://github.com/Soochol/chute.rs/pull/3");
  for (const slug of ["", "ab", "a/b/c", "a/", "/b", "a b/c", "a/b?x=1", "../b", "a/..", "(unnamed repo)", null, 5]) {
    assert.equal(pullUrl(slug, 3), null, String(slug));
  }
  assert.equal(pullUrl("a/b", 0), null);
  assert.equal(pullUrl("a/b", "?"), null);
  assert.equal(only({ ...clone(fixture), repo: { slug: "nope", path: "/p" } }).recent[0].url, null);
});

test("formatAgo", () => {
  assert.deepEqual([0, 59_000, 60_000, 3_599_000, 3_600_000, 86_400_000 * 3].map(formatAgo), ["just now", "just now", "1m ago", "59m ago", "1h ago", "3d ago"]);
  assert.equal(formatAgo(-1), "");
  assert.equal(formatAgo(NaN), "");
});

test("the repo shows its name only, and keeps the full slug for the tooltip", () => {
  const repo = only([fixture]);
  assert.equal(repo.name, "chute");
  assert.equal(repo.slug, "Soochol/chute");
  assert.equal(repo.hub.local, false);
});

const noticeKeys = (data) => interpret(ok(data), NOW).repos[0].notices.map((n) => n.key);

test("notices: a halt, a failed check after the merge and approvals waiting", () => {
  const keys = noticeKeys(fixture);
  assert.ok(keys.some((k) => k.startsWith("blocked:")));
  assert.ok(keys.includes("red:9f8e7d6c5b4a39281706f5e4d3c2b1a099887766"));
  assert.ok(keys.some((k) => k.startsWith("approval:21:")));
  assert.deepEqual(keys.filter((k) => k.startsWith("human:")), []);
  assert.deepEqual(noticeKeys(quiet()), []);
  const notice = interpret(ok(fixture), NOW).repos[0].notices;
  assert.equal(notice.find((n) => n.key.startsWith("blocked:")).level, "error");
});

test("diffNotices: the first read tells nothing, later only new items, and a gone-then-back item tells again", () => {
  const read = (data) => interpret(ok(data), NOW);
  const first = diffNotices(null, read(fixture));
  assert.deepEqual(first.fresh, []);
  const same = diffNotices(first.next, read(fixture));
  assert.deepEqual(same.fresh, []);

  const calm = diffNotices(same.next, read(quiet()));
  assert.deepEqual(calm.fresh, []);
  const back = diffNotices(calm.next, read(fixture));
  assert.equal(back.fresh.length, 3);
  assert.ok(back.fresh.every((n) => n.repo.name === "chute"));

  const grown = clone(quiet());
  const before = diffNotices(null, read(grown));
  grown.queue = [itemOf(21)];
  const after = diffNotices(before.next, read(grown));
  assert.deepEqual(after.fresh.map((n) => n.key.split(":")[0]), ["approval"]);
  assert.equal(after.fresh[0].level, "warning");
  assert.match(after.fresh[0].body, /^#21 awaiting approval/);
});

test("diffNotices: an unreachable hub keeps what was known, a failed read changes nothing", () => {
  const read = (data) => interpret(ok(data), NOW);
  const known = diffNotices(null, read(fixture)).next;
  const down = diffNotices(known, interpret(ok(unreachable), NOW));
  assert.deepEqual(down.fresh, []);
  const broken = diffNotices(known, interpret({ code: 1, stdout: "", stderr: "x" }, NOW));
  assert.equal(broken.next, known);
  assert.deepEqual(broken.fresh, []);
});

test("diffNotices: a repo first seen after the start tells nothing about what it already has", () => {
  const other = { ...clone(fixture), repo: { slug: "a/b", path: "/other", state_dir: "/s" } };
  const first = diffNotices(null, interpret(ok([quiet()]), NOW));
  const second = diffNotices(first.next, interpret(ok([quiet(), other]), NOW));
  assert.deepEqual(second.fresh, []);
});

test("the badge counts what needs a hand, and is empty at zero", () => {
  assert.equal(badgeText(interpret(ok(fixture), NOW)), String(interpret(ok(fixture), NOW).repos[0].notices.length));
  assert.equal(badgeText(interpret(ok(quiet()), NOW)), null);
  assert.equal(badgeText({ kind: "failed" }), null);
});

test("the clock runs only while a batch or the check after the merge is running", () => {
  assert.equal(needsClock(interpret(ok(fixture), NOW)), true);
  assert.equal(needsClock(interpret(ok(quiet()), NOW)), false);
  const checking = { ...quiet(), post_verify: { run: { commit: "abc1234", phase: "verifying", step: "e2e", started_ms: NOW } } };
  assert.equal(needsClock(interpret(ok(checking), NOW)), true);
  assert.equal(needsClock({ kind: "failed" }), false);
});

test("bounced cards read as one line, and carry the report to open", () => {
  const card = section(only([fixture]), "bounced").items.find((c) => c.pr === 15);
  assert.equal(card.headline, "Verification failed · Solo");
  assert.deepEqual(card.report, { label: "Report", path: fixture.queue.find((i) => i.pr === 15).last_failure.report });
});

test("tones map to theme tokens, and ok is green rather than done", () => {
  assert.deepEqual(
    ["ok", "error", "working", "human", "warn", "waiting"].map(toneToken),
    ["ansi2", "ansi1", "working", "blocked", "ansi3", "muted"],
  );
  for (const tone of ["ok", "error", "working", "human", "warn", "waiting", "muted", "toString", "", undefined, null]) {
    assert.notEqual(toneToken(tone), "done", String(tone));
  }
  assert.equal(toneToken("muted"), "dim");
  assert.equal(toneToken("constructor"), "dim");
});

test("every warning kind gets an English title, an unknown kind is just Warning, and the message stays apart", () => {
  const kinds = ["not_merged", "no_checks", "config", "history", "engine", "hub", "flaky_chronic", "gc", "post"];
  const data = { ...clone(fixture), blocked: null, post_verify: {}, warnings: [...kinds.map((kind) => ({ kind, message: `본체 ${kind}` })), { kind: "brand_new", message: "새 경고" }] };
  const warnings = section(only(data), "alerts").items.filter((a) => a.kind === "warning");
  assert.equal(warnings.length, kinds.length + 1);
  for (const [i, kind] of kinds.entries()) {
    assert.match(warnings[i].text, /^[A-Za-z' -]+$/, kind);
    assert.notEqual(warnings[i].text, "Warning", kind);
    assert.equal(warnings[i].detail, `본체 ${kind}`, kind);
  }
  assert.equal(warnings.at(-1).text, "Warning");
  assert.equal(warnings.at(-1).detail, "새 경고");
});

test("a recent merge without a title keeps its number and has an empty title, with no placeholder", () => {
  const data = clone(fixture);
  data.recent_merged = [{ pr: 421, batch: "b", merged_ms: NOW - 60_000, bounces: 0 }];
  assert.deepEqual(only(data).recent.map((m) => [m.pr, m.title]), [[421, ""]]);
  delete data.recent_merged;
  data.recent = [{ merged: [5], finished_ms: NOW - 60_000, seconds: 10 }];
  assert.deepEqual(only(data).recent.map((m) => [m.pr, m.title]), [[5, ""]]);
});

test("no Hangul is left in the English wording of formatted times", () => {
  assert.equal(formatDuration(3900), "1h 5m");
  assert.equal(formatDuration(45), "45s");
  assert.equal(formatDuration(252), "4m 12s");
});

const tonedRepo = (slug, mutate = () => {}) => {
  const data = queueOnly();
  data.repo = { ...data.repo, slug, path: `/r/${slug}` };
  mutate(data);
  return data;
};
const idle = (slug) => tonedRepo(slug);
const verifying = (slug) => tonedRepo(slug, (d) => { d.batch = clone(fixture.batch); });
const queuedOnly = (slug) => tonedRepo(slug, (d) => { d.queue = [{ ...itemOf(fixture.queue.find((i) => i.state === "queued").pr) }]; });
const humanTurn = (slug) => tonedRepo(slug, (d) => { d.queue = [itemOf(fixture.queue.find((i) => i.state === "awaiting_approval").pr)]; });
const stopped = (slug) => tonedRepo(slug, (d) => { d.blocked = { reason: "base_broken", since_ms: 1 }; });

test("repos are sorted by grade, and keep the status order within a grade", () => {
  const result = board([idle("o/ok1"), verifying("o/work"), stopped("o/bad1"), idle("o/ok2"), humanTurn("o/human"), queuedOnly("o/wait"), stopped("o/bad2")]);
  assert.deepEqual(result.repos.map((r) => r.slug), ["o/bad1", "o/human", "o/bad2", "o/work", "o/wait", "o/ok1", "o/ok2"]);
  assert.deepEqual(["error", "human", "working", "waiting", "ok"].map(gradeOf), [0, 0, 1, 2, 3]);
});

test("overview: grade is the most urgent repo, waiting alone counts as running", () => {
  const grade = (...views) => overview(board(views)).grade;
  assert.equal(grade(idle("o/a"), queuedOnly("o/b")), "running");
  assert.equal(grade(idle("o/a")), "ok");
  assert.equal(grade(idle("o/a"), humanTurn("o/b"), verifying("o/c")), "attention");
  assert.equal(grade(humanTurn("o/b"), stopped("o/c")), "failed");
  assert.equal(overview(board([verifying("o/a"), queuedOnly("o/b"), idle("o/c")])).running, 2);
  assert.equal(overview(board([verifying("o/a"), idle("o/c")])).repos, 2);
});

test("overview: a failed read or a missing chute is one failed issue, an empty list is ok", () => {
  const failed = { grade: "failed", issues: 1, running: 0, repos: 0 };
  assert.deepEqual(overview(interpret({ code: 1, stdout: "", stderr: "boom" }, NOW)), failed);
  assert.deepEqual(overview(interpret({ spawnError: "ENOENT" }, NOW)), failed);
  assert.deepEqual(overview(board([])), { grade: "ok", issues: 0, running: 0, repos: 0 });
  assert.equal(overview(null), null);
});

test("overview: issues are human turns, error alerts and unreachable hubs; warn alerts do not count", () => {
  assert.equal(overview(board([humanTurn("o/a")])).issues, 1);
  assert.equal(overview(board([stopped("o/a")])).issues, 1);
  const warned = tonedRepo("o/w", (d) => { d.warnings = [{ kind: "config", message: "x" }]; });
  assert.equal(overview(board([warned])).issues, 0);
  assert.equal(overview(board([warned])).grade, "ok");
  const daemonOff = tonedRepo("o/d", (d) => { d.daemon = { running: false }; d.queue = [itemOf(fixture.queue.find((i) => i.state === "queued").pr)]; });
  assert.equal(overview(board([daemonOff])).issues, 1);
  assert.equal(overview(board([tonedRepo("o/i", (d) => { d.daemon = { running: false }; })])).issues, 0);
  const hub = overview(interpret(ok(unreachable), NOW));
  assert.equal(hub.grade, "failed");
  assert.ok(hub.issues >= 1);
  const two = overview(board([humanTurn("o/a"), stopped("o/b"), idle("o/c")]));
  assert.equal(two.issues, 2);
});

test("overview text uses singular and plural", () => {
  assert.deepEqual(overviewText({ repos: 1, running: 1, issues: 1 }), ["1 repo", "1 running", "1 to do"]);
  assert.deepEqual(overviewText({ repos: 2, running: 0, issues: 0 }), ["2 repos", "0 running", "0 to do"]);
});

test("active: a batch or a running post-merge check is active, otherwise not", () => {
  assert.equal(only(idle("o/a")).active, false);
  assert.equal(only(verifying("o/a")).active, true);
  const post = tonedRepo("o/p", (d) => { d.post_verify = { run: { commit: "abcdef1234", started_ms: 1, phase: "checking" } }; });
  assert.equal(only(post).active, true);
  assert.equal(needsClock(board([post])), true);
  assert.equal(needsClock(board([idle("o/a")])), false);
  assert.equal(overview(board([post])).grade, "ok");
  assert.equal(interpret(ok(unreachable), NOW).repos.some((r) => r.active), false);
});

test("recent merges show three by default and say how many are hidden", () => {
  const recent = [1, 2, 3, 4, 5].map((n) => ({ pr: n }));
  assert.equal(RECENT_DEFAULT, 3);
  assert.deepEqual(recentWindow(recent, false).rows.map((r) => r.pr), [1, 2, 3]);
  assert.equal(recentWindow(recent, false).more, 2);
  assert.equal(recentWindow(recent, true).rows.length, 5);
  assert.equal(recentWindow(recent, true).more, 0);
  assert.equal(recentWindow(recent.slice(0, 3), false).more, 0);
  assert.equal(recentWindow(recent.slice(0, 2), false).rows.length, 2);
});

test("queue rows carry the PR URL, and none without a slug", () => {
  const repo = only(fixture ? [fixture] : []);
  const urls = repo.sections.filter((s) => s.id !== "alerts" && s.id !== "unknown" && s.id !== "batch").flatMap((s) => s.items.map((i) => i.url));
  assert.ok(urls.length > 0);
  for (const url of urls) assert.match(url, /^https:\/\/github\.com\/.+\/pull\/\d+$/);
  const noSlug = clone(fixture);
  delete noSlug.repo.slug;
  const bare = only([noSlug]);
  for (const s of bare.sections.filter((x) => x.id !== "alerts" && x.id !== "batch")) for (const item of s.items) assert.equal(item.url, null);
  for (const entry of section(bare, "batch").items[0].prs) assert.equal(entry.url, null);
});

const folds = (...steps) => {
  let state;
  const out = [];
  for (const [keys, saved] of steps) {
    state = nextFold(state, keys, saved);
    out.push(state);
  }
  return out;
};

test("fold: a folded repo opens for a new problem key, stays open while the same key lasts, folds when all are gone", () => {
  const [calm, trouble, same, other, cleared] = folds([[], true], [["a"], true], [["a"], true], [["a", "b"], true], [[], true]);
  assert.equal(calm.folded, true);
  assert.equal(trouble.folded, false);
  assert.equal(same.folded, false);
  assert.equal(other.folded, false);
  assert.equal(cleared.folded, true);
  assert.equal(cleared.auto, false);
});

test("fold: a repo with a problem on the first read opens", () => {
  assert.equal(nextFold(undefined, ["a"], true).folded, false);
  assert.equal(nextFold({ keys: null, auto: false }, ["a"], true).folded, false);
});

test("fold: pressing the head line while it is auto-opened makes the user's choice", () => {
  const [, trouble] = folds([[], true], [["a"], true]);
  assert.equal(trouble.auto, true);
  const chosen = toggleFold({ saved: true, auto: trouble.auto });
  assert.deepEqual(chosen, { saved: true, auto: false });
  const after = nextFold({ keys: trouble.keys, auto: chosen.auto }, ["a"], chosen.saved);
  assert.equal(after.folded, true);
  const again = nextFold(after, ["a", "b"], true);
  assert.equal(again.folded, false);
});

test("fold: toggling flips what the user sees", () => {
  assert.deepEqual(toggleFold({ saved: true, auto: false }), { saved: false, auto: false });
  assert.deepEqual(toggleFold({ saved: false, auto: false }), { saved: true, auto: false });
});

test("fold: a repo the user left open is never folded", () => {
  const [a, b, c] = folds([[], false], [["a"], false], [[], false]);
  assert.deepEqual([a.folded, b.folded, c.folded], [false, false, false]);
});

test("fold: an unreachable hub alone does not open a folded repo, and its problem keys are empty", () => {
  const hubRepo = interpret(ok(unreachable), NOW).repos[0];
  assert.equal(hubRepo.hub.reachable, false);
  assert.deepEqual(hubRepo.problems, []);
  assert.equal(nextFold({ keys: new Set(), auto: false }, hubRepo.problems, true).folded, true);
});

test("problem keys are the notice keys plus the stopped daemon", () => {
  const repo = only([fixture]);
  for (const notice of repo.notices) assert.ok(repo.problems.includes(notice.key));
  const off = only(tonedRepo("o/d", (d) => { d.daemon = { running: false }; d.queue = [itemOf(fixture.queue.find((i) => i.state === "queued").pr)]; }));
  assert.deepEqual(off.problems, ["daemon:off"]);
  assert.deepEqual(only(tonedRepo("o/i", (d) => { d.daemon = { running: false }; })).problems, []);
  const warned = only(tonedRepo("o/w", (d) => { d.warnings = [{ kind: "config", message: "x" }]; }));
  assert.deepEqual(warned.problems, []);
});

test("clock texts are local hh:mm and a full local time", () => {
  const at = new Date(2026, 10, 17, 9, 4, 31).getTime();
  assert.equal(clockText(at), "09:04");
  assert.equal(clockExact(at), "2026-11-17 09:04:31");
  assert.equal(clockText(NaN), "");
});

const MIN = 60_000;
const itemAt = (pr, state, patch = {}) => ({ ...itemOf(pr), state, ...patch });
const waitTexts = (data, id) => section(only(data), id).items.map((item) => item.wait);

test("a queued row says how long it has waited, in whole minutes", () => {
  const cases = [
    [10_000, "waiting <1m"],
    [3 * MIN + 59_000, "waiting 3m"],
    [125 * MIN, "waiting 2h 5m"],
    [120 * MIN, "waiting 2h"],
    [3 * 24 * 60 * MIN + 5 * MIN, "waiting 3d"],
  ];
  for (const [age, text] of cases) {
    const item = { ...itemOf(19), submitted_ms: NOW - age };
    assert.deepEqual(waitTexts(queueOnly(item), "waiting"), [text]);
  }
});

test("a queued row without a submit time has an empty wait", () => {
  const item = { ...itemOf(19), submitted_ms: null };
  assert.deepEqual(waitTexts(queueOnly(item), "waiting"), [""]);
});

test("a needs-you card waits from the first submit, and is empty without one", () => {
  const card = { ...itemOf(21), submitted_ms: NOW - 12 * MIN };
  assert.deepEqual(waitTexts(queueOnly(card), "human"), ["waiting 12m"]);
  assert.deepEqual(waitTexts(queueOnly({ ...card, submitted_ms: undefined }), "human"), [""]);
});

test("a bounced row's ago comes from bounced_ms, not from last_failure", () => {
  const bounced = { ...itemOf(15), bounced_ms: NOW - 12 * MIN };
  assert.equal(section(only(queueOnly(bounced)), "bounced").items[0].ago, "12m ago");
  const none = { ...itemOf(15), bounced_ms: null };
  assert.ok(none.last_failure);
  assert.equal(section(only(queueOnly(none)), "bounced").items[0].ago, "");
});

test("recent merges show the time from submit to merge, with the merge time in the tooltip", () => {
  const merged_ms = NOW - 41 * MIN;
  const data = { ...clone(fixture), recent_merged: [{ pr: 1, title: "x", batch: "b", merged_ms, bounces: 0, submitted_ms: merged_ms - 18 * MIN - 4000 }] };
  const [row] = only(data).recent;
  assert.equal(row.took, "18m 4s");
  assert.ok(row.tip.startsWith("Merged 41m ago · "));
  assert.match(row.tip, /· \d\d:\d\d$/);
});

test("recent merges with a missing or impossible submit time have an empty duration but keep the tooltip", () => {
  const merged_ms = NOW - 41 * MIN;
  const base = { pr: 1, title: "x", batch: "b", merged_ms, bounces: 0 };
  const data = {
    ...clone(fixture),
    recent_merged: [base, { ...base, submitted_ms: null }, { ...base, submitted_ms: merged_ms + 1 }],
  };
  for (const row of only(data).recent) {
    assert.equal(row.took, "");
    assert.ok(row.tip.startsWith("Merged 41m ago"));
  }
});

test("recent rows built from the old recent list have no duration", () => {
  const data = clone(fixture);
  delete data.recent_merged;
  const rows = only(data).recent;
  assert.ok(rows.length > 0);
  for (const row of rows) assert.equal(row.took, "");
});

test("an empty queue keeps four section heads and no alerts or unknown", () => {
  const repo = only(queueOnly());
  assert.deepEqual(repo.sections.map((s) => s.id), ["human", "batch", "waiting", "bounced"]);
  for (const s of repo.sections) assert.deepEqual(s.items, []);
});

test("empty section heads don't change the overview numbers", () => {
  const empty = overview(board(queueOnly()));
  assert.deepEqual(empty, { grade: "ok", issues: 0, running: 0, repos: 1 });
});

test("a repo whose hub can't be reached draws no section heads", () => {
  assert.deepEqual(only(unreachable).sections, []);
});

test("change lines: nested step keys keep the step wording and short values count characters", () => {
  const item = (changes) => ({ touches_config: true, config_change: { kind: "edited", changes, more: 0 } });
  const nested = changeSummary(
    item([{ op: "changed", path: ["post_verify", "e2e", "env", "FOO"], before: "1", after: "2" }]),
  );
  assert.equal(nested[0].text, "post-merge e2e: env.FOO: 1 → 2");
  const emoji = "😀".repeat(12);
  const wide = changeSummary(item([{ op: "changed", path: ["budget"], before: emoji, after: "1" }]));
  assert.equal(wide[0].text, `budget: ${emoji} → 1`);
});

const todoBounce = (pr, patch = {}) => ({
  ...itemOf(9),
  pr,
  submitted_ms: 1_000 + pr,
  attempts: 1,
  budget: 3,
  agent: { terminal: `t${pr}`, name: `w${pr}`, alive: true },
  ...patch,
});
const todoRepo = (slug, items, mutate = () => {}) => tonedRepo(slug, (d) => { d.queue = items; mutate(d); });
const approvalItem = (pr) => ({ ...itemOf(21), pr, submitted_ms: 1_000 + pr });
const todoKinds = (data) => board(data).todo.map((entry) => entry.kind);
const bouncedOf = (data) => section(only(data), "bounced").items;

test("todo: a stopped queue, a failed post-merge check, a stopped daemon with a queue, an unreachable hub and approvals are to-dos", () => {
  const stoppedQueue = board([stopped("o/a")]);
  assert.deepEqual(stoppedQueue.todo.map((e) => [e.kind, e.repoId, e.repoName, e.tone]), [["blocked", "/r/o/a", "a", "error"]]);
  assert.equal(stoppedQueue.todo[0].item, section(stoppedQueue.repos[0], "alerts").items[0]);

  const red = tonedRepo("o/b", (d) => { d.post_verify = { red: clone(fixture.post_verify.red) }; });
  assert.deepEqual(todoKinds([red]), ["post_verify"]);

  const daemonOff = board([todoRepo("o/c", [itemOf(20)], (d) => { d.daemon = { running: false }; })]);
  assert.deepEqual(daemonOff.todo.map((e) => e.kind), ["daemon"]);
  assert.equal(daemonOff.todo[0].item.kind, "daemon");

  const hub = interpret(ok(unreachable), NOW);
  assert.deepEqual(hub.todo.map((e) => [e.kind, e.tone, e.item.text]), [["hub", "error", "Hub unreachable"]]);
  assert.ok(hub.todo[0].item.detail.length > 0);

  const approval = board([humanTurn("o/h")]);
  assert.deepEqual(approval.todo.map((e) => [e.kind, e.tone]), [["approval", "human"]]);
  assert.equal(approval.todo[0].item, section(approval.repos[0], "human").items[0]);

  const idleDaemon = tonedRepo("o/i", (d) => { d.daemon = { running: false }; });
  const warned = tonedRepo("o/w", (d) => {
    d.warnings = [{ kind: "config", message: "x" }, { kind: "flaky_chronic", message: "y", at_ms: 1 }];
  });
  assert.deepEqual(board([idleDaemon, warned]).todo, []);

  const all = board([stopped("o/a"), red, humanTurn("o/h"), unreachable]);
  assert.equal(new Set(all.todo.map((e) => e.key)).size, all.todo.length);
});

test("todo: a bounce whose worker is gone or that has one retry left is a to-do, other bounces are not", () => {
  const needs = (patch) => bouncedOf(todoRepo("o/n", [todoBounce(30, patch)]))[0].needs;
  const gone = { kind: "worker_gone", text: "Worker exited" };
  const last = { kind: "last_retry", text: "1 retry left" };
  assert.deepEqual(needs({ agent: { terminal: "t1", name: "w", alive: false } }), [gone]);
  assert.deepEqual(needs({ agent: null }), [gone]);
  assert.deepEqual(needs({ attempts: 3 }), [last]);
  assert.deepEqual(needs({ attempts: 3, agent: { terminal: "t1", alive: false } }), [gone, last]);
  assert.deepEqual(needs({}), []);

  const mixed = todoRepo("o/m", [
    todoBounce(31, { agent: { terminal: "t1", alive: false } }),
    todoBounce(32, { agent: undefined }),
    todoBounce(33, { attempts: 3 }),
    todoBounce(34),
  ]);
  const read = board([mixed]);
  const cards = section(read.repos[0], "bounced").items;
  assert.deepEqual(read.todo.map((e) => e.item.pr), [31, 32, 33]);
  assert.ok(read.todo.every((e) => e.kind === "bounce" && e.item === cards.find((c) => c.pr === e.item.pr)));
  assert.deepEqual(cards.map((c) => c.needs.length > 0), [true, true, true, false]);
});

test("todo: errors first, then approvals, then bounces with a gone worker, then bounces on their last retry, each by repo order", () => {
  const gone = (pr) => todoBounce(pr, { agent: { terminal: "t1", alive: false } });
  const last = (pr) => todoBounce(pr, { attempts: 3 });
  const x = todoRepo("o/x", [approvalItem(40), last(41)]);
  const y = todoRepo("o/y", [approvalItem(42), gone(43), last(44)], (d) => { d.blocked = { reason: "base_broken", since_ms: 1 }; });
  const order = (data) => board(data).todo.map((e) => [e.kind, e.repoId, e.item.pr ?? null]);
  assert.deepEqual(order([x, y]), [
    ["blocked", "/r/o/y", null],
    ["approval", "/r/o/x", 40],
    ["approval", "/r/o/y", 42],
    ["bounce", "/r/o/y", 43],
    ["bounce", "/r/o/x", 41],
    ["bounce", "/r/o/y", 44],
  ]);
  assert.deepEqual(order([y, x]).map(([kind, repo]) => `${kind}${repo.slice(-1)}`), ["blockedy", "approvaly", "approvalx", "bouncey", "bouncey", "bouncex"]);
});

test("retries left: budget - attempts + 1, at least 1, and null for a broken attempts or budget", () => {
  const left = (attempts, budget) => bouncedOf(todoRepo("o/l", [todoBounce(50, { attempts, budget })]))[0].left;
  assert.equal(left(0, 3), 4);
  assert.equal(left(2, 3), 2);
  assert.equal(left(3, 3), 1);
  assert.equal(left(5, 3), 1);
  for (const odd of [-1, 1.5, "3", null, undefined]) {
    assert.equal(left(odd, 3), null, `attempts ${JSON.stringify(odd)}`);
    assert.equal(left(1, odd), null, `budget ${JSON.stringify(odd)}`);
  }
  assert.equal(left(0, 0), null);
});

test("overview issues are the number of to-dos", () => {
  const bounce = todoRepo("o/b", [todoBounce(60, { attempts: 3 })]);
  const one = board([bounce]);
  assert.equal(overview(one).issues, 1);
  assert.equal(overview(one).grade, "attention");
  const many = board([bounce, humanTurn("o/h"), stopped("o/s"), idle("o/i"), unreachable]);
  assert.equal(many.todo.length, 4);
  assert.equal(overview(many).issues, many.todo.length);
});

test("the status line counts to-do bounces as needing you", () => {
  const both = todoRepo("o/a", [approvalItem(70), todoBounce(71, { attempts: 3 })]);
  assert.deepEqual(only(both).status, { tone: "human", text: "Needs you · 2" });
  const calm = only(todoRepo("o/c", [todoBounce(72)]));
  assert.ok(!calm.status.text.startsWith("Needs you"), calm.status.text);
  assert.equal(calm.status.tone, "ok");
});

test("chronic flaky warnings fold into one line with the count, the latest time and the messages", () => {
  const data = quiet();
  data.warnings = [
    { kind: "flaky_chronic", message: "a::slow flaked 3 times", pr: null, at_ms: NOW - 600_000 },
    { kind: "flaky_chronic", message: "b::slow flaked 4 times", pr: null, at_ms: NOW - 120_000 },
    { kind: "config", message: "other", pr: null, at_ms: NOW - 1_000 },
    { kind: "flaky_chronic", message: "c::slow flaked 5 times", pr: null, at_ms: NOW - 3_600_000 },
  ];
  const repo = only(data);
  assert.deepEqual(repo.flaky, { count: 3, ago: "2m ago", messages: ["a::slow flaked 3 times", "b::slow flaked 4 times", "c::slow flaked 5 times"] });
  assert.deepEqual(section(repo, "alerts").items.map((a) => a.text), ["Chronic flaky test", "Chronic flaky test", "Can't read config", "Chronic flaky test"]);
  data.warnings = Array.from({ length: 200_000 }, (_, i) => ({ kind: "flaky_chronic", message: "m", at_ms: NOW - i }));
  assert.equal(only(data).flaky.count, 200_000);
  data.warnings = [{ kind: "flaky_chronic", message: "d::slow" }];
  assert.deepEqual(only(data).flaky, { count: 1, ago: "", messages: ["d::slow"] });
  data.warnings = [{ kind: "config", message: "x" }];
  assert.equal(only(data).flaky, null);
  assert.equal(only([fixture]).flaky, null);
  assert.equal(only(unreachable).flaky, null);
});

test("a quiet repo has nothing to do, no alerts, no batch, queue or running post-merge check", () => {
  assert.equal(only(quiet()).quiet, true);
  const breaks = {
    blocked: (d) => { d.blocked = { reason: "base_broken", since_ms: 1 }; },
    "failed post-merge check": (d) => { d.post_verify = { red: clone(fixture.post_verify.red) }; },
    "running post-merge check": (d) => { d.post_verify = { run: { commit: "abcdef1", started_ms: 1, phase: "checking" } }; },
    "other warning": (d) => { d.warnings = [{ kind: "config", message: "x" }]; },
    "chronic flaky": (d) => { d.warnings = [{ kind: "flaky_chronic", message: "x" }]; },
    batch: (d) => { d.batch = clone(fixture.batch); },
    queued: (d) => { d.queue = [itemOf(20)]; },
    bounced: (d) => { d.queue = [itemOf(9)]; },
    approval: (d) => { d.queue = [itemOf(21)]; },
    "unknown state": (d) => { d.queue = [{ ...itemOf(20), state: "weird" }]; },
    "stopped daemon with a queue": (d) => { d.daemon = { running: false }; d.queue = [itemOf(20)]; },
  };
  for (const [name, change] of Object.entries(breaks)) {
    const data = quiet();
    change(data);
    assert.equal(only(data).quiet, false, name);
  }
  const green = quiet();
  green.post_verify = { last_green: { commit: "abcdef1234", at_ms: NOW - 60_000 } };
  assert.equal(only(green).quiet, true);
  const resting = quiet();
  resting.daemon = { running: false };
  assert.equal(only(resting).quiet, true);
  assert.equal(only(unreachable).quiet, false);
});

test("the folded summary of an all-clear repo names today's merges and the last merge", () => {
  const summary = (patch) => only({ ...quiet(), ...patch }).summary;
  const merged = [{ pr: 105, title: "t", bounces: 0, merged_ms: NOW - 600_000 }];
  assert.equal(summary({ today: { merged: 1 }, recent_merged: merged }), "All clear · 1 merged today · #105 10m ago");
  assert.equal(summary({ today: undefined, recent_merged: merged }), "All clear · #105 10m ago");
  assert.equal(summary({ today: { merged: "1" }, recent_merged: merged }), "All clear · #105 10m ago");
  assert.equal(summary({ today: { merged: 0 }, recent_merged: [] }), "All clear · 0 merged today");
  assert.equal(summary({ today: undefined, recent_merged: [] }), "All clear");
  const stoppedView = { ...quiet(), blocked: { reason: "base_broken", since_ms: 1 }, today: { merged: 1 }, recent_merged: merged };
  assert.equal(only(stoppedView).summary, "Stopped · base failed verification");
  assert.equal(only(unreachable).summary, "Hub unreachable");
});

const queuedItem = () => itemOf(fixture.queue.find((i) => i.state === "queued").pr);
const bouncedItem = () => itemOf(fixture.queue.find((i) => i.state === "bounced").pr);
const stoppedDaemon = (slug, ...items) => tonedRepo(slug, (d) => { d.daemon = { running: false }; d.queue = items; });

test("a stopped daemon is an alert, a red status line and an issue only while the queue has items", () => {
  const resting = stoppedDaemon("o/r");
  const idleRepo = only(resting);
  assert.equal(section(idleRepo, "alerts"), undefined);
  assert.deepEqual(idleRepo.status, { tone: "ok", text: "All clear" });
  assert.equal(overview(board([resting])).issues, 0);
  assert.deepEqual(idleRepo.problems, []);
  assert.equal(idleRepo.daemon.state, "idle");

  const waiting = stoppedDaemon("o/w", queuedItem());
  const off = only(waiting);
  assert.deepEqual(section(off, "alerts").items.map((a) => a.kind), ["daemon"]);
  assert.deepEqual(off.status, { tone: "error", text: "Daemon is off" });
  assert.equal(overview(board([waiting])).issues, 1);
  assert.deepEqual(off.problems, ["daemon:off"]);
  assert.equal(off.daemon.state, "off");

  const bounced = only(stoppedDaemon("o/b", bouncedItem()));
  assert.deepEqual(bounced.status, { tone: "error", text: "Daemon is off" });
  assert.equal(bounced.daemon.state, "off");
});

test("the daemon is running, idle with an empty queue, off only with a queue, and unknown otherwise", () => {
  const daemonOf = (daemon, ...items) => only(tonedRepo("o/d", (d) => { d.daemon = daemon; d.queue = items; })).daemon;
  const running = daemonOf({ running: true });
  assert.equal(running.state, "running");
  assert.equal(running.tone, "ok");
  assert.equal(daemonOf({ running: true }, queuedItem()).state, "running");

  for (const state of ["queued", "bounced", "awaiting_approval", "in_batch"]) {
    const item = itemOf(fixture.queue.find((i) => i.state === state).pr);
    const off = daemonOf({ running: false }, item);
    assert.equal(off.state, "off", state);
    assert.equal(off.tone, "error", state);
  }
  assert.match(daemonOf({ running: false }, queuedItem()).text, /^Daemon off · 1 PR in queue$/);
  assert.match(daemonOf({ running: false }, queuedItem(), bouncedItem()).text, /^Daemon off · 2 PRs in queue$/);

  const idleDaemon = daemonOf({ running: false });
  assert.equal(idleDaemon.state, "idle");
  assert.equal(toneToken(idleDaemon.tone), "dim");

  for (const odd of [undefined, null, {}, { running: "yes" }, { running: 1 }, "up", []]) {
    for (const items of [[], [queuedItem()]]) {
      const unknown = daemonOf(odd, ...items);
      assert.equal(unknown.state, "unknown", JSON.stringify(odd));
      assert.equal(toneToken(unknown.tone), "dim");
    }
  }
});

test("a repo whose hub is unreachable has an unknown daemon", () => {
  const repo = only(unreachable);
  assert.equal(repo.daemon.state, "unknown");
});

test("tint gives #rrggbbaa for #rgb, #rrggbb and #rrggbbaa, and null for anything else", () => {
  assert.equal(tint("#fff", 0.5), "#ffffff80");
  assert.equal(tint("#f0a", 1), "#ff00aaff");
  assert.equal(tint("#336699", 0.1), "#3366991a");
  assert.equal(tint("#336699", 0), "#33669900");
  assert.equal(tint("#336699", 1), "#336699ff");
  assert.equal(tint("#336699", 7), "#336699ff");
  assert.equal(tint("#336699", -1), "#33669900");
  assert.equal(tint("#33669980", 0.5), "#33669940");
  assert.equal(tint("#336699ff", 0.5), tint("#336699", 0.5));
  assert.equal(tint("#ABCDEF", 1), "#abcdefff");
  assert.equal(tint("#ABC", 1), "#aabbccff");
  for (const odd of ["rgb(1, 2, 3)", "rgba(1,2,3,0.5)", "", "red", "#", "#12", "#abcd", "#12345", "#1234567", "#ggg", " #fff", "fff", undefined, null, 255, {}]) {
    assert.equal(tint(odd, 0.5), null, String(odd));
  }
  assert.equal(tint("#fff", NaN), null);
  assert.equal(tint("#fff", undefined), null);
});

test("isNarrow is true only for a finite width below NARROW_WIDTH", () => {
  assert.equal(isNarrow(300), true);
  assert.equal(isNarrow(NARROW_WIDTH - 1), true);
  assert.equal(isNarrow(NARROW_WIDTH), false);
  assert.equal(isNarrow(360), false);
  for (const odd of [null, undefined, NaN, Infinity]) assert.equal(isNarrow(odd), false, String(odd));
});
