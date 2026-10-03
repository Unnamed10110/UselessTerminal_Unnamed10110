//! Detected shells (§9.1) for the sidebar's "Shells" section and the session editor's presets. Detection runs off the UI
//! thread and is only repeated when something needs the list and it is older than the cache TTL, or on "Refresh shells".

use super::menus::{Act, MenuKind};
use super::paint::{glyph, section_header, G};
use super::Sidebar;
use crate::core::Core;
use crate::panels::PanelCtx;
use crate::theme::parse_color;
use egui::{Context, FontId, Pos2, Rect, Sense, Ui, Vec2};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;
use ut_shell::{ShellCache, ShellProfile};

#[derive(Default)]
pub struct Shells {
    pub list: Vec<ShellProfile>,
    rx: Option<Receiver<Vec<ShellProfile>>>,
    loaded: Option<Instant>,
}

impl Shells {
    pub fn loaded(&self) -> bool {
        self.loaded.is_some()
    }

    /// Pick up a finished detection. Returns true when the list changed.
    fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(v) => {
                self.list = v;
                self.loaded = Some(Instant::now());
                self.rx = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                false
            }
        }
    }

    /// Start a background detection when the list was never loaded, or `wanted` and it is stale (or `force`).
    pub fn tick(&mut self, core: &Arc<Core>, ctx: &Context, wanted: bool, force: bool) {
        self.poll();
        if self.rx.is_some() {
            return;
        }
        let stale = self.loaded.is_none_or(|t| t.elapsed() > ShellCache::MAX_AGE);
        if !(force || self.loaded.is_none() || (wanted && stale)) {
            return;
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let (core, ctx) = (core.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(core.shells.get(force));
            ctx.request_repaint();
        });
    }

    pub fn find(&self, id: &str) -> Option<&ShellProfile> {
        self.list.iter().find(|p| p.id == id)
    }
}

const ROW_H: f32 = 24.0;

impl Sidebar {
    /// The "Shells" section: quick SSH connect plus one entry per detected shell (double-click opens a new tab).
    pub(super) fn ui_shells(&mut self, ui: &mut Ui, x: &mut PanelCtx) {
        let theme = x.theme;
        let th = &theme.ui;
        let (toggle, _) = section_header(ui, th, "sb-shells", "Shells", Some(self.shells.list.len()), self.st.shells_open, None);
        if toggle {
            self.st.shells_open = !self.st.shells_open;
            self.st.save(&x.core.writer);
        }
        if !self.st.shells_open {
            return;
        }
        let mut act = None;
        egui::ScrollArea::vertical().id_salt("sb-shells-list").max_height(120.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            // Quick connect is a command, not an item: one click.
            let w = ui.available_width();
            let (_, slot) = ui.allocate_space(Vec2::new(w, ROW_H));
            let r = Rect::from_min_max(Pos2::new(slot.left() + 8.0, slot.top()), Pos2::new(slot.right() - 8.0, slot.bottom()));
            let resp = ui.interact(r, ui.id().with("sb-quick"), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, 4.0, th.hover_bg);
            }
            glyph(ui.painter(), G::Bolt, Rect::from_center_size(Pos2::new(r.left() + 14.0, r.center().y), Vec2::splat(14.0)), th.highlight);
            ui.painter().text(Pos2::new(r.left() + 30.0, r.center().y), egui::Align2::LEFT_CENTER, "Quick SSH connect…", FontId::proportional(13.0), th.foreground);
            if resp.on_hover_text("Connect to user@host without saving a session").clicked() {
                act = Some(Act::QuickConnect);
            }
            for p in self.shells.list.clone() {
                let (_, slot) = ui.allocate_space(Vec2::new(w, ROW_H));
                if !ui.is_rect_visible(slot) {
                    continue;
                }
                let r = Rect::from_min_max(Pos2::new(slot.left() + 8.0, slot.top()), Pos2::new(slot.right() - 8.0, slot.bottom()));
                let resp = ui.interact(r, ui.id().with(("sb-shell", &p.id)), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 4.0, th.hover_bg);
                }
                let c = parse_color(&p.color);
                ui.painter().circle_filled(Pos2::new(r.left() + 9.0, r.center().y), 3.5, c);
                let icon = Rect::from_center_size(Pos2::new(r.left() + 28.0, r.center().y), Vec2::splat(16.0));
                match x.icons.get(ui.ctx(), &p.command).map(|t| t.id()) {
                    Some(t) => {
                        ui.painter().image(t, icon, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), egui::Color32::WHITE);
                    }
                    None => super::paint::console(ui.painter(), icon, th.icon),
                }
                let g = ui.painter().layout(p.name.clone(), FontId::proportional(13.0), th.foreground, (r.width() - 46.0).max(10.0));
                ui.painter().galley(Pos2::new(r.left() + 42.0, r.center().y - g.size().y / 2.0), g, th.foreground);
                if resp.double_clicked() {
                    act = Some(Act::OpenShell(p.id.clone()));
                } else if resp.secondary_clicked() {
                    self.menu = Some(super::menus::Menu { pos: resp.interact_pointer_pos().unwrap_or(r.center()), kind: MenuKind::Shell(p.id.clone()) });
                }
                resp.on_hover_text(format!("{}\nDouble-click to open a new tab", p.command));
            }
        });
        if let Some(a) = act {
            self.run(a, x);
        }
    }
}
