import test from "node:test";
import assert from "node:assert/strict";

import { chuteDot, chuteTip, filesNow, panelActions, panelState, restoreActions } from "./chute.js";

const state = (panel, body, chute, active = "files") => ({ panel, body, chute, active });
const names = actions => actions.map(a => `${a.name} ${a.args.id ?? a.args.child}`);
const press = (s, button) => names(panelActions(s, button));

const CLOSED = state(false, false, false);
const BOTH = state(true, true, true);
const CHUTE_ONLY = state(true, false, true);
const FILES_ONLY = state(true, true, false);

test("closed panel: chute opens only chute", () => {
  assert.deepEqual(press(CLOSED, "chute"), ["layout.hide panel-body", "layout.show chute", "layout.show panel"]);
});

test("closed panel: Files opens only Files", () => {
  assert.deepEqual(press(CLOSED, "files"), [
    "layout.select files",
    "layout.show panel-body",
    "layout.hide chute",
    "layout.show panel",
  ]);
});

test("both open: chute closes only chute", () => {
  assert.deepEqual(press(BOTH, "chute"), ["layout.hide chute"]);
});

test("both open: Files closes only Files", () => {
  assert.deepEqual(press(BOTH, "files"), ["layout.hide panel-body"]);
});

test("chute only: chute closes the panel", () => {
  assert.deepEqual(press(CHUTE_ONLY, "chute"), ["layout.hide panel"]);
});

test("chute only: Files adds Files", () => {
  assert.deepEqual(press(CHUTE_ONLY, "files"), ["layout.select files", "layout.show panel-body"]);
});

test("Files only: chute adds chute", () => {
  assert.deepEqual(press(FILES_ONLY, "chute"), ["layout.show chute"]);
});

test("Files only: Files closes the panel", () => {
  assert.deepEqual(press(FILES_ONLY, "files"), ["layout.hide panel"]);
});

test("panel shown with panel-body and chute both hidden behaves like closed", () => {
  const empty = state(true, false, false);
  assert.deepEqual(press(empty, "chute"), press(CLOSED, "chute"));
  assert.deepEqual(press(empty, "files"), press(CLOSED, "files"));
});

test("hidden panel ignores stale node flags", () => {
  const stale = state(false, true, true);
  assert.deepEqual(press(stale, "chute"), press(CLOSED, "chute"));
  assert.deepEqual(press(stale, "changes"), press(CLOSED, "changes"));
});

test("another Files-side button only switches the selection", () => {
  const changes = state(true, true, true, "changes");
  assert.deepEqual(press(changes, "files"), ["layout.select files"]);
  assert.deepEqual(press(state(true, true, false, "changes"), "files"), ["layout.select files"]);
  assert.deepEqual(press(changes, "changes"), ["layout.hide panel-body"]);
});

test("commands carry the stack and ids Herdr expects", () => {
  assert.deepEqual(panelActions(CLOSED, "files")[0], {
    name: "layout.select",
    args: { stack: "panel-body", child: "files" },
  });
  assert.deepEqual(panelActions(BOTH, "chute")[0], { name: "layout.hide", args: { id: "chute" } });
});

test("panelState reads hidden flags and the selection", () => {
  const got = panelState({ hidden: false }, { hidden: true, active: "changes" }, undefined);
  assert.deepEqual(got, { panel: true, body: false, chute: false, active: "changes" });
  assert.deepEqual(panelState(undefined, undefined, { hidden: false }), {
    panel: false,
    body: false,
    chute: true,
    active: null,
  });
});

test("filesNow is the selection only while the Files side is on screen", () => {
  assert.equal(filesNow(BOTH), "files");
  assert.equal(filesNow(CHUTE_ONLY), null);
  assert.equal(filesNow(CLOSED), null);
  assert.equal(filesNow(state(true, true, false, "changes")), "changes");
});

test("restoring a space with a Files choice selects it", () => {
  assert.deepEqual(names(restoreActions("files", BOTH)), [
    "layout.select files",
    "layout.show panel-body",
    "layout.show panel",
  ]);
  assert.deepEqual(names(restoreActions("files", CLOSED)), [
    "layout.select files",
    "layout.show panel-body",
    "layout.hide chute",
    "layout.show panel",
  ]);
});

test("restoring a space without a choice keeps chute if it is on screen", () => {
  assert.deepEqual(names(restoreActions(null, BOTH)), ["layout.hide panel-body"]);
  assert.deepEqual(names(restoreActions(null, FILES_ONLY)), ["layout.hide panel"]);
  assert.deepEqual(names(restoreActions(null, CLOSED)), ["layout.hide panel"]);
});

test("the dot follows the board grade and falls back to muted", () => {
  const theme = { ansi1: "red", blocked: "amber", working: "blue", ansi2: "green", muted: "grey" };
  assert.equal(chuteDot(theme, { grade: "failed" }), "red");
  assert.equal(chuteDot(theme, { grade: "attention" }), "amber");
  assert.equal(chuteDot(theme, { grade: "running" }), "blue");
  assert.equal(chuteDot(theme, { grade: "ok" }), "green");
  assert.equal(chuteDot(theme, { grade: "unknown" }), "grey");
  assert.equal(chuteDot(theme, null), "grey");
});

test("the tooltip summarises the board", () => {
  assert.equal(chuteTip(null), "chute board is closed");
  assert.equal(chuteTip({ repos: 2, running: 1, issues: 3 }), "2 repos · 1 running · 3 issues");
  assert.equal(chuteTip({}), "? repos · 0 issues");
});
