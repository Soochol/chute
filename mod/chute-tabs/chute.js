// Kept free of Herdr imports so that `node --test` can run them.

const TONE = { failed: "ansi1", attention: "blocked", running: "working", ok: "ansi2" };

export function chuteDot(theme, status) {
  return (status && theme[TONE[status.grade]]) || theme.muted;
}

export function chuteTip(status) {
  if (!status) return "chute board is closed";
  const parts = [`${status.repos ?? "?"} repos`];
  if (status.running) parts.push(`${status.running} running`);
  parts.push(`${status.issues ?? 0} to do`);
  return parts.join(" · ");
}

export function panelState(panel, body, chute) {
  const shown = node => !!node && !node.hidden;
  return { panel: shown(panel), body: shown(body), chute: shown(chute), active: body?.active ?? null };
}

export function filesNow(state) {
  return state.panel && state.body ? state.active : null;
}

const show = id => ({ name: "layout.show", args: { id } });
const hide = id => ({ name: "layout.hide", args: { id } });
const select = child => ({ name: "layout.select", args: { stack: "panel-body", child } });

export function panelActions(state, button) {
  const files = state.panel && state.body;
  const chute = state.panel && state.chute;
  const empty = !files && !chute;
  if (button === "chute") {
    if (empty) return [hide("panel-body"), show("chute"), show("panel")];
    if (files && chute) return [hide("chute")];
    if (chute) return [hide("panel")];
    return [show("chute")];
  }
  if (empty) return [select(button), show("panel-body"), hide("chute"), show("panel")];
  if (files && state.active !== button) return [select(button)];
  if (files && chute) return [hide("panel-body")];
  if (files) return [hide("panel")];
  return [select(button), show("panel-body")];
}

export function restoreActions(want, state) {
  if (want) {
    const actions = [select(want), show("panel-body")];
    if (!state.panel) actions.push(hide("chute"));
    return [...actions, show("panel")];
  }
  if (state.panel && state.chute) return [hide("panel-body")];
  return [hide("panel")];
}
