//! Form plumbing for the settings window: every control edits ONE dotted path of the settings JSON
//! (`terminal.fontSize`) and records the change in a merge-patch ([`Patch`]); the window sends the patch to
//! `core.settings.patch` once per frame. The store sanitises/clamps (§14.1); the next frame reads the result back.

use crate::theme::Theme;
use egui::{Color32, Id, Key, RichText, Ui, Vec2};
use serde_json::{Map, Value};
use std::ops::RangeInclusive;
use ut_core::Settings;

/// A merge-patch under construction: `set("terminal.fontSize", 15)` builds `{"terminal":{"fontSize":15}}`.
#[derive(Default)]
pub struct Patch(Map<String, Value>);

impl Patch {
    pub fn set(&mut self, path: &str, v: impl Into<Value>) {
        let v = v.into();
        let mut cur = &mut self.0;
        let mut parts = path.split('.').peekable();
        while let Some(k) = parts.next() {
            if parts.peek().is_none() {
                cur.insert(k.to_string(), v);
                return;
            }
            let e = cur.entry(k.to_string()).or_insert_with(|| Value::Object(Map::new()));
            if !e.is_object() {
                *e = Value::Object(Map::new());
            }
            let Value::Object(m) = e else { return };
            cur = m;
        }
    }

    /// `null` = "back to the default" in a merge patch (RFC 7386).
    pub fn reset(&mut self, path: &str) {
        self.set(path, Value::Null);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}

/// `x` rounded to a multiple of `step` without float noise (`1.05`, not `1.0500000000000003`).
pub fn snap(x: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return x;
    }
    (((x / step).round() * step) * 1e4).round() / 1e4
}

/// Colour text typed by the user -> lowercase `#rrggbb`. Accepts `#rgb`, `#rrggbb` and the same without `#` (§14.3).
pub fn hex_input(s: &str) -> Option<String> {
    let s = s.trim();
    let with_hash = if s.starts_with('#') { s.to_string() } else { format!("#{s}") };
    ut_core::color::normalize_hex(&with_hash)
}

pub fn color_to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// The settings document as the controls see it, plus the patch they write.
pub struct Form<'a> {
    pub doc: Value,
    pub patch: Patch,
    pub th: &'a Theme,
    /// Messages for the toast area (invalid input), pushed by the window after the frame.
    pub errors: Vec<String>,
}

fn at<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(doc, |v, k| v.get(k))
}

impl<'a> Form<'a> {
    pub fn new(cfg: &Settings, th: &'a Theme) -> Self {
        Self { doc: serde_json::to_value(cfg).unwrap_or(Value::Null), patch: Patch::default(), th, errors: vec![] }
    }

    pub fn b(&self, path: &str) -> bool {
        at(&self.doc, path).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn f(&self, path: &str) -> f64 {
        at(&self.doc, path).and_then(Value::as_f64).unwrap_or(0.0)
    }

    pub fn s(&self, path: &str) -> &str {
        at(&self.doc, path).and_then(Value::as_str).unwrap_or("")
    }

    // ------------------------------------------------------------------------------------------ rows

    /// A settings group: a heading and the rows below it.
    pub fn group(ui: &mut Ui, th: &Theme, title: &str, add: impl FnOnce(&mut Ui)) {
        ui.add_space(12.0);
        ui.label(RichText::new(title).strong().size(15.0).color(th.ui.accent));
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(4.0);
        add(ui);
    }

    /// Two columns: label (+ small hint), control.
    pub fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
        egui::Grid::new(id).num_columns(2).spacing([20.0, 10.0]).min_col_width(230.0).show(ui, add);
    }

    pub fn row(ui: &mut Ui, th: &Theme, label: &str, hint: &str, add: impl FnOnce(&mut Ui)) {
        ui.vertical(|ui| {
            ui.set_max_width(260.0);
            ui.label(label);
            if !hint.is_empty() {
                ui.label(RichText::new(hint).small().color(th.ui.muted));
            }
        });
        ui.horizontal(|ui| add(ui));
        ui.end_row();
    }

    // ---------------------------------------------------------------------------------------- controls

    pub fn check(&mut self, ui: &mut Ui, path: &str, label: &str, hint: &str) {
        let mut v = self.b(path);
        let th = self.th;
        Self::row(ui, th, label, hint, |ui| {
            if ui.checkbox(&mut v, "").changed() {
                self.patch.set(path, v);
            }
        });
    }

    /// Float slider; the displayed decimals follow `step` (1 -> none, 0.5 -> one, 0.05 -> two).
    pub fn slider(&mut self, ui: &mut Ui, path: &str, label: &str, hint: &str, range: RangeInclusive<f64>, step: f64, suffix: &str) {
        let mut v = self.f(path);
        let th = self.th;
        Self::row(ui, th, label, hint, |ui| {
            ui.spacing_mut().slider_width = 200.0;
            let decimals = if step.fract() == 0.0 { 0 } else if (step * 10.0).fract() == 0.0 { 1 } else { 2 };
            let r = ui.add(egui::Slider::new(&mut v, range).step_by(step).suffix(suffix).fixed_decimals(decimals));
            if r.changed() {
                self.patch.set(path, snap(v, step));
            }
        });
    }

    /// A slider that shows its value in `unit` (a percentage of the stored fraction) and, when `deferred`, writes the
    /// setting only when the drag ends: the UI scale rescales the window under the pointer, which would fight a live drag.
    #[allow(clippy::too_many_arguments)]
    pub fn percent_slider(&mut self, ui: &mut Ui, path: &str, label: &str, hint: &str, range: RangeInclusive<f64>, step: f64, deferred: bool) {
        let id = Id::new(("percent-slider", path));
        let cur = self.f(path) * 100.0;
        let th = self.th;
        Self::row(ui, th, label, hint, |ui| {
            let editing = deferred && ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
            let mut v = if editing { ui.data(|d| d.get_temp::<f64>(id.with("v"))).unwrap_or(cur) } else { cur };
            ui.spacing_mut().slider_width = 200.0;
            let r = ui.add(egui::Slider::new(&mut v, range).step_by(step).suffix(" %").max_decimals(0));
            if !deferred {
                if r.changed() {
                    self.patch.set(path, snap(v / 100.0, step / 100.0));
                }
            } else if r.dragged() || (r.changed() && r.has_focus()) {
                ui.data_mut(|d| {
                    d.insert_temp(id, true);
                    d.insert_temp(id.with("v"), v);
                });
            } else if editing || r.changed() {
                ui.data_mut(|d| d.insert_temp(id, false));
                self.patch.set(path, snap(v / 100.0, step / 100.0));
            }
        });
    }

    /// Integer slider (the schema wants integers for these: `10000`, not `10000.0`).
    pub fn int_slider(&mut self, ui: &mut Ui, path: &str, label: &str, hint: &str, range: RangeInclusive<i64>, step: i64, suffix: &str) {
        let mut v = self.f(path).round() as i64;
        let th = self.th;
        Self::row(ui, th, label, hint, |ui| {
            ui.spacing_mut().slider_width = 200.0;
            let r = ui.add(egui::Slider::new(&mut v, range).step_by(step as f64).suffix(suffix));
            if r.changed() {
                self.patch.set(path, v);
            }
        });
    }

    /// Integer typed/dragged in a box (scrollback, seconds, percent).
    pub fn drag_int(&mut self, ui: &mut Ui, path: &str, label: &str, hint: &str, range: RangeInclusive<i64>, speed: f64, suffix: &str) {
        let mut v = self.f(path).round() as i64;
        let th = self.th;
        Self::row(ui, th, label, hint, |ui| {
            if ui.add(egui::DragValue::new(&mut v).range(range).speed(speed).suffix(suffix)).changed() {
                self.patch.set(path, v);
            }
        });
    }

    /// A few mutually exclusive options as a segmented row; `options` are `(json value, label)`.
    pub fn choice(&mut self, ui: &mut Ui, path: &str, label: &str, hint: &str, options: &[(&str, &str)]) {
        let cur = self.s(path).to_string();
        let th = self.th;
        Self::row(ui, th, label, hint, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (v, l) in options {
                if ui.selectable_label(cur == *v, *l).clicked() && cur != *v {
                    self.patch.set(path, *v);
                }
            }
        });
    }

    /// Dropdown for longer option lists.
    pub fn combo(&mut self, ui: &mut Ui, path: &str, label: &str, hint: &str, options: &[(String, String)]) {
        let cur = self.s(path).to_string();
        let th = self.th;
        let shown = options.iter().find(|(v, _)| *v == cur).map(|(_, l)| l.clone()).unwrap_or_else(|| cur.clone());
        Self::row(ui, th, label, hint, |ui| {
            egui::ComboBox::from_id_salt(path).selected_text(shown).width(260.0).show_ui(ui, |ui| {
                for (v, l) in options {
                    if ui.selectable_label(cur == *v, l).clicked() && cur != *v {
                        self.patch.set(path, v.clone());
                    }
                }
            });
        });
    }

    /// Single-line text that is committed on Enter / focus loss (typing must not re-install fonts per keystroke).
    /// `check` validates/normalises; an `Err` is reported and the old value stays.
    pub fn text(&mut self, ui: &mut Ui, path: &str, label: &str, hint: &str, width: f32, check: impl Fn(&str) -> Result<String, String>) {
        let cur = self.s(path).to_string();
        let th = self.th;
        let mut out = None;
        Self::row(ui, th, label, hint, |ui| {
            out = text_edit(ui, Id::new(("setting", path)), &cur, width, "", None);
        });
        if let Some(new) = out {
            match check(&new) {
                Ok(v) if v != cur => self.patch.set(path, v),
                Ok(_) => {}
                Err(e) => self.errors.push(e),
            }
        }
    }
}

/// A text field whose edit buffer lives in egui memory while it has focus (otherwise it mirrors `current`).
/// Returns the buffer when the user committed it (Enter / focus lost, not Esc) and it differs from `current`.
pub fn text_edit(ui: &mut Ui, id: Id, current: &str, width: f32, hint: &str, color: Option<Color32>) -> Option<String> {
    let mut buf: String = ui.data_mut(|d| d.get_temp(id)).unwrap_or_else(|| current.to_string());
    if !ui.memory(|m| m.has_focus(id)) {
        buf = current.to_string();
    }
    let resp = ui.add(egui::TextEdit::singleline(&mut buf).id(id).desired_width(width).hint_text(hint).text_color_opt(color));
    ui.data_mut(|d| d.insert_temp(id, buf.clone()));
    let esc = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape));
    (resp.lost_focus() && !esc && buf != current).then_some(buf)
}

/// A destructive button asks twice: the first click arms it for 3 s and relabels it, the second confirms.
pub fn armed_button(ui: &mut Ui, th: &Theme, id: Id, label: &str, confirm: &str) -> bool {
    let now = ui.input(|i| i.time);
    let armed = ui.data(|d| d.get_temp::<f64>(id)).is_some_and(|t| now - t < 3.0);
    let b = if armed { egui::Button::new(RichText::new(confirm).color(Color32::BLACK)).fill(th.ui.warning) } else { egui::Button::new(label) };
    let r = ui.add(b);
    if armed {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(400));
    }
    if r.clicked() {
        if armed {
            ui.data_mut(|d| d.remove::<f64>(id));
            return true;
        }
        ui.data_mut(|d| d.insert_temp(id, now));
    }
    false
}

/// What the user did to a colour row.
pub enum ColorEdit {
    Set(String),
    Reset,
}

/// Swatch (opens egui's colour picker) + hex field + reset button. `current` is the effective `#rrggbb`.
pub fn color_edit(ui: &mut Ui, th: &Theme, id: Id, current: &str, can_reset: bool, errors: &mut Vec<String>) -> Option<ColorEdit> {
    let mut out = None;
    let mut c = crate::theme::parse_color(current);
    let before = c;
    ui.spacing_mut().item_spacing.x = 6.0;
    if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() && c != before {
        out = Some(ColorEdit::Set(color_to_hex(c)));
    }
    let typed: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let bad = ui.memory(|m| m.has_focus(id)) && hex_input(&typed).is_none();
    let color = bad.then_some(th.ui.error);
    if let Some(buf) = text_edit(ui, id, current, 78.0, "#rrggbb", color) {
        match hex_input(&buf) {
            Some(h) if h != current => out = Some(ColorEdit::Set(h)),
            Some(_) => {}
            None => errors.push(format!("'{}' is not a colour. Use #rgb or #rrggbb.", buf.trim())),
        }
    }
    if ui.add_enabled(can_reset, egui::Button::new("↺").min_size(Vec2::new(24.0, 0.0))).on_hover_text("Back to the preset's colour").clicked() {
        out = Some(ColorEdit::Reset);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_builds_nested_objects_and_nulls() {
        let mut p = Patch::default();
        assert!(p.is_empty());
        p.set("terminal.fontSize", 15);
        p.set("terminal.bell.audible", true);
        p.set("theme.overrides.ui.accent", "#112233");
        p.reset("quake.hotkey");
        assert_eq!(p.into_value(), json!({"terminal": {"fontSize": 15, "bell": {"audible": true}}, "theme": {"overrides": {"ui": {"accent": "#112233"}}}, "quake": {"hotkey": null}}));
    }

    #[test]
    fn patch_goes_through_the_store_sanitiser() {
        let cur = Settings::default();
        let mut p = Patch::default();
        p.set("terminal.fontSize", 99);
        p.set("terminal.scrollback", 12_000);
        p.set("terminal.lineHeight", snap(1.0500000000000003, 0.05));
        let s = ut_core::apply_patch(&cur, p.into_value()).unwrap();
        assert_eq!((s.terminal.font_size, s.terminal.scrollback, s.terminal.line_height), (32.0, 12_000, 1.05));
    }

    #[test]
    fn snapping_removes_float_noise() {
        assert_eq!(snap(1.0500000000000003, 0.05), 1.05);
        assert_eq!(snap(0.7, 0.05), 0.7);
        assert_eq!(snap(13.4, 1.0), 13.0);
        assert_eq!(snap(1.234, 0.0), 1.234);
    }

    #[test]
    fn hex_input_rules() {
        assert_eq!(hex_input("#ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(hex_input("6BE5FF").as_deref(), Some("#6be5ff"));
        assert_eq!(hex_input("  #6be5ff ").as_deref(), Some("#6be5ff"));
        for bad in ["", "#", "#12", "#1234567", "red", "#ggg", "rgba(1,2,3,1)"] {
            assert_eq!(hex_input(bad), None, "{bad:?}");
        }
        assert_eq!(color_to_hex(Color32::from_rgb(0x6b, 0xe5, 0xff)), "#6be5ff");
    }

    #[test]
    fn form_reads_paths_from_the_document() {
        let th = crate::theme::Theme::from(&ut_core::effective(&Default::default()));
        let f = Form::new(&Settings::default(), &th);
        assert!(f.b("terminal.cursorBlink"));
        assert_eq!(f.f("terminal.fontSize"), 14.0);
        assert_eq!(f.s("terminal.cursorStyle"), "bar");
        assert_eq!(f.s("no.such.path"), "");
    }
}
