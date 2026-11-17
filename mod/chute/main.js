import { View, div } from "gpui-kit";
import { v_flex, h_flex, Editor, EditorState } from "gpui-base";
import {
  checking,
  closeOverlay,
  exec,
  layout,
  nextCommand,
  notify,
  openOverlay,
  props,
  readTextFile,
  run,
  setBadge,
  settings,
  theme,
  watchFolder,
} from "herdr";
import {
  LOCAL_POLL_MS,
  REMOTE_POLL_MS,
  badgeText,
  causeText,
  diffNotices,
  failureText,
  interpret,
  needsClock,
  pollInterval,
} from "./board.js";

const TICK_MS = 1000;
const MIN_READ_GAP_MS = 1000;
const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const MAX_VIEW_CHARS = 200_000;
const FOLD_KEY = "chute.folded";
const todayKey = (repoId) => `chute.today.${repoId}`;

// The folder name this copy was installed under; commands to this mod are addressed `<name>.<command>`.
const MOD = (() => {
  try {
    return new URL(import.meta.url).pathname.split("/").slice(-2)[0] || "chute";
  } catch {
    return "chute";
  }
})();

const remember = (key, value) => {
  try {
    if (value) localStorage.setItem(key, "1");
    else localStorage.removeItem(key);
  } catch {}
};
const recall = (key) => {
  try {
    return localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
};

// The one place that turns a meaning into a theme token.
const toneColor = (name, t) =>
  ({ ok: t.done, working: t.working, human: t.blocked, error: t.ansi1, warn: t.ansi3, waiting: t.muted }[name] ?? t.dim);

// The layout node that holds a copy of this mod, as `layout()` reports it. Null when there is not exactly one.
function ownNodeId() {
  const found = [];
  const walk = (node) => {
    if (Array.isArray(node)) return node.forEach(walk);
    if (node === null || typeof node !== "object") return;
    if (node.content?.mod === MOD && typeof node.id === "string") found.push(node.id);
    Object.values(node).forEach(walk);
  };
  try {
    walk(layout());
  } catch {
    return null;
  }
  return found.length === 1 ? found[0] : null;
}

const fileName = (path) => path.split("/").pop() || path;

export default class Chute extends View {
  init(_props, cx) {
    const given = props() ?? {};
    this.file = typeof given.file === "string" && given.file ? given : null;
    this.overlay = typeof given.overlay === "string" ? given.overlay : null;
    if (this.file) {
      this.loadFile(cx);
      return;
    }
    const bin = given.bin;
    this.bin = typeof bin === "string" && bin ? bin : "chute";
    this.result = null;
    this.board = null;
    this.reading = false;
    this.dirty = false;
    this.watching = new Set();
    this.busy = new Map();
    this.errors = new Map();
    this.sinceRead = 0;
    this.pollMs = LOCAL_POLL_MS;
    this.folded = recall(FOLD_KEY);
    this.opened = new Set();
    this.noticed = null;
    this.badge = null;
    this.pendingFile = null;
    this.request(cx);
    cx.timer.every(TICK_MS, (cx) => this.tick(cx));
    if (!checking()) this.listen(cx);
  }

  listen(cx) {
    cx.spawn(async (cx) => {
      for (;;) {
        const command = await nextCommand();
        if (command === "open-file" && this.pendingFile) this.openFile(this.pendingFile.file, this.pendingFile.on);
        else if (command === "fold") this.fold(cx);
      }
    });
  }

  tick(cx) {
    this.sinceRead += TICK_MS;
    if (!this.reading && this.sinceRead >= this.pollMs) this.request(cx);
    else if (needsClock(this.board)) cx.notify();
  }

  request(cx) {
    if (this.reading) {
      this.dirty = true;
      return;
    }
    this.reading = true;
    this.sinceRead = 0;
    cx.spawn(async (cx) => {
      try {
        let result;
        try {
          const r = (await exec(this.bin, ["status", "--json", "--all"])) ?? {};
          result = { code: r.code, stdout: r.stdout, stderr: r.stderr };
        } catch (e) {
          result = { spawnError: causeText(e) };
        }
        this.result = result;
        this.board = interpret(result);
        this.settle(this.board, cx);
        cx.notify();
      } finally {
        cx.timer.after(MIN_READ_GAP_MS, (cx) => {
          this.reading = false;
          if (this.dirty) {
            this.dirty = false;
            this.request(cx);
          }
        });
      }
    });
  }

  settle(board, cx) {
    if (board.kind !== "board") return;
    this.pollMs = pollInterval(board);
    const cards = board.repos.flatMap((repo) => repo.sections).filter((s) => s.id === "human").flatMap((s) => s.items);
    for (const [key, error] of this.errors) {
      if (!cards.some((card) => card.key === key && card.state === error.state)) this.errors.delete(key);
    }
    for (const repo of board.repos) {
      this.watch(repo.stateDir, cx);
      if (!this.opened.has(`seen:${repo.id}`)) {
        this.opened.add(`seen:${repo.id}`);
        if (recall(todayKey(repo.id))) this.opened.add(repo.id);
      }
    }
    if (this.overlay || checking()) return;
    const { fresh, next } = diffNotices(this.noticed, board);
    this.noticed = next;
    for (const notice of fresh) this.announce(notice);
    const badge = badgeText(board);
    if (badge !== this.badge) {
      this.badge = badge;
      setBadge(badge);
    }
  }

  // The toast click runs `<mod>.open-file`, which `listen()` receives.
  announce(notice) {
    const on = notice.repo.hub.local ? null : notice.repo.hub.machine;
    const action = notice.file ? { name: `${MOD}.open-file` } : undefined;
    if (notice.file) this.pendingFile = { file: notice.file, on };
    notify({ title: `chute · ${notice.repo.name}`, body: notice.body, level: notice.level, action });
  }

  // watchFolder rejects a tree with more than 4096 folders (Linux); the fallback timer covers it.
  watch(dir, cx) {
    if (!dir || this.watching.has(dir)) return;
    this.watching.add(dir);
    cx.spawn(async (cx) => {
      try {
        for (;;) {
          await watchFolder(dir);
          this.request(cx);
        }
      } catch {
        this.watching.delete(dir);
      }
    });
  }

  act(card, action, cx) {
    if (this.busy.has(card.key)) return;
    this.busy.set(card.key, action.verb);
    this.errors.delete(card.key);
    cx.notify();
    cx.spawn(async (cx) => {
      let text = "";
      try {
        const r = (await exec(this.bin, action.args)) ?? {};
        if (r.code !== 0) text = failureText(r);
      } catch (e) {
        text = causeText(e) || "명령을 실행하지 못했다";
      }
      if (text) this.errors.set(card.key, { text, state: card.state });
      this.busy.delete(card.key);
      cx.notify();
      this.request(cx);
    });
  }

  fold(cx) {
    this.folded = !this.folded;
    remember(FOLD_KEY, this.folded);
    cx.notify();
  }

  toggleToday(repo, cx) {
    const open = !this.opened.has(repo.id);
    if (open) this.opened.add(repo.id);
    else this.opened.delete(repo.id);
    remember(todayKey(repo.id), open);
    cx.notify();
  }

  openFile(file, on) {
    openOverlay({
      kind: "window",
      key: "chute-file",
      mod: MOD,
      width: 760,
      height: 520,
      props: { file: file.path, on: on ?? null, title: `${file.label} · ${fileName(file.path)}` },
    });
  }

  loadFile(cx) {
    this.code = null;
    this.fileError = null;
    this.cut = null;
    cx.spawn(async (cx) => {
      try {
        const text = await readTextFile(this.file.file, { on: this.file.on ?? null });
        if (text === null) this.fileError = "파일이 없다";
        else {
          let value = text;
          if (text.length > MAX_VIEW_CHARS) {
            const tail = text.slice(text.length - MAX_VIEW_CHARS);
            value = tail.slice(tail.indexOf("\n") + 1);
            this.cut = text.length - value.length;
          }
          const t = theme();
          this.code = EditorState.new({
            language: this.file.file.endsWith(".md") ? "markdown" : "log",
            readonly: true,
            soft_wrap: true,
            value,
            font_family: settings().font_family,
            font_size: 12,
            syntax: { keyword: t.ansi5, string: t.ansi2, comment: t.dim, function: t.ansi4, type: t.ansi3, number: t.ansi1, constant: t.ansi6 },
          });
        }
      } catch (e) {
        this.fileError = `파일을 읽지 못했다: ${causeText(e) || "알 수 없는 오류"}`;
      }
      cx.notify();
    });
  }

  render() {
    const t = theme();
    const frame = v_flex()
      .id("chute")
      .size_full()
      .min_h(0)
      .rounded(10)
      .overflow_hidden()
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .text_color(t.foreground)
      .text_size(13);
    if (this.file) return this.fileView(frame, t);
    const body = this.result ? interpret(this.result) : { kind: "loading" };
    return frame.child(this.headerView(t)).child(
      v_flex()
        .id("chute-body")
        .flex_1()
        .min_h(0)
        .overflow_y_scrollbar()
        .p(12)
        .gap(16)
        .children(this.bodyOf(body, t)),
    );
  }

  fileView(frame, t) {
    const close = iconButton("chute-file-close", "닫기", t, () => this.overlay && closeOverlay(this.overlay));
    return frame
      .child(
        h_flex()
          .h(42)
          .flex_none()
          .px(14)
          .gap(8)
          .items_center()
          .border_b(1)
          .border_color(t.border)
          .child(div().flex_1().min_w(0).truncate().font_semibold().child(this.file.title || fileName(this.file.file)))
          .when(Boolean(this.cut), (el) =>
            el.child(div().flex_none().text_size(11).text_color(t.ansi3).child(`앞 ${this.cut}자는 잘랐다`)),
          )
          .child(close),
      )
      .child(
        this.code
          ? Editor.new(this.code).flex_1().min_h(0)
          : div()
              .p(14)
              .text_size(12)
              .text_color(this.fileError ? t.ansi1 : t.dim)
              .whitespace_normal()
              .child(this.fileError || "불러오는 중"),
      );
  }

  headerView(t) {
    const node = this.overlay ? null : ownNodeId();
    return h_flex()
      .h(42)
      .flex_none()
      .px(14)
      .gap(4)
      .items_center()
      .border_b(1)
      .border_color(t.border)
      .child(div().flex_1().font_semibold().child("chute"))
      .when(this.overlay !== null, (el) => {
        const panel = ownNodeId();
        return panel
          ? el.child(
              iconButton("chute-dock", "패널 열기", t, () => {
                run("layout.show", { id: panel });
                closeOverlay(this.overlay);
              }),
            )
          : el;
      })
      .when(this.overlay === null, (el) =>
        el.child(
          iconButton("chute-fold", this.folded ? "▸" : "▾", t, (cx) => this.fold(cx), this.folded ? "펼치기" : "접기"),
        ),
      )
      .when(node !== null, (el) =>
        el.child(iconButton("chute-close", "✕", t, () => run("layout.hide", { id: node }), "닫기")),
      )
      .when(this.overlay !== null, (el) =>
        el.child(iconButton("chute-overlay-close", "닫기", t, () => closeOverlay(this.overlay))),
      );
  }

  bodyOf(body, t) {
    if (body.kind === "loading") {
      return [h_flex().gap(8).text_color(t.dim).child(div().frames(SPINNER, 90)).child("불러오는 중")];
    }
    if (body.kind !== "board") return [notice(body, t)];
    const repos = body.repos.map((repo) => (this.folded ? foldedRepo(repo, t) : this.repoView(repo, t)));
    return repos.flatMap((view, i) => (i === 0 ? [view] : [div().h(1).flex_none().bg(t.border), view]));
  }

  repoView(repo, t) {
    return v_flex()
      .gap(14)
      .child(
        v_flex()
          .gap(4)
          .child(repoHead(repo, t))
          .child(statusLine(repo.status, t))
          .when(!repo.hub.reachable, (el) =>
            el.child(
              v_flex()
                .gap(2)
                .pl(15)
                .when(Boolean(repo.hub.error), (box) =>
                  box.child(div().text_size(12).text_color(t.muted).whitespace_normal().child(repo.hub.error)),
                )
                .child(div().text_size(12).text_color(t.dim).child(`${REMOTE_POLL_MS / 1000}초마다 다시 읽는다`)),
            ),
          ),
      )
      .children(repo.sections.map((section) => this.sectionView(repo, section, t)))
      .when(Boolean(repo.postVerify), (el) => el.child(postVerifyView(repo.postVerify, t)))
      .when(repo.recent.length > 0, (el) => el.child(this.recentView(repo, t)))
      .when(Boolean(repo.today), (el) => el.child(this.todayView(repo, t)));
  }

  sectionView(repo, section, t) {
    const items = section.items;
    const rows = {
      alerts: (item) => this.alertCard(repo, item, t),
      human: (item) => this.humanCard(repo, item, t),
      batch: (item) => batchView(item, t),
      waiting: (item) => waitingRow(item, t),
      bounced: (item) => this.bouncedRow(repo, item, t),
      unknown: (item) => unknownRow(item, t),
    };
    const lead = section.id === "alerts" ? [] : [sectionTitle(section.title, section.id === "batch" ? null : items.length, t)];
    if (section.id === "batch") return v_flex().gap(6).children(items.map((item) => rows.batch(item)));
    return v_flex()
      .gap(section.id === "alerts" ? 8 : 4)
      .children(lead)
      .children(items.map((item) => rows[section.id](item)));
  }

  alertCard(repo, alert, t) {
    const color = toneColor(alert.tone, t);
    return v_flex()
      .gap(4)
      .p(10)
      .rounded(8)
      .border_1()
      .border_color(color)
      .child(div().font_semibold().text_color(color).whitespace_normal().child(alert.text))
      .when(Boolean(alert.detail), (el) => el.child(div().text_size(12).text_color(t.dim).whitespace_normal().child(alert.detail)))
      .children(alert.facts.map((fact) => div().text_size(12).text_color(t.muted).whitespace_normal().child(fact)))
      .when(alert.files.length > 0, (el) =>
        el.child(
          h_flex()
            .gap(6)
            .pt(4)
            .children(
              alert.files.map((file) =>
                pill(`alert-${repo.id}-${alert.kind}-${file.label}`, `${file.label} ›`, t, () =>
                  this.openFile(file, repo.hub.machine),
                ),
              ),
            ),
        ),
      );
  }

  humanCard(repo, card, t) {
    const failure = this.errors.get(card.key);
    const error = failure?.state === card.state ? failure.text : null;
    const detail = [
      card.preflight?.text,
      card.config,
      card.failure?.mode,
      card.approvalBlocked,
    ].filter(Boolean);
    return v_flex()
      .gap(5)
      .p(10)
      .rounded(8)
      .bg(t.surface)
      .border_1()
      .border_color(t.blocked)
      .child(titleLine(card, t))
      .child(
        h_flex()
          .gap(5)
          .flex_wrap()
          .items_center()
          .children(card.marks.map((mark) => chip(mark.text, toneColor(mark.tone, t))))
          .child(div().text_size(12).font_medium().text_color(card.approvalBlocked ? t.ansi1 : t.blocked).child(card.reason))
          .when(detail.length > 0, (el) =>
            el.child(div().text_size(12).text_color(card.configChanged ? t.blocked : t.muted).child(`· ${detail.join(" · ")}`)),
          ),
      )
      .when(Boolean(card.failure?.summary), (el) =>
        el.child(div().text_size(12).text_color(t.muted).whitespace_normal().child(card.failure.summary)),
      )
      .when(Boolean(card.submitter), (el) => el.child(submitterLine(card.submitter, t)))
      .child(
        h_flex()
          .gap(8)
          .items_start()
          .child(agentChip(card.agent, t, card.key))
          .child(div().flex_none().text_size(12).text_color(t.dim).child(`재시도 ${card.retries}`)),
      )
      .when(Boolean(card.report), (el) =>
        el.child(
          linkRow(`report-${card.key}`, t, () => this.openFile(card.report, repo.hub.machine))
            .child(
              div()
                .flex_1()
                .min_w(0)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .text_size(11)
                .text_color(t.dim)
                .child(card.report.path),
            )
            .child(chevron(t)),
        ),
      )
      .when(card.actions.length > 0, (el) =>
        el.child(
          h_flex()
            .gap(6)
            .flex_wrap()
            .children(card.actions.map((action, i) => this.button(card, action, i === 0, t))),
        ),
      )
      .when(Boolean(error), (el) => el.child(div().text_size(12).text_color(t.ansi1).whitespace_normal().child(error)));
  }

  bouncedRow(repo, card, t) {
    const who = card.agent ? card.agent.label : "에이전트 없음";
    const row = card.report
      ? linkRow(`bounced-${card.key}`, t, () => this.openFile(card.report, repo.hub.machine))
      : h_flex().id(`bounced-${card.key}`).px(6).py(4).gap(8);
    return row
      .items_start()
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(1)
          .child(
            h_flex()
              .gap(6)
              .items_baseline()
              .child(div().flex_none().font_semibold().text_color(t.accent).child(`#${card.pr}`))
              .child(div().flex_1().min_w(0).truncate().child(card.title))
              .child(div().flex_none().text_size(12).text_color(t.ansi1).child(card.headline)),
          )
          .child(
            div()
              .text_size(11)
              .text_color(t.dim)
              .truncate()
              .child(`${who} · 재시도 ${card.retries}`),
          ),
      )
      .when(Boolean(card.report), (el) => el.child(chevron(t)));
  }

  recentView(repo, t) {
    return v_flex()
      .gap(4)
      .child(sectionTitle("최근 머지", null, t))
      .children(
        repo.recent.map((m, i) => {
          const body = [
            div().w(26).flex_none().text_color(t.accent).font_semibold().child(`#${m.pr}`),
            div().flex_1().min_w(0).truncate().child(m.title || "제목 없음").text_color(m.title ? t.foreground : t.dim),
            m.bounces > 0 ? div().flex_none().text_size(11).text_color(t.dim).child(`반송 ${m.bounces}`) : null,
            div().flex_none().min_w(44).text_right().text_size(11).text_color(t.dim).child(m.ago),
          ].filter(Boolean);
          if (!m.url) return h_flex().id(`merged-${repo.id}-${i}`).px(6).py(4).gap(8).items_baseline().children(body);
          return linkRow(`merged-${repo.id}-${i}`, t, (cx) => cx.open_url(m.url))
            .items_baseline()
            .children(body)
            .child(chevron(t));
        }),
      );
  }

  todayView(repo, t) {
    const open = this.opened.has(repo.id);
    const today = repo.today;
    return v_flex()
      .gap(2)
      .child(
        linkRow(`today-${repo.id}`, t, (cx) => this.toggleToday(repo, cx))
          .items_baseline()
          .child(div().flex_none().text_size(11).font_semibold().text_color(t.muted).child("오늘"))
          .child(div().flex_1().min_w(0).truncate().text_size(12).child(today.line))
          .child(chevron(t, open ? "⌄" : "›")),
      )
      .when(open, (el) =>
        el.child(
          v_flex()
            .gap(3)
            .px(6)
            .pb(2)
            .children(
              today.details.map((d) =>
                v_flex()
                  .child(
                    h_flex()
                      .gap(8)
                      .items_baseline()
                      .child(div().flex_1().text_size(12).text_color(t.muted).child(d.label))
                      .when(Boolean(d.note), (row) => row.child(chip(d.note, t.ansi3)))
                      .child(div().min_w(64).text_right().text_size(12).font_medium().child(d.value)),
                  )
                  .when(Boolean(d.hint), (row) =>
                    row.child(div().text_size(11).text_color(t.dim).text_right().child(d.hint)),
                  ),
              ),
            ),
        ),
      );
  }

  button(card, action, primary, t) {
    const running = this.busy.get(card.key);
    return div()
      .id(`act-${card.key}-${action.verb}`)
      .px(10)
      .py(3)
      .rounded(6)
      .text_size(12)
      .font_medium()
      .cursor_pointer()
      .border_1()
      .border_color(primary ? t.solid : t.border)
      .bg(primary ? t.solid : t.surface)
      .text_color(primary ? t.solid_foreground : t.foreground)
      .when(running !== undefined, (el) => el.opacity(0.5))
      .hover((s) => s.bg(primary ? t.solid : t.hover))
      .on_click((_e, cx) => this.act(card, action, cx))
      .child(running === action.verb ? "처리 중…" : action.label);
  }
}

const dot = (color) => div().w(7).h(7).rounded(4).flex_none().bg(color);

const chevron = (t, mark = "›") => div().flex_none().w(10).text_color(t.dim).text_right().child(mark);

function iconButton(id, label, t, onClick, tip) {
  return div()
    .id(id)
    .flex_none()
    .px(7)
    .h(24)
    .flex()
    .items_center()
    .rounded(6)
    .text_size(12)
    .text_color(t.muted)
    .cursor_pointer()
    .hover((s) => s.bg(t.hover))
    .when(Boolean(tip), (el) => el.tooltip(tip))
    .on_click((_e, cx) => onClick(cx))
    .child(label);
}

function pill(id, label, t, onClick) {
  return div()
    .id(id)
    .px(9)
    .py(3)
    .rounded(6)
    .text_size(12)
    .font_medium()
    .cursor_pointer()
    .border_1()
    .border_color(t.border)
    .hover((s) => s.bg(t.hover))
    .on_click((_e, cx) => onClick(cx))
    .child(label);
}

function linkRow(id, t, onClick) {
  return h_flex()
    .id(id)
    .px(6)
    .py(4)
    .gap(8)
    .rounded(6)
    .cursor_pointer()
    .hover((s) => s.bg(t.hover))
    .on_click((_e, cx) => onClick(cx));
}

function sectionTitle(title, count, t) {
  return h_flex()
    .gap(6)
    .items_center()
    .px(6)
    .child(div().text_size(11).font_semibold().text_color(t.muted).child(title))
    .when(count != null, (el) => el.child(div().text_size(11).text_color(t.dim).child(String(count))));
}

function repoHead(repo, t) {
  const approval = repo.approval ? ` · ${repo.approval}` : "";
  return h_flex()
    .gap(8)
    .items_center()
    .child(
      div()
        .id(`name-${repo.id}`)
        .min_w(0)
        .truncate()
        .font_semibold()
        .tooltip(`${repo.slug}${approval}`)
        .child(repo.name),
    )
    .child(div().flex_1())
    .when(!repo.hub.local, (el) => el.child(chip(`허브 ${repo.hub.label}`, repo.hub.reachable ? t.muted : t.ansi1)));
}

function statusLine(status, t) {
  const color = toneColor(status.tone, t);
  return h_flex()
    .gap(8)
    .items_center()
    .child(dot(color))
    .child(div().flex_1().min_w(0).whitespace_normal().text_color(status.tone === "waiting" ? t.muted : color).child(status.text));
}

function foldedRepo(repo, t) {
  return h_flex()
    .id(`fold-${repo.id}`)
    .gap(8)
    .items_center()
    .tooltip(repo.slug)
    .child(dot(toneColor(repo.status.tone, t)))
    .child(div().flex_none().font_semibold().child(repo.name))
    .child(div().flex_1().min_w(0).truncate().text_size(12).text_color(t.muted).child(repo.status.text));
}

function notice(body, t) {
  const empty = body.kind === "empty";
  return v_flex()
    .gap(6)
    .p(12)
    .rounded(8)
    .border_1()
    .border_color(empty ? t.border : t.ansi1)
    .when(empty, (el) => el.items_center())
    .child(div().font_semibold().whitespace_normal().child(body.title))
    .when(Boolean(body.detail), (el) => el.child(div().text_size(12).text_color(t.muted).whitespace_normal().child(body.detail)))
    .when(Boolean(body.hint), (el) => el.child(div().text_size(12).text_color(t.dim).whitespace_normal().child(body.hint)));
}

function chip(text, color) {
  return div()
    .flex_none()
    .px(6)
    .py(1)
    .rounded(4)
    .border_1()
    .border_color(color)
    .text_size(11)
    .text_color(color)
    .child(text);
}

function titleLine(card, t) {
  return h_flex()
    .gap(6)
    .items_start()
    .child(div().flex_none().font_semibold().text_color(t.accent).child(`#${card.pr}`))
    .child(div().flex_1().min_w(0).whitespace_normal().child(card.title));
}

function agentChip(agent, t, cardKey) {
  if (!agent) return div().flex_1().min_w(0).text_size(12).text_color(t.dim).child("에이전트 없음");
  const label = h_flex()
    .flex_1()
    .min_w(0)
    .gap(5)
    .items_start()
    .child(div().w(7).h(7).mt(5).rounded(4).flex_none().bg(agent.alive ? t.ansi2 : t.dim))
    .child(
      div()
        .flex_1()
        .min_w(0)
        .whitespace_normal()
        .text_size(12)
        .text_color(agent.alive ? t.foreground : t.dim)
        .child(agent.alive ? agent.label : `${agent.label} · 종료됨`),
    )
    .when(Boolean(agent.host), (el) => el.child(chip(agent.host, t.muted)));
  if (!agent.alive) return label;
  return label
    .id(`peek-${cardKey}-${agent.terminal}`)
    .cursor_pointer()
    .rounded(4)
    .hover((s) => s.bg(t.hover))
    .on_click(() => run("terminal.peek", { terminal: agent.terminal }));
}

function submitterLine(submitter, t) {
  return div().text_size(12).text_color(t.dim).whitespace_normal().child(submitter.text);
}

function batchView(batch, t) {
  const step = batch.waitingMachine ? "PC 자리를 기다리는 중" : batch.stepText ?? batch.phase ?? "검증 중";
  const facts = [batch.round, batch.reused].filter(Boolean);
  return v_flex()
    .gap(6)
    .child(
      h_flex()
        .gap(6)
        .px(6)
        .items_center()
        .child(div().text_color(t.working).child("").frames(SPINNER, 90))
        .child(div().text_size(11).font_semibold().text_color(t.muted).child("지금 batch"))
        .child(div().flex_1())
        .when(Boolean(batch.elapsed), (el) =>
          el.child(div().min_w(48).text_right().text_size(12).text_color(t.foreground).child(batch.elapsed)),
        ),
    )
    .children(
      batch.prs.map((entry) =>
        v_flex()
          .gap(3)
          .px(6)
          .child(
            h_flex()
              .gap(6)
              .items_start()
              .child(div().flex_none().font_semibold().text_color(t.accent).child(`#${entry.pr}`))
              .child(div().flex_1().min_w(0).whitespace_normal().child(entry.title)),
          )
          .when(entry.marks.length > 0, (el) =>
            el.child(h_flex().gap(5).flex_wrap().children(entry.marks.map((mark) => chip(mark.text, toneColor(mark.tone, t))))),
          )
          .when(Boolean(entry.submitter), (el) => el.child(submitterLine(entry.submitter, t))),
      ),
    )
    .when(batch.progress != null, (el) =>
      el.child(
        div()
          .mx(6)
          .h(4)
          .rounded(2)
          .bg(t.border)
          .child(div().h_full().rounded(2).bg(batch.longer ? t.ansi3 : t.working).w(`${Math.round(batch.progress * 100)}%`)),
      ),
    )
    .child(
      h_flex()
        .gap(8)
        .px(6)
        .items_baseline()
        .child(div().flex_1().min_w(0).truncate().text_size(12).text_color(t.working).child(step))
        .when(batch.longer, (el) => el.child(div().flex_none().text_size(11).text_color(t.ansi3).child("평소보다 길다")))
        .when(Boolean(batch.usual), (el) => el.child(div().flex_none().text_size(11).text_color(t.dim).child(batch.usual))),
    )
    .when(facts.length > 0, (el) =>
      el.child(div().px(6).text_size(11).text_color(t.dim).whitespace_normal().child(facts.join(" · "))),
    );
}

function postVerifyView(post, t) {
  const row = (mark, color, text, right) =>
    h_flex()
      .gap(8)
      .px(6)
      .py(2)
      .items_baseline()
      .child(div().w(10).flex_none().text_color(color).child(mark))
      .child(div().flex_1().min_w(0).truncate().text_size(12).child(text))
      .when(Boolean(right), (el) => el.child(div().flex_none().min_w(44).text_right().text_size(11).text_color(t.dim).child(right)));
  const run = post.run;
  return v_flex()
    .gap(2)
    .child(sectionTitle("머지 뒤 검사", null, t))
    .when(Boolean(post.green), (el) => el.child(row("✓", t.done, `${post.green.commit} 통과`, post.green.ago)))
    .when(Boolean(run), (el) =>
      el.child(
        row(
          "◌",
          t.working,
          run.waitingMachine
            ? `${run.commit} PC 자리 기다리는 중`
            : `${run.commit} ${run.label}${run.step ? ` · ${run.step}` : ""}`,
          run.elapsed,
        ),
      ),
    );
}

function numberedTitle(prefix, item, t) {
  return h_flex()
    .gap(6)
    .items_start()
    .when(prefix != null, (el) => el.child(div().w(18).flex_none().text_right().text_color(t.dim).child(String(prefix))))
    .child(div().flex_none().font_semibold().text_color(t.accent).child(`#${item.pr}`))
    .child(div().flex_1().min_w(0).whitespace_normal().child(item.title));
}

function waitingRow(item, t) {
  const tags = [
    ...item.marks.map((mark) => ({ text: mark.text, color: toneColor(mark.tone, t) })),
    ...(item.ciPending ? [{ text: item.label, color: t.ansi3 }] : []),
    ...(item.note ? [{ text: item.note, color: t.dim }] : []),
  ];
  return v_flex()
    .gap(2)
    .px(6)
    .py(2)
    .child(
      h_flex()
        .gap(6)
        .items_baseline()
        .child(div().w(18).flex_none().text_right().text_size(12).text_color(t.dim).child(String(item.position)))
        .child(div().flex_none().font_semibold().text_color(t.accent).child(`#${item.pr}`))
        .child(div().flex_1().min_w(0).truncate().child(item.title))
        .children(tags.map((tag) => div().flex_none().text_size(11).text_color(tag.color).child(tag.text))),
    )
    .when(Boolean(item.submitter), (el) => el.child(div().pl(24).child(submitterLine(item.submitter, t))));
}

function unknownRow(item, t) {
  return v_flex()
    .gap(3)
    .px(6)
    .child(numberedTitle(null, item, t))
    .child(h_flex().gap(5).flex_wrap().child(chip(item.reason ? `${item.state} · ${item.reason}` : item.state, t.ansi3)));
}
