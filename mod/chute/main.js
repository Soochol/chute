import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { exec, props, run, theme, watchFolder } from "herdr";
import { firstLine, interpret } from "./board.js";

const FALLBACK_MS = 15000;
const MIN_READ_GAP_MS = 1000;
const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

export default class Chute extends View {
  init(_props, cx) {
    const bin = props()?.bin;
    this.bin = typeof bin === "string" && bin ? bin : "chute";
    this.result = null;
    this.loading = false;
    this.dirty = false;
    this.scheduled = false;
    this.last = 0;
    this.watching = new Set();
    this.busy = new Set();
    this.errors = new Map();
    this.request(cx);
    cx.timer.every(FALLBACK_MS, (cx) => this.request(cx));
  }

  request(cx) {
    if (this.loading) {
      this.dirty = true;
      return;
    }
    if (this.scheduled) return;
    this.scheduled = true;
    cx.timer.after(Math.max(0, this.last + MIN_READ_GAP_MS - Date.now()), (cx) => {
      this.scheduled = false;
      this.load(cx);
    });
  }

  load(cx) {
    this.loading = true;
    cx.spawn(async (cx) => {
      let result;
      try {
        const r = await exec(this.bin, ["status", "--json", "--all"]);
        result = { code: r.code, stdout: r.stdout, stderr: r.stderr };
      } catch (e) {
        result = { spawnError: String(e?.message ?? e) };
      }
      this.result = result;
      this.last = Date.now();
      this.loading = false;
      const board = interpret(result);
      if (board.kind === "board") for (const repo of board.repos) this.watch(repo.stateDir, cx);
      cx.notify();
      if (this.dirty) {
        this.dirty = false;
        this.request(cx);
      }
    });
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
    const key = `${card.key}:${action.verb}`;
    if (this.busy.has(key)) return;
    this.busy.add(key);
    this.errors.delete(card.key);
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const r = await exec(this.bin, action.args);
        if (r.code !== 0) this.errors.set(card.key, firstLine(r.stderr) || firstLine(r.stdout) || `종료 코드 ${r.code}`);
      } catch (e) {
        this.errors.set(card.key, firstLine(e?.message ?? e));
      }
      this.busy.delete(key);
      cx.notify();
      this.request(cx);
    });
  }

  render() {
    const t = theme();
    const body = this.result ? interpret(this.result) : { kind: "loading" };
    return v_flex()
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
      .child(
        h_flex()
          .h(42)
          .flex_none()
          .px(14)
          .gap(8)
          .items_center()
          .border_b(1)
          .border_color(t.border)
          .child(div().font_semibold().child("chute")),
      )
      .child(
        v_flex()
          .id("chute-body")
          .flex_1()
          .min_h(0)
          .overflow_y_scrollbar()
          .p(10)
          .gap(16)
          .children(this.bodyOf(body, t)),
      );
  }

  bodyOf(body, t) {
    if (body.kind === "loading") {
      return [h_flex().gap(8).text_color(t.dim).child(div().frames(SPINNER, 90)).child("불러오는 중")];
    }
    if (body.kind !== "board") return [notice(body, t)];
    return body.repos.map((repo) => this.repoView(repo, t));
  }

  repoView(repo, t) {
    return v_flex()
      .gap(10)
      .child(
        h_flex()
          .gap(8)
          .items_center()
          .child(div().flex_1().min_w(0).truncate().font_semibold().child(repo.slug))
          .when(Boolean(repo.approval), (el) => el.child(chip(repo.approval, t.muted))),
      )
      .children(repo.sections.map((section) => this.sectionView(section, t)));
  }

  sectionView(section, t) {
    const items = section.items;
    const rows = {
      alerts: (item) => alertRow(item, t),
      human: (item) => this.cardView(item, t, { human: true }),
      batch: (item) => batchCard(item, t),
      waiting: (item) => waitingRow(item, t),
      bounced: (item) => this.cardView(item, t, { human: false }),
      unknown: (item) => unknownRow(item, t),
    };
    return v_flex()
      .gap(6)
      .child(
        h_flex()
          .gap(6)
          .items_center()
          .child(div().text_size(11).font_semibold().text_color(t.muted).child(section.title))
          .when(section.id !== "today" && section.id !== "batch", (el) =>
            el.child(div().text_size(11).text_color(t.dim).child(String(items.length))),
          ),
      )
      .when(section.id === "today", (el) => el.child(todayGrid(items, t)))
      .when(section.id !== "today", (el) => el.children(items.map((item) => rows[section.id](item))));
  }

  cardView(card, t, { human }) {
    const error = this.errors.get(card.key);
    return v_flex()
      .gap(5)
      .p(9)
      .rounded(8)
      .bg(t.surface)
      .border_1()
      .border_color(human ? t.blocked : t.border)
      .child(titleLine(card, t))
      .child(
        h_flex()
          .gap(5)
          .flex_wrap()
          .items_center()
          .child(chip(card.reason, human ? t.blocked : t.ansi1))
          .when(Boolean(card.failure?.mode), (el) => el.child(chip(card.failure.mode, t.muted)))
          .when(Boolean(card.preflight), (el) => el.child(chip(card.preflight.text, tone(card.preflight.tone, t))))
          .when(Boolean(card.config), (el) =>
            el.child(chip(card.config, card.configChanged ? t.blocked : t.dim)),
          ),
      )
      .when(Boolean(card.failure?.summary), (el) =>
        el.child(div().text_size(12).text_color(t.muted).whitespace_normal().child(card.failure.summary)),
      )
      .child(
        h_flex()
          .gap(8)
          .items_start()
          .child(agentChip(card.agent, t))
          .child(div().flex_none().text_size(12).text_color(t.dim).child(`재시도 ${card.retries}`)),
      )
      .when(Boolean(card.failure?.report), (el) =>
        el.child(
          div()
            .w_full()
            .min_w(0)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis_start()
            .text_size(11)
            .text_color(t.dim)
            .child(card.failure.report),
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

  button(card, action, primary, t) {
    const pending = this.busy.has(`${card.key}:${action.verb}`);
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
      .when(pending, (el) => el.opacity(0.5))
      .hover((s) => s.bg(primary ? t.solid : t.hover))
      .on_click((_e, cx) => this.act(card, action, cx))
      .child(pending ? "처리 중…" : action.label);
  }
}

const tone = (name, t) => ({ ok: t.ansi2, error: t.ansi1, warn: t.ansi3 }[name] ?? t.dim);

function notice(body, t) {
  return v_flex()
    .gap(6)
    .p(10)
    .rounded(8)
    .border_1()
    .border_color(body.kind === "empty" ? t.border : t.ansi1)
    .child(div().font_semibold().whitespace_normal().child(body.title))
    .when(Boolean(body.detail), (el) => el.child(div().text_size(12).text_color(t.muted).whitespace_normal().child(body.detail)))
    .when(Boolean(body.hint), (el) => el.child(div().text_size(12).text_color(t.dim).whitespace_normal().child(body.hint)));
}

function chip(text, color) {
  return div()
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

function agentChip(agent, t) {
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
    );
  if (!agent.alive) return label;
  return label
    .id(`peek-${agent.terminal}`)
    .cursor_pointer()
    .rounded(4)
    .hover((s) => s.bg(t.hover))
    .on_click(() => run("terminal.peek", { terminal: agent.terminal }));
}

function alertRow(item, t) {
  const color = tone(item.tone, t);
  return h_flex()
    .gap(8)
    .items_start()
    .child(div().w(7).h(7).mt(5).rounded(4).flex_none().bg(color))
    .child(
      v_flex()
        .flex_1()
        .min_w(0)
        .child(div().whitespace_normal().text_color(item.tone === "error" ? t.ansi1 : t.foreground).child(item.text))
        .when(Boolean(item.detail), (el) => el.child(div().text_size(12).text_color(t.dim).whitespace_normal().child(item.detail))),
    );
}

function batchCard(batch, t) {
  const facts = [batch.step, batch.round, batch.elapsed && `경과 ${batch.elapsed}`].filter(Boolean);
  return v_flex()
    .gap(5)
    .p(9)
    .rounded(8)
    .bg(t.surface)
    .border_1()
    .border_color(t.working)
    .child(
      h_flex()
        .gap(6)
        .items_center()
        .child(div().text_color(t.working).child("").frames(SPINNER, 90))
        .child(div().font_semibold().text_color(t.working).child(batch.phase ?? "검증 중")),
    )
    .when(facts.length > 0, (el) => el.child(div().text_size(12).text_color(t.muted).whitespace_normal().child(facts.join(" · "))))
    .children(
      batch.prs.map((entry) =>
        h_flex()
          .gap(6)
          .items_start()
          .child(div().flex_none().font_semibold().text_color(t.accent).child(`#${entry.pr}`))
          .child(div().flex_1().min_w(0).whitespace_normal().child(entry.title)),
      ),
    );
}

function numberedTitle(prefix, item, t) {
  return h_flex()
    .gap(6)
    .items_start()
    .when(prefix != null, (el) => el.child(div().w(18).flex_none().text_color(t.dim).child(String(prefix))))
    .child(div().flex_none().font_semibold().text_color(t.accent).child(`#${item.pr}`))
    .child(div().flex_1().min_w(0).whitespace_normal().child(item.title));
}

function waitingRow(item, t) {
  return v_flex()
    .gap(3)
    .child(numberedTitle(item.position, item, t))
    .when(item.ciPending || Boolean(item.note), (el) =>
      el.child(
        h_flex()
          .pl(24)
          .gap(5)
          .flex_wrap()
          .when(item.ciPending, (row) => row.child(chip(item.label, t.ansi3)))
          .when(Boolean(item.note), (row) => row.child(chip(item.note, t.dim))),
      ),
    );
}

function unknownRow(item, t) {
  return v_flex()
    .gap(3)
    .child(numberedTitle(null, item, t))
    .child(h_flex().gap(5).flex_wrap().child(chip(item.reason ? `${item.state} · ${item.reason}` : item.state, t.ansi3)));
}

function todayGrid(stats, t) {
  return h_flex()
    .gap(6)
    .flex_wrap()
    .children(
      stats.map((stat) =>
        v_flex()
          .min_w(96)
          .flex_1()
          .px(9)
          .py(6)
          .rounded(8)
          .bg(t.surface)
          .border_1()
          .border_color(t.border)
          .child(div().font_semibold().text_size(15).child(stat.value))
          .child(div().text_size(11).text_color(t.dim).whitespace_normal().child(stat.label)),
      ),
    );
}
