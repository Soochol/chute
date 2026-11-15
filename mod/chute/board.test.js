import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { actionArgs, formatDuration, interpret } from "./board.js";

const fixture = JSON.parse(readFileSync(new URL("../../tests/fixtures/status-full.json", import.meta.url), "utf8"));
const NOW = fixture.batch.started_ms + 312_000;

const clone = (value) => structuredClone(value);
const ok = (data) => ({ code: 0, stdout: JSON.stringify(data), stderr: "" });
const board = (data, now = NOW) => interpret(ok(data), now);
const only = (data) => board(data).repos[0];
const section = (repo, id) => repo.sections.find((s) => s.id === id);
const prs = (s) => s.items.map((item) => item.pr);
const withQueue = (...items) => ({ ...clone(fixture), queue: items.map(clone) });
const itemOf = (pr) => clone(fixture.queue.find((item) => item.pr === pr));

test("every fixture item lands in one of the six sections", () => {
  const result = board([fixture]);
  assert.equal(result.kind, "board");
  const repo = result.repos[0];
  assert.deepEqual(repo.sections.map((s) => s.id), ["alerts", "human", "batch", "waiting", "bounced", "today"]);
  assert.deepEqual(prs(section(repo, "human")), [9, 10, 11, 21]);
  assert.deepEqual(section(repo, "batch").items[0].prs.map((p) => p.pr), [12, 13]);
  assert.deepEqual(prs(section(repo, "waiting")), [19, 20]);
  assert.deepEqual(prs(section(repo, "bounced")), [15, 14, 16, 18, 17]);

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
      "needs_human/budget",
      "needs_human/gave_up",
      "needs_human/unreachable",
      "queued/ci_pending",
      "queued/null",
    ],
  );
  const repo = only([fixture]);
  const reasons = Object.fromEntries(
    ["human", "bounced"].flatMap((id) => section(repo, id).items.map((item) => [item.pr, item.reason])),
  );
  assert.deepEqual(reasons, {
    9: "재시도 예산을 다 썼다",
    10: "에이전트에게 전달하지 못했다",
    11: "에이전트가 포기를 선언했다",
    21: "승인 대기",
    15: "검증 실패",
    14: "충돌",
    16: "시간 초과",
    18: "CI 실패",
    17: "HEAD 변경",
  });
});

test("buttons: approve and drop on awaiting approval, retry and drop on needs_human, none elsewhere", () => {
  const repo = only([fixture]);
  const buttons = (id) =>
    Object.fromEntries(section(repo, id).items.map((item) => [item.pr, item.actions.map((a) => a.label)]));
  assert.deepEqual(buttons("human"), {
    9: ["다시 시도", "빼기"],
    10: ["다시 시도", "빼기"],
    11: ["다시 시도", "빼기"],
    21: ["승인", "빼기"],
  });
  for (const id of ["bounced", "waiting"]) {
    for (const item of section(repo, id).items) assert.equal(item.actions?.length ?? 0, 0, `#${item.pr}`);
  }
});

test("button commands are [verb, pr, --repo, path]", () => {
  const repo = only([fixture]);
  const human = Object.fromEntries(section(repo, "human").items.map((item) => [item.pr, item.actions]));
  const path = fixture.repo.path;
  assert.deepEqual(human[21].map((a) => a.args), [
    ["approve", "21", "--repo", path],
    ["drop", "21", "--repo", path],
  ]);
  assert.deepEqual(human[9].map((a) => a.args), [
    ["retry", "9", "--repo", path],
    ["drop", "9", "--repo", path],
  ]);
  assert.deepEqual(actionArgs("approve", 7, "/r"), ["approve", "7", "--repo", "/r"]);
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
  assert.match(alerts.items[0].text, /멈췄다/);
  assert.equal(alerts.items[0].detail, "기준 0a1b2c3 · 7분 12초 전부터");
  assert.deepEqual(
    alerts.items.filter((a) => a.kind === "warning").map((a) => a.text),
    fixture.warnings.map((w) => w.message),
  );
});

test("a stopped daemon is announced in the first section", () => {
  const data = clone(fixture);
  data.blocked = null;
  data.warnings = [];
  data.daemon = { running: false, terminal: null, pid: null };
  const repo = only(data);
  assert.equal(repo.sections[0].id, "alerts");
  assert.equal(repo.sections[0].items.length, 1);
  assert.equal(repo.sections[0].items[0].kind, "daemon");
  assert.match(repo.sections[0].items[0].text, /데몬이 꺼져/);
});

test("a healthy repo has no alerts section", () => {
  const data = clone(fixture);
  data.blocked = null;
  data.warnings = [];
  assert.equal(section(only(data), "alerts"), undefined);
});

test("ci_pending is waiting for CI, a ci bounce is a CI failure", () => {
  const repo = only([fixture]);
  const waiting = section(repo, "waiting").items;
  assert.deepEqual(waiting.map((item) => [item.pr, item.label, item.ciPending]), [
    [19, "CI 대기", true],
    [20, "대기", false],
  ]);
  const ci = section(repo, "bounced").items.find((item) => item.pr === 18);
  assert.equal(ci.reason, "CI 실패");
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
    { pr: 12, title: "큐 상태 전이 추가" },
    { pr: 13, title: "검증 실행기" },
  ]);
  assert.equal(batch.phase, "검증 중");
  assert.equal(batch.step, "test");
  assert.equal(batch.round, "1번째 검증");
  assert.equal(batch.elapsed, "5분 12초");
});

test("an in_batch item with no batch record is still shown", () => {
  const data = withQueue(itemOf(12));
  data.batch = null;
  const [batch] = section(only(data), "batch").items;
  assert.deepEqual(batch.prs.map((p) => p.pr), [12]);
  assert.equal(batch.phase, null);
  assert.equal(batch.elapsed, null);
});

test("awaiting approval shows the preflight result and whether chute.toml changed", () => {
  const item = section(only([fixture]), "human").items.find((i) => i.pr === 21);
  assert.deepEqual([item.preflight.text, item.preflight.tone], ["preflight 통과", "ok"]);
  assert.equal(item.config, "chute.toml 변경");
  assert.equal(item.configChanged, true);

  const data = withQueue({ ...itemOf(21), touches_config: false, preflight: { result: "fail", head: "21e5678" } });
  const plain = section(only(data), "human").items[0];
  assert.deepEqual([plain.preflight.text, plain.preflight.tone], ["preflight 실패", "error"]);
  assert.equal(plain.config, "chute.toml 변경 없음");

  const stale = section(only(withQueue({ ...itemOf(21), preflight: { result: "pass", head: "old0000" } })), "human").items[0];
  assert.equal(stale.preflight.text, "preflight 통과 (이전 HEAD)");
  assert.equal(stale.preflight.stale, true);
  assert.equal(stale.preflight.tone, "muted");

  const none = section(only(withQueue({ ...itemOf(21), preflight: null })), "human").items[0];
  assert.equal(none.preflight.text, "preflight 없음");
});

test("human cards carry reason, agent, retries and the last failure", () => {
  const items = section(only([fixture]), "human").items;
  const budget = items.find((i) => i.pr === 9);
  assert.equal(budget.retries, "3/3");
  assert.deepEqual(budget.agent, { terminal: "t1rp", name: "impl-h", alive: true, label: "impl-h (t1rp)" });
  assert.equal(budget.failure.summary, "test 단계 실패: skill::tests::prints_manual");
  assert.equal(items.find((i) => i.pr === 10).agent.alive, false);
  assert.equal(items.find((i) => i.pr === 11).agent, null);
});

test("bounced cards carry mode, summary, agent, retries and report path", () => {
  const items = section(only([fixture]), "bounced").items;
  const verify = items.find((i) => i.pr === 15);
  assert.equal(verify.failure.mode, "단독");
  assert.equal(verify.failure.summary, "test 단계 실패: feedback::tests::report_lists_partners");
  assert.equal(verify.failure.report, "/home/dev/.local/state/chute/repos/Soochol__chute/reports/pr-15/2.md");
  assert.equal(verify.retries, "2/3");
  assert.equal(verify.agent.label, "impl-d (t1rv)");
  assert.equal(items.find((i) => i.pr === 16).failure.mode, "조합");
  assert.equal(items.find((i) => i.pr === 14).failure.mode, null);
  const headChanged = items.find((i) => i.pr === 17);
  assert.equal(headChanged.failure, null);
  assert.equal(headChanged.reason, "HEAD 변경");
});

test("today's numbers", () => {
  const today = section(only([fixture]), "today").items;
  assert.deepEqual(Object.fromEntries(today.map((s) => [s.key, s.value])), {
    merged: "7",
    bounced: "2",
    flaky: "1",
    batches: "3",
    lead: "6분 52초",
    verify: "21분",
  });
  const data = clone(fixture);
  data.today.median_lead_s = null;
  assert.equal(section(only(data), "today").items.find((s) => s.key === "lead").value, "—");
});

test("empty sections are not drawn", () => {
  const data = { ...clone(fixture), queue: [], batch: null, blocked: null, warnings: [] };
  assert.deepEqual(only(data).sections.map((s) => s.id), ["today"]);
});

test("several repos from --all, and a single object", () => {
  const second = clone(fixture);
  second.repo = { ...second.repo, slug: "Soochol/other", path: "/home/dev/code/other" };
  const result = board([fixture, second]);
  assert.deepEqual(result.repos.map((r) => r.slug), ["Soochol/chute", "Soochol/other"]);
  assert.equal(section(result.repos[1], "human").items[0].actions[0].args[3], "/home/dev/code/other");
  assert.equal(board(fixture).repos.length, 1);
  assert.equal(result.repos[0].stateDir, fixture.repo.state_dir);
  assert.equal(result.repos[0].approval, "에이전트 신뢰");
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
  assert.equal(interpret({ code: 3, stdout: "", stderr: "" }, NOW).detail, "종료 코드 3");
});

test("chute that cannot start shows that fact and install guidance", () => {
  const result = interpret({ spawnError: "cannot start chute: program not found" }, NOW);
  assert.equal(result.kind, "missing");
  assert.equal(result.detail, "cannot start chute: program not found");
  assert.match(result.title, /실행할 수 없다/);
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
  const needs = { ...itemOf(9), pr: 32, state_reason: "novel" };
  const waiting = { ...itemOf(20), pr: 33, state_reason: "later" };
  const data = withQueue(odd, bounced, needs, waiting, itemOf(21));
  data.batch = { ...data.batch, phase: "teleporting", prs: [] };
  data.blocked = { reason: "new_reason", base: "abc", since_ms: 1 };
  data.approval = "robot";
  data.queue[0].agent = null;
  const repo = only(data);

  assert.deepEqual(section(repo, "unknown").items.map((i) => [i.pr, i.state, i.reason]), [[30, "frobnicated", "because"]]);
  assert.equal(section(repo, "bounced").items[0].reason, "cosmic_rays");
  assert.equal(section(repo, "human").items.find((i) => i.pr === 32).reason, "novel");
  assert.equal(section(repo, "waiting").items[0].note, "later");
  assert.equal(section(repo, "alerts").items[0].text, "큐가 멈췄다: new_reason");
  assert.equal(repo.approval, "robot");
  assert.equal(
    repo.sections.reduce((n, s) => n + (s.id === "alerts" || s.id === "today" || s.id === "batch" ? 0 : s.items.length), 0),
    5,
  );
});

test("an unknown batch phase and preflight result are shown as they are", () => {
  const data = withQueue(itemOf(12), { ...itemOf(21), preflight: { result: "skipped", head: "21e5678" } });
  data.batch.phase = "teleporting";
  const repo = only(data);
  assert.equal(section(repo, "batch").items[0].phase, "teleporting");
  assert.equal(section(repo, "human").items[0].preflight.text, "preflight skipped");
});

test("missing fields do not break the board", () => {
  const sparse = { schema: 1, queue: [{ pr: 1, state: "queued" }, { pr: 2, state: "needs_human" }, "junk", null] };
  const repo = only(sparse);
  assert.equal(repo.slug, "(이름 없는 저장소)");
  assert.deepEqual(prs(section(repo, "waiting")), [1]);
  assert.deepEqual(prs(section(repo, "human")), [2]);
  assert.equal(section(repo, "human").items[0].retries, "—/—");
});

test("formatDuration", () => {
  assert.equal(formatDuration(0), "0초");
  assert.equal(formatDuration(59), "59초");
  assert.equal(formatDuration(60), "1분");
  assert.equal(formatDuration(125), "2분 5초");
  assert.equal(formatDuration(3600), "1시간");
  assert.equal(formatDuration(3660), "1시간 1분");
  assert.equal(formatDuration(null), "—");
  assert.equal(formatDuration(-1), "—");
});
