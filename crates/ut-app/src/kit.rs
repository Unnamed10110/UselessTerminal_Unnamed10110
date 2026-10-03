//! Small immediate-mode UI helpers shared by the app and its panels: modal dialogs, popup menus, toasts.
//!
//! Dialogs are plain values owned by whoever opened them — call `show()` every frame, act on the result.

use crate::theme::Theme;
use egui::{Align, Color32, Context, Id, Key, Layout, Order, Pos2, RichText, Ui, Vec2};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    Normal,
    Primary,
    Danger,
}

#[derive(Clone, Debug)]
pub struct Button {
    pub label: String,
    pub kind: ButtonKind,
}

impl Button {
    pub fn new(label: &str, kind: ButtonKind) -> Self {
        Self { label: label.into(), kind }
    }
}

/// Result of a dialog frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogResult {
    /// Index into the dialog's buttons (and the text typed, for prompts).
    Button(usize, String),
    /// Esc / click outside.
    Dismissed,
}

/// A modal dialog: title, message, optional text input / preformatted preview, buttons.
#[derive(Clone, Debug)]
pub struct Dialog {
    pub title: String,
    pub message: String,
    pub buttons: Vec<Button>,
    /// Which button Enter activates.
    pub default: usize,
    /// `Some` = show a text field (prompt); the value is returned with the result.
    pub input: Option<String>,
    pub multiline: bool,
    pub allow_empty: bool,
    pub label: String,
    pub preview: Option<String>,
    pub width: f32,
    pub dismissible: bool,
    focus_pending: bool,
}

impl Dialog {
    pub fn new(title: &str, message: &str, buttons: Vec<Button>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            default: buttons.len().saturating_sub(1),
            buttons,
            input: None,
            multiline: false,
            allow_empty: false,
            label: String::new(),
            preview: None,
            width: 420.0,
            dismissible: true,
            focus_pending: true,
        }
    }

    pub fn confirm(title: &str, message: &str, ok: &str, danger: bool) -> Self {
        let mut d = Dialog::new(title, message, vec![Button::new("Cancel", ButtonKind::Normal), Button::new(ok, if danger { ButtonKind::Danger } else { ButtonKind::Primary })]);
        d.default = if danger { 0 } else { 1 };
        d
    }

    pub fn prompt(title: &str, label: &str, value: &str, allow_empty: bool) -> Self {
        let mut d = Dialog::new(title, "", vec![Button::new("Cancel", ButtonKind::Normal), Button::new("OK", ButtonKind::Primary)]);
        d.input = Some(value.into());
        d.label = label.into();
        d.allow_empty = allow_empty;
        d.width = 360.0;
        d
    }

    pub fn message(title: &str, message: &str) -> Self {
        Dialog::new(title, message, vec![Button::new("OK", ButtonKind::Primary)])
    }

    /// Draw the dialog; `Some(result)` once the user decided.
    pub fn show(&mut self, ctx: &Context, theme: &Theme) -> Option<DialogResult> {
        let mut result = None;
        let modal = egui::Modal::new(Id::new(("dialog", &self.title))).show(ctx, |ui| {
            ui.set_width(self.width);
            ui.label(RichText::new(&self.title).strong().size(15.0));
            ui.add_space(6.0);
            if !self.message.is_empty() {
                ui.label(&self.message);
                ui.add_space(6.0);
            }
            if let Some(p) = &self.preview {
                egui::Frame::new().fill(theme.ui.input_bg).stroke(egui::Stroke::new(1.0, theme.ui.card_border)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| ui.add(egui::Label::new(RichText::new(p).monospace().size(12.0)).selectable(true)));
                });
                ui.add_space(6.0);
            }
            if let Some(text) = &mut self.input {
                if !self.label.is_empty() {
                    ui.label(RichText::new(&self.label).small().color(theme.ui.muted));
                }
                let r = if self.multiline {
                    ui.add(egui::TextEdit::multiline(text).desired_rows(4).desired_width(f32::INFINITY))
                } else {
                    ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY))
                };
                if self.focus_pending {
                    r.request_focus();
                    // select all on open (§7.12)
                    if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()))));
                        st.store(ui.ctx(), r.id);
                    }
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && (!self.multiline || ui.input(|i| i.modifiers.ctrl)) {
                    let i = self.buttons.len().saturating_sub(1);
                    if self.allow_empty || !text.trim().is_empty() {
                        result = Some(DialogResult::Button(i, text.trim().to_string()));
                    }
                }
            }
            self.focus_pending = false;
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for (i, b) in self.buttons.iter().enumerate().rev() {
                    let mut btn = egui::Button::new(RichText::new(&b.label).color(match b.kind {
                        ButtonKind::Primary => theme.ui.tab_selected_fg,
                        ButtonKind::Danger => Color32::BLACK,
                        ButtonKind::Normal => theme.ui.foreground,
                    }));
                    btn = match b.kind {
                        ButtonKind::Primary => btn.fill(theme.ui.accent),
                        ButtonKind::Danger => btn.fill(theme.ui.warning),
                        ButtonKind::Normal => btn,
                    };
                    let resp = ui.add_enabled(self.input.as_ref().is_none_or(|t| i == 0 || self.allow_empty || !t.trim().is_empty()), btn.min_size(Vec2::new(70.0, 24.0)));
                    if resp.clicked() {
                        result = Some(DialogResult::Button(i, self.input.clone().unwrap_or_default().trim().to_string()));
                    }
                    if self.input.is_none() && i == self.default && self.focus_pending_default() {
                        resp.request_focus();
                    }
                }
            });
        });
        if result.is_none() && self.dismissible && (modal.should_close()) {
            result = Some(DialogResult::Dismissed);
        }
        result
    }

    fn focus_pending_default(&self) -> bool {
        true
    }
}

// ----------------------------------------------------------------------------------------- toasts

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub until: Instant,
}

#[derive(Default)]
pub struct Toasts(pub Vec<Toast>);

impl Toasts {
    pub fn push(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.0.push(Toast { text: text.into(), kind, until: Instant::now() + Duration::from_millis(match kind { ToastKind::Error => 6000, _ => 3500 }) });
    }

    pub fn show(&mut self, ctx: &Context, theme: &Theme) {
        self.show_in(ctx, theme, 0.0);
    }

    /// `right_inset`: width of a panel docked on the right edge. The browser's child window paints above egui, so the
    /// toasts are placed left of it instead of underneath.
    pub fn show_in(&mut self, ctx: &Context, theme: &Theme, right_inset: f32) {
        let now = Instant::now();
        self.0.retain(|t| t.until > now);
        if self.0.is_empty() {
            return;
        }
        let screen = ctx.content_rect();
        let mut y = screen.bottom() - 36.0;
        for t in self.0.iter().rev() {
            let color = match t.kind {
                ToastKind::Info => theme.ui.accent,
                ToastKind::Success => theme.ui.success,
                ToastKind::Warning => theme.ui.warning,
                ToastKind::Error => theme.ui.error,
            };
            let resp = egui::Area::new(Id::new(("toast", &t.text, t.until.elapsed().as_nanos() / 1_000_000_000)))
                .order(Order::Tooltip)
                .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-14.0 - right_inset, -(screen.bottom() - y)))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.0, color)).show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.label(&t.text);
                    });
                });
            y -= resp.response.rect.height() + 6.0;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

// ----------------------------------------------------------------------------------------- menus

/// One entry for [`menu_item`].
pub fn menu_item(ui: &mut Ui, label: &str, hint: &str, enabled: bool, checked: bool) -> bool {
    ui.add_enabled_ui(enabled, |ui| {
        let r = ui.horizontal(|ui| {
            ui.set_min_width(190.0);
            ui.label(RichText::new(if checked { "✓" } else { " " }).color(ui.visuals().hyperlink_color));
            let resp = ui.add(egui::Button::new(label).frame(false).min_size(Vec2::new(0.0, 0.0)));
            if !hint.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(hint).small().weak()));
            }
            resp
        });
        r.inner.clicked()
    })
    .inner
}

/// Popup menu at a screen position; returns `true` while it should stay open.
pub fn popup_menu(ctx: &Context, id: Id, pos: Pos2, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    let mut keep = true;
    let area = egui::Area::new(id).order(Order::Foreground).fixed_pos(pos).constrain(true).show(ctx, |ui| {
        egui::Frame::menu(ui.style()).show(ui, |ui| {
            if add(ui) {
                keep = false;
            }
        });
    });
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        keep = false;
    }
    // click anywhere outside closes it
    if ctx.input(|i| i.pointer.any_pressed()) {
        if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
            if !area.response.rect.contains(p) {
                keep = false;
            }
        }
    }
    keep
}
