//! The theme presets of Appendix B, table-driven through the Dark / Amoled / Light factories (§13.3).

use crate::color::is_light;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// One preset = the colour columns of Appendix B. Same JSON schema as a user theme file (§13.4).
/// `input` is informational only: presets never touch the typed-input colour (§13.2, §23.18).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Preset {
    pub name: String,
    pub bg: String,
    pub fg: String,
    pub input: String,
    pub muted: String,
    pub err: String,
    pub warn: String,
    pub cmd: String,
    pub msg: String,
    pub acc: String,
    pub hi: String,
    pub cursor: String,
    pub sel_bg: String,
    pub sel_fg: String,
    /// Stronger UI derivation (§13.3 `neonUi`).
    pub neon_ui: bool,
    /// Light-style UI derivation (Light factory). `None` (user themes) = decide from `bg`.
    pub light: Option<bool>,
}

impl Default for Preset {
    fn default() -> Self {
        all()[0].clone()
    }
}

impl Preset {
    pub fn is_light(&self) -> bool {
        self.light.unwrap_or_else(|| is_light(&self.bg))
    }

    /// The 13 colour columns (everything but name and flags), in a fixed order.
    pub(crate) fn colours_mut(&mut self) -> [&mut String; 13] {
        [
            &mut self.bg, &mut self.fg, &mut self.input, &mut self.muted, &mut self.err, &mut self.warn,
            &mut self.cmd, &mut self.msg, &mut self.acc, &mut self.hi, &mut self.cursor, &mut self.sel_bg,
            &mut self.sel_fg,
        ]
    }
}

/// `Dark(bg, fg, input, muted, err, warn, cmd, msg, acc, hi, cursor, selBg, selFg, neonUi)`.
fn dark(name: &str, c: [&str; 13], neon_ui: bool) -> Preset {
    let [bg, fg, input, muted, err, warn, cmd, msg, acc, hi, cursor, sel_bg, sel_fg] = c.map(String::from);
    Preset { name: name.into(), bg, fg, input, muted, err, warn, cmd, msg, acc, hi, cursor, sel_bg, sel_fg, neon_ui, light: Some(false) }
}

/// `Amoled(neon, accent, warn, err, msg, highlight)`: every role gets its own colour (§23.17).
fn amoled(name: &str, [neon, accent, warn, err, msg, hi]: [&str; 6]) -> Preset {
    dark(name, ["#000000", "#f2f2f2", neon, "#6b6b6b", err, warn, neon, msg, accent, hi, "#f2f2f2", neon, "#000000"], true)
}

/// `Light(command, accent, warn, err, highlight)`.
fn light(name: &str, [command, accent, warn, err, hi]: [&str; 5]) -> Preset {
    let s = String::from;
    Preset {
        name: name.into(), bg: s("#f7f7f8"), fg: s("#1a1a1a"), input: s(command), muted: s("#6b7280"),
        err: s(err), warn: s(warn), cmd: s(command), msg: s(accent), acc: s(accent), hi: s(hi),
        cursor: s(command), sel_bg: s(command), sel_fg: s("#ffffff"), neon_ui: false, light: Some(true),
    }
}

// bg fg input muted err warn cmd msg acc hi cursor selBg selFg
const DARK: [(&str, [&str; 13]); 11] = [
    ("Default", ["#000000", "#ffffff", "#ffffff", "#888888", "#ff2b7b", "#ffef5c", "#b4fb00", "#56ffef", "#6be5ff", "#c47cff", "#ffffff", "#ffffff", "#000000"]),
    ("Dracula", ["#282a36", "#f8f8f2", "#f8f8f2", "#6272a4", "#ff5555", "#f1fa8c", "#50fa7b", "#8be9fd", "#bd93f9", "#ff79c6", "#f8f8f2", "#44475a", "#f8f8f2"]),
    ("Solarized Dark", ["#002b36", "#839496", "#93a1a1", "#586e75", "#dc322f", "#b58900", "#859900", "#2aa198", "#268bd2", "#d33682", "#839496", "#073642", "#93a1a1"]),
    ("Monokai", ["#272822", "#f8f8f2", "#f8f8f2", "#75715e", "#f92672", "#e6db74", "#a6e22e", "#66d9ef", "#ae81ff", "#fd971f", "#f8f8f0", "#49483e", "#f8f8f2"]),
    ("Nord", ["#2e3440", "#d8dee9", "#eceff4", "#4c566a", "#bf616a", "#ebcb8b", "#a3be8c", "#88c0d0", "#81a1c1", "#b48ead", "#d8dee9", "#434c5e", "#eceff4"]),
    ("Catppuccin Mocha", ["#1e1e2e", "#cdd6f4", "#cdd6f4", "#585b70", "#f38ba8", "#f9e2af", "#a6e3a1", "#94e2d5", "#89b4fa", "#cba6f7", "#f5e0dc", "#45475a", "#cdd6f4"]),
    ("One Dark", ["#282c34", "#abb2bf", "#abb2bf", "#5c6370", "#e06c75", "#e5c07b", "#98c379", "#56b6c2", "#61afef", "#c678dd", "#abb2bf", "#3e4451", "#abb2bf"]),
    ("Gruvbox Dark", ["#282828", "#ebdbb2", "#ebdbb2", "#928374", "#fb4934", "#fabd2f", "#b8bb26", "#8ec07c", "#83a598", "#d3869b", "#ebdbb2", "#3c3836", "#ebdbb2"]),
    ("Tokyo Night", ["#1a1b26", "#a9b1d6", "#c0caf5", "#565f89", "#f7768e", "#e0af68", "#9ece6a", "#7dcfff", "#7aa2f7", "#bb9af7", "#c0caf5", "#33467c", "#c0caf5"]),
    ("Ayu Dark", ["#0A0E14", "#BFBDB6", "#FFB454", "#626A73", "#F07178", "#E6B450", "#AAD94C", "#95E6CB", "#FFB454", "#D2A6FF", "#FFB454", "#253340", "#BFBDB6"]),
    ("Vesper", ["#101010", "#FFFFFF", "#FFFFFF", "#666666", "#D9827A", "#E8C989", "#A8C787", "#8FBCBB", "#FFC799", "#C9A0DC", "#FFFFFF", "#2A2A2A", "#FFFFFF"]),
];

// neon accent warn err msg highlight
const AMOLED: [(&str, [&str; 6]); 10] = [
    ("AMOLED Green", ["#39ff14", "#00e676", "#c6ff00", "#ff1744", "#18ffff", "#e040fb"]),
    ("AMOLED Red", ["#ff1744", "#ff5252", "#ffab00", "#b71c1c", "#ff80ab", "#7c4dff"]),
    ("AMOLED Purple Neon", ["#d500f9", "#ea80fc", "#f50057", "#ff1744", "#00e5ff", "#ffea00"]),
    ("AMOLED Cyan", ["#00e5ff", "#18ffff", "#00e676", "#ff1744", "#ea80fc", "#ff4081"]),
    ("AMOLED Orange", ["#ff6d00", "#ff9100", "#ffea00", "#ff1744", "#ff80ab", "#536dfe"]),
    ("AMOLED Pink", ["#ff4081", "#ff80ab", "#f50057", "#ff1744", "#e040fb", "#76ff03"]),
    ("AMOLED Blue", ["#2979ff", "#448aff", "#00e5ff", "#ff1744", "#7c4dff", "#ffd600"]),
    ("AMOLED Gold", ["#ffd600", "#ffea00", "#ffab00", "#ff1744", "#ff6d00", "#d500f9"]),
    ("AMOLED Matrix", ["#00ff41", "#33ff77", "#aaff00", "#ff003c", "#00e5ff", "#bf5fff"]),
    ("AMOLED Ice", ["#b3ffff", "#80d8ff", "#18ffff", "#ff5252", "#ea80fc", "#ffd740"]),
];

// command accent warn err highlight
const LIGHT: [(&str, [&str; 5]); 10] = [
    ("Light", ["#1565c0", "#00838f", "#2e7d32", "#c62828", "#6a1b9a"]),
    ("Light Green", ["#1b5e20", "#2e7d32", "#558b2f", "#c62828", "#00695c"]),
    ("Light Red", ["#b71c1c", "#c62828", "#e65100", "#b71c1c", "#ad1457"]),
    ("Light Purple Neon", ["#6a1b9a", "#8e24aa", "#c2185b", "#c62828", "#0277bd"]),
    ("Light Cyan", ["#006064", "#00838f", "#00695c", "#c62828", "#4527a0"]),
    ("Light Orange", ["#e65100", "#ef6c00", "#f9a825", "#c62828", "#ad1457"]),
    ("Light Pink", ["#ad1457", "#c2185b", "#d81b60", "#c62828", "#6a1b9a"]),
    ("Light Blue", ["#0d47a1", "#1565c0", "#0277bd", "#c62828", "#4527a0"]),
    ("Light Gold", ["#f9a825", "#f57f17", "#ef6c00", "#c62828", "#6a1b9a"]),
    ("Light Ice", ["#0277bd", "#0288d1", "#00838f", "#c62828", "#6a1b9a"]),
];

// [P1] extra dark presets, palettes from the upstream themes, appended after the documented 31.
const EXTRA: [(&str, [&str; 13]); 6] = [
    ("Rosé Pine", ["#191724", "#e0def4", "#e0def4", "#6e6a86", "#eb6f92", "#f6c177", "#31748f", "#ebbcba", "#9ccfd8", "#c4a7e7", "#e0def4", "#403d52", "#e0def4"]),
    ("Kanagawa", ["#1F1F28", "#DCD7BA", "#DCD7BA", "#727169", "#C34043", "#C0A36E", "#76946A", "#6A9589", "#7E9CD8", "#957FB8", "#C8C093", "#2D4F67", "#C8C093"]),
    ("Night Owl", ["#011627", "#d6deeb", "#d6deeb", "#637777", "#EF5350", "#ecc48d", "#addb67", "#7fdbca", "#82aaff", "#c792ea", "#80a4c2", "#1d3b53", "#d6deeb"]),
    ("Poimandres", ["#1b1e28", "#a6accd", "#a6accd", "#506477", "#d0679d", "#fffac2", "#5de4c7", "#89ddff", "#add7ff", "#f087bd", "#a6accd", "#303340", "#e4f0fb"]),
    ("Everforest Dark", ["#2d353b", "#d3c6aa", "#d3c6aa", "#859289", "#e67e80", "#dbbc7f", "#a7c080", "#83c092", "#7fbbb3", "#d699b6", "#d3c6aa", "#475258", "#d3c6aa"]),
    ("Iceberg Dark", ["#161821", "#c6c8d1", "#c6c8d1", "#6b7089", "#e27878", "#e2a478", "#b4be82", "#89b8c2", "#84a0c6", "#a093c7", "#c6c8d1", "#272c42", "#c6c8d1"]),
];

/// Every built-in preset: the 31 of Appendix B in documented order, then the [P1] extras.
pub fn all() -> &'static [Preset] {
    static ALL: OnceLock<Vec<Preset>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<Preset> = DARK.iter().map(|(n, c)| dark(n, *c, false)).collect();
        v.extend(AMOLED.iter().map(|(n, c)| amoled(n, *c)));
        v.extend(LIGHT.iter().map(|(n, c)| light(n, *c)));
        // Appendix B "Known issues" [P1]: Light Red's green must not equal its red; Light Cyan
        // gets its own accent so its chrome differs from Light. Only these two columns change.
        for p in &mut v {
            match p.name.as_str() {
                "Light Red" => p.cmd = "#2e7d32".into(),
                "Light Cyan" => p.acc = "#00796b".into(),
                _ => {}
            }
        }
        v.extend(EXTRA.iter().map(|(n, c)| dark(n, *c, false)));
        v
    })
}

/// Case-insensitive lookup among the built-ins.
pub fn find(name: &str) -> Option<&'static Preset> {
    all().iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
}
