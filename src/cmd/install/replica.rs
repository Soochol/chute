use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

pub const TABS_MOD: &str = "chute-tabs";
pub const LAYOUT_NAME: &str = "chute";

const MARKER: &str = "// chute install --layout made this file and the next install overwrites it.";
const FINGERPRINT_KEY: &str = "Source mods/tabs/main.js fnv1a64=";

const IMPORT_HERDR: &str = r#"import { spaces, panes, run, openOverlay, theme, window, shared, layout, settings } from "herdr";"#;
const NOW_LINE: &str = "    const now = panel.hidden ? null : body?.active ?? null;";
const RESTORE_SELECT: &str = r#"          if (want) { run("layout.select", { stack: "panel-body", child: want }); run("layout.show", { id: "panel" }); }"#;
const RESTORE_HIDE: &str = r#"          else run("layout.hide", { id: "panel" });"#;
const ON_LINE: &str = "      const on = panel && !panel.hidden && body?.active === id;";
const CLICK_HIDE: &str = r#"          if (on) run("layout.hide", { id: "panel" });"#;
const CLICK_SHOW: &str = r#"          else { run("layout.select", { stack: "panel-body", child: id }); run("layout.show", { id: "panel" }); }"#;
const WINDOW_BUTTONS: &str = r#"      .when(w?.buttons, el => el.child(button("minimize", w.platform === "windows" ? svg("mods/tabs/win-min.svg").size(16) : "−", () => run("window.minimize")))"#;

const CHUTE_BUTTON: &[&str] = &[
    r#"      .when(!!find(root, "chute"), el => {"#,
    r#"        const node = find(root, "chute"), state = panelState(panel, body, node), on = state.panel && state.chute;"#,
    r#"        return el.child(h_flex().id("panel-chute").h(26).px(8).gap(5).flex_none().items_center().rounded(6)"#,
    r#"          .text_size(12).bg(on ? t.selected : t.chrome).text_color(on ? t.foreground : t.muted)"#,
    r#"          .hover(s => s.bg(t.hover)).on_mouse_down("left", stop)"#,
    r#"          .on_click(() => { for (const c of panelActions(state, "chute")) run(c.name, c.args); })"#,
    r#"          .child(div().size(7).rounded(4).bg(chuteDot(t, shared("chute.status")))).tooltip(chuteTip(shared("chute.status"))).child("chute"));"#,
    r#"      })"#,
];

enum Edit {
    Replace(&'static [&'static str]),
    Before(&'static [&'static str]),
    After(&'static [&'static str]),
}

const EDITS: [(&str, Edit); 8] = [
    (
        IMPORT_HERDR,
        Edit::After(&[
            r#"import { chuteDot, chuteTip, filesNow, panelActions, panelState, restoreActions } from "./chute.js";"#,
        ]),
    ),
    (
        NOW_LINE,
        Edit::Replace(&["    const now = filesNow(panelState(panel, body));"]),
    ),
    (
        RESTORE_SELECT,
        Edit::Replace(&[
            r#"          for (const c of restoreActions(want, panelState(panel, body, find(layout(), "chute")))) run(c.name, c.args);"#,
        ]),
    ),
    (RESTORE_HIDE, Edit::Replace(&[])),
    (
        ON_LINE,
        Edit::Replace(&["      const on = filesNow(panelState(panel, body)) === id;"]),
    ),
    (
        CLICK_HIDE,
        Edit::Replace(&[
            r#"          for (const c of panelActions(panelState(panel, body, find(root, "chute")), id)) run(c.name, c.args);"#,
        ]),
    ),
    (CLICK_SHOW, Edit::Replace(&[])),
    (WINDOW_BUTTONS, Edit::Before(CHUTE_BUTTON)),
];

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub fn build_tabs_main(original: &str) -> Result<String> {
    let lines: Vec<&str> = original.lines().collect();
    for (find, _) in &EDITS {
        let count = lines.iter().filter(|l| l.trim_end() == *find).count();
        match count {
            1 => {}
            0 => bail!("mods/tabs/main.js에 바꿔 끼울 줄이 없다: {}", find.trim()),
            n => bail!(
                "mods/tabs/main.js에 바꿔 끼울 줄이 {n}번 나온다: {}",
                find.trim()
            ),
        }
    }
    let mut out = vec![format!(
        "{MARKER} {FINGERPRINT_KEY}{:016x}",
        fnv1a64(original.as_bytes())
    )];
    for line in lines {
        let edit = EDITS.iter().find(|(find, _)| line.trim_end() == *find);
        match edit {
            None => out.push(line.to_owned()),
            Some((_, Edit::Replace(new))) => out.extend(new.iter().map(|l| (*l).to_owned())),
            Some((_, Edit::Before(new))) => {
                out.extend(new.iter().map(|l| (*l).to_owned()));
                out.push(line.to_owned());
            }
            Some((_, Edit::After(new))) => {
                out.push(line.to_owned());
                out.extend(new.iter().map(|l| (*l).to_owned()));
            }
        }
    }
    Ok(out.join("\n") + "\n")
}

pub fn tabs_main_is_ours(text: &str) -> bool {
    text.lines().next().is_some_and(|l| l.starts_with(MARKER))
}

pub fn recorded_fingerprint(text: &str) -> Option<&str> {
    let first = text.lines().next()?;
    first.split_once(FINGERPRINT_KEY).map(|(_, hex)| hex.trim())
}

pub fn fingerprint_hex(original: &str) -> String {
    format!("{:016x}", fnv1a64(original.as_bytes()))
}

fn find_mut<'a>(node: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if node.get("id").and_then(Value::as_str) == Some(id) {
        return Some(node);
    }
    node.get_mut("children")?
        .as_array_mut()?
        .iter_mut()
        .find_map(|child| find_mut(child, id))
}

pub fn build_layout(main_json: &str) -> Result<String> {
    let mut root: Value = serde_json::from_str(main_json)
        .map_err(|e| anyhow!("layouts/main.json을 읽지 못했다: {e}"))?;
    if find_mut(&mut root, "chute").is_some() {
        bail!("layouts/main.json에 id `chute` 노드가 이미 있다");
    }
    let tabs = find_mut(&mut root, "tabs")
        .and_then(|n| n.get_mut("content"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("layouts/main.json에 content가 있는 id `tabs` 블록이 없다"))?;
    tabs.insert("mod".to_owned(), json!(TABS_MOD));
    let panel = find_mut(&mut root, "panel")
        .and_then(|n| n.get_mut("children"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("layouts/main.json에 id `panel` 열이 없다"))?;
    let at = panel
        .iter()
        .position(|c| c.get("id").and_then(Value::as_str) == Some("panel-body"))
        .ok_or_else(|| anyhow!("layouts/main.json의 `panel` 열에 id `panel-body`가 없다"))?;
    panel[at]["size"] = json!({"weight": 1});
    panel.insert(
        at + 1,
        json!({"id": "chute", "type": "block", "size": {"weight": 1}, "min": 120, "content": {"mod": "chute"}}),
    );
    Ok(serde_json::to_string_pretty(&root)? + "\n")
}

pub fn layout_is_ours(text: &str) -> bool {
    let Ok(mut root) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    find_mut(&mut root, "tabs")
        .and_then(|n| n.pointer("/content/mod"))
        .and_then(Value::as_str)
        == Some(TABS_MOD)
}

#[cfg(test)]
pub mod fake {
    pub fn tabs_main() -> String {
        include_str!("../../../tests/fixtures/herdr-tabs-main.js").to_owned()
    }

    pub fn main_layout() -> String {
        include_str!("../../../tests/fixtures/herdr-main-layout.json").to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{main_layout, tabs_main};
    use super::*;

    #[test]
    fn fnv_matches_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn main_js_gets_the_header_and_every_edit() {
        let original = tabs_main();
        let built = build_tabs_main(&original).unwrap();
        assert!(tabs_main_is_ours(&built));
        assert_eq!(
            recorded_fingerprint(&built),
            Some(fingerprint_hex(&original).as_str())
        );
        assert!(built.contains("./chute.js"));
        assert!(built.contains("filesNow(panelState(panel, body))"));
        assert!(built.contains("restoreActions("));
        assert!(built.contains("panelActions(panelState(panel, body, find(root, \"chute\")), id)"));
        assert!(built.contains("id(\"panel-chute\")"));
        assert!(!built.contains(NOW_LINE) && !built.contains(CLICK_SHOW));
        assert!(built.contains("mods/tabs/win-min.svg"));
        let chute_at = built.find("panel-chute").unwrap();
        assert!(chute_at < built.find(WINDOW_BUTTONS).unwrap());
    }

    #[test]
    fn a_missing_or_repeated_line_is_refused() {
        let original = tabs_main();
        let missing = original.replace(ON_LINE, "");
        let err = build_tabs_main(&missing).unwrap_err().to_string();
        assert!(err.contains("바꿔 끼울 줄이 없다"), "{err}");
        let doubled = format!("{original}{ON_LINE}\n");
        let err = build_tabs_main(&doubled).unwrap_err().to_string();
        assert!(err.contains("2번"), "{err}");
    }

    #[test]
    fn the_layout_gets_a_chute_block_after_panel_body() {
        let built: Value = serde_json::from_str(&build_layout(&main_layout()).unwrap()).unwrap();
        let mut built = built;
        assert_eq!(
            built["children"][0]["children"][0]["content"]["mod"],
            TABS_MOD
        );
        let panel = find_mut(&mut built, "panel").unwrap();
        let ids: Vec<_> = panel["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].clone())
            .collect();
        assert_eq!(ids, [json!("panel-body"), json!("chute")]);
        assert_eq!(panel["children"][0]["size"], json!({"weight": 1}));
        assert_eq!(panel["children"][1]["content"], json!({"mod": "chute"}));
        assert_eq!(panel["hidden"], json!(true));
        assert!(layout_is_ours(&serde_json::to_string(&built).unwrap()));
    }

    #[test]
    fn a_layout_without_the_expected_nodes_is_refused() {
        for id in ["tabs", "panel", "panel-body"] {
            let broken = main_layout().replace(&format!("\"id\": \"{id}\""), "\"id\": \"other\"");
            assert!(build_layout(&broken).is_err(), "{id}");
        }
        assert!(build_layout("not json").is_err());
        assert!(!layout_is_ours(&main_layout()));
    }
}
