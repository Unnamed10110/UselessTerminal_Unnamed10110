//! The settings window (§14.3): a sidebar of pages, every §14.2 setting reachable, changes applied live.
//!
//! Settings are only ever changed through `core.settings.patch(..)` (one merge patch per frame, built by the
//! controls in [`form`]); the app notices the change next frame and re-applies theme, fonts and panes. Keybindings go
//! through the `Keybindings` model and `Cmd::ReloadKeymap`. "Revert changes" restores what was in place when the window
//! opened; there is no Save button because everything is saved (debounced) as it changes.

mod capture;
mod fontpick;
mod form;
mod keys;
mod sections;
mod theme_ui;

use crate::panels::{Cmd, PanelCtx};
use crate::kit::ToastKind;
use capture::Recorder;
use egui::{Align, Align2, Context, Id, Key, Layout, RichText, Sense, Ui, Vec2};
use form::{armed_button, Form};
use keys::KeysUi;
use theme_ui::ThemeUi;
use ut_core::{Keybindings, Settings};
use ut_shell::ShellProfile;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Page {
    #[default]
    Terminal,
    Theme,
    Interface,
    Background,
    Behavior,
    Shells,
    Window,
    Network,
    Logging,
    Keys,
    About,
}

const PAGES: [(Page, &str); 11] = [
    (Page::Terminal, "Terminal"),
    (Page::Theme, "Theme & colours"),
    (Page::Interface, "Interface"),
    (Page::Background, "Background & effects"),
    (Page::Behavior, "Behavior"),
    (Page::Shells, "Shells & startup"),
    (Page::Window, "Window & Quake"),
    (Page::Network, "SSH & drops"),
    (Page::Logging, "Logging"),
    (Page::Keys, "Keybindings"),
    (Page::About, "About"),
];

const NAV_W: f32 = 176.0;

#[derive(Default)]
pub struct SettingsUi {
    pub open: bool,
    was_open: bool,
    page: Page,
    /// Settings and keymap as they were when the window opened ("Revert changes").
    snapshot: Option<(Settings, Keybindings)>,
    shells: Vec<ShellProfile>,
    theme: ThemeUi,
    keys: KeysUi,
    quake: Recorder,
}

impl SettingsUi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    fn on_open(&mut self, x: &mut PanelCtx) {
        self.was_open = true;
        self.snapshot = Some((x.core.cfg(), x.core.keys.lock().clone()));
        self.shells = x.core.shells.get(false);
    }

    fn on_close(&mut self, x: &mut PanelCtx) {
        self.was_open = false;
        self.snapshot = None;
        self.theme.stop_preview(x);
        self.keys.reset_transient();
        self.quake.stop();
        x.cmds.push(Cmd::FocusTerminal);
    }

    pub fn show(&mut self, ctx: &Context, x: &mut PanelCtx) {
        if !self.open {
            if self.was_open {
                self.on_close(x);
            }
            return;
        }
        if !self.was_open {
            self.on_open(x);
        }
        let cfg = x.core.cfg();
        let mut form = Form::new(&cfg, x.theme);
        // Esc closes the window, unless it is needed by a text field, a popup or the shortcut recorder.
        let esc_closes = !(self.keys.is_capturing() || self.quake.active || ctx.text_edit_focused() || egui::Popup::is_any_open(ctx));
        let screen = ctx.content_rect();
        let size = Vec2::new((screen.width() - 60.0).min(1000.0).max(360.0), (screen.height() - 70.0).min(740.0).max(300.0));
        let mut open = true;
        // A new UI scale is a new screen size in points: start from the default placement again instead of keeping
        // the position/size the window had at the old scale.
        let zoom = (ctx.zoom_factor() * 100.0).round() as i32;
        egui::Window::new("Settings")
            .id(Id::new(("settings-window", zoom)))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(size)
            .min_size([360.0, 280.0])
            .pivot(Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .constrain_to(screen)
            .show(ctx, |ui| self.body(ui, x, &mut form, &cfg));
        if esc_closes && ctx.input(|i| i.key_pressed(Key::Escape)) {
            open = false;
        }

        let Form { patch, errors, .. } = form;
        if !patch.is_empty() {
            if let Err(e) = x.core.settings.patch(patch.into_value()) {
                x.toasts.push(e, ToastKind::Error);
            }
        }
        for e in errors {
            x.toasts.push(e, ToastKind::Warning);
        }
        self.theme.finish_frame(x);
        if !open {
            self.open = false;
            self.on_close(x);
        }
    }

    fn nav(&mut self, ui: &mut Ui, x: &PanelCtx) {
        let th = x.theme;
        ui.spacing_mut().item_spacing.y = 2.0;
        for (page, label) in PAGES {
            let sel = self.page == page;
            let (rect, r) = ui.allocate_exact_size(Vec2::new(NAV_W - 14.0, 28.0), Sense::click());
            let p = ui.painter();
            if sel || r.hovered() {
                p.rect_filled(rect, 4.0, if sel { th.ui.folder_selected } else { th.ui.hover_bg });
            }
            if sel {
                p.rect_filled(egui::Rect::from_min_size(rect.min + Vec2::new(0.0, 5.0), Vec2::new(3.0, rect.height() - 10.0)), 1.5, th.ui.accent);
            }
            p.text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, label, egui::FontId::proportional(13.5), if sel { th.ui.accent } else { th.ui.foreground });
            if r.clicked() && !sel {
                self.keys.reset_transient();
                self.quake.stop();
                self.page = page;
            }
        }
    }

    fn body(&mut self, ui: &mut Ui, x: &mut PanelCtx, form: &mut Form, cfg: &Settings) {
        let avail = ui.available_size();
        let footer_h = 42.0;
        let body_h = (avail.y - footer_h).max(140.0);
        ui.allocate_ui_with_layout(Vec2::new(avail.x, body_h), Layout::left_to_right(Align::TOP), |ui| {
            ui.allocate_ui_with_layout(Vec2::new(NAV_W, body_h), Layout::top_down(Align::LEFT), |ui| self.nav(ui, x));
            ui.separator();
            let w = ui.available_width();
            ui.allocate_ui_with_layout(Vec2::new(w, body_h), Layout::top_down(Align::LEFT), |ui| {
                egui::ScrollArea::vertical().id_salt(("settings-page", self.page as u8)).auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_max_width(w - 14.0);
                    self.page_ui(ui, x, form, cfg);
                    ui.add_space(24.0);
                });
            });
        });
        ui.separator();
        self.footer(ui, x, form);
    }

    fn page_ui(&mut self, ui: &mut Ui, x: &mut PanelCtx, f: &mut Form, cfg: &Settings) {
        match self.page {
            Page::Terminal => sections::terminal(ui, x, f, cfg),
            Page::Theme => self.theme.page(ui, x, f, cfg),
            Page::Interface => sections::interface(ui, x, f, cfg),
            Page::Background => sections::background(ui, x, f, cfg),
            Page::Behavior => sections::behavior(ui, x, f, cfg),
            Page::Shells => sections::shells(ui, x, f, cfg, &mut self.shells),
            Page::Window => sections::window(ui, x, f, cfg, &mut self.quake),
            Page::Network => sections::network(ui, x, f, cfg),
            Page::Logging => sections::logging(ui, x, f, cfg),
            Page::Keys => self.keys.ui(ui, x, &cfg.quake.hotkey),
            Page::About => sections::about(ui, x, f, cfg),
        }
    }

    fn footer(&mut self, ui: &mut Ui, x: &mut PanelCtx, form: &mut Form) {
        let th = x.theme;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Changes apply immediately and are saved automatically.").small().color(th.ui.muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("Close").color(th.ui.tab_selected_fg)).fill(th.ui.accent).min_size(Vec2::new(80.0, 26.0))).clicked() {
                    self.open = false;
                }
                if ui.button("Revert changes").on_hover_text("Back to how everything was when this window opened").clicked() {
                    self.revert(x);
                }
                if armed_button(ui, th, Id::new("settings-reset-all"), "Reset all settings", "Click again to reset everything") {
                    let patch = sections::reset_patch(&form.doc);
                    if let Err(e) = x.core.settings.patch(patch) {
                        x.toasts.push(e, ToastKind::Error);
                    }
                    x.toasts.push("All settings were reset to their defaults.", ToastKind::Info);
                }
            });
        });
    }

    /// Restore the settings and keymap captured when the window opened.
    fn revert(&mut self, x: &mut PanelCtx) {
        let Some((settings, keymap)) = self.snapshot.clone() else { return };
        let cur = x.core.cfg();
        let doc = serde_json::to_value(&cur).unwrap_or_default();
        let _ = x.core.settings.patch(sections::reset_patch(&doc));
        let _ = x.core.settings.patch(serde_json::to_value(&settings).unwrap_or_default());
        let changed = *x.core.keys.lock() != keymap;
        if changed {
            let mut k = x.core.keys.lock();
            *k = keymap;
            if let Err(e) = k.save(&x.core.keys_path) {
                x.toasts.push(format!("Could not save keybindings.json: {e}"), ToastKind::Error);
            }
            x.cmds.push(Cmd::ReloadKeymap);
        }
        x.toasts.push("Changes reverted.", ToastKind::Info);
    }
}
