//! Legacy WPF `settings.json` (Appendix C.1) -> v3 `Settings` (§16.5).
//! (Keybindings C.5 and window state C.6 convert in their own modules.)

use crate::color::from_wpf;
use crate::settings::{Backdrop, CursorStyle, Settings};
use crate::theme::ThemeRef;
use serde_json::Value;

/// A settings file without `schemaVersion` whose keys are PascalCase is the WPF format (§16.5).
pub fn is_legacy_settings(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|m| !m.contains_key("schemaVersion") && m.keys().any(|k| k.starts_with(|c: char| c.is_ascii_uppercase())))
}

/// Semantic override key, then the legacy keys in precedence order: the modern key first, then the
/// "older still" primary key, then its fallback (C.1 table).
const TERMINAL_COLOURS: [(&str, &[&str]); 12] = [
    ("background", &["TerminalBackground", "Background"]),
    ("foreground", &["TextDefault", "Foreground", "White"]),
    ("muted", &["TextMuted", "BrightBlack"]),
    ("error", &["ColorError", "Red", "BrightRed"]),
    ("warning", &["ColorWarning", "Yellow", "BrightYellow"]),
    ("command", &["ColorCommand", "Green", "BrightGreen"]),
    ("message", &["ColorMessage", "Cyan", "BrightCyan"]),
    ("accent", &["ColorAccent", "Blue", "BrightBlue"]),
    ("highlight", &["ColorHighlight", "Magenta", "BrightMagenta"]),
    ("cursor", &["CursorColor", "Cursor"]),
    ("selectionBackground", &["SelectionBackground"]),
    ("selectionForeground", &["SelectionForeground"]),
];

const UI_COLOURS: [(&str, &str); 20] = [
    ("foreground", "UiForeground"), ("foregroundMuted", "UiForegroundMuted"), ("accent", "UiAccent"),
    ("highlight", "UiHighlight"), ("success", "UiSuccess"), ("warning", "UiWarning"), ("error", "UiError"),
    ("chromeBg", "UiChromeBackground"), ("cardBg", "UiCardBackground"), ("cardBorder", "UiCardBorder"),
    ("folderSelected", "UiFolderSelectedBackground"), ("tabFg", "UiTabForeground"),
    ("tabSelectedBg", "UiTabSelectedBackground"), ("tabSelectedFg", "UiTabSelectedForeground"),
    ("statusBg", "UiStatusBackground"), ("icon", "UiIcon"), ("inputBg", "UiInputBackground"),
    ("inputFg", "UiInputForeground"), ("hoverBg", "UiHoverBackground"), ("splitter", "UiSplitter"),
];

/// WPF family names (`Cascadia Code, Consolas`) -> a CSS family list; names with spaces get quoted.
fn css_family(s: &str, fallback: Option<&str>) -> Option<String> {
    let mut names: Vec<String> = s
        .split(',')
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .map(|n| if n.contains(' ') && !n.starts_with(['\'', '"']) { format!("'{n}'") } else { n.to_string() })
        .collect();
    if names.is_empty() {
        return None;
    }
    names.extend(fallback.filter(|f| !s.to_ascii_lowercase().contains(f)).map(String::from));
    Some(names.join(", "))
}

/// WPF `FontWeight`: a number, or a name like `SemiBold`.
fn font_weight(v: &Value) -> Option<u32> {
    if let Some(n) = v.as_f64() {
        return Some(n.max(0.0) as u32);
    }
    Some(match v.as_str()?.to_ascii_lowercase().as_str() {
        "thin" => 100,
        "extralight" | "ultralight" => 200,
        "light" => 300,
        "normal" | "regular" => 400,
        "medium" => 500,
        "semibold" | "demibold" => 600,
        "bold" => 700,
        "extrabold" | "ultrabold" => 800,
        "black" | "heavy" => 900,
        _ => return None,
    })
}

/// Convert a legacy settings object. Every legacy colour lands in `theme.overrides` with
/// `preset: "Custom"` (the old files do not record the preset); `ColorInput` becomes
/// `terminal.typedInputColor`; `UiSharpness` is dropped (§14.2). The result is sanitised.
pub fn settings_from_legacy(v: &Value) -> Settings {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let num = |k: &str| v.get(k).and_then(Value::as_f64);
    let colour = |keys: &[&str]| keys.iter().find_map(|k| s(k).and_then(from_wpf));
    let mut st = Settings::default();

    let t = &mut st.terminal;
    if let Some(f) = s("FontFamily").and_then(|f| css_family(f, Some("monospace"))) {
        t.font_family = f;
    }
    t.font_size = num("FontSize").unwrap_or(t.font_size);
    t.font_weight = v.get("FontWeight").and_then(font_weight).unwrap_or(t.font_weight);
    t.cursor_blink = v.get("CursorBlink").and_then(Value::as_bool).unwrap_or(t.cursor_blink);
    t.cursor_style = match s("CursorStyle").map(str::to_ascii_lowercase).as_deref() {
        Some("block") => CursorStyle::Block,
        Some("underline") => CursorStyle::Underline,
        _ => CursorStyle::Bar, // "bar" / "line" / "beam" / unknown
    };
    t.scrollback = num("Scrollback").map_or(t.scrollback, |n| n.clamp(0.0, 1e9) as u32);
    if let Some(c) = colour(&["ColorInput", "Foreground", "White"]) {
        t.typed_input_color = c;
    }
    if let Some(p) = s("ShellBackgroundImagePath") {
        t.background_image.path = p.to_string();
    }
    t.background_image.opacity = num("ShellBackgroundImageOpacity").unwrap_or(t.background_image.opacity);

    let u = &mut st.ui;
    u.scale = num("UiScale").unwrap_or(u.scale);
    if let Some(f) = s("UiFontFamily").and_then(|f| css_family(f, None)) {
        u.font_family = f;
    }
    u.font_size = num("UiFontSize").unwrap_or(u.font_size);
    u.font_weight = v.get("UiFontWeight").and_then(font_weight).unwrap_or(u.font_weight);
    u.backdrop = match s("WindowBackdrop").map(str::to_ascii_lowercase).as_deref() {
        Some(b) if b.contains("acrylic") || b.contains("transient") => Backdrop::Acrylic,
        Some(b) if b.contains("mica") || b.contains("tabbed") => Backdrop::Mica,
        _ => Backdrop::Off,
    };

    let mut theme = ThemeRef { preset: "Custom".into(), ..Default::default() };
    for (key, legacy) in TERMINAL_COLOURS {
        if let Some(c) = colour(legacy) {
            theme.overrides.terminal.insert(key.into(), c);
        }
    }
    for (token, legacy) in UI_COLOURS {
        if let Some(c) = colour(&[legacy]) {
            theme.overrides.ui.insert(token.into(), c);
        }
    }
    st.theme = theme;
    st.sanitize();
    st
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detection() {
        assert!(is_legacy_settings(&json!({"FontSize": 14})));
        assert!(!is_legacy_settings(&json!({"schemaVersion": 3, "FontSize": 14})));
        assert!(!is_legacy_settings(&json!({"terminal": {}})));
        assert!(!is_legacy_settings(&json!({})));
        assert!(!is_legacy_settings(&json!([1])));
    }

    #[test]
    fn family_and_weight_helpers() {
        assert_eq!(css_family("Cascadia Code", Some("monospace")).unwrap(), "'Cascadia Code', monospace");
        assert_eq!(css_family("Consolas, Courier New", Some("monospace")).unwrap(), "Consolas, 'Courier New', monospace");
        assert_eq!(css_family("Consolas, monospace", Some("monospace")).unwrap(), "Consolas, monospace");
        assert_eq!(css_family("Segoe UI", None).unwrap(), "'Segoe UI'");
        assert_eq!(css_family(" , ", None), None);
        assert_eq!(font_weight(&json!("SemiBold")), Some(600));
        assert_eq!(font_weight(&json!(700)), Some(700));
        assert_eq!(font_weight(&json!("weird")), None);
    }

    #[test]
    fn precedence_modern_then_older_then_fallback() {
        let s = settings_from_legacy(&json!({
            "ColorError": "#111111", "Red": "#222222", "BrightRed": "#333333",
            "Yellow": "#444444", "BrightYellow": "#555555", "BrightGreen": "#666666",
            "White": "#777777", "Background": "#101010", "ColorAccent": "bad"
        }));
        let o = &s.theme.overrides.terminal;
        assert_eq!(o["error"], "#111111");
        assert_eq!(o["warning"], "#444444");
        assert_eq!(o["command"], "#666666");
        assert_eq!(o["foreground"], "#777777");
        assert_eq!(s.terminal.typed_input_color, "#777777");
        assert_eq!(o["background"], "#101010");
        assert!(!o.contains_key("accent"), "an invalid colour is skipped, not turned into black");
        assert_eq!(s.theme.preset, "Custom");
    }
}
