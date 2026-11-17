import { View, div, svg } from "gpui-kit";
import { spaces, panes, run, openOverlay, theme, window, shared, layout, settings } from "herdr";
class Tabs {
  syncPanel(space, panel, body, enabled) {
    const now = panel.hidden ? null : body?.active ?? null;
    if (changed) {
          if (want) { run("layout.select", { stack: "panel-body", child: want }); run("layout.show", { id: "panel" }); }
          else run("layout.hide", { id: "panel" });
    }
  }
  render() {
      const on = panel && !panel.hidden && body?.active === id;
        .on_click(() => {
          if (on) run("layout.hide", { id: "panel" });
          else { run("layout.select", { stack: "panel-body", child: id }); run("layout.show", { id: "panel" }); }
        }).child(label);
      .when(w?.buttons, el => el.child(button("minimize", w.platform === "windows" ? svg("mods/tabs/win-min.svg").size(16) : "−", () => run("window.minimize")))
  }
}
