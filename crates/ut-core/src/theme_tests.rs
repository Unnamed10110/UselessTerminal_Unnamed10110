use super::*;
use crate::settings::{apply_patch, Settings};
use serde_json::json;
use std::collections::BTreeMap;

fn eff(name: &str) -> EffectiveTheme {
    effective(&ThemeRef { preset: name.into(), ..Default::default() })
}

/// The §13.3 test-vector table, compared exactly.
#[test]
fn spec_13_3_test_vectors() {
    let rows = [
        ("Default", ["#0D1B1F", "#1E4047", "#152E33", "#0F2024", "#112529", "#2B5C66", "#141414", "#111111"]),
        ("Dracula", ["#3A374D", "#52476D", "#463F5D", "#3D3951", "#403B55", "#645484", "#393A45", "#111111"]),
        ("AMOLED Green", ["#00170C", "#006132", "#00371C", "#002915", "#00331A", "#007F41", "#131313", "#111111"]),
        ("Light", ["#ffffff", "#7AB4C0", "#D6EBED", "#CDE2E8", "#E0F0F2", "#5195A6", "#ffffff", "#ffffff"]),
    ];
    let keys = ["cardBg", "cardBorder", "folderSelected", "statusBg", "hoverBg", "splitter", "inputBg", "tabSelectedFg"];
    for (name, want) in rows {
        let t = eff(name);
        for (k, w) in keys.iter().zip(want) {
            assert_eq!(t.ui[*k], w, "{name} {k}");
        }
    }
    assert_eq!(eff("Light").ui["chromeBg"], "#DFEEF1");
}

#[test]
fn names_and_order() {
    let n = preset_names();
    let documented = [
        "Default", "Dracula", "Solarized Dark", "Monokai", "Nord", "Catppuccin Mocha", "One Dark", "Gruvbox Dark",
        "Tokyo Night", "Ayu Dark", "Vesper", "AMOLED Green", "AMOLED Red", "AMOLED Purple Neon", "AMOLED Cyan",
        "AMOLED Orange", "AMOLED Pink", "AMOLED Blue", "AMOLED Gold", "AMOLED Matrix", "AMOLED Ice", "Light",
        "Light Green", "Light Red", "Light Purple Neon", "Light Cyan", "Light Orange", "Light Pink", "Light Blue",
        "Light Gold", "Light Ice",
    ];
    assert_eq!(documented.len(), 31);
    assert_eq!(n[..31], documented);
    assert_eq!(n[31..], ["Rosé Pine", "Kanagawa", "Night Owl", "Poimandres", "Everforest Dark", "Iceberg Dark"]);
    let mut u = n.clone();
    u.sort();
    u.dedup();
    assert_eq!(u.len(), n.len(), "unique names");
}

/// Appendix B is copied exactly: compare every column of every row against the spec file itself.
#[test]
fn appendix_b_matches_the_spec_table() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../RUST_TAURI_SPEC.md");
    let Ok(spec) = std::fs::read_to_string(path) else { return }; // crate used outside the repo
    let (mut rows, mut in_b) = (0, false);
    for line in spec.lines() {
        if line.starts_with("## ") {
            in_b = line.contains("Appendix B");
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if !in_b || cells.len() != 18 || cells[1].is_empty() || !cells[1].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        rows += 1;
        let mut p = crate::presets::find(cells[2]).unwrap_or_else(|| panic!("missing preset {}", cells[2])).clone();
        assert_eq!(p.name, cells[2]);
        let cols = ["bg", "fg", "input", "muted", "err", "warn", "cmd", "msg", "acc", "hi", "cursor", "selBg", "selFg"];
        for ((have, want), col) in p.colours_mut().into_iter().zip(&cells[3..16]).zip(cols) {
            // the two [P1] fixes listed under "Known issues" are the only intended differences
            let fixed = matches!((cells[2], col), ("Light Red", "cmd") | ("Light Cyan", "acc"));
            assert_eq!(have.eq_ignore_ascii_case(want), !fixed, "{} {col}: {have} vs {want}", cells[2]);
        }
        assert_eq!(p.neon_ui, cells[16] == "Y", "{} neonUi", cells[2]);
    }
    assert_eq!(rows, 31);
}

#[test]
fn known_issue_fixes() {
    let find = |n| crate::presets::find(n).unwrap();
    assert_eq!(find("Light Red").cmd, "#2e7d32");
    assert_ne!(find("Light Red").cmd, find("Light Red").err);
    assert_eq!(find("Light Cyan").acc, "#00796b");
    assert_eq!(find("Light").acc, "#00838f");
    assert_ne!(eff("Light Cyan").ui["chromeBg"], eff("Light").ui["chromeBg"]);
    // white stays the dark default text on light presets
    assert_eq!(eff("Light Red").terminal["white"], "#1a1a1a");
}

#[test]
fn amoled_roles_are_distinct() {
    for p in crate::presets::all().iter().filter(|p| p.name.starts_with("AMOLED")) {
        let t = eff(&p.name).terminal;
        let roles = ["red", "yellow", "green", "cyan", "blue", "magenta", "white", "brightBlack", "black"];
        let set: std::collections::BTreeSet<_> = roles.iter().map(|r| t[*r].to_ascii_lowercase()).collect();
        assert_eq!(set.len(), 9, "{} collapses roles: {t:?}", p.name);
        assert!(p.neon_ui);
    }
}

#[test]
fn brights_are_derived_and_differ() {
    let t = eff("Dracula").terminal;
    assert_eq!(t["red"], "#ff5555");
    assert_eq!(t["brightRed"], mix("#ff5555", "#ffffff", 0.15));
    assert_eq!(t["brightWhite"], mix("#f8f8f2", "#ffffff", 0.25));
    assert_eq!(t["brightBlack"], "#6272a4", "brightBlack = muted");
    assert_eq!(t["black"], "#000000");
    assert_eq!(t["white"], "#f8f8f2");
    assert_eq!(eff("Light").terminal["brightRed"], mix("#c62828", "#000000", 0.15), "light themes darken");
    for p in crate::presets::all() {
        let t = eff(&p.name).terminal;
        for (b, br) in ANSI_BASE.iter().zip(ANSI_BRIGHT).skip(1).take(6) {
            assert_ne!(t[*b].to_ascii_lowercase(), t[br].to_ascii_lowercase(), "{} {b}", p.name);
        }
        if !p.fg.eq_ignore_ascii_case("#ffffff") {
            assert_ne!(t["white"].to_ascii_lowercase(), t["brightWhite"].to_ascii_lowercase(), "{}", p.name);
        }
        assert!(!t.contains_key("typedInputColor") && !t.contains_key("input"), "typed input is not part of a theme");
        assert_eq!(t.len(), 22, "{}", p.name);
    }
    assert_eq!(eff("Default").terminal["cursorAccent"], "#000000");
}

fn with(preset: &str, overrides: serde_json::Value) -> ThemeRef {
    ThemeRef { preset: preset.into(), overrides: serde_json::from_value(overrides).unwrap() }
}

#[test]
fn overrides_unknown_presets_and_custom() {
    let t = effective(&with("Dracula", json!({"ansi": {"blue": "#00e676"}, "error": "#123456", "background": "#101010",
        "ui": {"accent": "#abcdef"}})));
    assert_eq!(t.terminal["blue"], "#00e676");
    assert_eq!(t.terminal["brightBlue"], mix("#00e676", "#ffffff", 0.15), "bright follows the overridden base");
    assert_eq!(t.terminal["red"], "#123456");
    assert_eq!(t.terminal["brightRed"], mix("#123456", "#ffffff", 0.15));
    assert_eq!(t.terminal["background"], "#101010");
    assert_eq!(t.terminal["cursorAccent"], "#101010");
    assert_eq!(t.ui["accent"], "#abcdef");
    assert_eq!(t.ui["accentDim"], "rgba(171,205,239,0.14)", "accent-dim follows the effective accent (§24 #11)");
    assert_eq!(t.ui["terminalBg"], "#101010");
    assert_eq!(t.ui["cardBg"], eff("Dracula").ui["cardBg"], "other tokens still come from the preset");
    let t = effective(&with("Dracula", json!({"ansi": {"brightGreen": "#111111"}})));
    assert_eq!(t.terminal["brightGreen"], "#111111");
    // unknown / Custom -> Default preset + overrides
    for name in ["Custom", "No Such Theme", ""] {
        let t = effective(&with(name, json!({"accent": "#ff0000"})));
        assert_eq!(t.terminal["blue"], "#ff0000");
        assert_eq!(t.terminal["background"], "#000000");
        assert_eq!(t.ui["cardBg"], eff("Default").ui["cardBg"]);
    }
    assert_eq!(eff("dracula"), eff("Dracula"), "case-insensitive");
    assert_eq!(eff("Dracula").ui["accentDim"], "rgba(189,147,249,0.14)");
    assert!(!eff("Dracula").light && eff("Light").light);
}

#[test]
fn theme_ref_serde_shape_and_typed_input_untouched() {
    let t: ThemeRef = serde_json::from_value(
        json!({"preset": "AMOLED Green", "overrides": {"ansi": {"blue": "#00e676"}, "ui": {}, "cursor": "#fff"}}),
    )
    .unwrap();
    assert_eq!(t.overrides.ansi["blue"], "#00e676");
    assert_eq!(t.overrides.terminal["cursor"], "#fff");
    assert_eq!(serde_json::to_value(ThemeRef::default()).unwrap(), json!({"preset": "Default", "overrides": {}}));
    // applying a preset never touches the typed-input colour (§13.2, §23.18)
    let s = apply_patch(&Settings::default(), json!({"terminal": {"typedInputColor": "#12ab34"}})).unwrap();
    let s = apply_patch(&s, json!({"theme": {"preset": "AMOLED Matrix"}})).unwrap();
    assert_eq!(s.terminal.typed_input_color, "#12ab34");
    assert_eq!(s.theme.preset, "AMOLED Matrix");
}

#[test]
fn css_vars() {
    let vars = |t: &EffectiveTheme, ui: &UiSettings| ui_css_vars(t, ui).into_iter().collect::<BTreeMap<_, _>>();
    let v = vars(&eff("Default"), &Settings::default().ui);
    assert_eq!(v["--ui-accent"], "#6be5ff");
    assert_eq!(v["--ui-accent-dim"], "rgba(107,229,255,0.14)");
    assert_eq!(v["--ui-chrome-bg"], "#000000");
    assert_eq!(v["--ui-card-bg"], "#0D1B1F");
    assert_eq!(v["--ui-foreground-muted"], "#888888");
    assert_eq!(v["--ui-tab-selected-fg"], "#111111");
    assert_eq!(v["--ui-terminal-bg"], "#000000");
    assert_eq!(v["--ui-font-family"], "Segoe UI");
    assert_eq!(v["--ui-font-size"], "13px");
    assert_eq!(v["--ui-font-size-small"], "11px");
    assert_eq!(v["--ui-font-size-title"], "15px");
    assert_eq!(v["--ui-font-size-tab"], "10px");
    assert_eq!(v["--ui-font-weight"], "400");
    assert_eq!(v["--ui-scale"], "1");
    assert_eq!(v.keys().filter(|k| !k.contains("font") && *k != "--ui-scale").count(), 22);
    let mut ui = Settings::default().ui;
    ui.font_size = 13.5;
    ui.scale = 1.25;
    let v = vars(&eff("Default"), &ui);
    assert_eq!((v["--ui-font-size"].as_str(), v["--ui-font-size-small"].as_str(), v["--ui-scale"].as_str()), ("13.5px", "11.5px", "1.25"));
}

#[test]
fn user_themes_skip_bad_files() {
    let d = std::env::temp_dir().join(format!("ut core themes {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    assert!(load_user_themes(&d.join("missing")).is_empty());
    let w = |n: &str, c: &str| std::fs::write(d.join(n), c).unwrap();
    w("a-mine.json", r##"{"name": "Mine", "bg": "#101820", "fg": "#EEE", "acc": "#ff8800", "neonUi": true}"##);
    w("b-broken.json", "{ nope");
    w("c-noname.json", r##"{"bg": "#111111"}"##);
    w("d-shadow.json", r##"{"name": "dracula", "bg": "#111111"}"##);
    w("e-light.json", r##"{"name": "Paper", "bg": "#fafafa", "fg": "#222", "err": "oops", "acc": "#0066cc"}"##);
    w("f-wrongtypes.json", r##"{"name": "Odd", "bg": 5, "fg": ["x"], "neonUi": "yes"}"##);
    w("g-bom.json", "\u{feff}{\"name\": \"Bom\"}");
    w("h-dup.json", r##"{"name": "MINE"}"##);
    w("notes.txt", "ignored");
    let u = load_user_themes(&d);
    assert_eq!(u.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Mine", "Paper", "Odd", "Bom"]);
    let def = Preset::default();
    assert_eq!((u[0].bg.as_str(), u[0].fg.as_str(), u[0].acc.as_str(), u[0].neon_ui), ("#101820", "#eeeeee", "#ff8800", true));
    assert_eq!(u[0].err, def.err, "missing columns fall back to Default");
    assert_eq!(u[1].err, def.err, "invalid colour -> Default's");
    assert_eq!((u[2].bg.as_str(), u[2].fg.as_str(), u[2].neon_ui), (def.bg.as_str(), def.fg.as_str(), false));
    // user themes resolve through effective_with; light is detected from the background
    let t = |n: &str| effective_with(&ThemeRef { preset: n.into(), ..Default::default() }, &u);
    assert_eq!(t("Mine").terminal["background"], "#101820");
    assert_eq!(t("Mine").ui["cardBg"], mix("#101820", "#ff8800", 0.10), "neonUi honoured");
    assert_eq!(t("Paper").ui["cardBg"], "#ffffff", "light bg -> Light factory UI");
    assert_eq!(t("Paper").terminal["brightRed"], mix(&def.err, "#000000", 0.15));
    assert_eq!(eff("Mine").terminal["background"], "#000000", "unknown without the user list -> Default");
}
