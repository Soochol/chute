const table = (entries) => Object.assign(Object.create(null), entries);

const SECTION_OF_STATE = table({
  awaiting_approval: "human",
  needs_human: "human",
  in_batch: "batch",
  queued: "waiting",
  bounced: "bounced",
});

const ACTIONS_OF_STATE = table({
  awaiting_approval: ["approve", "drop"],
  needs_human: ["retry", "drop"],
});

const ACTION_LABEL = table({ approve: "Approve", retry: "Retry", drop: "Drop" });

// The one place that turns a meaning into a theme token name; `main.js` reads `theme()[name]`.
const TONE_TOKEN = table({
  ok: "ansi2",
  error: "ansi1",
  working: "working",
  human: "blocked",
  warn: "ansi3",
  waiting: "muted",
});

export const toneToken = (tone) => TONE_TOKEN[tone] ?? "dim";

// Mirrors `WarningKind` in src/model.rs.
const WARNING_TITLE = table({
  not_merged: "Not merged",
  no_checks: "No checks",
  config: "Can't read config",
  history: "Can't read history",
  engine: "Engine problem",
  hub: "Hub problem",
  flaky_chronic: "Chronic flaky test",
  gc: "Cleanup incomplete",
  post: "Post-merge check couldn't run",
});

const SECTION_ORDER = ["alerts", "human", "batch", "waiting", "bounced", "unknown"];

// These four keep their head line when empty; `alerts` and `unknown` appear only with something to show.
const ALWAYS_SHOWN = new Set(["human", "batch", "waiting", "bounced"]);

const SECTION_TITLE = {
  alerts: "Alerts",
  human: "Needs you",
  batch: "Current batch",
  waiting: "Queued",
  bounced: "Bounced",
  unknown: "Unknown state",
};

const HUMAN_REASON = table({
  budget: "Out of retries",
  unreachable: "Couldn't reach the agent",
  gave_up: "Agent gave up",
});

const BOUNCE_REASON = table({
  conflict: "Conflict",
  verify: "Verification failed",
  timeout: "Timed out",
  head_changed: "HEAD changed",
  ci: "CI failed",
});

const FAILURE_MODE = table({ solo: "Solo", combo: "Combined" });
const PHASE = table({ building: "Building", verifying: "Verifying", bisecting: "Bisecting", pushing: "Pushing" });
const APPROVAL = table({ agent: "Agent trusted", human: "Human review" });
const BLOCK_REASON = table({ base_broken: "Queue stopped: base commit failed verification" });
const PREFLIGHT = table({
  pass: { text: "Preflight passed", tone: "ok" },
  fail: { text: "Preflight failed", tone: "error" },
  conflict: { text: "Preflight conflict", tone: "error" },
  base_broken: { text: "Preflight: base broken", tone: "error" },
  skipped: { text: "Preflight skipped", tone: "muted" },
});

const plain = (v) => {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v) ?? "";
  } catch {
    return "(can't display)";
  }
};
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const list = (v) => (Array.isArray(v) ? v : []);
const str = (v) => (typeof v === "string" ? v : "");
const num = (v) => (Number.isFinite(v) ? String(v) : "—");
const prOf = (v) => (Number.isSafeInteger(v) && v > 0 ? v : "?");
const blank = (v) => typeof v === "string" && v.trim() === "";
const rawText = (v) => (v == null || blank(v) ? null : plain(v));
const named = (names, code) => {
  if (code == null || blank(code)) return null;
  const key = plain(code);
  return names[key] ?? key;
};

export const firstLine = (value) =>
  (typeof value === "string" ? value : "")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find(Boolean) ?? "";

export const causeText = (cause) =>
  firstLine(typeof cause === "string" ? cause : typeof cause?.message === "string" ? cause.message : "");

export const failureText = (r) =>
  firstLine(r.stderr) || firstLine(r.stdout) || (Number.isInteger(r.code) ? `Exit code ${r.code}` : "Exited abnormally");

export function formatDuration(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const s = Math.round(seconds);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return s % 60 ? `${m}m ${s % 60}s` : `${m}m`;
  const h = Math.floor(m / 60);
  return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
}

// Mirrors `Sha` in src/model.rs: lowercase hex, 4 to 64 characters.
const SAFE_HEAD = /^[0-9a-f]{4,64}$/;

const safeHeadOf = (item) => (typeof item.head === "string" && SAFE_HEAD.test(item.head) ? item.head : null);

export const actionArgs = (verb, pr, repoPath, head) =>
  verb === "approve" ? [verb, String(pr), "--head", head, "--repo", repoPath] : [verb, String(pr), "--repo", repoPath];

// Herdr takes `machine/address` for a terminal on another machine (herdr-app AGENTS.md).
function agentOf(agent) {
  if (!isObject(agent) || !str(agent.terminal)) return null;
  const name = str(agent.name);
  const remote = str(agent.machine) !== "" || agent.terminal.includes("/");
  return {
    terminal: agent.terminal,
    name: name || null,
    alive: agent.alive === true,
    remote,
    host: remote ? str(agent.host) || str(agent.machine) || null : null,
    label: name ? `${name} (${agent.terminal})` : agent.terminal,
  };
}

function marksOf(item) {
  const marks = [];
  if (item.priority === true) marks.push({ kind: "priority", text: "Urgent", tone: "warn" });
  if (item.repair === true) marks.push({ kind: "repair", text: "Repair", tone: "error" });
  const after = [...new Set(list(item.after).filter((pr) => Number.isSafeInteger(pr) && pr > 0))];
  if (after.length > 0) marks.push({ kind: "after", text: `after ${after.map((pr) => `#${pr}`).join(", ")}`, tone: "muted" });
  return marks;
}

function submitterOf(item) {
  const submitter = agentOf(item.submitter);
  if (!submitter || submitter.terminal === agentOf(item.agent)?.terminal) return null;
  return { ...submitter, text: `submit ${submitter.label}` };
}

function hubOf(hub) {
  const h = isObject(hub) ? hub : {};
  const local = h.local !== false;
  return {
    local,
    reachable: h.reachable !== false,
    error: str(h.error),
    label: local ? "this PC" : str(h.host) || str(h.machine) || "unknown hub",
    machine: local ? null : str(h.machine) || null,
  };
}

export const REMOTE_POLL_MS = 5000;
export const LOCAL_POLL_MS = 15000;

export const pollInterval = (board) =>
  board?.kind === "board" && board.repos.some((repo) => !repo.hub.local) ? REMOTE_POLL_MS : LOCAL_POLL_MS;

function failureOf(item) {
  const failure = item.last_failure;
  if (!isObject(failure)) return null;
  return {
    kind: str(failure.kind),
    mode: named(FAILURE_MODE, failure.mode),
    summary: str(failure.summary),
    report: str(failure.report),
  };
}

function preflightOf(item) {
  const record = item.preflight;
  if (!isObject(record)) return { text: "No preflight", tone: "muted", stale: false };
  const result = rawText(record.result) ?? "?";
  const known = PREFLIGHT[result] ?? { text: `Preflight ${result}`, tone: "muted" };
  const stale = record.head !== item.head;
  return {
    text: stale ? `${known.text} (old HEAD)` : known.text,
    tone: stale ? "muted" : known.tone,
    stale,
  };
}

function actionsOf(item, repoPath) {
  if (!repoPath || !Number.isInteger(item.pr)) return [];
  const head = safeHeadOf(item);
  return (ACTIONS_OF_STATE[item.state] ?? [])
    .filter((verb) => verb !== "approve" || head !== null)
    .map((verb) => ({
      verb,
      label: ACTION_LABEL[verb],
      args: actionArgs(verb, item.pr, repoPath, head),
    }));
}

function cardOf(item, repoPath, slug, now) {
  const card = {
    key: `${repoPath}#${item.pr}`,
    pr: item.pr,
    url: pullUrl(slug, item.pr),
    title: str(item.title),
    state: item.state,
    agent: agentOf(item.agent),
    retries: `${num(item.attempts)}/${num(item.budget)}`,
    failure: failureOf(item),
    actions: actionsOf(item, repoPath),
    marks: marksOf(item),
    submitter: submitterOf(item),
    wait: item.state === "bounced" ? "" : waitOf(item, now),
    ago: item.state === "bounced" && Number.isFinite(item.bounced_ms) ? formatAgo(now - item.bounced_ms) : "",
  };
  card.report = card.failure?.report ? { label: "Report", path: card.failure.report } : null;
  if (item.state === "awaiting_approval") {
    card.reason = "Awaiting approval";
    card.preflight = preflightOf(item);
    card.config = item.touches_config === true ? "chute.toml changed" : "chute.toml unchanged";
    card.configChanged = item.touches_config === true;
    card.approvalBlocked = safeHeadOf(item) ? null : "Can't approve: HEAD unknown";
  } else if (item.state === "needs_human") {
    card.reason = named(HUMAN_REASON, item.state_reason) ?? "Needs you";
  } else if (item.state === "bounced") {
    card.reason = named(BOUNCE_REASON, item.state_reason) ?? named(BOUNCE_REASON, card.failure?.kind || null) ?? "No reason";
    card.headline = card.failure?.mode ? `${card.reason} · ${card.failure.mode}` : card.reason;
  }
  return card;
}

const REPAIR_STATE = table({ queued: "queued", in_batch: "verifying" });

function repairFacts(queue) {
  return queue
    .filter((item) => item.repair === true)
    .map((item) => `Repair PR #${item.pr} ${REPAIR_STATE[item.state] ?? "needs attention"}`);
}

function haltFacts(blocked, queue) {
  const facts = [];
  if (str(blocked.step)) facts.push(`Failed step ${blocked.step}`);
  if (blocked.pushed_by_chute === true) facts.push("Commit pushed by chute");
  else if (blocked.pushed_by_chute === false) facts.push("Commit not pushed by chute");
  if (blocked.rechecked === true) facts.push("Rechecked");
  return [...facts, ...repairFacts(queue)];
}

function alertsOf(view, queue, now, merged) {
  const alerts = [];
  if (view.blocked) {
    const blocked = isObject(view.blocked) ? view.blocked : { reason: view.blocked };
    const reason = rawText(blocked.reason) ?? "";
    const parts = [];
    if (str(blocked.base)) parts.push(`base ${blocked.base}`);
    if (Number.isFinite(blocked.since_ms) && now >= blocked.since_ms) parts.push(`for ${formatDuration((now - blocked.since_ms) / 1000)}`);
    alerts.push({
      kind: "blocked",
      tone: "error",
      text: BLOCK_REASON[reason] ?? `Queue stopped: ${reason || "unknown reason"}`,
      detail: parts.join(" · "),
      facts: haltFacts(blocked, queue),
      files: [fileOf("Log", blocked.log)].filter(Boolean),
      log: str(blocked.log) || null,
    });
  }
  if (isObject(view.daemon) && view.daemon.running === false) {
    alerts.push({ kind: "daemon", tone: "error", text: "Daemon is off", detail: "Run `chute up` to start it.", facts: [], files: [], log: null });
  }
  alerts.push(...postVerifyAlerts(view.post_verify, now, merged));
  for (const warning of list(view.warnings)) {
    const message = typeof warning === "string" ? warning : warning?.message;
    const kind = typeof warning === "string" ? "" : str(warning?.kind);
    const title = kind ? WARNING_TITLE[kind] ?? "Warning" : "Warning";
    alerts.push({ kind: "warning", tone: "warn", text: title, detail: str(message).trim(), facts: [], files: [], log: null });
  }
  return alerts;
}

const shortSha = (v) => str(v).slice(0, 7);

export function formatAgo(ms) {
  if (!Number.isFinite(ms) || ms < 0) return "";
  const s = Math.floor(ms / 1000);
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

// Whole minutes, so a value polled every 15 seconds doesn't flicker: `<1m`, `3m`, `2h 5m`, `2h`, `3d`.
export function formatWait(ms) {
  if (!Number.isFinite(ms) || ms < 0) return "";
  const m = Math.floor(ms / 60000);
  if (m < 1) return "<1m";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

const waitOf = (item, now) => (Number.isFinite(item.submitted_ms) ? `waiting ${formatWait(now - item.submitted_ms)}` : "");

const SLUG = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

export function pullUrl(slug, pr) {
  if (!SLUG.test(str(slug)) || slug.split("/").some((part) => part === "." || part === "..")) return null;
  return Number.isSafeInteger(pr) && pr > 0 ? `https://github.com/${slug}/pull/${pr}` : null;
}

export const MIN_USUAL_SAMPLES = 3;

export function usualSeconds(recent) {
  const samples = list(recent)
    .map((r) => (isObject(r) ? r.seconds : null))
    .filter((v) => Number.isFinite(v) && v >= 0)
    .sort((a, b) => a - b);
  if (samples.length < MIN_USUAL_SAMPLES) return null;
  const mid = Math.floor(samples.length / 2);
  return samples.length % 2 ? samples[mid] : (samples[mid - 1] + samples[mid]) / 2;
}

const RECENT_MERGED_SHOWN = 5;

function recentMergedOf(view, slug, now) {
  const row = (pr, title, bounces, at, submitted) => {
    const ago = Number.isFinite(at) ? formatAgo(now - at) : "";
    const took = Number.isFinite(at) && Number.isFinite(submitted) && submitted <= at ? formatDuration((at - submitted) / 1000) : "";
    return {
      pr: prOf(pr),
      title: str(title),
      bounces: Number.isSafeInteger(bounces) && bounces > 0 ? bounces : 0,
      ago,
      took,
      tip: ago ? `Merged ${ago}${clockText(at) ? ` · ${clockText(at)}` : ""}` : "",
      url: pullUrl(slug, pr),
    };
  };
  if (Array.isArray(view.recent_merged)) {
    return view.recent_merged
      .filter(isObject)
      .slice(0, RECENT_MERGED_SHOWN)
      .map((m) => row(m.pr, m.title, m.bounces, m.merged_ms, m.submitted_ms));
  }
  return list(view.recent)
    .filter(isObject)
    .flatMap((r) => list(r.merged).map((pr) => row(pr, "", 0, r.finished_ms)))
    .slice(0, RECENT_MERGED_SHOWN);
}

const fileOf = (label, path) => (str(path) ? { label, path: str(path) } : null);

function postVerifyAlerts(post, now, merged) {
  const red = isObject(post) && isObject(post.red) ? post.red : null;
  if (!red) return [];
  const culprit = isObject(red.culprit) ? red.culprit : null;
  const culpritTitle = culprit ? merged.find((m) => m.pr === culprit.pr)?.title : "";
  const facts = [];
  if (culprit && Number.isSafeInteger(culprit.pr)) facts.push(`Culprit #${culprit.pr}${culpritTitle ? ` ${culpritTitle}` : ""}`);
  else if (culprit) facts.push(`Culprit commit ${shortSha(culprit.commit)}`);
  else facts.push("Culprit merge not found");
  const tests = list(red.tests).filter((t) => typeof t === "string" && t);
  if (tests.length > 0) facts.push(`Failed tests ${tests.join(", ")}`);
  if (str(red.last_green)) facts.push(`Last green ${shortSha(red.last_green)}`);
  if (red.holding === true) facts.push("Related PRs wait until it's fixed");
  if (culprit && str(culprit.batch)) facts.push(`To undo: chute revert ${culprit.batch}`);
  const parts = [shortSha(red.commit)];
  if (Number.isFinite(red.since_ms) && now >= red.since_ms) parts.push(`for ${formatDuration((now - red.since_ms) / 1000)}`);
  const files = [fileOf("Report", red.report), fileOf("Log", red.log)].filter(Boolean);
  return [
    {
      kind: "post_verify",
      tone: "error",
      text: `Post-merge check failed · ${str(red.step) || "?"}`,
      detail: parts.join(" · "),
      facts,
      files,
      log: str(red.log) || null,
    },
  ];
}

function postVerifyOf(post, now) {
  if (!isObject(post)) return null;
  const green = isObject(post.last_green) ? post.last_green : null;
  const run = isObject(post.run) ? post.run : null;
  if (!green && !run) return null;
  return {
    green: green
      ? { commit: shortSha(green.commit), ago: Number.isFinite(green.at_ms) ? formatAgo(now - green.at_ms) : "" }
      : null,
    run: run
      ? {
          commit: shortSha(run.commit),
          label: run.phase === "searching" ? "Finding culprit" : "Checking",
          step: str(run.step) || null,
          waitingMachine: run.waiting_machine === true,
          elapsed: Number.isFinite(run.started_ms) ? formatDuration(Math.max(0, (now - run.started_ms) / 1000)) : null,
        }
      : null,
  };
}

function batchOf(batch, members, now, usualS, slug) {
  if (!isObject(batch) && members.length === 0) return [];
  const b = isObject(batch) ? batch : {};
  const unlisted = new Map();
  for (const item of members) unlisted.set(item.pr, [...(unlisted.get(item.pr) ?? []), item]);
  const entryOf = (pr, item) => ({
    pr,
    url: pullUrl(slug, pr),
    title: str(item?.title),
    marks: item ? marksOf(item) : [],
    submitter: item ? submitterOf(item) : null,
  });
  const prs = list(b.prs).map((pr) => entryOf(prOf(pr), unlisted.get(pr)?.shift()));
  for (const rest of unlisted.values()) {
    for (const item of rest) prs.push(entryOf(item.pr, item));
  }
  const step = str(b.step) || null;
  const steps = list(b.steps).filter((name) => typeof name === "string" && name);
  const at = step ? steps.indexOf(step) : -1;
  const stepText = step && at >= 0 ? `${step} ${at + 1}/${steps.length}` : step;
  const elapsedS = Number.isFinite(b.started_ms) ? Math.max(0, (now - b.started_ms) / 1000) : null;
  const timed = usualS != null && usualS > 0 && elapsedS != null;
  return [
    {
      prs,
      phase: b.phase == null ? null : named(PHASE, b.phase),
      step,
      stepText,
      waitingMachine: b.waiting_machine === true,
      reused: b.reused === "preflight" || b.reused === "batch" ? `Reused ${b.reused} pass` : null,
      round: Number.isFinite(b.round) && b.round > 0 ? `Round ${b.round}` : null,
      elapsed: elapsedS != null ? formatDuration(elapsedS) : null,
      usual: usualS != null ? `usually ${formatDuration(usualS)}` : null,
      progress: timed ? Math.min(1, elapsedS / usualS) : null,
      longer: timed && elapsedS > usualS,
    },
  ];
}

const chronicFlaky = (view) =>
  list(view.warnings).some((warning) => isObject(warning) && warning.kind === "flaky_chronic");

function todayOf(today, chronic) {
  if (!isObject(today)) return null;
  return {
    parts: [`${num(today.merged)} merged`, `${num(today.bounced)} bounced`, `median ${formatDuration(today.median_lead_s)}`],
    details: [
      { key: "flaky", label: "Flaky", value: num(today.flaky), note: chronic ? "chronic" : null },
      { key: "batches", label: "Batches", value: num(today.batches) },
      {
        key: "verify",
        label: "Total verify time",
        value: formatDuration(today.verify_s),
        hint: Number.isFinite(today.batches) ? `(sum of ${today.batches} ${today.batches === 1 ? "batch" : "batches"})` : null,
      },
    ],
  };
}

const submittedAt = (item) => (Number.isFinite(item.submitted_ms) ? item.submitted_ms : Infinity);
const bySubmission = (a, b) => {
  const [x, y] = [submittedAt(a), submittedAt(b)];
  if (x !== y) return x < y ? -1 : 1;
  const [p, q] = [Number.isInteger(a.pr), Number.isInteger(b.pr)];
  if (p && q) return a.pr - b.pr;
  return p === q ? 0 : p ? -1 : 1;
};

const BLOCK_SHORT = table({ base_broken: "base failed verification" });

function statusLineOf(view, queue, batch) {
  const red = isObject(view.post_verify) && isObject(view.post_verify.red) ? view.post_verify.red : null;
  if (view.blocked) {
    const reason = rawText(isObject(view.blocked) ? view.blocked.reason : view.blocked) ?? "";
    return { tone: "error", text: `Stopped · ${BLOCK_SHORT[reason] ?? (reason || "unknown reason")}` };
  }
  if (red) return { tone: "error", text: `Post-merge check failed · ${str(red.step) || "?"}` };
  if (isObject(view.daemon) && view.daemon.running === false) return { tone: "error", text: "Daemon is off" };
  const human = queue.filter((item) => item.state === "awaiting_approval" || item.state === "needs_human").length;
  if (human > 0) return { tone: "human", text: `Needs you · ${human}` };
  if (batch) {
    if (batch.waitingMachine) return { tone: "working", text: "Waiting for a slot" };
    return { tone: "working", text: batch.stepText ? `Verifying · ${batch.stepText}` : "Verifying" };
  }
  const waiting = queue.filter((item) => item.state === "queued").length;
  if (waiting > 0) return { tone: "waiting", text: `Queued · ${waiting}` };
  return { tone: "ok", text: "All clear" };
}

function noticesOf(view, queue, alerts) {
  const notices = [];
  const blocked = alerts.find((a) => a.kind === "blocked");
  if (blocked) {
    const since = isObject(view.blocked) && Number.isFinite(view.blocked.since_ms) ? view.blocked.since_ms : "";
    notices.push({ key: `blocked:${since}`, level: "error", body: blocked.text, file: blocked.files[0] ?? null });
  }
  const red = alerts.find((a) => a.kind === "post_verify");
  if (red) {
    const commit = isObject(view.post_verify?.red) ? str(view.post_verify.red.commit) : "";
    notices.push({ key: `red:${commit}`, level: "error", body: red.text, file: red.files[0] ?? null });
  }
  for (const item of queue) {
    const head = str(item.head);
    if (item.state === "awaiting_approval") {
      notices.push({ key: `approval:${item.pr}:${head}`, level: "warning", body: `#${item.pr} awaiting approval · ${str(item.title)}`, file: null });
    } else if (item.state === "needs_human") {
      const reason = named(HUMAN_REASON, item.state_reason) ?? "Needs you";
      const report = str(item.last_failure?.report);
      notices.push({
        key: `human:${item.pr}:${item.state_reason ?? ""}`,
        level: "warning",
        body: `#${item.pr} needs you · ${reason}`,
        file: report ? { label: "Report", path: report } : null,
      });
    }
  }
  return notices;
}

export function diffNotices(prev, board) {
  const next = new Map();
  const fresh = [];
  if (board?.kind !== "board") return { fresh, next: prev ?? null };
  for (const repo of board.repos) {
    if (!repo.hub.reachable) {
      if (prev?.has(repo.id)) next.set(repo.id, prev.get(repo.id));
      continue;
    }
    next.set(repo.id, new Set(repo.notices.map((n) => n.key)));
    const before = prev?.get(repo.id);
    if (!before) continue;
    for (const notice of repo.notices) if (!before.has(notice.key)) fresh.push({ repo, ...notice });
  }
  return { fresh, next };
}

export function badgeText(board) {
  if (board?.kind !== "board") return null;
  const count = board.repos.filter((repo) => repo.hub.reachable).reduce((sum, repo) => sum + repo.notices.length, 0);
  return count > 0 ? String(count) : null;
}

export const needsClock = (board) => board?.kind === "board" && board.repos.some((repo) => repo.active);

export const RECENT_DEFAULT = 3;

// The recent merges to draw: the first few, or all of them when `open`. `hidden` is how many `+ N more` stands for.
export function recentWindow(recent, open) {
  const rows = open ? recent : recent.slice(0, RECENT_DEFAULT);
  return { rows, hidden: Math.max(0, recent.length - RECENT_DEFAULT), more: recent.length - rows.length };
}

// Most urgent first. A repo's grade is its status line's tone.
const GRADE = table({ error: 0, human: 0, working: 1, waiting: 2, ok: 3 });
export const gradeOf = (tone) => GRADE[tone] ?? 3;

const DAEMON_OFF_KEY = "daemon:off";

// What the next fold state is for one repo.
// `prev` is `{ keys, auto }` from the last read (`keys` null before the first read), `keys` the problem keys now,
// `saved` whether the user left the repo folded. A folded repo opens by itself while a key it did not have before
// is on it, and folds again once every key is gone.
export function nextFold(prev, keys, saved) {
  const had = prev?.keys instanceof Set ? prev.keys : new Set();
  const now = new Set(keys);
  const fresh = keys.some((key) => !had.has(key));
  const auto = !saved ? false : fresh ? true : now.size === 0 ? false : prev?.auto === true;
  return { folded: saved && !auto, auto, keys: now };
}

// The user pressed the head line: what they see flips and becomes their choice.
export const toggleFold = ({ saved, auto }) => ({ saved: !(saved && !auto), auto: false });

const TIME = (ms) => new Date(ms);
const two = (n) => String(n).padStart(2, "0");
export const clockText = (ms) => (Number.isFinite(ms) ? `${two(TIME(ms).getHours())}:${two(TIME(ms).getMinutes())}` : "");
export const clockExact = (ms) => {
  if (!Number.isFinite(ms)) return "";
  const d = TIME(ms);
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
};

const OVERALL = table({ error: "failed", human: "attention", working: "running", waiting: "running", ok: "ok" });
const URGENCY = ["failed", "attention", "running", "ok"];

// One number set for the head line and for `chute.status`. Null while nothing has been read.
export function overview(board) {
  if (board?.kind === "failed" || board?.kind === "missing") return { grade: "failed", issues: 1, running: 0, repos: 0 };
  if (board?.kind === "empty") return { grade: "ok", issues: 0, running: 0, repos: 0 };
  if (board?.kind !== "board") return null;
  const grades = board.repos.map((repo) => OVERALL[repo.status.tone] ?? "ok");
  const issues = board.repos.reduce((sum, repo) => {
    if (!repo.hub.reachable) return sum + 1;
    const human = repo.sections.find((s) => s.id === "human")?.items.length ?? 0;
    const alerts = repo.sections.find((s) => s.id === "alerts")?.items.filter((a) => a.tone === "error").length ?? 0;
    return sum + human + alerts;
  }, 0);
  return {
    grade: URGENCY.find((g) => grades.includes(g)) ?? "ok",
    issues,
    running: grades.filter((g) => g === "running").length,
    repos: board.repos.length,
  };
}

export const overviewText = (o) =>
  [`${o.repos} ${o.repos === 1 ? "repo" : "repos"}`, `${o.running} running`, `${o.issues} ${o.issues === 1 ? "issue" : "issues"}`];

function repoBoard(view, now) {
  const repo = isObject(view.repo) ? view.repo : {};
  const path = str(repo.path);
  const slug = str(repo.slug) || "(unnamed repo)";
  const hub = hubOf(view.hub);
  const base = {
    id: path || slug,
    slug,
    name: slug.split("/").pop() || slug,
    stateDir: str(repo.state_dir),
    hub,
    notices: [],
    problems: [],
    active: false,
    postVerify: null,
    recent: [],
    today: null,
  };
  if (!hub.reachable) {
    return {
      ...base,
      approval: null,
      status: { tone: "error", text: "Hub unreachable" },
      sections: [],
    };
  }
  const queue = list(view.queue)
    .filter(isObject)
    .map((item) => ({ ...item, pr: prOf(item.pr), state: rawText(item.state), state_reason: rawText(item.state_reason) }))
    .sort(bySubmission);
  const placed = Object.fromEntries(SECTION_ORDER.map((id) => [id, []]));
  const inBatch = [];
  for (const item of queue) {
    const section = SECTION_OF_STATE[item.state] ?? "unknown";
    if (section === "batch") inBatch.push(item);
    else if (section === "waiting") {
      const ciPending = item.state_reason === "ci_pending";
      placed.waiting.push({
        pr: item.pr,
        url: pullUrl(slug, item.pr),
        title: str(item.title),
        position: placed.waiting.length + 1,
        ciPending,
        label: ciPending ? "Waiting on CI" : "Queued",
        note: item.state_reason && !ciPending ? item.state_reason : null,
        marks: marksOf(item),
        submitter: submitterOf(item),
        wait: waitOf(item, now),
      });
    } else if (section === "unknown") {
      placed.unknown.push({
        pr: item.pr,
        title: str(item.title),
        state: item.state ?? "(none)",
        reason: item.state_reason,
      });
    } else placed[section].push(cardOf(item, path, slug, now));
  }
  const recent = recentMergedOf(view, slug, now);
  placed.alerts = alertsOf(view, queue, now, recent);
  placed.batch = batchOf(view.batch, inBatch, now, usualSeconds(view.recent), slug);

  const notices = noticesOf(view, queue, placed.alerts);
  const postVerify = postVerifyOf(view.post_verify, now);
  return {
    ...base,
    approval: named(APPROVAL, view.approval),
    status: statusLineOf(view, queue, placed.batch[0] ?? null),
    notices,
    problems: [...notices.map((n) => n.key), ...(placed.alerts.some((a) => a.kind === "daemon") ? [DAEMON_OFF_KEY] : [])],
    active: placed.batch.length > 0 || postVerify?.run != null,
    postVerify,
    recent,
    today: todayOf(view.today, chronicFlaky(view)),
    sections: SECTION_ORDER.filter((id) => ALWAYS_SHOWN.has(id) || placed[id].length > 0).map((id) => ({
      id,
      title: SECTION_TITLE[id],
      items: placed[id],
    })),
  };
}

const failed = (detail) => ({ kind: "failed", title: "Couldn't read status", detail, hint: "" });

export function interpret(run, clock = Date.now()) {
  const now = Number.isFinite(clock) ? clock : Date.now();
  const r = isObject(run) ? run : {};
  if (r.spawnError != null) {
    return {
      kind: "missing",
      title: "Can't run chute",
      detail: firstLine(r.spawnError),
      hint: "Run `cargo install --path .` in the repository, or put target/release/chute on PATH. If Herdr can't find it, set the mod's `bin` prop to the executable path.",
    };
  }
  if (r.code !== 0) return failed(failureText(r));

  let data;
  try {
    data = JSON.parse(str(r.stdout));
  } catch {
    const excerpt = firstLine(r.stdout).slice(0, 120);
    return failed(`chute status printed non-JSON output${excerpt ? `: ${excerpt}` : ""}`);
  }
  const views = (Array.isArray(data) ? data : [data]).filter(isObject);
  if (Array.isArray(data) && data.length === 0) {
    return {
      kind: "empty",
      title: "No repositories",
      detail: "",
      hint: "Run `chute init` in a repository.",
    };
  }
  if (views.length === 0) return failed("chute status output isn't repository state")
;
  const repos = views.map((view) => repoBoard(view, now)).sort((a, b) => gradeOf(a.status.tone) - gradeOf(b.status.tone));
  return { kind: "board", repos };
}
