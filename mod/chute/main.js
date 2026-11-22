import { View, div } from "gpui-kit";
import { v_flex, h_flex, Editor, EditorState, Tabs, Tab } from "gpui-base";
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
  setShared,
  settings,
  theme,
  watchFolder,
} from "herdr";
import {
  FLAKY_ALERT_TEXT,
  LOCAL_POLL_MS,
  REMOTE_POLL_MS,
  badgeText,
  causeText,
  clockExact,
  clockText,
  diffNotices,
  failureText,
  formatDuration,
  interpret,
  isNarrow,
  focusOf,
  hiddenText,
  idleText,
  isWide,
  listOrder,
  needsClock,
  overview,
  overviewText,
  pollInterval,
  recentGroups,
  recentWindow,
  tint,
  toneToken,
} from "./board.js";
import {
  DETAIL_EVENTS,
  DETAIL_HEIGHT,
  FLAKY_SHOWN,
  TABS,
  batchRows,
  detailArgs,
  detailSpot,
  eventRows,
  flakyRows,
  overviewOf,
  parseBatches,
  parseEvents,
  parseFlaky,
} from "./detail.js";

const TICK_MS = 1000;
const MIN_READ_GAP_MS = 1000;
const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const MAX_VIEW_CHARS = 200_000;
const FOCUS_KEY = "chute.focus";
const PINS_KEY = "chute.pins";
const HIDDEN_KEY = "chute.hidden";
const SHARED_KEY = "chute.status";
const SURFACE_ALPHA = 0.06;
const CARD_ALPHA = 0.12;
const CHIP_ALPHA = 0.14;
const SKIPPED_ALPHA = 0.03;
const ICON_SIZE = 20;
const CARD_SYMBOL = { error: "✕", warn: "△" };
const NEED_TONE = { worker_gone: "error", last_retry: "warn" };
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
const rememberText = (key, value) => {
  try {
    if (value) localStorage.setItem(key, value);
    else localStorage.removeItem(key);
  } catch {}
};
const recallText = (key) => {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};
const recallIds = (key) => {
  try {
    const value = JSON.parse(recallText(key) ?? "[]");
    return new Set(Array.isArray(value) ? value.filter((id) => typeof id === "string") : []);
  } catch {
    return new Set();
  }
};

const toneColor = (name, t) => t[toneToken(name)];

// `surface` equals `background` in the bundled themes, so the fallback is `raised`, which shows.
const faded = (t, color, alpha) => tint(color, alpha) ?? t.raised;

// gpui-kit has no tabular-nums feature; the terminal font keeps digits one width.
const mono = (el) => {
  const family = settings().font_family;
  return typeof family === "string" && family ? el.font_family(family) : el;
};

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
    this.narrow = false;
    this.wide = false;
    this.origin = null;
    this.width = 0;
    if (this.file) {
      this.loadFile(cx);
      return;
    }
    const bin = given.bin;
    this.bin = typeof bin === "string" && bin ? bin : "chute";
    const detail = given.detail;
    this.detail = detail !== null && typeof detail === "object" && typeof detail.repo === "string" ? detail : null;
    if (this.detail) {
      this.initDetail(cx);
      return;
    }
    this.result = null;
    this.board = null;
    this.reading = false;
    this.dirty = false;
    this.watching = new Set();
    this.busy = new Map();
    this.errors = new Map();
    this.sinceRead = 0;
    this.pollMs = LOCAL_POLL_MS;
    this.focus = recallText(FOCUS_KEY);
    this.pins = recallIds(PINS_KEY);
    this.hidden = recallIds(HIDDEN_KEY);
    this.idleOpen = false;
    this.hiddenOpen = false;
    this.moreOpen = new Set();
    this.flakyOpen = new Set();
    this.readAt = null;
    this.shared = null;
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
        else if (command === "fold") this.showList(cx);
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
        this.readAt = Date.now();
        this.settle(this.board, cx);
        this.publish(this.board);
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

  // Tells other mods (the tab bar button) how the board is doing. Only a change is written: `setShared` redraws every mod.
  publish(board) {
    if (this.overlay || checking()) return;
    const value = overview(board);
    if (!value) return;
    const key = JSON.stringify(value);
    if (key === this.shared?.key) return;
    this.shared = { key };
    setShared(SHARED_KEY, { ...value, since: Date.now() });
  }

  async close() {
    if (this.file || this.overlay || checking() || !this.shared) return;
    this.shared = null;
    setShared(SHARED_KEY, null);
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
        text = causeText(e) || "Couldn't run the command";
      }
      if (text) this.errors.set(card.key, { text, state: card.state });
      this.busy.delete(card.key);
      cx.notify();
      this.request(cx);
    });
  }

  showRepo(repo, cx) {
    this.focus = repo.id;
    rememberText(FOCUS_KEY, repo.id);
    cx.notify();
  }

  showList(cx) {
    this.focus = null;
    rememberText(FOCUS_KEY, null);
    cx.notify();
  }

  saveIds() {
    rememberText(PINS_KEY, this.pins.size > 0 ? JSON.stringify([...this.pins]) : null);
    rememberText(HIDDEN_KEY, this.hidden.size > 0 ? JSON.stringify([...this.hidden]) : null);
  }

  togglePin(repo, cx) {
    if (!this.pins.delete(repo.id)) {
      this.pins.add(repo.id);
      this.hidden.delete(repo.id);
    }
    this.saveIds();
    cx.notify();
  }

  // Hiding takes the repo back to the list, where it has moved to the hidden line.
  toggleHidden(repo, cx) {
    if (this.hidden.delete(repo.id)) {
      this.saveIds();
      cx.notify();
      return;
    }
    this.hidden.add(repo.id);
    this.pins.delete(repo.id);
    this.saveIds();
    this.showList(cx);
  }

  toggleIdle(cx) {
    this.idleOpen = !this.idleOpen;
    cx.notify();
  }

  toggleHiddenLine(cx) {
    this.hiddenOpen = !this.hiddenOpen;
    cx.notify();
  }

  toggleFlaky(repo, cx) {
    if (!this.flakyOpen.delete(repo.id)) this.flakyOpen.add(repo.id);
    cx.notify();
  }

  toggleMore(repo, cx) {
    if (!this.moreOpen.delete(repo.id)) this.moreOpen.add(repo.id);
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
        if (text === null) this.fileError = "File not found";
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
        this.fileError = `Couldn't read file: ${causeText(e) || "unknown error"}`;
      }
      cx.notify();
    });
  }

  openDetail(repo, tab) {
    const panel = this.origin ? { x: this.origin.x, width: this.width } : null;
    let viewport = null;
    try {
      viewport = window.viewport_size();
    } catch {}
    const spot = detailSpot(viewport, panel);
    openOverlay({
      kind: "window",
      key: "chute-detail",
      mod: MOD,
      width: spot.width,
      height: DETAIL_HEIGHT,
      ...(spot.at ? { at: spot.at } : {}),
      props: { detail: { repo: repo.id, name: repo.name, tab }, bin: this.bin },
    });
  }

  initDetail(cx) {
    this.tab = TABS.includes(this.detail.tab) ? this.detail.tab : "overview";
    this.flakyMore = false;
    this.generation = 0;
    this.data = { batches: null, flaky: null, events: null };
    this.loadDetail(cx);
  }

  loadDetail(cx) {
    const generation = ++this.generation;
    const args = detailArgs(this.detail.repo);
    const read = {
      batches: (result) => {
        const parsed = parseBatches(result);
        return parsed.kind === "ok" ? { ...parsed, rows: batchRows(parsed.batches) } : parsed;
      },
      flaky: (result) => {
        const parsed = parseFlaky(result);
        return parsed.kind === "ok" ? { ...parsed, rows: flakyRows(parsed.entries) } : parsed;
      },
      events: (result) => {
        const parsed = parseEvents(result);
        return parsed.kind === "ok" ? { ...parsed, rows: eventRows(parsed.events) } : parsed;
      },
    };
    this.data = { batches: null, flaky: null, events: null };
    this.flakyMore = false;
    cx.notify();
    for (const name of Object.keys(args)) {
      cx.spawn(async (cx) => {
        let result;
        try {
          const r = (await exec(this.bin, args[name])) ?? {};
          result = { code: r.code, stdout: r.stdout, stderr: r.stderr };
        } catch (e) {
          result = { spawnError: e };
        }
        if (generation !== this.generation) return;
        this.data[name] = read[name](result);
        cx.notify();
      });
    }
  }

  detailView(frame, t) {
    const batches = this.data.batches;
    const count = batches?.kind === "ok" ? batches.batches.length : null;
    const flaky = this.data.flaky;
    const labels = {
      overview: "Overview",
      flaky: flaky?.kind === "ok" ? `Flaky ${flaky.chronicCount}` : "Flaky",
      events: "Events",
      batches: "Batches",
    };
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
          .child(div().flex_none().max_w(260).truncate().font_semibold().child(this.detail.name || "chute"))
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(11)
              .text_color(t.dim)
              .child(count === null ? "" : `· last ${count} ${count === 1 ? "batch" : "batches"}`),
          )
          .child(iconButton("chute-detail-refresh", "Refresh", t, (cx) => this.loadDetail(cx)))
          .child(iconButton("chute-detail-close", "Close", t, () => this.overlay && closeOverlay(this.overlay))),
      )
      .child(
        Tabs.new("chute-detail-tabs")
          .h_flex()
          .flex_none()
          .px(10)
          .gap(2)
          .border_b(1)
          .border_color(t.border)
          .children(
            TABS.map((name) =>
              Tab.new(`chute-detail-tab-${name}`)
                .selected(this.tab === name)
                .px(10)
                .py(7)
                .text_size(12)
                .font_medium()
                .cursor_pointer()
                .text_color(this.tab === name ? t.foreground : t.dim)
                .border_b(2)
                .border_color(this.tab === name ? t.foreground : t.background)
                .on_click((_e, cx) => {
                  this.tab = name;
                  cx.notify();
                })
                .child(labels[name]),
            ),
          ),
      )
      .child(
        v_flex()
          .id("chute-detail-body")
          .flex_1()
          .min_h(0)
          .overflow_y_scrollbar()
          .p(14)
          .gap(12)
          .children(this.tabBody(t)),
      );
  }

  tabBody(t) {
    const empty = { overview: "No batches", flaky: "No flaky tests", events: "No events", batches: "No batches" }[this.tab];
    const data = this.tab === "overview" ? this.data.batches : this.data[this.tab];
    const state = (text, color) => [div().text_size(12).text_color(color).whitespace_normal().child(text)];
    if (data === null) return [h_flex().gap(8).text_color(t.dim).child(div().frames(SPINNER, 90)).child("Loading")];
    if (data.kind !== "ok") return state(data.detail, t.ansi1);
    const rows = this.tab === "overview" ? data.batches : this.tab === "flaky" ? data.entries : data.rows;
    if (rows.length === 0) return state(empty, t.dim);
    if (this.tab === "overview") return this.overviewTab(data, t);
    if (this.tab === "flaky") return this.flakyTab(data, t);
    if (this.tab === "events") return this.eventsTab(data, t);
    return this.batchesTab(data, t);
  }

  overviewTab(data, t) {
    const view = overviewOf(data.batches, this.data.events);
    const cut = this.data.events?.kind === "ok" && this.data.events.cut;
    return [
      h_flex()
        .flex_wrap()
        .gap(12)
        .children(
          view.counts.map((c) =>
            v_flex()
              .flex_1()
              .min_w(120)
              .gap(2)
              .child(mono(div().text_size(24).font_semibold().whitespace_nowrap()).child(c.value))
              .child(div().text_size(11).text_color(t.dim).child(c.label)),
          ),
        ),
      v_flex()
        .gap(4)
        .when(view.bars.length > 0, (el) =>
          el
            .child(
              h_flex()
                .h(80)
                .gap(2)
                .items_end()
                .children(
                  view.bars.map((bar, i) =>
                    div()
                      .id(`detail-bar-${i}`)
                      .flex_1()
                      .min_w(1)
                      .max_w(28)
                      .h(4 + 76 * bar.height)
                      .rounded(2)
                      .bg(bar.tone === "bounced" ? t.ansi1 : t.ansi2)
                      .when(bar.faded, (b) => b.opacity(0.4))
                      .tooltip(bar.tip),
                  ),
                ),
            )
            .child(
              h_flex()
                .text_size(11)
                .text_color(t.dim)
                .child(div().flex_1().child(view.axis[0]))
                .child(div().flex_none().child(view.axis[1]))
                .child(div().flex_1().text_right().child(view.axis[2])),
            )
            .child(
              div()
                .text_size(11)
                .text_color(t.dim)
                .whitespace_normal()
                .child("Bar = one batch · height = duration (log scale) · red = bounced a PR"),
            ),
        ),
      ...(view.reasons
        ? [
            div()
              .text_size(12)
              .text_color(t.muted)
              .whitespace_normal()
              .child(`Bounce reasons in 24h: ${view.reasons}${cut ? ` (latest ${DETAIL_EVENTS} events)` : ""}`),
          ]
        : []),
    ];
  }

  flakyTab(data, t) {
    const shown = this.flakyMore ? data.rows : data.rows.slice(0, FLAKY_SHOWN);
    const hidden = data.rows.length - shown.length;
    return [
      h_flex()
        .gap(8)
        .items_center()
        .child(
          div()
            .flex_1()
            .min_w(0)
            .text_size(11)
            .font_semibold()
            .text_color(t.muted)
            .child(data.days === null ? "FLAKY LEDGER" : `FLAKY LEDGER · ${data.days} DAYS`),
        )
        .child(div().flex_none().text_size(11).text_color(t.dim).child(`${data.chronicCount} chronic`)),
      v_flex()
        .gap(2)
        .children(
          shown.map((row) =>
            h_flex()
              .id(`detail-flaky-${row.key}`)
              .gap(10)
              .px(6)
              .py(3)
              .rounded(6)
              .items_baseline()
              .hover((s) => s.bg(t.hover))
              .child(mono(div().flex_1().min_w(0).truncate().text_size(12)).tooltip(row.test).child(row.test))
              .child(div().flex_none().w(90).truncate().text_size(11).text_color(t.dim).child(row.step))
              .child(mono(div().flex_none().w(36).text_right().text_size(12)).child(String(row.count)))
              .child(mono(div().flex_none().w(72).text_right().whitespace_nowrap().text_size(11).text_color(t.dim)).child(row.ago))
              .child(div().flex_none().w(60).child(row.chronic ? chip("chronic", t.ansi3, t) : div())),
          ),
        ),
      ...(hidden > 0 || this.flakyMore
        ? [
            linkRow("detail-flaky-more", t, (cx) => {
              this.flakyMore = !this.flakyMore;
              cx.notify();
            }).child(div().text_size(12).text_color(t.dim).child(hidden > 0 ? `+ ${hidden} more` : "Show less")),
          ]
        : []),
    ];
  }

  eventsTab(data, t) {
    return [
      ...(data.cut ? [div().text_size(11).text_color(t.ansi3).child(`Showing the latest ${DETAIL_EVENTS} events`)] : []),
      v_flex()
        .gap(1)
        .children(
          data.rows.map((row) =>
            h_flex()
              .gap(8)
              .px(6)
              .py(2)
              .items_baseline()
              .child(timeCell(row.time, t))
              .child(div().flex_none().w(12).text_color(toneColor(row.tone, t)).child(row.glyph))
              .child(div().flex_1().min_w(0).text_size(12).whitespace_normal().child(row.text)),
          ),
        ),
    ];
  }

  batchesTab(data, t) {
    return [
      v_flex()
        .gap(2)
        .children(
          data.rows.map((row) =>
            v_flex()
              .id(`detail-batch-${row.key}`)
              .px(6)
              .py(3)
              .rounded(6)
              .hover((s) => s.bg(t.hover))
              .child(
                h_flex()
                  .gap(10)
                  .items_baseline()
                  .child(mono(div().flex_none().w(96).truncate().text_size(12)).tooltip(row.key).child(row.id))
                  .child(timeCell(row.started, t))
                  .child(mono(div().flex_none().w(64).text_right().whitespace_nowrap().text_size(12)).child(row.took))
                  .child(mono(div().flex_1().min_w(0).truncate().text_size(12)).child(row.merged))
                  .when(Boolean(row.bounced), (el) =>
                    el.child(mono(div().flex_none().whitespace_nowrap().text_size(12).text_color(t.ansi1)).child(row.bounced)),
                  )
                  .when(row.reverted, (el) => el.child(chip("reverted", t.ansi3, t))),
              )
              .when(row.steps !== null, (el) => el.child(div().text_size(11).text_color(t.dim).whitespace_normal().child(row.steps))),
          ),
        ),
    ];
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
      .text_size(13)
      .on_mouse_down("left", (e) => {
        if (e.local_position) this.origin = { x: e.position.x - e.local_position.x };
      })
      .on_size_change((size, cx) => {
        this.width = size.width;
        const narrow = isNarrow(size.width);
        const wide = isWide(size.width);
        if (narrow === this.narrow && wide === this.wide) return;
        this.narrow = narrow;
        this.wide = wide;
        cx.notify();
      });
    if (this.file) return this.fileView(frame, t);
    if (this.detail) return this.detailView(frame, t);
    const body = this.result ? interpret(this.result) : { kind: "loading" };
    return frame.child(this.headerView(t)).child(
      v_flex()
        .id("chute-body")
        .flex_1()
        .min_h(0)
        .overflow_y_scrollbar()
        .p(12)
        .gap(20)
        .children(this.bodyOf(body, t)),
    );
  }

  fileView(frame, t) {
    const close = iconButton("chute-file-close", "Close", t, () => this.overlay && closeOverlay(this.overlay));
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
            el.child(div().flex_none().text_size(11).text_color(t.ansi3).child(`First ${this.cut} characters cut`)),
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
              .child(this.fileError || "Loading"),
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
      .child(div().flex_none().font_semibold().child("chute"))
      .child(this.summaryView(t))
      .when(this.overlay !== null, (el) => {
        const panel = ownNodeId();
        return panel
          ? el.child(
              iconButton("chute-dock", "Open panel", t, () => {
                run("layout.show", { id: panel });
                closeOverlay(this.overlay);
              }),
            )
          : el;
      })
      .when(this.board?.kind === "board" && this.board.repos.length > 1, (el) => {
        const repo = focusOf(this.board, this.focus);
        if (!repo) return el;
        const pinned = this.pins.has(repo.id);
        const hidden = this.hidden.has(repo.id);
        return el
          .child(iconButton("chute-hide", hidden ? "Show" : "Hide", t, (cx) => this.toggleHidden(repo, cx), hidden ? "Show in the list again" : "Hide from the list. What needs you still shows"))
          .child(iconButton("chute-pin", pinned ? "★" : "☆", t, (cx) => this.togglePin(repo, cx), pinned ? "Unpin" : "Pin to top"))
          .child(iconButton("chute-all", "‹ All", t, (cx) => this.showList(cx), "All repos"));
      })
      .when(node !== null, (el) =>
        el.child(iconButton("chute-close", "✕", t, () => run("layout.hide", { id: node }), "Close")),
      )
      .when(this.overlay !== null, (el) =>
        el.child(iconButton("chute-overlay-close", "Close", t, () => closeOverlay(this.overlay))),
      );
  }

  summaryView(t) {
    const value = overview(this.result ? this.board : null);
    const left = h_flex().flex_1().min_w(0).gap(6).text_size(11).text_color(t.dim);
    if (!value) return left;
    const [repos, running, todo] = overviewText(value);
    const todoColor = value.grade === "failed" ? t.ansi1 : t.blocked;
    const calm = value.running === 0 && value.issues === 0;
    return left
      .when(value.issues > 0, (el) => el.child(chip(todo, todoColor, t)))
      .when(!this.narrow, (el) => el.child(div().min_w(0).truncate().child(`${repos} · ${calm ? "all clear" : running}`)))
      .child(div().flex_1())
      .when(this.readAt !== null, (el) =>
        el.child(
          mono(div().id("chute-updated").flex_none().whitespace_nowrap())
            .tooltip(`Updated ${clockExact(this.readAt)}`)
            .child(clockText(this.readAt)),
        ),
      );
  }

  bodyOf(body, t) {
    if (body.kind === "loading") {
      return [h_flex().gap(8).text_color(t.dim).child(div().frames(SPINNER, 90)).child("Loading")];
    }
    if (body.kind !== "board") return [notice(body, t)];
    const lifted = new Set(body.todo.map((entry) => entry.item));
    const todo = body.todo.length > 0 ? [this.todoView(body, t)] : [];
    const focused = focusOf(body, this.focus);
    if (focused) return [...todo, this.repoView(focused, body.repos.length > 1, t, lifted)];
    return [...todo, this.listView(body.repos, t)];
  }

  listView(repos, t) {
    const { shown, idle, hidden } = listOrder(repos, this.pins, this.hidden);
    const rows = shown.map((repo) => this.repoHead(repo, "list", t));
    const idleRows =
      idle.length === 1
        ? [this.repoHead(idle[0], "list", t)]
        : idle.length > 1
          ? [
              this.foldLine("idle", idleText(idle), this.idleOpen, (cx) => this.toggleIdle(cx), t),
              ...(this.idleOpen ? idle.map((repo) => this.repoHead(repo, "list", t)) : []),
            ]
          : [];
    const hiddenRows =
      hidden.length > 0
        ? [
            this.foldLine("hidden", hiddenText(hidden), this.hiddenOpen, (cx) => this.toggleHiddenLine(cx), t),
            ...(this.hiddenOpen ? hidden.map((repo) => this.repoHead(repo, "list", t)) : []),
          ]
        : [];
    return v_flex()
      .gap(4)
      .child(sectionTitle("Repos", repos.length, t))
      .child(v_flex().gap(2).children([...rows, ...idleRows, ...hiddenRows]));
  }

  // One line that stands for several repos and opens to list them.
  foldLine(id, text, open, onClick, t) {
    return linkRow(`${id}-repos`, t, onClick)
      .items_center()
      .child(chevron(t, open ? "▾" : "▸"))
      .child(div().flex_none().w(10).h(16).flex().items_center().justify_center().child(dot(t.muted, false)))
      .child(div().id(`${id}-names`).flex_1().min_w(0).truncate().text_size(12).text_color(t.muted).tooltip(text).child(text));
  }

  todoView(body, t) {
    const named = body.repos.length > 1;
    const repoOf = new Map(body.repos.map((repo) => [repo.id, repo]));
    const owner = new Map(body.repos.flatMap((repo) => repo.sections.flatMap((section) => section.items.map((item) => [item, repo]))));
    return v_flex()
      .gap(8)
      .child(sectionTitle("Needs you", body.todo.length, t))
      .children(
        body.todo.map((entry) => {
          const repo = owner.get(entry.item) ?? repoOf.get(entry.repoId);
          const card =
            entry.kind === "approval"
              ? this.humanCard(repo, entry.item, t)
              : entry.kind === "bounce"
                ? this.bouncedRow(repo, entry.item, t)
                : this.alertCard(repo, { kind: entry.kind, tone: entry.tone, facts: [], files: [], ...entry.item }, t);
          return v_flex()
            .id(`todo-${entry.key}`)
            .gap(3)
            .when(named, (el) => el.child(div().px(6).text_size(11).text_color(t.dim).child(entry.repoName)))
            .child(card);
        }),
      );
  }

  repoView(repo, many, t, lifted) {
    const view = v_flex().gap(6).child(this.repoHead(repo, many ? "focus" : "only", t));
    const drawn = (section) =>
      section.id === "human"
        ? false
        : section.id === "alerts"
          ? section.items.some((alert) => !lifted.has(alert))
          : section.items.length > 0;
    const filled = repo.sections.filter(drawn);
    const empty = repo.sections.filter((section) => section.id !== "human" && section.items.length === 0);
    const retry = view.when(!repo.hub.reachable, (el) =>
      el.child(
        v_flex()
          .gap(2)
          .pl(24)
          .child(div().text_size(12).text_color(t.dim).child(`Retrying every ${REMOTE_POLL_MS / 1000}s`)),
      ),
    );
    const live = filled.map((section) => this.sectionView(repo, section, t, lifted));
    const emptyRow = empty.length > 0 && !repo.quiet ? [emptyLine(empty, t)] : [];
    const past = [
      ...(repo.postVerify ? [postVerifyView(repo.postVerify, t)] : []),
      ...(repo.strip ? [this.stripView(repo, t)] : []),
      ...(repo.today ? [this.todayView(repo, t)] : []),
      ...(repo.recent.length > 0 ? [this.recentView(repo, t)] : []),
    ];
    if (this.wide) {
      const left = [...live, ...emptyRow];
      const column = (items) => v_flex().min_w(0).gap(12).children(items);
      const columns =
        left.length > 0 && past.length > 0
          ? h_flex()
              .items_start()
              .gap(18)
              .child(column(left).flex_grow(1.25).flex_basis(0))
              .child(column(past).flex_grow(1).flex_basis(0))
          : column([...left, ...past]);
      return v_flex().gap(12).child(retry).child(columns);
    }
    return v_flex().gap(12).child(retry).children([...live, ...past, ...emptyRow]);
  }

  // `mode` is "list" for a row of the repo list, "focus" for the head of the one repo shown, "only" when it is the only repo.
  repoHead(repo, mode, t) {
    const approval = repo.approval ? ` · ${repo.approval}` : "";
    const color = toneColor(repo.status.tone, t);
    const status = mode === "list" || repo.quiet ? repo.summary : repo.status.text;
    const statusColor = repo.status.tone === "waiting" ? t.muted : repo.status.tone === "ok" ? t.foreground : color;
    const onClick = mode === "list" ? (cx) => this.showRepo(repo, cx) : mode === "focus" ? (cx) => this.showList(cx) : null;
    const row = () => (onClick ? linkRow(`head-${repo.id}`, t, onClick) : h_flex().id(`head-${repo.id}`).px(6).py(4).gap(8));
    const first = h_flex()
      .items_center()
      .gap(8)
      .child(chevron(t, mode === "list" ? "›" : mode === "focus" ? "‹" : ""))
      .child(
        div()
          .id(`daemon-${repo.id}`)
          .flex_none()
          .w(10)
          .h(16)
          .flex()
          .items_center()
          .justify_center()
          .tooltip(repo.daemon.text)
          .child(dot(toneColor(repo.daemon.tone, t), repo.active)),
      )
      .child(div().id(`name-${repo.id}`).flex_shrink(0).max_w(140).truncate().font_semibold().tooltip(`${repo.slug}${approval}`).child(repo.name))
      .when(mode === "list" && this.pins.has(repo.id), (el) => el.child(div().flex_none().text_size(11).text_color(t.dim).child("★")))
      .when(!repo.hub.local, (el) => el.child(chip(`hub ${repo.hub.label}`, repo.hub.reachable ? t.muted : t.ansi1, t)))
      .child(div().flex_1());
    if (this.narrow) {
      return row().child(
        v_flex()
          .flex_1()
          .min_w(0)
          .child(first)
          .child(
            div()
              .id(`status-${repo.id}`)
              .pl(HEAD_INDENT)
              .text_size(12)
              .text_color(statusColor)
              .whitespace_normal()
              .tooltip(status)
              .child(status),
          ),
      );
    }
    return row()
      .items_center()
      .child(first.flex_1().min_w(0))
      .child(
        h_flex()
          .id(`status-${repo.id}`)
          .min_w(0)
          .gap(6)
          .items_center()
          .tooltip(status)
          .child(div().min_w(0).truncate().text_size(12).text_color(statusColor).child(status)),
      );
  }

  sectionView(repo, section, t, lifted) {
    const items = section.items;
    if (section.id === "alerts") {
      const cards = items.filter((alert) => !lifted.has(alert) && !(alert.kind === "warning" && alert.text === FLAKY_ALERT_TEXT));
      return v_flex()
        .gap(8)
        .children(cards.map((alert) => this.alertCard(repo, alert, t)))
        .when(repo.flaky !== null, (el) => el.child(this.flakyLine(repo, t)));
    }
    const rows = {
      batch: (item) => batchView(repo, item, t, this.narrow),
      waiting: (item, i) => waitingRow(repo, item, i, t, this.narrow),
      bounced: (item) => (lifted.has(item) ? underNeedsYou(item, t) : this.bouncedRow(repo, item, t)),
      unknown: (item) => unknownRow(item, t, this.narrow),
    };
    if (section.id === "batch") return v_flex().gap(6).children(items.map((item) => rows.batch(item)));
    return v_flex()
      .gap(4)
      .child(sectionTitle(section.title, items.length, t))
      .children(items.map((item, i) => rows[section.id](item, i)));
  }

  flakyLine(repo, t) {
    const flaky = repo.flaky;
    const open = this.flakyOpen.has(repo.id);
    return v_flex()
      .gap(2)
      .child(
        linkRow(`flaky-${repo.id}`, t, (cx) => this.toggleFlaky(repo, cx))
          .items_center()
          .child(div().flex_none().text_color(t.ansi3).child(CARD_SYMBOL.warn))
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(12)
              .text_color(t.muted)
              .child(`${flaky.count} chronic flaky ${flaky.count === 1 ? "test" : "tests"}`),
          )
          .when(Boolean(flaky.ago), (el) =>
            el.child(mono(div().flex_none().whitespace_nowrap().text_size(11).text_color(t.dim)).child(flaky.ago)),
          )
          .child(textLink(`ledger-${repo.id}`, "Ledger ›", "Open the flaky ledger", t, () => this.openDetail(repo, "flaky"))),
      )
      .when(open, (el) =>
        el.child(
          v_flex()
            .gap(3)
            .pl(24)
            .pr(6)
            .children(
              flaky.messages
                .filter(Boolean)
                .map((message) => div().text_size(12).text_color(t.muted).whitespace_normal().child(message)),
            ),
        ),
      );
  }

  alertCard(repo, alert, t) {
    const color = toneColor(alert.tone, t);
    const symbol = CARD_SYMBOL[alert.tone];
    return v_flex()
      .gap(4)
      .p(10)
      .rounded(8)
      .bg(faded(t, color, CARD_ALPHA))
      .child(
        h_flex()
          .gap(6)
          .items_start()
          .text_color(color)
          .font_semibold()
          .when(Boolean(symbol), (el) => el.child(div().flex_none().child(symbol)))
          .child(div().flex_1().min_w(0).whitespace_normal().child(alert.text)),
      )
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
    const people = [];
    if (card.sameSubmitter) people.push(agentChip(card.agent, t, card.key, card.submittedBy));
    else {
      if (card.agent) people.push(agentChip(card.agent, t, card.key));
      if (card.submittedBy) people.push(div().min_w(0).text_size(12).text_color(t.dim).whitespace_normal().child(card.submittedBy));
    }
    const buttons = h_flex()
      .gap(6)
      .items_center()
      .children(card.actions.map((action, i) => this.button(card, action, i === 0, t)))
      .child(div().flex_1())
      .when(Boolean(card.diffUrl), (row) =>
        row.child(textLink(`diff-${card.key}`, "View diff ↗", `Open the diff of #${card.pr} on GitHub`, t, (cx) => cx.open_url(card.diffUrl))),
      );
    return v_flex()
      .gap(5)
      .p(10)
      .rounded(8)
      .bg(faded(t, t.blocked, CARD_ALPHA))
      .child(
        h_flex()
          .gap(6)
          .items_start()
          .font_semibold()
          .text_color(t.blocked)
          .child(div().flex_none().child("?"))
          .child(div().flex_1().min_w(0).whitespace_normal().child(card.question)),
      )
      .child(titleLine({ ...card, wait: "" }, t, rowIcons(`human-${card.key}`, { url: card.url, pr: card.pr }, t)))
      .when(card.marks.length > 0, (el) => el.child(chipRow(card.marks.map((mark) => chip(mark.text, toneColor(mark.tone, t), t)))))
      .child(div().text_size(12).text_color(t.muted).whitespace_normal().child(card.reason))
      .when(Boolean(card.approvalBlocked), (el) =>
        el.child(div().text_size(12).text_color(t.ansi1).whitespace_normal().child(card.approvalBlocked)),
      )
      .when(Boolean(card.failure?.summary), (el) =>
        el.child(div().text_size(12).text_color(t.muted).whitespace_normal().child(card.failure.summary)),
      )
      .children(
        card.summary.map((line, i) =>
          div()
            .id(`chg-${card.key}-${i}`)
            .text_size(12)
            .text_color(toneColor(line.tone, t))
            .whitespace_normal()
            .when(Boolean(line.tip), (el) => el.tooltip(line.tip))
            .child(line.mark ? `${line.mark} ${line.text}` : line.text),
        ),
      )
      .when(Boolean(card.overlap), (el) =>
        el.child(div().text_size(12).text_color(t.ansi3).whitespace_normal().child(card.overlap)),
      )
      .child(div().text_size(11).text_color(t.dim).whitespace_normal().child(card.facts.join(" · ")))
      .when(people.length > 0, (el) => el.child(h_flex().gap(5).items_start().children(people)))
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
      .when(Boolean(error), (el) => el.child(div().text_size(12).text_color(t.ansi1).whitespace_normal().child(error)))
      .when(card.actions.length > 0 || Boolean(card.diffUrl), (el) => el.child(buttons));
  }

  bouncedRow(repo, card, t) {
    const id = `bounced-${card.key}`;
    const row = card.report
      ? linkRow(id, t, () => this.openFile(card.report, repo.hub.machine))
      : hoverRow(h_flex().id(id).px(6).py(4), t);
    const dots = card.retryDots;
    const retries = dots
      ? h_flex()
          .id(`retries-${card.key}`)
          .flex_none()
          .gap(3)
          .items_center()
          .tooltip(dots.tip)
          .children(
            Array.from({ length: dots.total }, (_, i) =>
              div().w(7).h(7).rounded(4).flex_none().bg(i < dots.used ? toneColor(dots.tone, t) : t.border),
            ),
          )
      : div().flex_none().whitespace_nowrap().text_size(11).text_color(t.dim).child(`Retries ${card.retries}`);
    const note = card.summaryLine
      ? div()
          .id(`note-${card.key}`)
          .min_w(0)
          .truncate()
          .text_size(12)
          .text_color(t.muted)
          .tooltip(card.failure.summary)
          .child(card.summaryLine)
      : null;
    return gridRow(row, t, {
      note,
      narrow: this.narrow,
      pr: card.pr,
      title: card.title,
      time: timeCell(card.ago ?? "", t),
      icons: rowIcons(id, card, t, (file) => this.openFile(file, repo.hub.machine)),
      chips: [
        chip(card.headline, t.ansi1, t),
        ...card.needs.map((need) => chip(need.text, toneColor(NEED_TONE[need.kind], t), t)),
        h_flex().flex_none().gap(6).items_center().child(workerLine(card.agent, t)).child(retries),
      ],
    });
  }

  stripView(repo, t) {
    const strip = repo.strip;
    return v_flex()
      .gap(4)
      .child(
        sectionTitle("Last batches", strip.bars.length, t, textLink(`details-${repo.id}`, "Details ›", "Open batch details", t, () => this.openDetail(repo, "overview"))),
      )
      .child(
        h_flex()
          .h(28)
          .px(6)
          .gap(3)
          .items_end()
          .children(
            strip.bars.map((bar, i) =>
              div()
                .id(`bar-${repo.id}-${i}`)
                .flex_1()
                .min_w(0)
                .max_w(30)
                .h(4 + 24 * bar.height)
                .rounded(2)
                .bg(bar.tone === "bounced" ? t.ansi1 : t.ansi2)
                .tooltip(bar.tip),
            ),
          ),
      )
      .child(
        h_flex()
          .px(6)
          .text_size(11)
          .text_color(t.dim)
          .child(div().flex_1().child(strip.from))
          .child(div().flex_none().child(strip.to)),
      );
  }

  recentView(repo, t) {
    const open = this.moreOpen.has(repo.id);
    const { rows, hidden, more } = recentWindow(repo.recent, open);
    const groups = recentGroups(rows, repo.recentBatches);
    let at = 0;
    return v_flex()
      .gap(4)
      .child(sectionTitle("Recent", null, t))
      .children(
        groups.map((group) =>
          v_flex()
            .gap(2)
            .when(group.head != null, (el) =>
              el.child(div().px(6).min_w(0).truncate().text_size(11).text_color(t.dim).child(group.head)),
            )
            .children(
              group.rows.map((m) => {
                const i = at++;
                const id = `merged-${repo.id}-${i}`;
                const row = m.url ? linkRow(id, t, (cx) => cx.open_url(m.url)) : hoverRow(h_flex().id(id).px(6).py(4), t);
                return gridRow(row, t, {
                  narrow: this.narrow || this.wide,
                  pr: m.pr,
                  title: m.title,
                  time: timeCell(m.took, t).id(`took-${repo.id}-${i}`).when(Boolean(m.tip), (el) => el.tooltip(m.tip)),
                  icons: rowIcons(id, m, t),
                  chips: m.bounces > 0 ? [chip(`${m.bounces} ${m.bounces === 1 ? "bounce" : "bounces"}`, t.dim, t)] : [],
                });
              }),
            ),
        ),
      )
      .when(hidden > 0, (el) =>
        el.child(
          linkRow(`more-${repo.id}`, t, (cx) => this.toggleMore(repo, cx)).child(
            div().text_size(12).text_color(t.dim).child(more > 0 ? `+ ${more} more` : "Show less"),
          ),
        ),
      );
  }

  todayView(repo, t) {
    const open = this.opened.has(repo.id);
    const today = repo.today;
    return v_flex()
      .gap(2)
      .child(
        linkRow(`today-${repo.id}`, t, (cx) => this.toggleToday(repo, cx))
          .items_start()
          .child(div().flex_none().text_size(11).font_semibold().text_color(t.muted).child("TODAY"))
          .child(
            h_flex()
              .flex_1()
              .min_w(0)
              .flex_wrap()
              .gap_x(5)
              .items_baseline()
              .text_size(12)
              .children(
                today.parts.map((part, i) =>
                  div().flex_none().whitespace_nowrap().child(i < today.parts.length - 1 ? `${part} ·` : part),
                ),
              ),
          )
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
                      .when(Boolean(d.note), (row) => row.child(chip(d.note, t.ansi3, t)))
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
      .child(running === action.verb ? "Working…" : action.label);
  }
}

// A dot that fades in and out while the repo is verifying; the pulse runs natively and never calls `render`.
const dot = (color, active = false) =>
  div()
    .w(7)
    .h(7)
    .rounded(4)
    .flex_none()
    .bg(color)
    .when(active, (el) => el.loop("opacity", { from: 1, to: 0.35, period: 1600 }));

// chevron (10) + gap (8) + daemon dot (10) + gap (8)
const HEAD_INDENT = 36;
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

const emptyLine = (sections, t) =>
  div()
    .px(6)
    .text_size(11)
    .text_color(t.dim)
    .whitespace_normal()
    .child(`Empty: ${sections.map((section) => section.title).join(" · ")}`);

const underNeedsYou = (card, t) =>
  div().px(6).py(2).text_size(12).text_color(t.dim).child(`#${card.pr} is under Needs you`);

// The time at the right end of a row: it never pushes the title out, and it keeps its place when empty.
const timeCell = (text, t) =>
  mono(div().flex_none().whitespace_nowrap().min_w(44).text_right().text_size(11).text_color(t.dim)).child(text);

function sectionTitle(title, count, t, aside) {
  return h_flex()
    .gap(6)
    .items_center()
    .px(6)
    .child(div().flex_1().min_w(0).text_size(11).font_semibold().text_color(t.muted).child(title.toUpperCase()))
    .when(count != null, (el) => el.child(div().flex_none().min_w(24).text_right().text_size(11).text_color(t.dim).child(String(count))))
    .when(aside != null, (el) => el.child(aside));
}

// A square icon button at the end of a row. Its click must not reach the row's own handler.
function iconLink(id, glyph, tip, t, onClick) {
  return div()
    .id(id)
    .flex_none()
    .w(ICON_SIZE)
    .h(ICON_SIZE)
    .flex()
    .items_center()
    .justify_center()
    .rounded(4)
    .text_size(12)
    .text_color(t.dim)
    .cursor_pointer()
    .hover((s) => s.bg(t.hover).text_color(t.foreground))
    .tooltip(tip)
    .on_click((_e, cx) => {
      cx.stop_propagation();
      onClick(cx);
    })
    .child(glyph);
}

// A small text button inside a card; like an icon button, its click stays its own.
function textLink(id, label, tip, t, onClick) {
  return div()
    .id(id)
    .flex_none()
    .px(4)
    .rounded(4)
    .text_size(11)
    .text_color(t.dim)
    .cursor_pointer()
    .hover((s) => s.bg(t.hover).text_color(t.foreground))
    .tooltip(tip)
    .on_click((_e, cx) => {
      cx.stop_propagation();
      onClick(cx);
    })
    .child(label);
}

const iconSlot = () => div().flex_none().w(ICON_SIZE).h(ICON_SIZE);

// Two fixed slots, `≡` then `↗`; a slot with nothing to open stays empty so `↗` keeps its column.
function rowIcons(id, row, t, openReport) {
  const log = row.report
    ? iconLink(`${id}-log`, "≡", `Open ${row.report.label.toLowerCase()}`, t, () => openReport(row.report))
    : iconSlot();
  const pr = row.url ? iconLink(`${id}-pr`, "↗", `Open #${row.pr} on GitHub`, t, (cx) => cx.open_url(row.url)) : iconSlot();
  return [log, pr];
}

const prCell = (pr, t) => mono(div().flex_none().whitespace_nowrap().min_w(40).font_semibold().text_color(t.accent)).child(`#${pr}`);

const chipRow = (children) => h_flex().gap(5).flex_wrap().items_center().children(children);

// One grid for every PR row: index · PR · title (chips and submitter below it) · time and two icon slots.
// `row` is the row's own element (id, hover, click); `spec.index` is empty for rows without a position.
// When `spec.narrow`, the time and icons move to the line under the title, right-aligned beside the chips.
function gridRow(row, t, spec) {
  const aside = () =>
    h_flex()
      .flex_none()
      .items_start()
      .gap(2)
      .child(spec.time ?? timeCell("", t))
      .children(spec.icons ?? [iconSlot(), iconSlot()]);
  const hasChips = Boolean(spec.chips?.length);
  const submitter = Boolean(spec.submitter);
  const indexCell = () =>
    div().w(18).flex_none().text_right().text_size(12).text_color(t.dim).child(spec.index == null ? "" : String(spec.index));
  if (spec.narrow) {
    const body = v_flex()
      .flex_1()
      .min_w(0)
      .gap(3)
      .child(div().whitespace_normal().child(spec.title))
      .child(
        h_flex()
          .gap(6)
          .items_start()
          // Chips may shrink and wrap their text here, so a long one never runs under the time and icons.
          .child(div().flex_1().min_w(0).when(hasChips, (el) => el.child(chipRow(spec.chips.map((c) => c.min_w(0).flex_shrink(1))))))
          .child(aside()),
      )
      .when(Boolean(spec.note), (el) => el.child(spec.note))
      .when(submitter, (el) => el.child(submitterLine(spec.submitter, t)));
    return row
      .gap(6)
      .items_start()
      .when(spec.index != null, (el) => el.child(indexCell()))
      .child(prCell(spec.pr, t))
      .child(body);
  }
  const body = v_flex()
    .flex_1()
    .min_w(0)
    .gap(3)
    .child(div().whitespace_normal().child(spec.title))
    .when(hasChips, (el) => el.child(chipRow(spec.chips)))
    .when(Boolean(spec.note), (el) => el.child(spec.note))
    .when(submitter, (el) => el.child(submitterLine(spec.submitter, t)));
  return row.gap(6).items_start().child(indexCell()).child(prCell(spec.pr, t)).child(body).child(aside());
}

// The worker of a bounced PR: a life dot, its name, and `exited` once it is gone.
function workerLine(agent, t) {
  if (!agent) return div().flex_none().text_size(11).text_color(t.dim).child("No agent");
  return h_flex()
    .flex_none()
    .gap(5)
    .items_center()
    .child(div().w(7).h(7).rounded(4).flex_none().bg(agent.alive ? t.ansi2 : t.dim))
    .child(
      div()
        .whitespace_nowrap()
        .text_size(11)
        .text_color(agent.alive ? t.muted : t.dim)
        .child(agent.alive ? agent.label : `${agent.label} · exited`),
    );
}

const hoverRow = (el, t) => el.rounded(6).hover((s) => s.bg(t.hover));

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

function chip(text, color, t) {
  return div().flex_none().px(6).py(1).rounded(4).bg(faded(t, color, CHIP_ALPHA)).text_size(11).text_color(color).child(text);
}

function titleLine(card, t, icons = []) {
  return h_flex()
    .gap(6)
    .items_start()
    .child(prCell(card.pr, t))
    .child(div().flex_1().min_w(0).whitespace_normal().child(card.title))
    .when(Boolean(card.wait), (el) => el.child(timeCell(card.wait, t)))
    .children(icons);
}

function agentChip(agent, t, cardKey, text) {
  if (!agent) return div().flex_1().min_w(0).text_size(12).text_color(t.dim).child("No agent");
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
        .child(text ? (agent.alive ? text : `${text} · exited`) : agent.alive ? agent.label : `${agent.label} · exited`),
    )
    .when(Boolean(agent.host), (el) => el.child(chip(agent.host, t.muted, t)));
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

function batchView(repo, batch, t, narrow) {
  const step = batch.waitingMachine ? "Waiting for a slot" : batch.stepText ?? batch.phase ?? "Verifying";
  const bar = batch.stepBar;
  const at = bar.findIndex((s) => s.state === "current");
  const names = (state) => bar.filter((s) => s.state === state).map((s) => s.name);
  const done = bar
    .filter((s) => s.state === "done")
    .map((s) => (s.seconds != null ? `${s.name} ${formatDuration(s.seconds)}` : s.name));
  const todo = names("todo");
  const skipped = names("skipped");
  const runCount = bar.length - skipped.length;
  const current = at >= 0 ? bar[at] : null;
  const currentTimes = current && current.elapsed != null
    ? ` · ${formatDuration(current.elapsed)}${current.usual != null ? ` / ${formatDuration(current.usual)}` : ""}`
    : "";
  const fill = batch.waitingMachine ? t.muted : batch.longer ? t.ansi3 : t.working;
  return v_flex()
    .gap(6)
    .p(10)
    .rounded(8)
    .bg(faded(t, t.foreground, SURFACE_ALPHA))
    .child(
      h_flex()
        .gap(6)
        .px(6)
        .items_center()
        .child(div().text_color(t.working).child("").frames(SPINNER, 90))
        .child(div().text_size(11).font_semibold().text_color(t.muted).child("CURRENT BATCH"))
        .child(div().flex_1())
        .when(Boolean(batch.elapsed), (el) =>
          el.child(
            mono(div().flex_none().whitespace_nowrap().text_size(12))
              .text_color(batch.longer ? t.ansi3 : t.foreground)
              .child(batch.elapsed),
          ),
        )
        .when(Boolean(batch.usual), (el) =>
          el.child(div().flex_none().whitespace_nowrap().text_size(11).text_color(t.dim).child(`/ ${batch.usual}`)),
        ),
    )
    .children(
      batch.prs.map((entry, i) =>
        gridRow(hoverRow(h_flex().id(`batch-${repo.id}-${i}`).px(6).py(2), t), t, {
          narrow,
          pr: entry.pr,
          title: entry.title,
          icons: rowIcons(`batch-${repo.id}-${i}`, entry, t),
          chips: entry.marks.map((mark) => chip(mark.text, toneColor(mark.tone, t), t)),
          submitter: entry.submitter,
        }),
      ),
    )
    .when(bar.length > 0, (el) =>
      el.child(
        h_flex()
          .gap(3)
          .px(6)
          .children(
            bar.map((s, i) =>
              div()
                .id(`step-${repo.id}-${i}`)
                .flex_1()
                .min_w(0)
                .h(4)
                .rounded(2)
                .bg(
                  s.state === "done"
                    ? t.ansi2
                    : s.state === "current"
                      ? fill
                      : s.state === "skipped"
                        ? faded(t, t.foreground, SKIPPED_ALPHA)
                        : t.border,
                )
                .tooltip(s.state === "skipped" ? `${s.name} · skipped (paths)` : s.name),
            ),
          ),
      ),
    )
    .when(done.length > 0, (el) =>
      el.child(div().px(6).text_size(11).text_color(t.dim).whitespace_normal().child(`✓ ${done.join(" · ")}`)),
    )
    .child(
      h_flex()
        .gap(8)
        .px(6)
        .items_baseline()
        .child(
          div()
            .flex_1()
            .min_w(0)
            .when(bar.length === 0, (d) => d.truncate())
            .text_size(12)
            .text_color(batch.waitingMachine ? t.muted : t.working)
            .child(
              bar.length === 0 || batch.waitingMachine
                ? step
                : at >= 0
                  ? `▸ ${bar[at].name}  ${at + 1} of ${runCount}${currentTimes}`
                  : step,
            ),
        )
        .when(batch.longer, (el) => el.child(div().flex_none().text_size(11).text_color(t.ansi3).child("Slower than usual"))),
    )
    .when(todo.length > 0 && bar.length > 0, (el) =>
      el.child(div().px(6).text_size(11).text_color(t.dim).whitespace_normal().child(`○ ${todo.join(" · ")}`)),
    )
    .when(skipped.length > 0, (el) =>
      el.child(div().px(6).text_size(11).text_color(t.dim).whitespace_normal().child(`– ${skipped.join(" · ")} · skipped (paths)`)),
    )
    .child(div().px(6).text_size(11).text_color(t.dim).whitespace_normal().child(batch.phaseLine));
}

function postVerifyView(post, t) {
  const row = (mark, color, text, right) =>
    h_flex()
      .gap(8)
      .px(6)
      .py(2)
      .items_start()
      .child(div().w(10).flex_none().text_color(color).child(mark))
      .child(div().flex_1().min_w(0).whitespace_normal().text_size(12).child(text))
      .when(Boolean(right), (el) => el.child(div().flex_none().min_w(44).text_right().text_size(11).text_color(t.dim).child(right)));
  const run = post.run;
  return v_flex()
    .gap(2)
    .child(sectionTitle("Post-merge", null, t))
    .when(Boolean(post.green), (el) => el.child(row("✓", t[toneToken("ok")], `${post.green.commit} passed`, post.green.ago)))
    .when(Boolean(run), (el) =>
      el
        .child(
          row(
            "◌",
            t.working,
            run.waitingMachine
              ? `${run.commit} waiting for a slot`

              : `${run.commit} ${run.label}${run.step ? ` · ${run.step}` : ""}`,
            run.elapsed && run.usual ? `${run.elapsed} / usually ${run.usual}` : run.elapsed,
          ),
        )
        .child(
          run.progress != null
            ? h_flex()
                .ml(24)
                .mr(6)
                .h(4)
                .rounded(2)
                .bg(t.border)
                .child(div().h(4).rounded(2).flex_grow(run.progress).flex_basis(0).bg(run.longer ? t.ansi3 : t.working))
                .child(div().h(4).flex_grow(1 - run.progress).flex_basis(0))
            : div()
                .ml(24)
                .mr(6)
                .h(4)
                .rounded(2)
                .bg(run.waitingMachine ? t.muted : t.working)
                .when(!run.waitingMachine, (bar) => bar.loop("opacity", { from: 1, to: 0.35, period: 1600 })),
        ),
    );
}

function waitingRow(repo, item, index, t, narrow) {
  const id = `queued-${repo.id}-${index}`;
  const chips = [
    ...item.marks.map((mark) => chip(mark.text, toneColor(mark.tone, t), t)),
    ...(item.ciPending ? [chip(item.label, t.ansi3, t)] : []),
    ...(item.note ? [chip(item.note, t.dim, t)] : []),
  ];
  return gridRow(hoverRow(h_flex().id(id).px(6).py(2), t), t, {
    narrow,
    index: item.position,
    pr: item.pr,
    title: item.title,
    time: timeCell(item.wait ?? "", t),
    icons: rowIcons(id, item, t),
    chips,
    submitter: item.submitter,
  });
}

function unknownRow(item, t, narrow) {
  return gridRow(h_flex().px(6).py(2), t, {
    narrow,
    pr: item.pr,
    title: item.title,
    chips: [chip(item.reason ? `${item.state} · ${item.reason}` : item.state, t.ansi3, t)],
  });
}
