//! Persisted theme = preset reference + overrides (§13.1); `effective()` resolves it into the
//! xterm theme and the UI tokens (§13.2, §13.3, §13.5).

use crate::color::{contrast_fg, mix, normalize_hex, with_alpha};
use crate::presets::{self, Preset};
use crate::settings::UiSettings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const ANSI_BASE: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
pub const ANSI_BRIGHT: [&str; 8] = [
    "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
];
/// Semantic terminal colours that can be overridden (the colour grid of §14.3, minus typed input).
pub const SEMANTIC_KEYS: [&str; 13] = [
    "background", "foreground", "muted", "error", "warning", "command", "message", "accent", "highlight", "cursor",
    "cursorAccent", "selectionBackground", "selectionForeground",
];
/// UI token keys; the CSS property is `--ui-` + kebab-case (`chromeBg` -> `--ui-chrome-bg`).
pub const UI_KEYS: [&str; 22] = [
    "foreground", "foregroundMuted", "accent", "accentDim", "highlight", "success", "warning", "error", "chromeBg",
    "cardBg", "cardBorder", "folderSelected", "tabFg", "tabSelectedBg", "tabSelectedFg", "statusBg", "icon", "inputBg",
    "inputFg", "hoverBg", "splitter", "terminalBg",
];

/// Overrides on top of a preset, all optional. JSON: semantic keys flat (`"background": "#101010"`),
/// plus `ansi: {name: hex}` (xterm names, e.g. `blue`) and `ui: {token: hex}`.
/// `typedInputColor` is deliberately not here (§13.2).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ThemeOverrides {
    #[serde(flatten)]
    pub terminal: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub ansi: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub ui: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ThemeRef {
    pub preset: String,
    pub overrides: ThemeOverrides,
}

impl Default for ThemeRef {
    fn default() -> Self {
        Self { preset: "Default".into(), overrides: ThemeOverrides::default() }
    }
}

impl ThemeRef {
    /// Drop unknown override keys, normalise colours to lowercase `#rrggbb`, drop invalid ones.
    pub fn sanitize(&mut self) {
        self.preset = self.preset.trim().to_string();
        if self.preset.is_empty() {
            self.preset = "Default".into();
        }
        fn clean(m: &mut BTreeMap<String, String>, allowed: impl Fn(&str) -> bool) {
            *m = std::mem::take(m)
                .into_iter()
                .filter(|(k, _)| allowed(k))
                .filter_map(|(k, v)| normalize_hex(&v).map(|h| (k, h)))
                .collect();
        }
        let o = &mut self.overrides;
        clean(&mut o.terminal, |k| SEMANTIC_KEYS.contains(&k));
        clean(&mut o.ansi, |k| ANSI_BASE.contains(&k) || ANSI_BRIGHT.contains(&k));
        clean(&mut o.ui, |k| UI_KEYS.contains(&k));
    }
}

/// The resolved theme handed to the web UI.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveTheme {
    /// xterm `ITheme`-shaped (camelCase keys).
    pub terminal: BTreeMap<String, String>,
    /// UI tokens keyed as in [`UI_KEYS`].
    pub ui: BTreeMap<String, String>,
    /// The effective terminal background is light (drives `color-scheme`).
    pub light: bool,
}

pub fn preset_names() -> Vec<&'static str> {
    presets::all().iter().map(|p| p.name.as_str()).collect()
}

/// Built-in first, then user themes; unknown names and `"Custom"` resolve to the Default preset.
pub fn find_preset(name: &str, user: &[Preset]) -> Preset {
    presets::find(name)
        .or_else(|| user.iter().find(|p| p.name.eq_ignore_ascii_case(name.trim())))
        .unwrap_or(&presets::all()[0])
        .clone()
}

pub fn effective(t: &ThemeRef) -> EffectiveTheme {
    effective_with(t, &[])
}

pub fn effective_with(t: &ThemeRef, user: &[Preset]) -> EffectiveTheme {
    let p = find_preset(&t.preset, user);
    let terminal = terminal_theme(&p, &t.overrides);
    let ui = ui_tokens(&p, &t.overrides, &terminal["background"]);
    let light = crate::color::is_light(&terminal["background"]);
    EffectiveTheme { terminal, ui, light }
}

fn terminal_theme(p: &Preset, o: &ThemeOverrides) -> BTreeMap<String, String> {
    let sem = |k: &str, d: &str| o.terminal.get(k).cloned().unwrap_or_else(|| d.to_string());
    let (bg, fg, muted) = (sem("background", &p.bg), sem("foreground", &p.fg), sem("muted", &p.muted));
    // Semantic role -> ANSI slot (§13.2); white = default text.
    let roles = ["#000000".to_string(), sem("error", &p.err), sem("command", &p.cmd), sem("warning", &p.warn),
        sem("accent", &p.acc), sem("highlight", &p.hi), sem("message", &p.msg), fg.clone()];
    let toward = if p.is_light() { "#000000" } else { "#ffffff" };
    let mut m = BTreeMap::new();
    for (i, base_name) in ANSI_BASE.iter().enumerate() {
        let base = o.ansi.get(*base_name).cloned().unwrap_or_else(|| roles[i].clone());
        let bright = match i {
            0 => muted.clone(), // brightBlack = muted
            7 if !p.is_light() => mix(&base, "#ffffff", 0.25),
            _ => mix(&base, toward, 0.15),
        };
        m.insert(ANSI_BRIGHT[i].to_string(), o.ansi.get(ANSI_BRIGHT[i]).cloned().unwrap_or(bright));
        m.insert(base_name.to_string(), base);
    }
    for (k, v) in [
        ("cursor", sem("cursor", &p.cursor)),
        ("cursorAccent", o.terminal.get("cursorAccent").cloned().unwrap_or_else(|| bg.clone())),
        ("selectionBackground", sem("selectionBackground", &p.sel_bg)),
        ("selectionForeground", sem("selectionForeground", &p.sel_fg)),
        ("foreground", fg),
        ("background", bg),
    ] {
        m.insert(k.to_string(), v);
    }
    m
}

/// Dark and Light factory UI tokens (§13.3). `neonUi` presets use the stronger `t` column.
fn derive_ui(p: &Preset) -> Vec<(&'static str, String)> {
    let (acc, s) = (p.acc.as_str(), String::from);
    let mut v = vec![
        ("accent", s(acc)), ("highlight", s(&p.hi)), ("success", s(&p.cmd)), ("warning", s(&p.warn)),
        ("error", s(&p.err)), ("tabSelectedBg", s(acc)), ("icon", s(acc)), ("tabSelectedFg", s(contrast_fg(acc))),
    ];
    if p.is_light() {
        v.extend([
            ("foreground", s("#111827")), ("foregroundMuted", s("#6b7280")), ("tabFg", s("#111827")),
            ("inputFg", s("#111827")), ("inputBg", s("#ffffff")), ("cardBg", s("#ffffff")),
            ("chromeBg", mix("#f8fafc", acc, 0.10)), ("cardBorder", mix("#cbd5e1", acc, 0.40)),
            ("folderSelected", mix("#ffffff", acc, 0.16)), ("statusBg", mix("#eef2f7", acc, 0.14)),
            ("hoverBg", mix("#ffffff", acc, 0.12)), ("splitter", mix("#94a3b8", acc, 0.45)),
        ]);
    } else {
        v.extend([
            ("foreground", s(&p.fg)), ("foregroundMuted", s(&p.muted)), ("tabFg", s(&p.fg)), ("inputFg", s(&p.fg)),
            ("chromeBg", s(&p.bg)), ("inputBg", mix(&p.bg, &p.fg, 0.08)),
        ]);
        // token, t (normal), t (neonUi)
        for (k, normal, neon) in [("cardBg", 0.12, 0.10), ("cardBorder", 0.28, 0.42), ("folderSelected", 0.20, 0.24),
            ("statusBg", 0.14, 0.18), ("hoverBg", 0.16, 0.22), ("splitter", 0.40, 0.55)]
        {
            v.push((k, mix(&p.bg, acc, if p.neon_ui { neon } else { normal })));
        }
    }
    v
}

fn ui_tokens(p: &Preset, o: &ThemeOverrides, terminal_bg: &str) -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = derive_ui(p).into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    m.extend(o.ui.iter().map(|(k, v)| (k.clone(), v.clone())));
    // Derived from the *effective* accent (WPF's AccentDim never followed the theme, §24 #11).
    let dim = with_alpha(&m["accent"], 0.14);
    m.entry("accentDim".into()).or_insert(dim);
    m.entry("terminalBg".into()).or_insert_with(|| terminal_bg.to_string());
    m
}

/// `--ui-*` custom properties for `:root` (§13.5): colour tokens plus the font tokens and scale.
pub fn ui_css_vars(t: &EffectiveTheme, ui: &UiSettings) -> Vec<(String, String)> {
    let kebab = |k: &str| k.chars().flat_map(|c| if c.is_ascii_uppercase() { vec!['-', c.to_ascii_lowercase()] } else { vec![c] }).collect::<String>();
    let mut v: Vec<(String, String)> = t.ui.iter().map(|(k, c)| (format!("--ui-{}", kebab(k)), c.clone())).collect();
    let sz = ui.font_size;
    v.extend([
        ("--ui-font-family".to_string(), ui.font_family.clone()),
        ("--ui-font-size".to_string(), format!("{sz}px")),
        ("--ui-font-size-small".to_string(), format!("{}px", sz - 2.0)),
        ("--ui-font-size-title".to_string(), format!("{}px", sz + 2.0)),
        ("--ui-font-size-tab".to_string(), format!("{}px", sz - 3.0)),
        ("--ui-font-weight".to_string(), ui.font_weight.to_string()),
        ("--ui-scale".to_string(), ui.scale.to_string()),
    ]);
    v
}

impl Preset {
    /// A user theme file → preset. Needs a `name`; missing/invalid colours fall back to the
    /// Default preset's, so a half-written file still yields a usable theme.
    fn from_user_json(v: &serde_json::Value) -> Option<Preset> {
        let mut p: Preset = crate::lenient(v);
        p.name = v.get("name")?.as_str()?.trim().to_string();
        if v.get("light").is_none() {
            p.light = None; // not inherited from the Default preset: decide from `bg`
        }
        if p.name.is_empty() {
            return None;
        }
        let mut d = Preset::default();
        for (c, dc) in p.colours_mut().into_iter().zip(d.colours_mut()) {
            *c = normalize_hex(c).unwrap_or_else(|| dc.clone());
        }
        Some(p)
    }
}

/// [P1] `%APPDATA%\UselessTerminal\themes\*.json` (§13.4). Bad files are skipped, never fatal;
/// a user theme cannot shadow a built-in or an earlier file (so preset fixes always reach users).
pub fn load_user_themes(dir: &Path) -> Vec<Preset> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut files: Vec<_> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")))
        .collect();
    files.sort();
    let mut out: Vec<Preset> = vec![];
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        let Some(p) = serde_json::from_slice(bytes).ok().and_then(|v| Preset::from_user_json(&v)) else { continue };
        if presets::find(&p.name).is_none() && !out.iter().any(|q| q.name.eq_ignore_ascii_case(&p.name)) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod tests;
