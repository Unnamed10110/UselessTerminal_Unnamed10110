//! Fixture-based migration tests (§16.5, Appendix C) through the public API.

use serde_json::Value;
use std::path::{Path, PathBuf};
use ut_core::keybindings;
use ut_core::migrate::{is_legacy_settings, settings_from_legacy};
use ut_core::settings::{Backdrop, CursorStyle};
use ut_core::{effective, windowstate, LoadStatus, SettingsStore};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
}

fn json(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(fixture(name)).unwrap()).unwrap()
}

/// A fresh directory with spaces in its name, holding a copy of the fixture as `target_name`.
fn staged(fixture_name: &str, target_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ut core legacy {fixture_name} {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join(target_name);
    std::fs::copy(fixture(fixture_name), &dest).unwrap();
    dest
}

#[test]
fn modern_legacy_settings_become_a_custom_theme() {
    let v = json("legacy_settings.json");
    assert!(is_legacy_settings(&v));
    let s = settings_from_legacy(&v);

    assert_eq!(s.schema_version, 3);
    assert_eq!(s.theme.preset, "Custom");
    let t = &s.terminal;
    assert_eq!(t.font_family, "'Cascadia Code', Consolas, monospace");
    assert_eq!((t.font_size, t.font_weight, t.scrollback), (15.0, 600, 25_000));
    assert_eq!((t.cursor_style, t.cursor_blink), (CursorStyle::Block, false));
    assert_eq!(t.typed_input_color, "#ffb454", "ColorInput -> terminal.typedInputColor");
    assert_eq!(t.background_image.path, r"C:\Users\sbritos\OneDrive - BEPSA DEL PARAGUAY SAECA\Pictures\bg.png");
    assert_eq!(t.background_image.opacity, 0.4);
    assert_eq!((s.ui.scale, s.ui.font_family.as_str(), s.ui.font_size), (1.15, "'Segoe UI Variable'", 14.0));
    assert_eq!(s.ui.backdrop, Backdrop::Mica);

    // every legacy colour is an override, lowercase #rrggbb
    let o = &s.theme.overrides;
    assert_eq!(o.terminal.len(), 12);
    assert_eq!(o.terminal["background"], "#0a0e14");
    assert_eq!(o.terminal["command"], "#aad94c");
    assert_eq!(o.terminal["selectionForeground"], "#bfbdb6");
    assert_eq!(o.ui.len(), 20);
    assert_eq!(o.ui["tabSelectedBg"], "#6be5ff", "WPF #AARRGGBB loses its alpha");
    assert_eq!(o.ui["folderSelected"], "#2f2a22");
    assert_eq!(o.ui["splitter"], "#7a6238");

    // the migrated theme reproduces the legacy look
    let e = effective(&s.theme);
    assert_eq!(e.terminal["background"], "#0a0e14");
    assert_eq!(e.terminal["red"], "#f07178");
    assert_eq!(e.terminal["green"], "#aad94c");
    assert_eq!(e.terminal["blue"], "#ffb454");
    assert_eq!(e.terminal["brightBlack"], "#626a73");
    assert_eq!(e.terminal["cursor"], "#ffb454");
    assert_eq!(e.ui["cardBg"], "#1b1a18");
    assert_eq!(e.ui["accentDim"], "rgba(255,180,84,0.14)", "derived from the effective accent (UiAccent #ffb454)");
    assert_eq!(e.ui["terminalBg"], "#0a0e14");
}

#[test]
fn pre_semantic_keys_use_the_fallback_table() {
    let s = settings_from_legacy(&json("legacy_settings_older.json"));
    let o = &s.theme.overrides.terminal;
    assert_eq!(o["background"], "#101010");
    assert_eq!(o["foreground"], "#dddddd", "Foreground wins over White");
    assert_eq!(s.terminal.typed_input_color, "#dddddd");
    assert_eq!(o["cursor"], "#00ff00");
    assert_eq!(o["muted"], "#777777");
    assert_eq!(o["error"], "#cc0000", "Red wins over BrightRed");
    assert_eq!(o["warning"], "#ffff55", "no Yellow: falls back to BrightYellow");
    assert_eq!(o["command"], "#00cc00");
    assert_eq!(o["message"], "#55ffff", "no Cyan: falls back to BrightCyan");
    assert_eq!(o["accent"], "#0000cc");
    assert_eq!(o["highlight"], "#ff55ff", "no Magenta: falls back to BrightMagenta");
    assert!(!o.contains_key("selectionBackground") && s.theme.overrides.ui.is_empty());
    assert_eq!(s.terminal.font_size, 12.0);
    assert_eq!(s.terminal.font_weight, 400, "absent keys keep their defaults");
    let e = effective(&s.theme);
    assert_eq!(e.terminal["red"], "#cc0000");
    assert_ne!(e.terminal["brightRed"], e.terminal["red"], "derived brights, even for migrated themes");
}

#[test]
fn settings_store_backs_up_and_rewrites_the_legacy_file() {
    let f = staged("legacy_settings.json", "settings.json");
    let store = SettingsStore::load(&f);
    let LoadStatus::Migrated { backup } = store.status() else { panic!("{:?}", store.status()) };
    assert_eq!(Path::new(&backup), f.with_file_name("settings.legacy-bak.json"));
    assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(&backup).unwrap()).unwrap(), json("legacy_settings.json"));
    let on_disk: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
    assert_eq!(on_disk["schemaVersion"], 3);
    assert_eq!(on_disk["theme"]["preset"], "Custom");
    assert_eq!(on_disk["theme"]["overrides"]["accent"], "#ffb454");
    assert_eq!(on_disk["terminal"]["typedInputColor"], "#ffb454");
    assert!(on_disk.get("UiSharpness").is_none() && on_disk["ui"].get("sharpness").is_none(), "UiSharpness dropped");
    // second start: plain v3 file
    let again = SettingsStore::load(&f);
    assert_eq!(again.status(), LoadStatus::Loaded);
    assert_eq!(again.get(), store.get());
}

#[test]
fn keybindings_legacy_import() {
    let f = staged("legacy_keybindings.json", "keybindings.json");
    let l = keybindings::load(&f);
    assert!(matches!(l.status, LoadStatus::Migrated { .. }));
    let e = l.keybindings.effective();
    assert_eq!(e["settings"], ["Ctrl+Comma"]);
    assert_eq!(e["selectTab1"], ["Ctrl+1"]);
    assert_eq!(e["zoomIn"], ["Ctrl+Equal"]);
    assert_eq!(e["zoomOut"], ["Ctrl+Minus"]);
    assert_eq!(e["quake"], ["Win+Backquote"]);
    assert_eq!(e["movePaneFocus"], ["Ctrl+Shift+Arrow"]);
    assert_eq!(e["prevCommand"], ["Ctrl+Alt+Up"]);
    assert_eq!(e["selectTabNumpad1"], ["Ctrl+Alt+Numpad1"]);
    assert_eq!(e["search"], ["Ctrl+Shift+F"], "an unparsable combo keeps the default");
    assert_eq!(l.invalid.len(), 1);
    assert_eq!(l.invalid[0].action, "search");
    assert!(f.with_file_name("keybindings.legacy-bak.json").exists());
    let v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
    assert_eq!(v["schemaVersion"], 3);
    assert_eq!(v["bindings"]["settings"], "Ctrl+Comma");
    assert!(v.get("Bindings").is_none());
}

#[test]
fn window_state_legacy_import_and_restore_clamping() {
    let f = staged("legacy_windowstate.json", "windowstate.json");
    let (s, status) = windowstate::load(&f);
    assert!(matches!(status, LoadStatus::Migrated { .. }));
    let b = s.bounds.unwrap();
    assert_eq!((b.x, b.y, b.width, b.height), (121, 80, 1400, 900), "doubles are rounded");
    assert!(s.sidebar_open && !s.maximized);
    assert_eq!((s.sidebar_width, s.active_tab_index), (320, 1));
    assert_eq!(s.tabs.len(), 2);
    assert_eq!(s.tabs[0].color.as_deref(), Some("#00e5ff"));
    assert!(!s.tabs[0].title_locked && s.tabs[1].title_locked, "Renamed -> titleLocked");
    assert_eq!(s.tabs[1].starting_command.as_deref(), Some("tmux attach"));
    assert_eq!(s.tabs[0].cwd.as_deref(), Some(r"C:\Users\sbritos\OneDrive - BEPSA DEL PARAGUAY SAECA\workspace_sbritos"));
    assert!(f.with_file_name("windowstate.legacy-bak.json").exists());
    assert_eq!(windowstate::load(&f).1, LoadStatus::Loaded);
    // restoring onto a smaller monitor than the one it was saved on
    let only = [windowstate::Rect { x: 0, y: 0, width: 1280, height: 720 }];
    let c = windowstate::clamp_bounds(b, &only, 0);
    assert_eq!((c.width, c.height), (1200, 640), "larger than the monitor: monitor minus 80 px");
}
