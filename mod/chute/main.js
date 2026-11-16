import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { exec, props, run, theme, watchFolder } from "herdr";
import { LOCAL_POLL_MS, causeText, failureText, interpret, pollInterval } from "./board.js";

const TICK_MS = 1000;
const MIN_READ_GAP_MS = 1000;
const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

export default class Chute extends View {
  init(_props, cx) {
    const bin = props()?.bin;
    this.bin = typeof bin === "string" && bin ? bin : "chute";
    this.result = null;
    this.reading = false;
    this.dirty = false;
    this.watching = new Set();
    this.busy = new Map();
    this.errors = new Map();
    this.sinceRead = 0;
    this.pollMs = LOCAL_POLL_MS;
    this.request(cx);
    cx.timer.every(TICK_MS, (cx) => this.tick(cx));
  }

  tick(cx) {
    this.sinceRead += TICK_MS;
    if (!this.reading && this.sinceRead >= this.pollMs) this.request(cx);
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
        this.settle(interpret(result), cx);
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
    for (const repo of board.repos) this.watch(repo.stateDir, cx);
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
          .child(chip(`허브 ${repo.hub.label}`, repo.hub.reachable ? t.muted : t.ansi1))
          .when(Boolean(repo.approval), (el) => el.child(chip(repo.approval, t.muted))),
      )
      .when(!repo.hub.reachable, (el) =>
        el.child(
          v_flex()
            .gap(4)
            .p(9)
            .rounded(8)
            .border_1()
            .border_color(t.ansi1)
            .child(div().font_semibold().text_color(t.ansi1).child("허브에 닿지 않는다"))
            .when(Boolean(repo.hub.error), (box) =>
              box.child(div().text_size(12).text_color(t.muted).whitespace_normal().child(repo.hub.error)),
            ),
        ),
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
    const failure = this.errors.get(card.key);
    const error = failure?.state === card.state ? failure.text : null;
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
          .children(card.marks.map((mark) => chip(mark.text, tone(mark.tone, t))))
          .child(chip(card.reason, human ? t.blocked : t.ansi1))
          .when(Boolean(card.failure?.mode), (el) => el.child(chip(card.failure.mode, t.muted)))
          .when(Boolean(card.preflight), (el) => el.child(chip(card.preflight.text, tone(card.preflight.tone, t))))
          .when(Boolean(card.config), (el) =>
            el.child(chip(card.config, card.configChanged ? t.blocked : t.dim)),
          )
          .when(Boolean(card.approvalBlocked), (el) => el.child(chip(card.approvalBlocked, t.ansi1))),
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
        .when(Boolean(item.detail), (el) => el.child(div().text_size(12).text_color(t.dim).whitespace_normal().child(item.detail)))
        .children(item.facts.map((fact) => div().text_size(12).text_color(t.muted).whitespace_normal().child(fact)))
        .when(Boolean(item.log), (el) =>
          el.child(
            div()
              .w_full()
              .min_w(0)
              .overflow_hidden()
              .whitespace_nowrap()
              .text_ellipsis_start()
              .text_size(11)
              .text_color(t.dim)
              .child(item.log),
          ),
        ),
    );
}

function batchCard(batch, t) {
  const facts = [
    batch.waitingMachine ? "PC 자리를 기다리는 중" : batch.step,
    batch.round,
    batch.reused,
    batch.elapsed && `경과 ${batch.elapsed}`,
  ].filter(Boolean);
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
        v_flex()
          .gap(3)
          .child(
            h_flex()
              .gap(6)
              .items_start()
              .child(div().flex_none().font_semibold().text_color(t.accent).child(`#${entry.pr}`))
              .child(div().flex_1().min_w(0).whitespace_normal().child(entry.title)),
          )
          .when(entry.marks.length > 0, (el) =>
            el.child(h_flex().gap(5).flex_wrap().children(entry.marks.map((mark) => chip(mark.text, tone(mark.tone, t))))),
          )
          .when(Boolean(entry.submitter), (el) => el.child(submitterLine(entry.submitter, t))),
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
    .when(item.ciPending || Boolean(item.note) || item.marks.length > 0, (el) =>
      el.child(
        h_flex()
          .pl(24)
          .gap(5)
          .flex_wrap()
          .children(item.marks.map((mark) => chip(mark.text, tone(mark.tone, t))))
          .when(item.ciPending, (row) => row.child(chip(item.label, t.ansi3)))
          .when(Boolean(item.note), (row) => row.child(chip(item.note, t.dim))),
      ),
    )
    .when(Boolean(item.submitter), (el) => el.child(div().pl(24).child(submitterLine(item.submitter, t))));
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
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(div().font_semibold().text_size(15).child(stat.value))
              .when(Boolean(stat.note), (el) => el.child(chip(stat.note, t.ansi3))),
          )
          .child(div().text_size(11).text_color(t.dim).whitespace_normal().child(stat.label)),
      ),
    );
}
