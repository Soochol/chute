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

const ACTION_LABEL = table({ approve: "승인", retry: "다시 시도", drop: "빼기" });

const SECTION_ORDER = ["alerts", "human", "batch", "waiting", "bounced", "unknown", "today"];

const SECTION_TITLE = {
  alerts: "멈춤·경고",
  human: "사람 차례",
  batch: "검증 중",
  waiting: "대기",
  bounced: "반송",
  unknown: "알 수 없는 상태",
  today: "오늘",
};

const HUMAN_REASON = table({
  budget: "재시도 예산을 다 썼다",
  unreachable: "에이전트에게 전달하지 못했다",
  gave_up: "에이전트가 포기를 선언했다",
});

const BOUNCE_REASON = table({
  conflict: "충돌",
  verify: "검증 실패",
  timeout: "시간 초과",
  head_changed: "HEAD 변경",
  ci: "CI 실패",
});

const FAILURE_MODE = table({ solo: "단독", combo: "조합" });
const PHASE = table({ building: "합치는 중", verifying: "검증 중", bisecting: "이분 탐색 중", pushing: "push 중" });
const APPROVAL = table({ agent: "에이전트 신뢰", human: "사람 확인" });
const BLOCK_REASON = table({ base_broken: "기준 커밋이 검증에 실패해 큐가 멈췄다" });
const PREFLIGHT = table({
  pass: { text: "preflight 통과", tone: "ok" },
  fail: { text: "preflight 실패", tone: "error" },
  conflict: { text: "preflight 충돌", tone: "error" },
});

const plain = (v) => {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v) ?? "";
  } catch {
    return "(표시할 수 없는 값)";
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
  firstLine(r.stderr) || firstLine(r.stdout) || (Number.isInteger(r.code) ? `종료 코드 ${r.code}` : "비정상 종료");

export function formatDuration(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const s = Math.round(seconds);
  if (s < 60) return `${s}초`;
  const m = Math.floor(s / 60);
  if (m < 60) return s % 60 ? `${m}분 ${s % 60}초` : `${m}분`;
  const h = Math.floor(m / 60);
  return m % 60 ? `${h}시간 ${m % 60}분` : `${h}시간`;
}

// Mirrors `Sha` in src/model.rs: lowercase hex, 4 to 64 characters.
const SAFE_HEAD = /^[0-9a-f]{4,64}$/;

export const actionArgs = (verb, pr, repoPath, head) =>
  verb === "approve" ? [verb, String(pr), "--head", head, "--repo", repoPath] : [verb, String(pr), "--repo", repoPath];

function agentOf(agent) {
  if (!isObject(agent) || !str(agent.terminal)) return null;
  const name = str(agent.name);
  return {
    terminal: agent.terminal,
    name: name || null,
    alive: agent.alive === true,
    label: name ? `${name} (${agent.terminal})` : agent.terminal,
  };
}

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
  if (!isObject(record)) return { text: "preflight 없음", tone: "muted", stale: false };
  const result = rawText(record.result) ?? "?";
  const known = PREFLIGHT[result] ?? { text: `preflight ${result}`, tone: "muted" };
  const stale = record.head !== item.head;
  return {
    text: stale ? `${known.text} (이전 HEAD)` : known.text,
    tone: stale ? "muted" : known.tone,
    stale,
  };
}

function actionsOf(item, repoPath) {
  if (!repoPath || !Number.isInteger(item.pr)) return [];
  const head = typeof item.head === "string" && SAFE_HEAD.test(item.head) ? item.head : null;
  return (ACTIONS_OF_STATE[item.state] ?? [])
    .filter((verb) => verb !== "approve" || head !== null)
    .map((verb) => ({
      verb,
      label: ACTION_LABEL[verb],
      args: actionArgs(verb, item.pr, repoPath, head),
    }));
}

function cardOf(item, repoPath) {
  const card = {
    key: `${repoPath}#${item.pr}`,
    pr: item.pr,
    title: str(item.title),
    state: item.state,
    agent: agentOf(item.agent),
    retries: `${num(item.attempts)}/${num(item.budget)}`,
    failure: failureOf(item),
    actions: actionsOf(item, repoPath),
  };
  if (item.state === "awaiting_approval") {
    card.reason = "승인 대기";
    card.preflight = preflightOf(item);
    card.config = item.touches_config === true ? "chute.toml 변경" : "chute.toml 변경 없음";
    card.configChanged = item.touches_config === true;
    card.approvalBlocked = card.actions.some((a) => a.verb === "approve") ? null : "HEAD를 알 수 없어 승인할 수 없다";
  } else if (item.state === "needs_human") {
    card.reason = named(HUMAN_REASON, item.state_reason) ?? "사람 차례";
  } else if (item.state === "bounced") {
    card.reason = named(BOUNCE_REASON, item.state_reason) ?? named(BOUNCE_REASON, card.failure?.kind || null) ?? "사유 없음";
  }
  return card;
}

function alertsOf(view, now) {
  const alerts = [];
  if (view.blocked) {
    const blocked = isObject(view.blocked) ? view.blocked : { reason: view.blocked };
    const reason = rawText(blocked.reason) ?? "";
    const parts = [];
    if (str(blocked.base)) parts.push(`기준 ${blocked.base}`);
    if (Number.isFinite(blocked.since_ms) && now >= blocked.since_ms) parts.push(`${formatDuration((now - blocked.since_ms) / 1000)} 전부터`);
    alerts.push({
      kind: "blocked",
      tone: "error",
      text: BLOCK_REASON[reason] ?? `큐가 멈췄다: ${reason || "사유 모름"}`,
      detail: parts.join(" · "),
    });
  }
  if (isObject(view.daemon) && view.daemon.running === false) {
    alerts.push({ kind: "daemon", tone: "error", text: "데몬이 꺼져 있다", detail: "`chute up`으로 켠다" });
  }
  for (const warning of list(view.warnings)) {
    const words = typeof warning === "string" ? [warning] : [warning?.message, warning?.kind];
    alerts.push({ kind: "warning", tone: "warn", text: words.map((w) => str(w).trim()).find(Boolean) ?? "경고", detail: "" });
  }
  return alerts;
}

function batchOf(batch, members, now) {
  if (!isObject(batch) && members.length === 0) return [];
  const b = isObject(batch) ? batch : {};
  const unlisted = new Map();
  for (const item of members) unlisted.set(item.pr, [...(unlisted.get(item.pr) ?? []), item]);
  const prs = list(b.prs).map((pr) => ({ pr: prOf(pr), title: str(unlisted.get(pr)?.shift()?.title) }));
  for (const rest of unlisted.values()) {
    for (const item of rest) prs.push({ pr: item.pr, title: str(item.title) });
  }
  return [
    {
      prs,
      phase: b.phase == null ? null : named(PHASE, b.phase),
      step: str(b.step) || null,
      round: Number.isFinite(b.round) && b.round > 0 ? `${b.round}번째 검증` : null,
      elapsed: Number.isFinite(b.started_ms) ? formatDuration(Math.max(0, (now - b.started_ms) / 1000)) : null,
    },
  ];
}

function todayOf(today) {
  if (!isObject(today)) return [];
  return [
    { key: "merged", label: "머지", value: num(today.merged) },
    { key: "bounced", label: "반송", value: num(today.bounced) },
    { key: "flaky", label: "flaky", value: num(today.flaky) },
    { key: "batches", label: "batch", value: num(today.batches) },
    { key: "lead", label: "머지까지 중앙값", value: formatDuration(today.median_lead_s) },
    { key: "verify", label: "검증 시간", value: formatDuration(today.verify_s) },
  ];
}

const submittedAt = (item) => (Number.isFinite(item.submitted_ms) ? item.submitted_ms : Infinity);
const bySubmission = (a, b) => {
  const [x, y] = [submittedAt(a), submittedAt(b)];
  if (x !== y) return x < y ? -1 : 1;
  const [p, q] = [Number.isInteger(a.pr), Number.isInteger(b.pr)];
  if (p && q) return a.pr - b.pr;
  return p === q ? 0 : p ? -1 : 1;
};

function repoBoard(view, now) {
  const repo = isObject(view.repo) ? view.repo : {};
  const path = str(repo.path);
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
        title: str(item.title),
        position: placed.waiting.length + 1,
        ciPending,
        label: ciPending ? "CI 대기" : "대기",
        note: item.state_reason && !ciPending ? item.state_reason : null,
      });
    } else if (section === "unknown") {
      placed.unknown.push({
        pr: item.pr,
        title: str(item.title),
        state: item.state ?? "(없음)",
        reason: item.state_reason,
      });
    } else placed[section].push(cardOf(item, path));
  }
  placed.alerts = alertsOf(view, now);
  placed.batch = batchOf(view.batch, inBatch, now);
  placed.today = todayOf(view.today);

  return {
    slug: str(repo.slug) || "(이름 없는 저장소)",
    stateDir: str(repo.state_dir),
    approval: named(APPROVAL, view.approval),
    sections: SECTION_ORDER.filter((id) => placed[id].length > 0).map((id) => ({
      id,
      title: SECTION_TITLE[id],
      items: placed[id],
    })),
  };
}

const failed = (detail) => ({ kind: "failed", title: "상태를 읽지 못했다", detail, hint: "" });

export function interpret(run, clock = Date.now()) {
  const now = Number.isFinite(clock) ? clock : Date.now();
  const r = isObject(run) ? run : {};
  if (r.spawnError != null) {
    return {
      kind: "missing",
      title: "chute를 실행할 수 없다",
      detail: firstLine(r.spawnError),
      hint: "저장소에서 `cargo install --path .`을 실행하거나 target/release/chute를 PATH에 둔다. Herdr가 찾지 못하면 mod의 props `bin`에 실행 파일 경로를 준다.",
    };
  }
  if (r.code !== 0) return failed(failureText(r));

  let data;
  try {
    data = JSON.parse(str(r.stdout));
  } catch {
    const excerpt = firstLine(r.stdout).slice(0, 120);
    return failed(`chute status가 JSON이 아닌 출력을 냈다${excerpt ? `: ${excerpt}` : ""}`);
  }
  const views = (Array.isArray(data) ? data : [data]).filter(isObject);
  if (Array.isArray(data) && data.length === 0) {
    return {
      kind: "empty",
      title: "등록된 저장소가 없다",
      detail: "",
      hint: "저장소에서 `chute init`을 실행한다.",
    };
  }
  if (views.length === 0) return failed("chute status의 출력이 저장소 상태가 아니다");
  return { kind: "board", repos: views.map((view) => repoBoard(view, now)) };
}
