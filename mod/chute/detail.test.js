import test from "node:test";
import assert from "node:assert/strict";

import {
  DETAIL_BATCHES,
  DETAIL_EVENTS,
  batchRows,
  bounceReasons,
  detailArgs,
  eventRows,
  flakyRows,
  overviewOf,
  parseBatches,
  parseEvents,
  parseFlaky,
} from "./detail.js";
import { clockText } from "./board.js";

const HANGUL = /[ㄱ-힣]/;
const ok = (stdout, stderr = "") => ({ code: 0, stdout, stderr });
const T0 = Date.UTC(2026, 10, 19, 3, 0, 0);
const entry = (id, startedAt, seconds, extra = {}) => ({
  id,
  started_ms: T0 + startedAt * 1000,
  finished_ms: T0 + (startedAt + seconds) * 1000,
  merged: [],
  bounced: [],
  reverted: false,
  revert_commit: null,
  ...extra,
});
const batchesOf = (entries) => parseBatches(ok(JSON.stringify(entries))).batches;
const line = (e) => JSON.stringify(e);

test("the detail window asks for 120 batches, the flaky ledger and a day of events", () => {
  assert.equal(DETAIL_BATCHES, 120);
  assert.equal(DETAIL_EVENTS, 2000);
  assert.deepEqual(detailArgs("/r"), {
    batches: ["--repo", "/r", "batches", "--json", "-n", "120"],
    flaky: ["--repo", "/r", "flaky", "--json"],
    events: ["--repo", "/r", "events", "--since", "1d", "--limit", "2000", "--json"],
  });
});

test("a failed or unreadable command becomes a failed tab", () => {
  const parsers = [parseBatches, parseFlaky, parseEvents];
  for (const parse of parsers) {
    const exit = parse({ code: 1, stdout: "", stderr: "\ndetail\nmore" });
    assert.deepEqual(exit, { kind: "failed", detail: "detail" });
    const gone = parse({ spawnError: new Error("no such file") });
    assert.deepEqual(gone, { kind: "failed", detail: "no such file" });
    assert.equal(parse(undefined).kind, "failed");
    assert.equal(parse({ code: 1 }).detail, "Exit code 1");
  }
  for (const parse of [parseBatches, parseFlaky]) {
    const bad = parse(ok("not json"));
    assert.equal(bad.kind, "failed");
    assert.doesNotMatch(bad.detail, HANGUL);
    assert.match(bad.detail, /^Couldn't read/);
  }
  assert.equal(parseBatches(ok('{"a":1}')).kind, "failed");
  assert.equal(parseFlaky(ok("[]")).kind, "failed");
  assert.equal(parseBatches({ spawnError: null }).detail, "Couldn't run the command");
});

test("the overview counts batches, merged and bounced PRs and the median of batches over a minute", () => {
  const batches = batchesOf([
    entry("a", 0, 30, { merged: [1] }),
    entry("b", 100, 120, { merged: [2, 3] }),
    entry("c", 400, 300, { merged: [4], bounced: [9] }),
    entry("d", 1000, 540, { merged: [5], bounced: [10] }),
  ]);
  const { counts } = overviewOf(batches);
  assert.deepEqual(
    counts.map((c) => c.value),
    ["4", "5", "2", "5m"],
  );
  assert.equal(overviewOf([]).counts[3].value, "—");
  assert.equal(overviewOf(batchesOf([entry("x", 0, 30)])).counts[3].value, "—");
  assert.equal(overviewOf(batchesOf([entry("x", 0, 120), entry("y", 200, 240)])).counts[3].value, "3m");
});

test("overview bars run oldest first on a log scale, red when bounced and faded when reverted", () => {
  const batches = batchesOf([
    entry("b20261119-120000", 0, 10, { merged: [1] }),
    entry("b20261119-130000", 100, 1000, { merged: [2], bounced: [7] }),
    entry("b20261119-140000", 2000, 100, { merged: [3, 4], reverted: true }),
  ]);
  const view = overviewOf(batches);
  assert.deepEqual(
    view.bars.map((b) => b.key),
    ["b20261119-120000", "b20261119-130000", "b20261119-140000"],
  );
  assert.deepEqual(
    view.bars.map((b) => b.tone),
    ["merged", "bounced", "merged"],
  );
  assert.deepEqual(
    view.bars.map((b) => b.faded),
    [false, false, true],
  );
  const [low, high, mid] = view.bars.map((b) => b.height);
  assert.equal(high, 1);
  assert.ok(low >= 0.15 && low < mid && mid < high);
  assert.equal(mid, Math.log(101) / Math.log(1001));
  assert.equal(view.bars[0].tip, `${clockText(T0 + 10_000)} · 10s · merged #1`);
  assert.equal(view.bars[1].tip, `${clockText(T0 + 1_100_000)} · 16m 40s · merged #2 · bounced #7`);
  assert.equal(view.bars[2].tip, `${clockText(T0 + 2_100_000)} · 1m 40s · merged #3 #4 · reverted`);
  assert.deepEqual(view.axis, [clockText(T0 + 10_000), clockText(T0 + 1_100_000), clockText(T0 + 2_100_000)]);
  assert.deepEqual(overviewOf([]).axis, []);
  const flat = overviewOf(batchesOf([entry("z", 0, 0)]));
  assert.equal(flat.bars[0].height, 0.15);
});

test("bounce reasons are counted from the bounced events, most first", () => {
  const events = [
    { at_ms: 1, event: "bounced", pr: 1, reason: "verify", attempts: 1 },
    { at_ms: 2, event: "bounced", pr: 2, reason: "conflict", attempts: 1 },
    { at_ms: 3, event: "bounced", pr: 3, reason: "conflict", attempts: 2 },
    { at_ms: 4, event: "merged", pr: 4, reason: "conflict" },
    { at_ms: 5, event: "bounced", pr: 5, reason: "conflict", attempts: 1 },
  ];
  assert.equal(bounceReasons(events), "3 conflict, 1 verify");
  assert.equal(bounceReasons([{ at_ms: 1, event: "merged", pr: 1 }]), "");
  const view = overviewOf([], parseEvents(ok(events.map(line).join("\n"))));
  assert.equal(view.reasons, "3 conflict, 1 verify");
  assert.equal(overviewOf([], { kind: "failed", detail: "x" }).reasons, "");
  assert.equal(overviewOf([], null).reasons, "");
});

test("events are read one per line, newest first, skipping broken lines", () => {
  const stdout = [
    line({ at_ms: 1000, event: "submitted", pr: 1 }),
    "{broken",
    "",
    line({ at_ms: 3000, event: "merged", pr: 2 }),
    line({ at_ms: 2000, event: "closed", pr: 3 }),
  ].join("\n");
  const read = parseEvents(ok(stdout));
  assert.equal(read.kind, "ok");
  assert.equal(read.events.length, 3);
  assert.equal(read.skipped, 1);
  assert.equal(read.cut, false);
  assert.deepEqual(
    eventRows(read.events).map((r) => r.text),
    ["merged #2", "closed #3", "submitted #1"],
  );
  assert.equal(parseEvents(ok(stdout, "limit warning\n")).cut, true);
  assert.equal(parseEvents(ok("[]\n{}\n")).skipped, 2);
});

test("each event kind has an english line and an unknown kind shows its name", () => {
  const sha = "c21b6f2aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const cases = [
    [{ event: "merged", pr: 616 }, "merged #616"],
    [{ event: "bounced", pr: 616, reason: "conflict", attempts: 1 }, "bounced #616 · conflict · attempt 1"],
    [{ event: "bounced", pr: 616, reason: "head_changed", attempts: 2 }, "bounced #616 · head changed · attempt 2"],
    [{ event: "flaky", step: "e2e", tests: ["a.spec.ts:1", "b.spec.ts:2"] }, "flaky a.spec.ts:1, b.spec.ts:2 · e2e"],
    [{ event: "flaky", step: "e2e", tests: [] }, "flaky e2e"],
    [{ event: "batch_finished", merged: [616, 617], bounced: [], seconds: 475 }, "batch done · merged #616 #617 · 7m 55s"],
    [{ event: "post_green", commit: sha }, "post-merge green c21b6f2"],
    [{ event: "post_failed", commit: sha, step: "e2e", tests: [], culprit: { commit: sha, pr: 539, batch: "b" } }, "post-merge failed e2e · culprit #539"],
    [{ event: "post_failed", commit: sha, step: "e2e", tests: [], culprit: null }, "post-merge failed e2e · culprit unknown"],
    [{ event: "post_failed", commit: sha, step: "e2e", tests: [], culprit: { commit: sha, pr: null, batch: null } }, "post-merge failed e2e · culprit unknown"],
    [{ event: "post_fixed", commit: sha, was: sha, pr: null }, "post-merge fixed c21b6f2"],
    [{ event: "blocked", reason: "base_broken", base: "c05a894bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }, "queue stopped · base broken c05a894"],
    [{ event: "resumed", base: sha, by: "repair", pr: 588 }, "resumed by repair #588"],
    [{ event: "resumed", base: sha, by: "main_moved", pr: null }, "resumed by main moved"],
    [{ event: "reverted", batch: "b", prs: [5, 6], commit: sha }, "reverted #5 #6"],
    [{ event: "submitted", pr: 9 }, "submitted #9"],
    [{ event: "removed", pr: 9, reason: "closed" }, "removed #9"],
    [{ event: "dropped", pr: 9 }, "dropped #9"],
    [{ event: "gave_up", pr: 9, reason: "x" }, "gave up #9"],
    [{ event: "closed", pr: 9 }, "closed #9"],
    [{ event: "needs_human", pr: 9, reason: "budget" }, "needs human #9"],
    [{ event: "reused", batch: "b" }, "reused"],
    [{ event: "flaky_probe", batch: "b", step: "e2e", tests: [] }, "flaky probe"],
    [{ event: "notified", kind: "merged", pr: 4 }, "notified #4"],
    [{ event: "new_thing", pr: 4, detail: "extra" }, "new_thing"],
  ];
  for (const [event, text] of cases) {
    const [row] = eventRows([{ at_ms: T0, ...event }]);
    assert.equal(row.text, text, event.event);
    assert.doesNotMatch(row.text, HANGUL);
    assert.equal(row.time, clockText(T0));
    assert.ok(row.glyph && row.tone && row.key !== undefined);
  }
  assert.equal(eventRows([{ at_ms: T0, event: "constructor" }])[0].text, "constructor");
  assert.equal(eventRows([{ at_ms: T0, event: "merged", pr: 1, detail: "한글" }])[0].text, "merged #1");
});

test("the flaky ledger lists chronic tests first, then by count and last time", () => {
  const now = T0 + 3 * 3_600_000;
  const e = (test, count, hoursAgo, chronic) => ({
    test,
    step: "e2e",
    count,
    last_ms: now - hoursAgo * 3_600_000,
    last_batch: "b",
    evidence: null,
    chronic,
  });
  const read = parseFlaky(
    ok(JSON.stringify({ days: 7, chronic: 3, chronic_days: 7, entries: [e("quiet", 9, 1, false), e("old", 4, 30, true), e("new", 4, 2, true), e("top", 7, 5, true), e("tie-old", 2, 10, false), e("tie-new", 2, 3, false)] })),
  );
  assert.equal(read.kind, "ok");
  assert.equal(read.days, 7);
  assert.equal(read.chronicCount, 3);
  const rows = flakyRows(read.entries, now);
  assert.deepEqual(
    rows.map((r) => r.test),
    ["top", "new", "old", "quiet", "tie-new", "tie-old"],
  );
  assert.deepEqual(rows[1], { key: rows[1].key, test: "new", step: "e2e", count: 4, ago: "2h ago", chronic: true });
  assert.equal(flakyRows([{ step: "check", count: 1, last_ms: now, chronic: false }], now)[0].test, "(whole step)");
  assert.deepEqual(flakyRows([], now), []);
});

test("batch rows show step times only when chute records them", () => {
  const batches = batchesOf([
    entry("b20261119-120000", 0, 475, { merged: [616, 617], bounced: [5] }),
    entry("b20261119-110000-2", -4000, 100, {
      step_times: [
        { step: "check", seconds: 48 },
        { step: "clippy", seconds: 71 },
      ],
      skipped: [{ round: 1, step: "e2e" }, { round: 2, step: "e2e" }],
      reruns: [{ pr: 3 }],
      reverted: true,
    }),
    entry("b20261119-100000", -9000, 60, { step_times: { check: 48, clippy: 71 } }),
    { id: "odd", started_ms: T0 - 20_000_000, finished_ms: null, merged: [], bounced: [], reverted: false },
  ]);
  const rows = batchRows(batches);
  assert.deepEqual(
    rows.map((r) => r.id),
    ["120000", "110000-2", "100000", "odd"],
  );
  assert.equal(rows[0].steps, null);
  assert.equal(rows[0].took, "7m 55s");
  assert.equal(rows[0].merged, "#616 #617");
  assert.equal(rows[0].bounced, "#5");
  assert.equal(rows[0].started, clockText(T0));
  assert.equal(rows[1].steps, "check 48s · clippy 1m 11s · 2 skipped · 1 rerun");
  assert.equal(rows[1].reverted, true);
  assert.equal(rows[2].steps, "check 48s · clippy 1m 11s");
  assert.equal(rows[3].took, "—");
  assert.equal(rows[3].merged, "");
  assert.equal(parseBatches(ok(JSON.stringify([entry("x", 0, 1, { step_times: "bad", skipped: "bad" })]))).kind, "ok");
  assert.equal(batchRows(batchesOf([entry("x", 0, 1, { step_times: "bad", skipped: "bad" })]))[0].steps, null);
});
