//! Theme → egui: chrome colours from the backend-derived UI tokens, terminal palette for `ut-term`.

use egui::{Color32, CornerRadius, Stroke, Visuals};
use ut_core::EffectiveTheme;
use ut_term::Palette;

/// Chrome colours (spec §13.5 tokens).
#[derive(Clone)]
pub struct Ui {
    pub foreground: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_dim: Color32,
    pub highlight: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    pub chrome_bg: Color32,
    pub card_bg: Color32,
    pub card_border: Color32,
    pub folder_selected: Color32,
    pub tab_fg: Color32,
    pub tab_selected_fg: Color32,
    pub status_bg: Color32,
    pub icon: Color32,
    pub input_bg: Color32,
    pub input_fg: Color32,
    pub hover_bg: Color32,
    pub splitter: Color32,
    pub terminal_bg: Color32,
    pub light: bool,
}

pub struct Theme {
    pub ui: Ui,
    pub palette: Palette,
    pub selection_bg: Color32,
    pub selection_fg: Option<Color32>,
    pub cursor: Color32,
    pub term_bg: Color32,
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa` or `rgba(r,g,b,a)`; anything else is black (never panics, §13.3).
pub fn parse_color(s: &str) -> Color32 {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("rgba(").and_then(|r| r.strip_suffix(')')) {
        let p: Vec<&str> = inner.split(',').map(str::trim).collect();
        if p.len() == 4 {
            let n = |i: usize| p[i].parse::<f32>().unwrap_or(0.0).clamp(0.0, 255.0) as u8;
            let a = (p[3].parse::<f32>().unwrap_or(1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            return Color32::from_rgba_unmultiplied(n(0), n(1), n(2), a);
        }
    }
    let h = s.trim_start_matches('#');
    let v = |i: usize, len: usize| u8::from_str_radix(&h[i..i + len], 16).unwrap_or(0);
    match h.len() {
        3 if h.is_ascii() => Color32::from_rgb(v(0, 1) * 17, v(1, 1) * 17, v(2, 1) * 17),
        6 if h.is_ascii() => Color32::from_rgb(v(0, 2), v(2, 2), v(4, 2)),
        8 if h.is_ascii() => Color32::from_rgba_unmultiplied(v(0, 2), v(2, 2), v(4, 2), v(6, 2)),
        _ => Color32::BLACK,
    }
}

fn rgb3(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

impl Theme {
    pub fn from(t: &EffectiveTheme) -> Theme {
        let u = |k: &str| t.ui.get(k).map(|s| parse_color(s)).unwrap_or(Color32::MAGENTA);
        let term = |k: &str| t.terminal.get(k).map(|s| parse_color(s)).unwrap_or(Color32::MAGENTA);
        const ANSI: [&str; 16] = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white", "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
        ];
        let mut ansi = [[0u8; 3]; 16];
        for (i, k) in ANSI.iter().enumerate() {
            ansi[i] = rgb3(term(k));
        }
        let ui = Ui {
            foreground: u("foreground"),
            muted: u("foregroundMuted"),
            accent: u("accent"),
            accent_dim: u("accentDim"),
            highlight: u("highlight"),
            success: u("success"),
            warning: u("warning"),
            error: u("error"),
            chrome_bg: u("chromeBg"),
            card_bg: u("cardBg"),
            card_border: u("cardBorder"),
            folder_selected: u("folderSelected"),
            tab_fg: u("tabFg"),
            tab_selected_fg: u("tabSelectedFg"),
            status_bg: u("statusBg"),
            icon: u("icon"),
            input_bg: u("inputBg"),
            input_fg: u("inputFg"),
            hover_bg: u("hoverBg"),
            splitter: u("splitter"),
            terminal_bg: u("terminalBg"),
            light: t.light,
        };
        let bg = term("background");
        let sel = term("selectionBackground");
        Theme {
            ui,
            palette: Palette { fg: rgb3(term("foreground")), bg: rgb3(bg), cursor: rgb3(term("cursor")), ansi },
            selection_bg: sel,
            selection_fg: t.terminal.get("selectionForeground").map(|s| parse_color(s)),
            cursor: term("cursor"),
            term_bg: bg,
        }
    }

    /// Apply the chrome colours to egui's widgets.
    pub fn apply(&self, ctx: &egui::Context) {
        let u = &self.ui;
        let mut v = if u.light { Visuals::light() } else { Visuals::dark() };
        v.override_text_color = Some(u.foreground);
        v.panel_fill = u.chrome_bg;
        v.window_fill = u.card_bg;
        v.window_stroke = Stroke::new(1.0, u.accent);
        v.window_corner_radius = CornerRadius::same(8);
        v.menu_corner_radius = CornerRadius::same(6);
        v.extreme_bg_color = u.input_bg;
        v.faint_bg_color = u.card_bg;
        v.code_bg_color = u.input_bg;
        v.hyperlink_color = u.accent;
        v.warn_fg_color = u.warning;
        v.error_fg_color = u.error;
        v.selection.bg_fill = u.accent_dim.gamma_multiply(2.5);
        v.selection.stroke = Stroke::new(1.0, u.accent);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = u.card_bg;
        w.noninteractive.weak_bg_fill = u.card_bg;
        w.noninteractive.bg_stroke = Stroke::new(1.0, u.card_border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, u.foreground);
        for (st, fill) in [(&mut w.inactive, u.input_bg), (&mut w.hovered, u.hover_bg), (&mut w.active, u.accent_dim.gamma_multiply(2.0)), (&mut w.open, u.hover_bg)] {
            st.bg_fill = fill;
            st.weak_bg_fill = fill;
            st.fg_stroke = Stroke::new(1.0, u.foreground);
            st.corner_radius = CornerRadius::same(4);
        }
        w.inactive.bg_stroke = Stroke::new(1.0, u.card_border);
        w.hovered.bg_stroke = Stroke::new(1.0, u.splitter);
        w.active.bg_stroke = Stroke::new(1.0, u.accent);
        ctx.set_visuals(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_formats_never_panic() {
        assert_eq!(parse_color("#6be5ff"), Color32::from_rgb(0x6b, 0xe5, 0xff));
        assert_eq!(parse_color("#fff"), Color32::from_rgb(255, 255, 255));
        assert_eq!(parse_color("rgba(107,229,255,0.14)").a(), 36);
        assert_eq!(parse_color("zzz"), Color32::BLACK);
        assert_eq!(parse_color("#é"), Color32::BLACK);
        assert_eq!(parse_color(""), Color32::BLACK);
    }

    #[test]
    fn default_theme_maps() {
        let t = Theme::from(&ut_core::effective(&Default::default()));
        assert_eq!(t.palette.bg, [0, 0, 0]);
        assert_ne!(t.palette.ansi[1], t.palette.ansi[9], "bright variants differ (§13.2)");
        assert_eq!(t.ui.accent, Color32::from_rgb(0x6b, 0xe5, 0xff));
    }
}
