import { causeText, clockText, failureText, formatAgo, formatDuration } from "./board.js";

export const DETAIL_BATCHES = 120;
export const DETAIL_EVENTS = 2000;
export const FLAKY_SHOWN = 20;
export const TABS = ["overview", "flaky", "events", "batches"];

const BAR_FLOOR = 0.15;
const LONG_BATCH_SECONDS = 60;
const SHORT_ID = 12;

export const detailArgs = (repoPath) => ({
  batches: ["--repo", repoPath, "batches", "--json", "-n", String(DETAIL_BATCHES)],
  flaky: ["--repo", repoPath, "flaky", "--json"],
  events: ["--repo", repoPath, "events", "--since", "1d", "--limit", String(DETAIL_EVENTS), "--json"],
});

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const list = (v) => (Array.isArray(v) ? v : []);
const str = (v) => (typeof v === "string" ? v : "");
const prs = (v) => list(v).filter((pr) => Number.isSafeInteger(pr) && pr > 0);
const prText = (v) => prs(v).map((pr) => `#${pr}`).join(" ");
const spaced = (v) => str(v).replaceAll("_", " ");
const shortSha = (v) => str(v).slice(0, 7);
const failed = (detail) => ({ kind: "failed", detail });

function output(result, what) {
  if (!isObject(result)) return failed("Couldn't run the command");
  if (result.spawnError !== undefined) return failed(causeText(result.spawnError) || "Couldn't run the command");
  if (result.code !== 0) return failed(failureText(result));
  if (typeof result.stdout !== "string") return failed(`Couldn't read the ${what}`);
  return { kind: "ok", stdout: result.stdout, stderr: str(result.stderr) };
}

function json(result, what) {
  const out = output(result, what);
  if (out.kind !== "ok") return out;
  try {
    return { kind: "ok", value: JSON.parse(out.stdout) };
  } catch {
    return failed(`Couldn't read the ${what}`);
  }
}

const countOf = (v) => (Array.isArray(v) ? v.length : Number.isSafeInteger(v) && v > 0 ? v : 0);

function secondsOf(entry) {
  if (!isObject(entry)) return null;
  const s = entry.seconds ?? entry.secs ?? entry.s;
  return Number.isFinite(s) && s >= 0 ? s : null;
}

function stepTimesOf(value) {
  const pairs = Array.isArray(value)
    ? value.map((e) => (Array.isArray(e) ? [e[0], e[1]] : [e?.step ?? e?.name, secondsOf(e)]))
    : isObject(value)
      ? Object.entries(value)
      : [];
  const times = pairs.filter(([name, s]) => typeof name === "string" && name !== "" && Number.isFinite(s) && s >= 0);
  return times.length > 0 ? times.map(([name, s]) => `${name} ${formatDuration(s)}`).join(" · ") : "";
}

function batchOf(entry) {
  if (!isObject(entry) || typeof entry.id !== "string" || entry.id === "") return null;
  const finished = Number.isFinite(entry.finished_ms) ? entry.finished_ms : null;
  const started = Number.isFinite(entry.started_ms) ? entry.started_ms : null;
  return {
    id: entry.id,
    started,
    finished,
    seconds: started !== null && finished !== null && finished >= started ? (finished - started) / 1000 : null,
    merged: prs(entry.merged),
    bounced: prs(entry.bounced),
    reverted: entry.reverted === true,
    stepTimes: stepTimesOf(entry.step_times),
    skipped: countOf(entry.skipped),
    reruns: countOf(entry.reruns),
  };
}

const batchTime = (b) => b.finished ?? b.started ?? 0;

export function parseBatches(result) {
  const read = json(result, "batch list");
  if (read.kind !== "ok") return read;
  if (!Array.isArray(read.value)) return failed("Couldn't read the batch list");
  const batches = read.value
    .map(batchOf)
    .filter(Boolean)
    .map((b, i) => ({ b, i }))
    .sort((x, y) => batchTime(y.b) - batchTime(x.b) || x.i - y.i)
    .map(({ b }) => b);
  return { kind: "ok", batches };
}

export function parseFlaky(result) {
  const read = json(result, "flaky ledger");
  if (read.kind !== "ok") return read;
  const v = read.value;
  if (!isObject(v) || !Array.isArray(v.entries)) return failed("Couldn't read the flaky ledger");
  const entries = v.entries.filter((e) => isObject(e) && typeof e.step === "string");
  return {
    kind: "ok",
    days: Number.isSafeInteger(v.days) ? v.days : null,
    chronicCount: entries.filter((e) => e.chronic === true).length,
    entries,
  };
}

export function parseEvents(result) {
  const out = output(result, "event list");
  if (out.kind !== "ok") return out;
  const events = [];
  let skipped = 0;
  for (const line of out.stdout.split("\n")) {
    if (line.trim() === "") continue;
    try {
      const event = JSON.parse(line);
      if (isObject(event) && typeof event.event === "string" && Number.isFinite(event.at_ms)) events.push(event);
      else skipped++;
    } catch {
      skipped++;
    }
  }
  return { kind: "ok", events, skipped, cut: out.stderr.trim() !== "" };
}

const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
};

function barOf(b, max) {
  const raw = max > 0 ? Math.log(1 + b.seconds) / Math.log(1 + max) : 0;
  const tip = [
    clockText(b.finished),
    formatDuration(b.seconds),
    b.merged.length > 0 ? `merged ${prText(b.merged)}` : "",
    b.bounced.length > 0 ? `bounced ${prText(b.bounced)}` : "",
    b.reverted ? "reverted" : "",
  ];
  return {
    key: b.id,
    height: Math.max(BAR_FLOOR, raw),
    tone: b.bounced.length > 0 ? "bounced" : "merged",
    faded: b.reverted,
    tip: tip.filter(Boolean).join(" · "),
  };
}

export function bounceReasons(events) {
  const counts = new Map();
  for (const event of events) {
    if (event.event !== "bounced" || typeof event.reason !== "string" || event.reason === "") continue;
    counts.set(event.reason, (counts.get(event.reason) ?? 0) + 1);
  }
  return [...counts]
    .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))
    .map(([reason, n]) => `${n} ${reason}`)
    .join(", ");
}

export function overviewOf(batches, events = null) {
  const longOnes = batches.filter((b) => b.seconds !== null && b.seconds >= LONG_BATCH_SECONDS).map((b) => b.seconds);
  const drawn = batches.filter((b) => b.finished !== null && b.seconds !== null).reverse();
  const max = Math.max(0, ...drawn.map((b) => b.seconds));
  const bars = drawn.map((b) => barOf(b, max));
  const at = (i) => clockText(drawn[i].finished);
  return {
    counts: [
      { label: "Batches", value: String(batches.length) },
      { label: "PRs merged", value: String(batches.reduce((n, b) => n + b.merged.length, 0)) },
      { label: "PRs bounced", value: String(batches.reduce((n, b) => n + b.bounced.length, 0)) },
      { label: "Median over 1m", value: longOnes.length > 0 ? formatDuration(median(longOnes)) : "—" },
    ],
    bars,
    axis: drawn.length > 0 ? [at(0), at((drawn.length - 1) >> 1), at(drawn.length - 1)] : [],
    reasons: events && events.kind === "ok" ? bounceReasons(events.events) : "",
  };
}

export function flakyRows(entries, now = Date.now()) {
  const last = (e) => (Number.isFinite(e.last_ms) ? e.last_ms : 0);
  const count = (e) => (Number.isFinite(e.count) ? e.count : 0);
  return entries
    .map((e, i) => ({ e, i }))
    .sort((a, b) => Number(b.e.chronic === true) - Number(a.e.chronic === true) || count(b.e) - count(a.e) || last(b.e) - last(a.e) || a.i - b.i)
    .map(({ e, i }) => ({
      key: `${i}`,
      test: typeof e.test === "string" && e.test !== "" ? e.test : "(whole step)",
      step: e.step,
      count: count(e),
      ago: Number.isFinite(e.last_ms) ? formatAgo(now - e.last_ms) : "",
      chronic: e.chronic === true,
    }));
}

const withPr = (name) => (e) => `${name}${Number.isSafeInteger(e.pr) && e.pr > 0 ? ` #${e.pr}` : ""}`;

const kind = (glyph, tone, text) => ({ glyph, tone, text });

const EVENT = {
  merged: kind("✓", "ok", (e) => `merged #${e.pr}`),
  bounced: kind(
    "✕",
    "error",
    (e) =>
      [`bounced #${e.pr}`, spaced(e.reason), Number.isSafeInteger(e.attempts) ? `attempt ${e.attempts}` : ""]
        .filter(Boolean)
        .join(" · "),
  ),
  flaky: kind(
    "△",
    "warn",
    (e) => {
      const tests = list(e.tests).filter((t) => typeof t === "string");
      return tests.length > 0 ? `flaky ${tests.join(", ")} · ${str(e.step)}` : `flaky ${str(e.step)}`;
    },
  ),
  batch_finished: kind(
    "▪",
    "waiting",
    (e) =>
      [
        "batch done",
        prs(e.merged).length > 0 ? `merged ${prText(e.merged)}` : "",
        prs(e.bounced).length > 0 ? `bounced ${prText(e.bounced)}` : "",
        Number.isFinite(e.seconds) ? formatDuration(e.seconds) : "",
      ]
        .filter(Boolean)
        .join(" · "),
  ),
  post_green: kind("✓", "ok", (e) => `post-merge green ${shortSha(e.commit)}`),
  post_failed: kind(
    "✕",
    "error",
    (e) => `post-merge failed ${str(e.step)} · culprit ${Number.isSafeInteger(e.culprit?.pr) && e.culprit.pr > 0 ? `#${e.culprit.pr}` : "unknown"}`,
  ),
  post_fixed: kind("✓", "ok", (e) => `post-merge fixed ${shortSha(e.commit)}`),
  blocked: kind("✕", "error", (e) => `queue stopped · ${spaced(e.reason)} ${shortSha(e.base)}`),
  resumed: kind("▸", "working", (e) => `resumed by ${spaced(e.by)}${Number.isSafeInteger(e.pr) && e.pr > 0 ? ` #${e.pr}` : ""}`),
  reverted: kind("↺", "warn", (e) => `reverted ${prText(e.prs)}`),
  submitted: kind("+", "waiting", (e) => `submitted #${e.pr}`),
  needs_human: kind("?", "human", withPr("needs human")),
  removed: kind("–", "waiting", withPr("removed")),
  dropped: kind("–", "waiting", withPr("dropped")),
  gave_up: kind("–", "waiting", withPr("gave up")),
  closed: kind("–", "waiting", withPr("closed")),
  reused: kind("·", "dim", withPr("reused")),
  flaky_probe: kind("·", "dim", withPr("flaky probe")),
  notified: kind("·", "dim", withPr("notified")),
};

export function eventRows(events) {
  return events
    .map((e, i) => ({ e, i }))
    .sort((a, b) => b.e.at_ms - a.e.at_ms || b.i - a.i)
    .map(({ e, i }) => {
      const known = Object.hasOwn(EVENT, e.event) ? EVENT[e.event] : null;
      const { glyph, tone, text } = known ?? kind("·", "dim", () => e.event);
      return { key: `${i}`, time: clockText(e.at_ms), glyph, text: text(e), tone };
    });
}

const shortId = (id) => {
  const m = /^b\d{8}-(.+)$/.exec(id);
  return (m ? m[1] : id).slice(0, SHORT_ID);
};

export function batchRows(batches) {
  return batches.map((b) => {
    const parts = [
      b.stepTimes,
      b.skipped > 0 ? `${b.skipped} skipped` : "",
      b.reruns > 0 ? `${b.reruns} ${b.reruns === 1 ? "rerun" : "reruns"}` : "",
    ].filter(Boolean);
    return {
      key: b.id,
      id: shortId(b.id),
      started: b.started === null ? "" : clockText(b.started),
      took: b.seconds === null ? "—" : formatDuration(b.seconds),
      merged: prText(b.merged),
      bounced: prText(b.bounced),
      reverted: b.reverted,
      steps: parts.length > 0 ? parts.join(" · ") : null,
    };
  });
}
