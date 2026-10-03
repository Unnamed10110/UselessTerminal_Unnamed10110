//! Window chrome and frame composition: title bar, tab strip, status bar, banners, menus, resize zones.

use crate::app::{App, BannerAction, DialogId, MenuKind};
use crate::kit::{menu_item, popup_menu};
use crate::panels::{Cmd, PanelCtx};
use crate::tab::TabOut;
use egui::{Align2, Color32, Context, CursorIcon, FontId, Frame, Id, Margin, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, ViewportCommand};
use std::time::Duration;

pub const TAB_COLORS: [&str; 8] = ["#00ff44", "#ff003c", "#ffff00", "#00e5ff", "#ff00ff", "#ff8800", "#ffffff", "#888888"];

/// Mono icons drawn with the painter (no icon font dependency).
#[derive(Clone, Copy)]
pub enum Glyph {
    Sidebar,
    Globe,
    Gear,
    Plus,
    Chevron,
    Close,
    Minimize,
    Maximize,
    Restore,
    Pin,
    Lock,
    Console,
    Branch,
}

pub fn draw_glyph(p: &egui::Painter, g: Glyph, r: Rect, c: Color32) {
    let s = Stroke::new(1.3, c);
    let (cx, cy) = (r.center().x, r.center().y);
    let k = r.width().min(r.height()) / 2.0 - 1.0;
    match g {
        Glyph::Sidebar => {
            p.rect_stroke(Rect::from_center_size(r.center(), Vec2::splat(k * 1.7)), 1.5, s, egui::StrokeKind::Middle);
            p.line_segment([Pos2::new(cx - k * 0.35, cy - k * 0.85), Pos2::new(cx - k * 0.35, cy + k * 0.85)], s);
        }
        Glyph::Globe => {
            p.circle_stroke(r.center(), k * 0.9, s);
            p.line_segment([Pos2::new(cx - k * 0.9, cy), Pos2::new(cx + k * 0.9, cy)], s);
            p.line_segment([Pos2::new(cx, cy - k * 0.9), Pos2::new(cx, cy + k * 0.9)], s);
            p.circle_stroke(r.center(), k * 0.4, Stroke::new(1.0, c));
        }
        Glyph::Gear => {
            p.circle_stroke(r.center(), k * 0.45, s);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let (sn, cs) = a.sin_cos();
                p.line_segment([Pos2::new(cx + cs * k * 0.7, cy + sn * k * 0.7), Pos2::new(cx + cs * k * 1.0, cy + sn * k * 1.0)], Stroke::new(1.8, c));
            }
        }
        Glyph::Plus => {
            p.line_segment([Pos2::new(cx - k * 0.7, cy), Pos2::new(cx + k * 0.7, cy)], s);
            p.line_segment([Pos2::new(cx, cy - k * 0.7), Pos2::new(cx, cy + k * 0.7)], s);
        }
        Glyph::Chevron => {
            p.add(egui::Shape::line(vec![Pos2::new(cx - k * 0.6, cy - k * 0.25), Pos2::new(cx, cy + k * 0.35), Pos2::new(cx + k * 0.6, cy - k * 0.25)], s));
        }
        Glyph::Close => {
            p.line_segment([Pos2::new(cx - k * 0.55, cy - k * 0.55), Pos2::new(cx + k * 0.55, cy + k * 0.55)], s);
            p.line_segment([Pos2::new(cx - k * 0.55, cy + k * 0.55), Pos2::new(cx + k * 0.55, cy - k * 0.55)], s);
        }
        Glyph::Minimize => {
            p.line_segment([Pos2::new(cx - k * 0.7, cy), Pos2::new(cx + k * 0.7, cy)], s);
        }
        Glyph::Maximize => {
            p.rect_stroke(Rect::from_center_size(r.center(), Vec2::splat(k * 1.3)), 0.0, s, egui::StrokeKind::Middle);
        }
        Glyph::Restore => {
            p.rect_stroke(Rect::from_center_size(Pos2::new(cx - 1.0, cy + 1.0), Vec2::splat(k * 1.1)), 0.0, s, egui::StrokeKind::Middle);
            p.line_segment([Pos2::new(cx - k * 0.1, cy - k * 0.8), Pos2::new(cx + k * 0.8, cy - k * 0.8)], s);
            p.line_segment([Pos2::new(cx + k * 0.8, cy - k * 0.8), Pos2::new(cx + k * 0.8, cy + k * 0.1)], s);
        }
        Glyph::Pin => {
            p.circle_filled(Pos2::new(cx, cy - k * 0.3), k * 0.45, c);
            p.line_segment([Pos2::new(cx, cy + k * 0.1), Pos2::new(cx, cy + k * 0.9)], s);
        }
        Glyph::Lock => {
            p.rect_filled(Rect::from_center_size(Pos2::new(cx, cy + k * 0.25), Vec2::new(k * 1.2, k * 0.9)), 1.0, c);
            p.add(egui::Shape::line(vec![Pos2::new(cx - k * 0.35, cy - k * 0.2), Pos2::new(cx - k * 0.35, cy - k * 0.6), Pos2::new(cx + k * 0.35, cy - k * 0.6), Pos2::new(cx + k * 0.35, cy - k * 0.2)], s));
        }
        Glyph::Console => {
            p.rect_stroke(Rect::from_center_size(r.center(), Vec2::new(k * 1.8, k * 1.4)), 2.0, s, egui::StrokeKind::Middle);
            p.add(egui::Shape::line(vec![Pos2::new(cx - k * 0.6, cy - k * 0.3), Pos2::new(cx - k * 0.1, cy), Pos2::new(cx - k * 0.6, cy + k * 0.3)], s));
            p.line_segment([Pos2::new(cx + k * 0.05, cy + k * 0.35), Pos2::new(cx + k * 0.6, cy + k * 0.35)], s);
        }
        Glyph::Branch => {
            p.circle_stroke(Pos2::new(cx - k * 0.5, cy - k * 0.6), k * 0.25, s);
            p.circle_stroke(Pos2::new(cx - k * 0.5, cy + k * 0.6), k * 0.25, s);
            p.circle_stroke(Pos2::new(cx + k * 0.5, cy - k * 0.1), k * 0.25, s);
            p.line_segment([Pos2::new(cx - k * 0.5, cy - k * 0.35), Pos2::new(cx - k * 0.5, cy + k * 0.35)], s);
            p.add(egui::Shape::line(vec![Pos2::new(cx + k * 0.5, cy + k * 0.15), Pos2::new(cx + k * 0.5, cy + k * 0.3), Pos2::new(cx - k * 0.5, cy + k * 0.3)], s));
        }
    }
}

fn icon_button(ui: &mut Ui, g: Glyph, size: Vec2, hover: Color32, fg: Color32, tip: &str) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 0.0, hover);
    }
    draw_glyph(ui.painter(), g, Rect::from_center_size(r.center(), Vec2::splat(16.0)), fg);
    resp.on_hover_text(tip)
}

impl App {
    /// One frame: panels, then overlays, then queued commands.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.begin_frame(&ctx);
        let mut cmds: Vec<Cmd> = Vec::new();
        let live = self.live_sessions();
        let tabinfos = self.tab_infos();
        let theme_bg = self.theme.ui.chrome_bg;

        // ----- title bar
        egui::Panel::top("titlebar").exact_size(30.0).frame(Frame::new().fill(theme_bg).stroke(Stroke::new(1.0, self.theme.ui.card_border))).show_inside(ui, |ui| self.ui_titlebar(ui, &ctx));
        // ----- status bar
        egui::Panel::bottom("status").exact_size(22.0).frame(Frame::new().fill(self.theme.ui.status_bg).stroke(Stroke::new(1.0, self.theme.ui.card_border))).show_inside(ui, |ui| self.ui_statusbar(ui));
        // ----- sessions sidebar
        if self.sidebar_open {
            let r = egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(self.sidebar_width)
                .size_range(160.0..=900.0)
                .frame(Frame::new().fill(theme_bg).stroke(Stroke::new(1.0, self.theme.ui.card_border)))
                .show_inside(ui, |ui| {
                    let mut x = PanelCtx { core: &self.core, theme: &self.theme, fonts: &self.fonts, live_sessions: &live, tabs: &tabinfos, icons: &mut self.icons, cmds: &mut cmds, toasts: &mut self.toasts, elevated: self.core.elevated };
                    self.sidebar.ui(ui, &mut x);
                });
            let w = r.response.rect.width();
            if (w - self.sidebar_width).abs() > 0.5 {
                self.sidebar_width = w.clamp(160.0, 900.0);
                self.mark_dirty();
            }
        }
        // ----- browser panel (right)
        self.show_browser(ui);
        // ----- tabs + panes
        let mut tab_outs: Vec<(u64, Vec<TabOut>)> = Vec::new();
        egui::CentralPanel::default().frame(Frame::new().fill(self.theme.term_bg)).show_inside(ui, |ui| {
            self.ui_tabstrip(ui, &ctx);
            let area = ui.available_rect_before_wrap();
            self.pane_area = area;
            let active = self.active;
            let (bg_started, focused_win) = (self.bg_started, self.window_focused);
            let mut tabs = std::mem::take(&mut self.tabs);
            for (i, t) in tabs.iter_mut().enumerate() {
                let mut out = Vec::new();
                {
                    let mut env = self.pane_env(&mut out, i == active);
                    if i == active {
                        t.show(ui, area, &mut env, focused_win);
                    } else {
                        t.background_tick(ui.ctx(), area, &mut env, bg_started);
                    }
                }
                if !out.is_empty() {
                    tab_outs.push((t.id, out));
                }
            }
            self.tabs = tabs;
        });
        for (tid, outs) in tab_outs {
            self.handle_outs(tid, outs);
        }
        // ----- overlays
        self.resize_zones(&ctx);
        self.show_banners(&ctx);
        self.show_menu(&ctx, &mut cmds);
        {
            let mut x = PanelCtx { core: &self.core, theme: &self.theme, fonts: &self.fonts, live_sessions: &live, tabs: &tabinfos, icons: &mut self.icons, cmds: &mut cmds, toasts: &mut self.toasts, elevated: self.core.elevated };
            self.sidebar.show_windows(&ctx, &mut x);
            let mut pal = std::mem::take(&mut self.palette);
            pal.show(&ctx, &mut x);
            self.palette = pal;
            let mut st = std::mem::take(&mut self.settings_ui);
            st.show(&ctx, &mut x);
            self.settings_ui = st;
        }
        self.show_dialog(&ctx);
        self.show_diagnostics(&ctx);
        self.toasts.show_in(&ctx, &self.theme, self.browser.inset());
        self.panel_ctx_cmds(cmds);
        self.browser_sync(&ctx);
        self.end_frame(&ctx);
    }

    // ------------------------------------------------------------------------------ title bar

    fn ui_titlebar(&mut self, ui: &mut Ui, ctx: &Context) {
        let rect = ui.max_rect();
        let ctrl_w = 46.0 * 3.0;
        let drag_rect = Rect::from_min_max(rect.min, Pos2::new(rect.right() - ctrl_w, rect.bottom()));
        let resp = ui.interact(drag_rect, Id::new("titlebar-drag"), Sense::click_and_drag());
        if resp.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if resp.double_clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!self.maximized));
        }
        let p = ui.painter();
        // logo ring + name
        p.circle_stroke(Pos2::new(rect.left() + 17.0, rect.center().y), 6.0, Stroke::new(2.0, self.theme.ui.accent));
        p.text(Pos2::new(rect.left() + 32.0, rect.center().y), Align2::LEFT_CENTER, "Useless Terminal", FontId::proportional(11.0), self.theme.ui.muted);
        // centre: cwd of the active pane
        let cwd = self.active_tab().and_then(|t| t.focused_pane()).and_then(|p| p.cwd.clone().or_else(|| p.start_cwd.clone()));
        let base = if self.core.elevated { "Useless Terminal — Administrator" } else { "Useless Terminal" };
        let title = match &cwd {
            Some(c) => format!("{base}  —  {c}"),
            None => base.to_string(),
        };
        if let Some(c) = &cwd {
            let maxw = (rect.width() - ctrl_w - 360.0).max(100.0);
            let g = p.layout(c.clone(), FontId::proportional(11.0), self.theme.ui.muted, maxw);
            p.galley(Pos2::new(rect.center().x - g.size().x / 2.0, rect.center().y - g.size().y / 2.0), g, self.theme.ui.muted);
        }
        if self.last_title != title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        // window controls
        let cr = Rect::from_min_max(Pos2::new(rect.right() - ctrl_w, rect.top()), rect.right_bottom());
        let mut x = cr.left();
        let btn = |ui: &mut Ui, x: &mut f32, g: Glyph, close: bool, theme: &crate::theme::Theme, tip: &str| {
            let r = Rect::from_min_size(Pos2::new(*x, cr.top()), Vec2::new(46.0, cr.height()));
            *x += 46.0;
            let resp = ui.interact(r, Id::new(("tb", tip)), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, if close { Color32::from_rgb(196, 43, 28) } else { theme.ui.hover_bg });
            }
            draw_glyph(ui.painter(), g, Rect::from_center_size(r.center(), Vec2::splat(16.0)), if close && resp.hovered() { Color32::WHITE } else { theme.ui.foreground });
            resp.on_hover_text(tip)
        };
        if btn(ui, &mut x, Glyph::Minimize, false, &self.theme, "Minimize").clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        if btn(ui, &mut x, if self.maximized { Glyph::Restore } else { Glyph::Maximize }, false, &self.theme, if self.maximized { "Restore" } else { "Maximize" }).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!self.maximized));
        }
        if btn(ui, &mut x, Glyph::Close, true, &self.theme, "Close").clicked() {
            self.request_close();
        }
    }

    /// The frameless window must stay resizable at every edge and corner (§7.1, §23.25).
    fn resize_zones(&mut self, ctx: &Context) {
        if self.maximized {
            return;
        }
        use egui::viewport::ResizeDirection as D;
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let s = ctx.content_rect();
        let m = 5.0;
        let (l, r, t, b) = (p.x - s.left() < m, s.right() - p.x < m, p.y - s.top() < m, s.bottom() - p.y < m);
        let dir = match (l, r, t, b) {
            (true, _, true, _) => Some((D::NorthWest, CursorIcon::ResizeNwSe)),
            (_, true, true, _) => Some((D::NorthEast, CursorIcon::ResizeNeSw)),
            (true, _, _, true) => Some((D::SouthWest, CursorIcon::ResizeNeSw)),
            (_, true, _, true) => Some((D::SouthEast, CursorIcon::ResizeNwSe)),
            (true, ..) => Some((D::West, CursorIcon::ResizeHorizontal)),
            (_, true, ..) => Some((D::East, CursorIcon::ResizeHorizontal)),
            (_, _, true, _) => Some((D::North, CursorIcon::ResizeVertical)),
            (.., true) => Some((D::South, CursorIcon::ResizeVertical)),
            _ => None,
        };
        if let Some((d, icon)) = dir {
            ctx.set_cursor_icon(icon);
            if ctx.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(d));
            }
        }
    }

    // ------------------------------------------------------------------------------ tab strip

    fn ui_tabstrip(&mut self, ui: &mut Ui, ctx: &Context) {
        let th = self.theme.ui.clone();
        let th = &th;
        let (accent, fg, hover) = (th.accent, th.tab_fg, th.hover_bg);
        let full = Rect::from_min_size(ui.available_rect_before_wrap().min, Vec2::new(ui.available_width(), 34.0));
        ui.allocate_rect(full, Sense::hover());
        let p = ui.painter();
        p.rect_filled(full, 0.0, th.chrome_bg);
        p.rect_filled(Rect::from_min_max(Pos2::new(full.left(), full.bottom() - 2.0), full.right_bottom()), 0.0, accent);

        let left_w = 32.0;
        let right_w = 4.0 * 32.0;
        let tabs_rect = Rect::from_min_max(Pos2::new(full.left() + left_w, full.top()), Pos2::new(full.right() - right_w, full.bottom() - 2.0));
        // left: sessions toggle
        ui.scope_builder(UiBuilder::new().max_rect(Rect::from_min_size(full.min, Vec2::new(left_w, 32.0))), |ui| {
            if icon_button(ui, Glyph::Sidebar, Vec2::new(left_w, 32.0), hover, th.icon, "Toggle Sessions (Ctrl+B)").clicked() {
                self.run_action("togglePanel");
            }
        });
        // right: browser, settings, new tab, shells
        let mut x = full.right() - right_w;
        let mut open_shells = None;
        ui.scope_builder(UiBuilder::new().max_rect(Rect::from_min_size(Pos2::new(x, full.top()), Vec2::new(right_w, 32.0))), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                if icon_button(ui, Glyph::Globe, Vec2::new(32.0, 32.0), hover, th.icon, "Toggle Browser (Ctrl+Shift+B)").clicked() {
                    self.run_action("toggleBrowser");
                }
                if icon_button(ui, Glyph::Gear, Vec2::new(32.0, 32.0), hover, th.icon, "Settings (Ctrl+,)").clicked() {
                    self.run_action("settings");
                }
                if icon_button(ui, Glyph::Plus, Vec2::new(32.0, 32.0), hover, th.icon, "New Tab (Ctrl+T)").clicked() {
                    self.run_action("newTab");
                }
                let r = icon_button(ui, Glyph::Chevron, Vec2::new(32.0, 32.0), hover, th.icon, "Shells");
                if r.clicked() {
                    open_shells = Some(r.rect.left_bottom());
                }
            });
        });
        x += 0.0;
        let _ = x;
        if let Some(pos) = open_shells {
            self.menu = Some(crate::app::MenuState { pos, kind: MenuKind::Shells });
        }

        // tabs
        let mut select = None;
        let mut close = None;
        let mut menu = None;
        let mut reorder: Option<(usize, usize)> = None;
        let mut new_rects: Vec<(u64, Rect)> = Vec::new();
        ui.scope_builder(UiBuilder::new().max_rect(tabs_rect), |ui| {
            egui::ScrollArea::horizontal().id_salt("tabstrip").scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let n = self.tabs.len();
                    for i in 0..n {
                        let (tid, selected) = (self.tabs[i].id, i == self.active);
                        let title = format!("{}  {}", i + 1, self.tabs[i].title);
                        let pinned = self.tabs[i].pinned;
                        let font = FontId::proportional(self.cfg.ui.font_size as f32 - 2.0);
                        let tw = if pinned { 0.0 } else { ui.painter().layout_no_wrap(title.clone(), font.clone(), Color32::WHITE).size().x };
                        let group_w = if self.tabs[i].group.is_empty() { 0.0 } else { 40.0 };
                        let w = if pinned { 44.0 } else { (tw + 78.0 + group_w).clamp(110.0, 240.0) };
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(w, tabs_rect.height()), Sense::click_and_drag());
                        new_rects.push((tid, r));
                        let color = self.tabs[i].color;
                        let p = ui.painter();
                        if selected {
                            p.rect_filled(r, 0.0, th.accent_dim);
                            p.rect_filled(Rect::from_min_max(Pos2::new(r.left(), r.bottom() - 2.0), r.right_bottom()), 0.0, accent);
                        } else if resp.hovered() {
                            p.rect_filled(r, 0.0, hover);
                        }
                        if let Some(c) = color {
                            p.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0, if selected { c } else { c.gamma_multiply(0.5) }), egui::StrokeKind::Inside);
                        }
                        let mut cx = r.left() + 8.0;
                        if let Some(c) = color {
                            p.circle_filled(Pos2::new(cx + 3.5, r.center().y), 3.5, c);
                            cx += 12.0;
                        }
                        if pinned {
                            draw_glyph(p, Glyph::Pin, Rect::from_center_size(Pos2::new(cx + 6.0, r.center().y), Vec2::splat(12.0)), th.icon);
                            cx += 16.0;
                        }
                        // shell icon
                        let cmd = self.tabs[i].command.clone();
                        match self.icons.get(ctx, &cmd) {
                            Some(tex) => {
                                p.image(tex.id(), Rect::from_min_size(Pos2::new(cx, r.center().y - 8.0), Vec2::splat(16.0)), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                            }
                            None => draw_glyph(p, Glyph::Console, Rect::from_min_size(Pos2::new(cx, r.center().y - 8.0), Vec2::splat(16.0)), th.icon),
                        }
                        cx += 22.0;
                        if !self.tabs[i].group.is_empty() {
                            p.text(Pos2::new(cx, r.center().y), Align2::LEFT_CENTER, self.tabs[i].group.to_uppercase(), FontId::proportional(8.0), Color32::from_rgb(0x88, 0x8c, 0xf8));
                            cx += group_w;
                        }
                        let close_r = Rect::from_center_size(Pos2::new(r.right() - 14.0, r.center().y), Vec2::splat(18.0));
                        let mut right = r.right() - 28.0;
                        if self.tabs[i].read_only {
                            draw_glyph(p, Glyph::Lock, Rect::from_center_size(Pos2::new(right - 4.0, r.center().y), Vec2::splat(12.0)), Color32::from_rgb(255, 170, 0));
                            right -= 14.0;
                        }
                        if self.tabs[i].broadcast {
                            p.text(Pos2::new(right - 4.0, r.center().y), Align2::CENTER_CENTER, "⇉", FontId::proportional(12.0), th.warning);
                            right -= 14.0;
                        }
                        if self.tabs[i].logging {
                            p.circle_filled(Pos2::new(right - 3.0, r.center().y), 3.0, Color32::from_rgb(255, 60, 60));
                            right -= 10.0;
                        }
                        if self.tabs[i].activity && !selected {
                            p.circle_filled(Pos2::new(right - 4.0, r.center().y), 3.5, Color32::from_rgb(0, 255, 68));
                            right -= 12.0;
                        }
                        if !pinned {
                            let clip = Rect::from_min_max(Pos2::new(cx, r.top()), Pos2::new(right, r.bottom()));
                            p.with_clip_rect(clip).text(Pos2::new(cx, r.center().y), Align2::LEFT_CENTER, title, font, fg);
                        }
                        let close_hover = resp.hovered() && ui.input(|i| i.pointer.hover_pos()).is_some_and(|h| close_r.contains(h));
                        if resp.hovered() || selected {
                            if close_hover {
                                p.rect_filled(close_r, 3.0, th.error);
                            }
                            draw_glyph(p, Glyph::Close, Rect::from_center_size(close_r.center(), Vec2::splat(12.0)), fg);
                        }
                        if resp.clicked() {
                            if close_hover {
                                close = Some(i);
                            } else {
                                select = Some(i);
                            }
                        }
                        if resp.middle_clicked() {
                            close = Some(i);
                        }
                        if resp.secondary_clicked() {
                            select = Some(i);
                            if let Some(pos) = resp.interact_pointer_pos() {
                                menu = Some((tid, pos));
                            }
                        }
                        if resp.drag_started() {
                            self.tab_drag = Some(tid);
                            select = Some(i);
                        }
                        if resp.dragged() {
                            if let Some(px) = ui.input(|i| i.pointer.hover_pos()).map(|p| p.x) {
                                // drop index = first tab whose midpoint is right of the pointer
                                let to = self.tab_rects.iter().position(|(id, r)| *id != tid && r.center().x > px).map(|j| if j > i { j - 1 } else { j }).unwrap_or(n - 1);
                                if to != i {
                                    reorder = Some((i, to));
                                }
                            }
                        }
                        if selected && self.scroll_to_active {
                            ui.scroll_to_rect(r.expand2(Vec2::new(8.0, 0.0)), None);
                        }
                    }
                    self.scroll_to_active = false;
                });
            });
        });
        self.tab_rects = new_rects;
        if let Some(i) = select {
            self.activate(i);
        }
        if let Some((from, to)) = reorder {
            if self.tab_drag.is_some() {
                self.move_tab(from, to);
            }
        }
        if !ctx.input(|i| i.pointer.any_down()) {
            self.tab_drag = None;
        }
        if let Some(i) = close {
            self.close_tab(i, false);
        }
        if let Some((tid, pos)) = menu {
            self.menu = Some(crate::app::MenuState { pos, kind: MenuKind::Tab(tid) });
        }
    }

    // ------------------------------------------------------------------------------ status bar

    fn ui_statusbar(&mut self, ui: &mut Ui) {
        let th = self.theme.ui.clone();
        let th = &th;
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(p) = t.focused_pane() else { return };
        let small = FontId::proportional(self.cfg.ui.font_size as f32 - 2.0);
        ui.horizontal_centered(|ui| {
            ui.add_space(10.0);
            let sep = |ui: &mut Ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, 12.0), Sense::hover());
                ui.painter().rect_filled(r, 0.0, th.splitter);
            };
            let cmd = if p.command_line.is_empty() { &t.command } else { &p.command_line };
            let shell = ut_shell::file_stem_lower(&ut_shell::split_exe_and_args(cmd).0);
            let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            draw_glyph(ui.painter(), Glyph::Console, r, th.success);
            ui.label(egui::RichText::new(shell).font(small.clone()).color(th.success));
            sep(ui);
            if let Some(s) = &p.session {
                ui.label(egui::RichText::new(format!("PID {}", s.pid())).font(small.clone()));
                sep(ui);
            }
            let where_ = p.cwd.clone().or_else(|| p.start_cwd.clone()).unwrap_or_default();
            let where_ = if !p.cwd_local && !p.cwd_host.is_empty() { format!("{}:{}", p.cwd_host, where_) } else { where_ };
            ui.add(egui::Label::new(egui::RichText::new(&where_).font(small.clone())).truncate()).on_hover_text(&where_);
            if let Some(b) = &p.branch {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                draw_glyph(ui.painter(), Glyph::Branch, r, th.highlight);
                ui.label(egui::RichText::new(b).font(small.clone()).color(th.highlight));
            }
            sep(ui);
            // A bare OSC 133;D keeps whatever was shown before (§7.5): `last_exit` only changes on a real code.
            if let Some(code) = p.last_exit {
                let c = if code == 0 { Color32::from_rgb(0x22, 0xc5, 0x5e) } else { Color32::from_rgb(0xff, 0x00, 0x3c) };
                let d = p.last_duration_ms.filter(|d| *d >= 1000).map(|d| format!(" · {}", fmt_dur(d))).unwrap_or_default();
                ui.label(egui::RichText::new(format!("exit: {code}{d}")).font(small.clone()).color(c));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                let (txt, c) = if p.exited { ("○ exited", Color32::from_rgb(0x6b, 0x72, 0x80)) } else if p.started() { ("● running", Color32::from_rgb(0x22, 0xc5, 0x5e)) } else { ("", th.muted) };
                ui.label(egui::RichText::new(txt).font(small.clone()).color(c));
                if let Some(fg) = &p.foreground {
                    ui.label(egui::RichText::new(fg).font(small.clone()).color(th.muted));
                }
            });
        });
    }

    // ------------------------------------------------------------------------------ banners / menus

    fn show_banners(&mut self, ctx: &Context) {
        if self.banners.is_empty() {
            return;
        }
        let mut remove = None;
        let mut act = None;
        egui::Area::new(Id::new("banners")).order(egui::Order::Foreground).anchor(Align2::CENTER_TOP, Vec2::new(0.0, 38.0)).show(ctx, |ui| {
            ui.set_max_width(720.0);
            for (i, b) in self.banners.iter().enumerate() {
                let c = self.banner_kind_color(b.kind);
                Frame::popup(ui.style()).stroke(Stroke::new(1.0, c)).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(&b.text).wrap());
                        if let Some(a) = b.action {
                            let label = match a {
                                BannerAction::OpenSettingsFile => "Open file",
                                BannerAction::ResetSettings => "Reset (backup created)",
                            };
                            if ui.small_button(label).clicked() {
                                act = Some((i, a));
                            }
                        }
                        if ui.small_button("✕").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
        });
        if let Some((i, a)) = act {
            self.banners.remove(i.min(self.banners.len() - 1));
            self.show_banners_actions(a);
        } else if let Some(i) = remove {
            self.banners.remove(i);
        }
    }

    fn show_menu(&mut self, ctx: &Context, cmds: &mut Vec<Cmd>) {
        let Some(m) = self.menu.take() else { return };
        let pos = m.pos;
        let mut keep = true;
        let chosen = std::cell::RefCell::new(String::new());
        match &m.kind {
            MenuKind::Shells => {
                let shells = self.core.shells.get(false);
                keep = popup_menu(ctx, Id::new("menu-shells"), pos, |ui| {
                    let mut done = false;
                    for s in &shells {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(200.0, 22.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 3.0, self.theme.ui.accent_dim);
                        }
                        ui.painter().circle_filled(Pos2::new(r.left() + 8.0, r.center().y), 4.0, crate::theme::parse_color(&s.color));
                        ui.painter().text(Pos2::new(r.left() + 20.0, r.center().y), Align2::LEFT_CENTER, &s.name, FontId::proportional(13.0), self.theme.ui.foreground);
                        if resp.clicked() {
                            cmds.push(Cmd::OpenProfile(s.clone()));
                            done = true;
                        }
                    }
                    ui.separator();
                    if menu_item(ui, "Refresh shells", "", true, false) {
                        cmds.push(Cmd::RefreshShells);
                        done = true;
                    }
                    done
                });
            }
            MenuKind::Tab(tid) => {
                let tid = *tid;
                let Some(ti) = self.tab_index(tid) else { return };
                let (pinned, broadcast, logging, ro, multi, color, n, idx) = {
                    let t = &self.tabs[ti];
                    (t.pinned, t.broadcast, t.logging, t.read_only, t.panes.len() > 1, t.color, self.tabs.len(), ti)
                };
                let recording = self.tabs[ti].focused_pane().and_then(|p| p.session.as_ref()).is_some_and(|s| s.recording());
                // Rebuilt on every open so labels never go stale (§7.3, §24 #30).
                keep = popup_menu(ctx, Id::new("menu-tab"), pos, |ui| {
                    let mut c = |ui: &mut Ui, label: &str, enabled: bool, checked: bool, action: &str| -> bool {
                        if menu_item(ui, label, "", enabled, checked) {
                            *chosen.borrow_mut() = action.to_string();
                            true
                        } else {
                            false
                        }
                    };
                    let mut done = false;
                    done |= c(ui, if pinned { "Unpin" } else { "Pin" }, true, false, "pinTab");
                    done |= c(ui, "Rename", true, false, "renameTab");
                    ui.menu_button("Tab Color", |ui| {
                        if ui.selectable_label(color.is_none(), "None").clicked() {
                            *chosen.borrow_mut() = "color:".into();
                            ui.close();
                        }
                        for col in TAB_COLORS {
                            let cc = crate::theme::parse_color(col);
                            let (r, resp) = ui.allocate_exact_size(Vec2::new(120.0, 20.0), Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(r, 3.0, self.theme.ui.accent_dim);
                            }
                            ui.painter().circle_filled(Pos2::new(r.left() + 10.0, r.center().y), 5.0, cc);
                            ui.painter().text(Pos2::new(r.left() + 24.0, r.center().y), Align2::LEFT_CENTER, col, FontId::proportional(12.0), self.theme.ui.foreground);
                            if resp.clicked() {
                                *chosen.borrow_mut() = format!("color:{col}");
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    done |= c(ui, "Split Right", true, false, "splitRight");
                    done |= c(ui, "Split Down", true, false, "splitDown");
                    done |= c(ui, "Unsplit All", multi, false, "unsplitAll");
                    done |= c(ui, if broadcast { "Broadcast Input: on" } else { "Broadcast Input: off" }, true, broadcast, "broadcastToggle");
                    done |= c(ui, if logging { "Stop Logging" } else { "Start Logging" }, true, false, "toggleLog");
                    done |= c(ui, if recording { "Stop Recording (.cast)" } else { "Start Recording (.cast)" }, true, false, "toggleRecording");
                    done |= c(ui, if ro { "Read-Only: on" } else { "Read-Only: off" }, true, ro, "toggleReadOnly");
                    done |= c(ui, "Set Tab Group…", true, false, "setGroup");
                    ui.separator();
                    done |= c(ui, "Duplicate Tab", true, false, "duplicateTab");
                    done |= c(ui, "Save as Session…", true, false, "saveSession");
                    ui.separator();
                    done |= c(ui, "Close Tab", true, false, "closeTabNow");
                    done |= c(ui, "Close Other Tabs", n > 1, false, "closeOthers");
                    done |= c(ui, "Close Tabs to the Right", idx + 1 < n, false, "closeRight");
                    done || !chosen.borrow().is_empty()
                });
                let picked = chosen.borrow().clone();
                if !picked.is_empty() {
                    keep = false;
                    self.tab_menu_action(ti, &picked);
                }
            }
            MenuKind::Pane { tab, pane } => {
                let (tab, pane) = (*tab, *pane);
                let has_sel = self.tab_index(tab).and_then(|i| self.tabs[i].pane(pane)).and_then(|p| p.session.as_ref()).is_some_and(|s| s.has_selection());
                keep = popup_menu(ctx, Id::new("menu-pane"), pos, |ui| {
                    let mut c = |ui: &mut Ui, label: &str, hint: &str, enabled: bool, action: &str| -> bool {
                        if menu_item(ui, label, hint, enabled, false) {
                            *chosen.borrow_mut() = action.to_string();
                            true
                        } else {
                            false
                        }
                    };
                    let mut done = false;
                    done |= c(ui, "Copy", "Ctrl+Shift+C", has_sel, "copy");
                    done |= c(ui, "Paste", "Ctrl+V", true, "paste");
                    done |= c(ui, "Select All", "", true, "selectAll");
                    done |= c(ui, "Clear", "", true, "clear");
                    done |= c(ui, "Search", "Ctrl+Shift+F", true, "search");
                    done |= c(ui, "Save Output…", "Ctrl+Shift+S", true, "exportBuffer");
                    ui.separator();
                    done |= c(ui, "Split Right", "", true, "splitRight");
                    done |= c(ui, "Split Down", "", true, "splitDown");
                    done
                });
                let picked = chosen.borrow().clone();
                if !picked.is_empty() {
                    keep = false;
                    match picked.as_str() {
                        "selectAll" => {
                            if let Some(s) = self.focused_session() {
                                s.select_all();
                            }
                        }
                        "clear" => {
                            if let Some(s) = self.focused_session() {
                                s.clear_scrollback();
                                s.feed_local(b"\x1b[3J");
                            }
                        }
                        a => self.run_action(a),
                    }
                }
            }
        }
        if keep {
            self.menu = Some(m);
        } else {
            self.refocus = true;
        }
    }

    fn tab_menu_action(&mut self, ti: usize, action: &str) {
        let tid = self.tabs[ti].id;
        if let Some(c) = action.strip_prefix("color:") {
            self.tabs[ti].color = if c.is_empty() { None } else { Some(crate::theme::parse_color(c)) };
            self.mark_dirty();
            return;
        }
        // Everything else acts on the selected tab: it already is (the context menu selected it on open).
        self.activate(ti);
        match action {
            "setGroup" => {
                let g = self.tabs[ti].group.clone();
                let mut d = crate::kit::Dialog::prompt("Set tab group", "Group label (leave empty to clear)", &g, true);
                d.width = 360.0;
                self.dialog = Some((DialogId::SetGroup(tid), d));
            }
            "saveSession" => {
                let t = self.tabs[ti].title.clone();
                self.dialog = Some((DialogId::SaveSession(tid), crate::kit::Dialog::prompt("Save as session", "Session name", &t, false)));
            }
            "closeTabNow" => self.close_tab(ti, true),
            a => self.run_action(a),
        }
    }

    // ------------------------------------------------------------------------------ diagnostics

    fn show_diagnostics(&mut self, ctx: &Context) {
        if !self.diag_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Diagnostics").open(&mut open).default_width(640.0).show(ctx, |ui| {
            let ws = working_set();
            ui.label(format!("renderer: egui (wgpu)    backend working set: {:.1} MiB    tabs: {}", ws as f64 / 1048576.0, self.tabs.len()));
            ui.add_space(4.0);
            egui::Grid::new("diag").striped(true).show(ui, |ui| {
                for h in ["tab", "pane", "pid", "rx bytes", "frames", "avg frame"] {
                    ui.strong(h);
                }
                ui.end_row();
                for t in &self.tabs {
                    for p in &t.panes {
                        if let Some(s) = &p.session {
                            let (rx, fr) = (s.inner.rx_bytes.load(std::sync::atomic::Ordering::Relaxed), s.inner.frames.load(std::sync::atomic::Ordering::Relaxed));
                            ui.label(&t.title);
                            ui.label(p.id.to_string());
                            ui.label(s.pid().to_string());
                            ui.label(rx.to_string());
                            ui.label(fr.to_string());
                            ui.label(if fr > 0 { (rx / fr).to_string() } else { "-".into() });
                            ui.end_row();
                        }
                    }
                }
            });
            ctx.request_repaint_after(Duration::from_secs(1));
        });
        self.diag_open = open;
    }
}

fn fmt_dur(ms: u64) -> String {
    let s = ms.div_ceil(1000).max(1);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

fn working_set() -> u64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb).map(|_| c.WorkingSetSize as u64).unwrap_or(0) }
}
