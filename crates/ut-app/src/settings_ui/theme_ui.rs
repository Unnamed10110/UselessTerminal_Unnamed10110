//! Theme pages of the settings window (§13, §14.3): preset gallery (hover = live preview, click = apply), per-colour
//! overrides with derived defaults, the 16 ANSI slots, the UI-token grid, a live sample pane and user-theme
//! import/export (`themes\*.json`, §13.4). The model (`ut_core`) resolves everything; this file only picks values.

use super::form::{color_edit, ColorEdit, Form};
use crate::core::Core;
use crate::kit::ToastKind;
use crate::panels::{Cmd, PanelCtx};
use crate::theme::{parse_color, Theme};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Id, Margin, RichText, Sense, Stroke, Ui, Vec2};
use std::collections::BTreeMap;
use ut_core::color::normalize_hex;
use ut_core::theme::{find_preset, ANSI_BASE, ANSI_BRIGHT};
use ut_core::{effective_with, EffectiveTheme, Preset, Settings, ThemeOverrides, ThemeRef};

/// The colour grid of §14.3: semantic override key and its label (typed input is a personal setting, handled apart).
pub const TERMINAL_ROWS: [(&str, &str); 13] = [
    ("background", "Background"),
    ("foreground", "Default text"),
    ("muted", "Muted text"),
    ("error", "Errors"),
    ("warning", "Warnings"),
    ("command", "Commands & success"),
    ("message", "Info"),
    ("accent", "Paths & links"),
    ("highlight", "Highlights"),
    ("cursor", "Cursor"),
    ("cursorAccent", "Text under cursor"),
    ("selectionBackground", "Selection background"),
    ("selectionForeground", "Selection text"),
];

/// The 20 editable UI tokens (`accentDim` is derived from the accent, `terminalBg` from the terminal background).
pub const UI_ROWS: [(&str, &str); 20] = [
    ("foreground", "Text"),
    ("foregroundMuted", "Muted text"),
    ("accent", "Accent"),
    ("highlight", "Highlight"),
    ("success", "Success"),
    ("warning", "Warning"),
    ("error", "Error"),
    ("chromeBg", "Window background"),
    ("cardBg", "Card background"),
    ("cardBorder", "Card border"),
    ("folderSelected", "Selected folder"),
    ("tabFg", "Tab text"),
    ("tabSelectedBg", "Selected tab"),
    ("tabSelectedFg", "Selected tab text"),
    ("statusBg", "Status bar"),
    ("icon", "Icons"),
    ("inputBg", "Input background"),
    ("inputFg", "Input text"),
    ("hoverBg", "Hover background"),
    ("splitter", "Splitter"),
];

fn lower(hex: &str) -> String {
    normalize_hex(hex).unwrap_or_else(|| hex.to_string())
}

/// The preset's own value of a semantic colour (what "reset" returns to), lowercase `#rrggbb`.
pub fn semantic_default(p: &Preset, key: &str, effective_bg: &str) -> String {
    lower(match key {
        "background" => &p.bg,
        "foreground" => &p.fg,
        "muted" => &p.muted,
        "error" => &p.err,
        "warning" => &p.warn,
        "command" => &p.cmd,
        "message" => &p.msg,
        "accent" => &p.acc,
        "highlight" => &p.hi,
        "cursor" => &p.cursor,
        "cursorAccent" => effective_bg, // follows the (possibly overridden) background (§13.2)
        "selectionBackground" => &p.sel_bg,
        "selectionForeground" => &p.sel_fg,
        _ => "#000000",
    })
}

/// What the theme's overrides say about a semantic colour: the override, else the preset's value.
pub fn semantic_value(p: &Preset, o: &ThemeOverrides, key: &str, effective_bg: &str) -> String {
    o.terminal.get(key).cloned().unwrap_or_else(|| semantic_default(p, key, effective_bg))
}

/// Turn a user-picked colour into the patch value: the preset's own colour is "no override" (`None` = reset).
pub fn override_value(new_hex: &str, default_hex: &str) -> Option<String> {
    let n = lower(new_hex);
    (n != lower(default_hex)).then_some(n)
}

/// A user theme file from the current theme: the resolved preset with the semantic overrides baked in.
/// (ANSI/UI-token overrides have no field in the preset schema and stay in `settings.json`.)
pub fn preset_from_theme(name: &str, t: &ThemeRef, user: &[Preset], typed_input: &str) -> Preset {
    let p = find_preset(&t.preset, user);
    let bg = semantic_value(&p, &t.overrides, "background", &p.bg);
    let v = |k: &str| semantic_value(&p, &t.overrides, k, &bg);
    Preset {
        name: name.trim().to_string(),
        bg: v("background"),
        fg: v("foreground"),
        input: typed_input.to_string(),
        muted: v("muted"),
        err: v("error"),
        warn: v("warning"),
        cmd: v("command"),
        msg: v("message"),
        acc: v("accent"),
        hi: v("highlight"),
        cursor: v("cursor"),
        sel_bg: v("selectionBackground"),
        sel_fg: v("selectionForeground"),
        neon_ui: p.neon_ui,
        light: p.light,
    }
}

/// A theme file's `name`, or why it can't be imported (§13.4: a user theme cannot shadow a built-in).
pub fn theme_file_name(bytes: &[u8]) -> Result<String, String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("not a theme file ({e})"))?;
    let name = v.get("name").and_then(|n| n.as_str()).map(str::trim).filter(|n| !n.is_empty()).ok_or("the file has no \"name\"")?;
    if ut_core::presets::find(name).is_some() {
        return Err(format!("\"{name}\" is a built-in theme name; rename it in the file"));
    }
    Ok(name.to_string())
}

/// Preset names grouped for the gallery: dark, AMOLED, light, then the user's own.
pub fn gallery(core: &Core) -> Vec<(&'static str, Vec<Preset>)> {
    let (mut dark, mut amoled, mut light) = (vec![], vec![], vec![]);
    for p in ut_core::presets::all() {
        if p.is_light() {
            light.push(p.clone());
        } else if p.neon_ui {
            amoled.push(p.clone());
        } else {
            dark.push(p.clone());
        }
    }
    let mut v = vec![("Dark", dark), ("AMOLED", amoled), ("Light", light)];
    let mine = core.user_themes.read().clone();
    if !mine.is_empty() {
        v.push(("My themes", mine));
    }
    v
}

fn write_user_theme(core: &Core, p: &Preset) -> Result<(), String> {
    if p.name.is_empty() || p.name.chars().count() > 60 {
        return Err("Give the theme a name (up to 60 characters).".into());
    }
    if ut_core::presets::find(&p.name).is_some() {
        return Err(format!("\"{}\" is a built-in theme name.", p.name));
    }
    let dir = ut_fs::app_data_dir().join("themes");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create the themes folder: {e}"))?;
    let file = dir.join(format!("{}.json", ut_fs::sanitize_file_name(&p.name, "theme")));
    ut_fs::write_json_atomic(&file, p).map_err(|e| format!("Cannot write {}: {e}", file.display()))?;
    *core.user_themes.write() = ut_core::load_user_themes(&dir);
    if core.user_themes.read().iter().any(|q| q.name.eq_ignore_ascii_case(&p.name) && q == p) {
        Ok(())
    } else {
        Err(format!("A different theme called \"{}\" already exists in the themes folder.", p.name))
    }
}

// ----------------------------------------------------------------------------------------------- UI

#[derive(Default)]
pub struct ThemeUi {
    /// Preset under the pointer this frame / the one the app is previewing now.
    hover_now: Option<String>,
    previewing: Option<String>,
    advanced: bool,
    new_name: String,
}

fn card(ui: &mut Ui, th: &Theme, p: &Preset, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(176.0, 58.0), Sense::click());
    let painter = ui.painter_at(rect);
    let bg = parse_color(&p.bg);
    painter.rect_filled(rect, 6.0, bg);
    let stroke = if selected {
        Stroke::new(2.0, th.ui.accent)
    } else if resp.hovered() {
        Stroke::new(1.5, th.ui.foreground)
    } else {
        Stroke::new(1.0, th.ui.card_border)
    };
    painter.rect_stroke(rect, 6.0, stroke, egui::StrokeKind::Inside);
    painter.text(rect.left_top() + Vec2::new(9.0, 7.0), egui::Align2::LEFT_TOP, &p.name, FontId::proportional(12.5), parse_color(&p.fg));
    if selected {
        painter.text(rect.right_top() + Vec2::new(-8.0, 6.0), egui::Align2::RIGHT_TOP, "✓", FontId::proportional(13.0), parse_color(&p.acc));
    }
    let mut x = rect.left() + 9.0;
    for c in [&p.err, &p.warn, &p.cmd, &p.msg, &p.acc, &p.hi, &p.fg, &p.muted] {
        painter.rect_filled(egui::Rect::from_min_size(egui::Pos2::new(x, rect.bottom() - 19.0), Vec2::new(14.0, 11.0)), 2.0, parse_color(c));
        x += 17.0;
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A colour cell: effective value, derived default, and whether it is overridden. Writes the patch.
fn edit_cell(ui: &mut Ui, f: &mut Form, path: &str, cur: &str, default: &str, overridden: bool) {
    let Form { patch, errors, th, .. } = f;
    if let Some(e) = color_edit(ui, th, Id::new(("color", path)), cur, overridden, errors) {
        match e {
            ColorEdit::Set(h) => match override_value(&h, default) {
                Some(v) => patch.set(path, v),
                None => patch.reset(path),
            },
            ColorEdit::Reset => patch.reset(path),
        }
    }
}

impl ThemeUi {
    /// Preview the hovered preset; revert when the pointer leaves (or the page is not shown). Call once per frame.
    pub fn finish_frame(&mut self, x: &mut PanelCtx) {
        let want = self.hover_now.take();
        if want != self.previewing {
            let t = want.as_ref().map(|n| effective_with(&ThemeRef { preset: n.clone(), overrides: Default::default() }, &x.core.user_themes.read()));
            x.cmds.push(Cmd::PreviewTheme(t));
            self.previewing = want;
        }
    }

    /// Drop a running preview immediately (window closed).
    pub fn stop_preview(&mut self, x: &mut PanelCtx) {
        self.hover_now = None;
        self.finish_frame(x);
    }

    /// "Theme" page: gallery, user themes, terminal colours, advanced ANSI slots, live sample.
    pub fn page(&mut self, ui: &mut Ui, x: &mut PanelCtx, f: &mut Form, cfg: &Settings) {
        let th = x.theme;
        let user = x.core.user_themes.read().clone();
        let base = find_preset(&cfg.theme.preset, &user);
        let o = &cfg.theme.overrides;
        let n_over = o.terminal.len() + o.ansi.len() + o.ui.len();

        Form::group(ui, th, "Preset", |ui| {
            ui.horizontal_wrapped(|ui| {
                // The dropdown always shows the ACTIVE preset (WPF showed "Custom", §14.3); items preview on hover.
                egui::ComboBox::from_id_salt("preset-dropdown").selected_text(RichText::new(&base.name).strong()).width(220.0).height(420.0).show_ui(ui, |ui| {
                    for (_, presets) in gallery(x.core) {
                        for p in presets {
                            let r = ui.selectable_label(p.name.eq_ignore_ascii_case(&base.name), &p.name);
                            if r.hovered() {
                                self.hover_now = Some(p.name.clone());
                            }
                            if r.clicked() {
                                f.patch.set("theme.preset", p.name.clone());
                                f.patch.reset("theme.overrides");
                            }
                        }
                    }
                });
                if cfg.theme.preset != base.name {
                    ui.label(RichText::new(format!("(\"{}\" is not installed; showing {})", cfg.theme.preset, base.name)).small().color(th.ui.warning));
                }
                if n_over > 0 {
                    ui.label(RichText::new(format!("+ {n_over} customised colour{}", if n_over == 1 { "" } else { "s" })).color(th.ui.muted));
                    if ui.button("Reset customisations").on_hover_text("Back to the plain preset").clicked() {
                        f.patch.reset("theme.overrides");
                    }
                }
            });
            ui.label(RichText::new("Hover a preset to preview it on the whole window, click to apply. Applying a preset replaces your colour customisations.").small().color(th.ui.muted));
            ui.add_space(4.0);
            for (group, presets) in gallery(x.core) {
                ui.add_space(4.0);
                ui.label(RichText::new(group).color(th.ui.muted));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                    for p in &presets {
                        let selected = p.name.eq_ignore_ascii_case(&base.name);
                        let r = card(ui, th, p, selected);
                        if r.hovered() && !(selected && n_over == 0) {
                            self.hover_now = Some(p.name.clone());
                        }
                        if r.clicked() {
                            f.patch.set("theme.preset", p.name.clone());
                            f.patch.reset("theme.overrides");
                        }
                    }
                });
            }
        });

        Form::group(ui, th, "My themes", |ui| {
            ui.label(RichText::new("Save the current colours as a theme file in the themes folder, or import one. Files are plain JSON (same schema as a preset).").small().color(th.ui.muted));
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("Theme name").desired_width(180.0));
                if ui.add_enabled(!self.new_name.trim().is_empty(), egui::Button::new("Save current colours")).clicked() {
                    let p = preset_from_theme(&self.new_name, &cfg.theme, &user, &cfg.terminal.typed_input_color);
                    match write_user_theme(x.core, &p) {
                        Ok(()) => {
                            f.patch.set("theme.preset", p.name.clone());
                            f.patch.reset("theme.overrides");
                            x.toasts.push(format!("Saved theme \"{}\".", p.name), ToastKind::Success);
                            self.new_name.clear();
                        }
                        Err(e) => f.errors.push(e),
                    }
                }
                if ui.button("Import…").clicked() {
                    self.import(x, f);
                }
                if ui.button("Open themes folder").clicked() {
                    let dir = ut_fs::app_data_dir().join("themes");
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = crate::sys::shell_open(&dir.to_string_lossy());
                }
                if ui.button("Reload").on_hover_text("Re-read the themes folder").clicked() {
                    *x.core.user_themes.write() = ut_core::load_user_themes(&ut_fs::app_data_dir().join("themes"));
                }
            });
        });

        let eff = effective_with(&cfg.theme, &user);
        let bg = eff.terminal.get("background").cloned().unwrap_or_default();
        Form::group(ui, th, "Terminal colours", |ui| {
            Form::grid(ui, "term-colours", |ui| {
                for (key, label) in TERMINAL_ROWS {
                    let default = semantic_default(&base, key, &bg);
                    let cur = lower(&semantic_value(&base, o, key, &bg));
                    let overridden = o.terminal.contains_key(key);
                    let path = format!("theme.overrides.{key}");
                    let fth = f.th;
                    Form::row(ui, fth, label, "", |ui| edit_cell(ui, f, &path, &cur, &default, overridden));
                    if key == "foreground" {
                        // personal setting: never changed by a preset (§13.2)
                        let typed = cfg.terminal.typed_input_color.clone();
                        Form::row(ui, fth, "Typed input", "Personal setting, kept when you change preset", |ui| edit_cell(ui, f, "terminal.typedInputColor", &typed, "#ffffff", typed != "#ffffff"));
                    }
                }
            });
            ui.add_space(6.0);
            ui.checkbox(&mut self.advanced, "Advanced: the 16 ANSI colours");
            if self.advanced {
                let mut plain = cfg.theme.clone();
                plain.overrides.ansi.clear();
                let derived = effective_with(&plain, &user);
                ui.label(RichText::new("What programs get for \"red\", \"green\"… Normally derived from the colours above (bright = lighter variant).").small().color(th.ui.muted));
                egui::Grid::new("ansi-colours").num_columns(4).spacing([16.0, 8.0]).show(ui, |ui| {
                    for i in 0..8 {
                        for name in [ANSI_BASE[i], ANSI_BRIGHT[i]] {
                            ui.label(name);
                            let cur = eff.terminal.get(name).cloned().unwrap_or_default();
                            let default = derived.terminal.get(name).cloned().unwrap_or_default();
                            ui.horizontal(|ui| edit_cell(ui, f, &format!("theme.overrides.ansi.{name}"), &lower(&cur), &lower(&default), o.ansi.contains_key(name)));
                        }
                        ui.end_row();
                    }
                });
            }
        });

        Form::group(ui, th, "Preview", |ui| sample(ui, th, &eff, &x.fonts.regular, &cfg.terminal.typed_input_color));
    }

    fn import(&mut self, x: &mut PanelCtx, f: &mut Form) {
        let Some(path) = rfd::FileDialog::new().add_filter("Theme (JSON)", &["json"]).add_filter("All files", &["*"]).pick_file() else { return };
        let res = std::fs::metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|m| if m.len() > 256 * 1024 { Err("the file is too large for a theme".to_string()) } else { Ok(()) })
            .and_then(|()| std::fs::read(&path).map_err(|e| e.to_string()))
            .and_then(|bytes| theme_file_name(&bytes).map(|n| (n, bytes)));
        match res {
            Err(e) => f.errors.push(format!("Cannot import {}: {e}.", path.display())),
            Ok((name, bytes)) => {
                let dir = ut_fs::app_data_dir().join("themes");
                let dest = dir.join(format!("{}.json", ut_fs::sanitize_file_name(&name, "theme")));
                let ok = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&dest, &bytes)).is_ok();
                *x.core.user_themes.write() = ut_core::load_user_themes(&dir);
                if ok && x.core.user_themes.read().iter().any(|p| p.name.eq_ignore_ascii_case(&name)) {
                    f.patch.set("theme.preset", name.clone());
                    f.patch.reset("theme.overrides");
                    x.toasts.push(format!("Imported theme \"{name}\"."), ToastKind::Success);
                } else {
                    let _ = std::fs::remove_file(&dest);
                    f.errors.push(format!("Cannot import {}: a theme called \"{name}\" already exists.", path.display()));
                }
            }
        }
    }
}

/// "Interface" colours: the 20 UI tokens with their derived defaults.
pub fn ui_tokens(ui: &mut Ui, x: &mut PanelCtx, f: &mut Form, cfg: &Settings) {
    let th = x.theme;
    let user = x.core.user_themes.read().clone();
    let eff = effective_with(&cfg.theme, &user);
    let mut plain = cfg.theme.clone();
    plain.overrides.ui.clear();
    let derived = effective_with(&plain, &user);
    let get = |m: &BTreeMap<String, String>, k: &str| lower(m.get(k).map(String::as_str).unwrap_or("#000000"));
    Form::group(ui, th, "Interface colours", |ui| {
        ui.label(RichText::new("Derived from the theme unless you change them here (cards, borders, hover, status bar…).").small().color(th.ui.muted));
        ui.add_space(4.0);
        if !cfg.theme.overrides.ui.is_empty() && ui.button("Reset interface colours").clicked() {
            f.patch.reset("theme.overrides.ui");
        }
        Form::grid(ui, "ui-colours", |ui| {
            for (key, label) in UI_ROWS {
                let (cur, default) = (get(&eff.ui, key), get(&derived.ui, key));
                let overridden = cfg.theme.overrides.ui.contains_key(key);
                let fth = f.th;
                Form::row(ui, fth, label, "", |ui| edit_cell(ui, f, &format!("theme.overrides.ui.{key}"), &cur, &default, overridden));
            }
        });
    });
}

// ------------------------------------------------------------------------------------------- sample

/// One styled run of the sample: text, foreground key in the effective terminal map, optional background key.
pub struct Span {
    pub text: String,
    pub fg: &'static str,
    pub bg: Option<&'static str>,
}

fn s(text: &str, fg: &'static str) -> Span {
    Span { text: text.into(), fg, bg: None }
}

/// A fixed ANSI test card: `ls --color`, a git status, prompts, warnings/errors, a selection and the cursor (§14.3).
pub fn sample_lines() -> Vec<Vec<Span>> {
    let prompt = |cwd: &str, typed: &str| vec![s("PS ", "foreground"), s(cwd, "blue"), s("> ", "foreground"), s(typed, "typed")];
    vec![
        prompt("C:\\Users\\you", "ls --color"),
        vec![s("drwxr-xr-x  ", "brightBlack"), s("src", "blue")],
        vec![s("-rwxr-xr-x  ", "brightBlack"), s("build.cmd", "green")],
        vec![s("-rw-r--r--  ", "brightBlack"), s("README.md", "foreground")],
        vec![s("lrwxrwxrwx  ", "brightBlack"), s("latest -> ", "foreground"), s("target", "cyan")],
        prompt("C:\\Users\\you\\repo", "git status"),
        vec![s("On branch ", "foreground"), s("main", "magenta")],
        vec![s("Changes not staged for commit:", "foreground")],
        vec![s("        modified:   ", "foreground"), s("src/main.rs", "red")],
        vec![s("        new file:   ", "foreground"), s("notes.txt", "green")],
        prompt("C:\\Users\\you\\repo", "cargo build"),
        vec![s("warning", "yellow"), s(": unused variable `x`", "foreground")],
        vec![s("error", "brightRed"), s(": could not compile `app`", "foreground"), s("  (", "brightBlack"), s("exit 101", "red"), s(")", "brightBlack")],
        vec![Span { text: " selected text ".into(), fg: "selectionForeground", bg: Some("selectionBackground") }, s("  ", "foreground"), s("muted text", "brightBlack")],
        vec![s("PS ", "foreground"), s("C:\\Users\\you", "blue"), s("> ", "foreground"), Span { text: "█".into(), fg: "cursor", bg: None }],
    ]
}

fn sample(ui: &mut Ui, th: &Theme, eff: &EffectiveTheme, font: &FontId, typed: &str) {
    let get = |k: &str| eff.terminal.get(k).map(|h| parse_color(h)).unwrap_or(Color32::MAGENTA);
    let color = |k: &str| if k == "typed" { parse_color(typed) } else { get(k) };
    egui::Frame::new().fill(get("background")).stroke(Stroke::new(1.0, th.ui.card_border)).corner_radius(6).inner_margin(Margin::same(10)).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        for line in sample_lines() {
            let mut job = LayoutJob::default();
            for sp in &line {
                job.append(&sp.text, 0.0, TextFormat { font_id: font.clone(), color: color(sp.fg), background: sp.bg.map(get).unwrap_or(Color32::TRANSPARENT), ..Default::default() });
            }
            ui.add(egui::Label::new(job).selectable(false));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> Preset {
        ut_core::presets::find(name).unwrap().clone()
    }

    #[test]
    fn the_grid_has_the_spec_rows() {
        assert_eq!(UI_ROWS.len(), 20);
        for (k, _) in UI_ROWS {
            assert!(ut_core::theme::UI_KEYS.contains(&k), "{k}");
        }
        for (k, _) in TERMINAL_ROWS {
            assert!(ut_core::theme::SEMANTIC_KEYS.contains(&k), "{k}");
        }
        assert_eq!(TERMINAL_ROWS.len(), ut_core::theme::SEMANTIC_KEYS.len());
    }

    #[test]
    fn defaults_come_from_the_preset_and_follow_the_background() {
        let d = p("Dracula");
        assert_eq!(semantic_default(&d, "accent", "#000000"), "#bd93f9");
        assert_eq!(semantic_default(&d, "selectionBackground", "#000000"), "#44475a");
        assert_eq!(semantic_default(&d, "cursorAccent", "#123456"), "#123456", "text under the cursor follows the background");
        let a = p("Ayu Dark");
        assert_eq!(semantic_default(&a, "background", ""), "#0a0e14", "normalised to lowercase");
        let mut o = ThemeOverrides::default();
        o.terminal.insert("accent".into(), "#112233".into());
        assert_eq!(semantic_value(&d, &o, "accent", ""), "#112233");
        assert_eq!(semantic_value(&d, &o, "error", ""), "#ff5555");
    }

    #[test]
    fn choosing_the_presets_own_colour_clears_the_override() {
        assert_eq!(override_value("#BD93F9", "#bd93f9"), None);
        assert_eq!(override_value("#bd93fa", "#bd93f9").as_deref(), Some("#bd93fa"));
        assert_eq!(override_value("#ABC", "#aabbcc"), None);
    }

    #[test]
    fn each_semantic_override_reaches_the_effective_theme() {
        // every row of the grid must change the resolved theme (so the swatch really edits something)
        let base = ThemeRef::default();
        let e0 = effective_with(&base, &[]);
        for (key, _) in TERMINAL_ROWS {
            let mut t = base.clone();
            t.overrides.terminal.insert(key.into(), "#010203".into());
            t.sanitize();
            assert_ne!(effective_with(&t, &[]), e0, "{key}");
        }
        for (key, _) in UI_ROWS {
            let mut t = base.clone();
            t.overrides.ui.insert(key.into(), "#010203".into());
            t.sanitize();
            assert_ne!(effective_with(&t, &[]).ui, e0.ui, "{key}");
        }
        for name in ANSI_BASE.iter().chain(ANSI_BRIGHT.iter()) {
            let mut t = base.clone();
            t.overrides.ansi.insert((*name).into(), "#010203".into());
            t.sanitize();
            assert_eq!(effective_with(&t, &[]).terminal[*name], "#010203");
        }
    }

    #[test]
    fn patches_for_presets_and_overrides_go_through_the_store() {
        use super::super::form::Patch;
        let mut cur = Settings::default();
        // pick a preset + clear overrides, as the gallery does
        let mut pt = Patch::default();
        pt.set("theme.overrides.ui.accent", "#ff0000");
        pt.set("theme.overrides.background", "#101010");
        cur = ut_core::apply_patch(&cur, pt.into_value()).unwrap();
        assert_eq!(cur.theme.overrides.ui.len() + cur.theme.overrides.terminal.len(), 2);
        let mut pt = Patch::default();
        pt.set("theme.preset", "Nord");
        pt.reset("theme.overrides");
        let cur = ut_core::apply_patch(&cur, pt.into_value()).unwrap();
        assert_eq!(cur.theme.preset, "Nord");
        assert_eq!(cur.theme.overrides, ThemeOverrides::default(), "applying a preset drops the customisations");
        // an invalid colour is dropped by the model, never stored
        let mut pt = Patch::default();
        pt.set("theme.overrides.accent", "banana");
        let cur = ut_core::apply_patch(&cur, pt.into_value()).unwrap();
        assert!(cur.theme.overrides.terminal.is_empty());
        // and the typed-input colour is independent of the theme
        let mut pt = Patch::default();
        pt.set("terminal.typedInputColor", "#ABCDEF");
        pt.set("theme.preset", "Monokai");
        let cur = ut_core::apply_patch(&cur, pt.into_value()).unwrap();
        assert_eq!(cur.terminal.typed_input_color, "#abcdef");
        let mut pt = Patch::default();
        pt.set("theme.preset", "Dracula");
        assert_eq!(ut_core::apply_patch(&cur, pt.into_value()).unwrap().terminal.typed_input_color, "#abcdef", "presets never touch it (§13.2)");
    }

    #[test]
    fn exported_theme_reproduces_the_look() {
        let mut t = ThemeRef { preset: "Dracula".into(), overrides: Default::default() };
        t.overrides.terminal.insert("accent".into(), "#00ff00".into());
        t.overrides.terminal.insert("background".into(), "#101010".into());
        let exported = preset_from_theme("  My Dracula ", &t, &[], "#eeeeee");
        assert_eq!((exported.name.as_str(), exported.acc.as_str(), exported.bg.as_str(), exported.err.as_str()), ("My Dracula", "#00ff00", "#101010", "#ff5555"));
        assert_eq!(exported.input, "#eeeeee");
        // the file survives a JSON round trip and resolves to the same terminal colours as theme + overrides
        let json = serde_json::to_string(&exported).unwrap();
        let back: Preset = serde_json::from_str(&json).unwrap();
        assert_eq!(back, exported);
        let a = effective_with(&t, &[]);
        let b = effective_with(&ThemeRef { preset: "My Dracula".into(), overrides: Default::default() }, &[back]);
        assert_eq!(a.terminal, b.terminal);
        assert_eq!(a.light, b.light);
    }

    #[test]
    fn theme_import_validation() {
        assert_eq!(theme_file_name(br##"{"name": " Mine ", "bg": "#000"}"##).unwrap(), "Mine");
        assert_eq!(theme_file_name(b"\xEF\xBB\xBF{\"name\":\"Bom\"}").unwrap(), "Bom");
        assert!(theme_file_name(b"{ nope").is_err());
        assert!(theme_file_name(br##"{"bg": "#000"}"##).is_err(), "no name");
        assert!(theme_file_name(br#"{"name": "  "}"#).is_err());
        let e = theme_file_name(br#"{"name": "dracula"}"#).unwrap_err();
        assert!(e.contains("built-in"), "{e}");
    }

    #[test]
    fn user_theme_file_is_found_by_the_loader() {
        let d = std::env::temp_dir().join(format!("ut app themes {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let exported = preset_from_theme("Mine", &ThemeRef::default(), &[], "#ffffff");
        ut_fs::write_json_atomic(&d.join("Mine.json"), &exported).unwrap();
        let loaded = ut_core::load_user_themes(&d);
        assert_eq!(loaded, vec![exported]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sample_uses_only_known_colours() {
        let eff = effective_with(&ThemeRef::default(), &[]);
        let lines = sample_lines();
        assert!(lines.len() >= 12);
        for l in &lines {
            for sp in l {
                assert!(sp.fg == "typed" || eff.terminal.contains_key(sp.fg), "{}", sp.fg);
                assert!(sp.bg.is_none_or(|b| eff.terminal.contains_key(b)));
            }
        }
        let text: String = lines.iter().flatten().map(|sp| sp.text.as_str()).collect();
        assert!(text.contains("error") && text.contains("warning") && text.contains("modified:"));
    }

    #[test]
    fn gallery_lists_every_builtin_once() {
        let n: usize = ut_core::presets::all().len();
        assert_eq!(n, 37, "31 documented + 6 extras");
        let (mut dark, mut amoled, mut light) = (0, 0, 0);
        for p in ut_core::presets::all() {
            if p.is_light() {
                light += 1;
            } else if p.neon_ui {
                amoled += 1;
            } else {
                dark += 1;
            }
        }
        assert_eq!((dark, amoled, light), (17, 10, 10));
    }
}
