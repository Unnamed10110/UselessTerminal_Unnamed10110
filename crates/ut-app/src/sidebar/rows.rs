//! The tree itself: folder headers, session cards, hover actions, inline rename and the pointer drag-and-drop (§8.2).
//! Rows are painted by hand so a card can carry the colour tag tint, the live dot and the hover-reveal buttons.

use super::menus::{Act, MenuKind};
use super::paint::{self, glyph, glyph_button, job, G};
use super::tree::{self, Dragged, Mode, Slot};
use super::{Cache, Rename, Sidebar};
use crate::panels::PanelCtx;
use crate::theme::{parse_color, Theme};
use egui::text::{CCursor, CCursorRange};
use egui::{Align, Context, FontId, Id, Key, LayerId, Layout, Order, PointerButton, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2};
use ut_data::{Session, Target, TreeFolder};

const SESSION_H: f32 = 48.0;
const FOLDER_H: f32 = 34.0;
const MARGIN: f32 = 8.0;
/// Children sit inside an indent guide 14 px from the left edge (§8.2).
const GUIDE_X: f32 = 14.0;
const CHILD_INDENT: f32 = 15.0;
const EDGE_ZONE: f32 = 26.0;

/// An in-flight drag: either one folder or one or more sessions (in sidebar order).
pub(super) struct Drag {
    pub folder: Option<String>,
    pub sessions: Vec<String>,
    pub label: String,
    pub cancelled: bool,
}

impl Sidebar {
    pub(super) fn ui_tree(&mut self, ui: &mut Ui, x: &mut PanelCtx) {
        let theme: &Theme = x.theme;
        let th = &theme.ui;
        let area = ui.available_rect_before_wrap();
        self.tree_rect = area;
        // Empty space: clears the selection, takes keyboard focus, opens the background menu. Rows are registered after
        // it, so they sit on top.
        let bg = ui.interact(area, self.tree_id, Sense::click());
        if bg.secondary_clicked() {
            self.set_sel(vec![]);
            self.open_menu(bg.interact_pointer_pos().unwrap_or(area.center()), MenuKind::Background);
        } else if bg.clicked() {
            self.set_sel(vec![]);
        }
        if bg.clicked() || bg.secondary_clicked() {
            self.focus_tree = true;
        }
        self.slots.clear();
        let cache = std::mem::take(&mut self.cache);
        let q = cache.query.clone();
        egui::ScrollArea::vertical().id_salt("sb-tree").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.add_space(2.0);
            if let Some(hits) = &cache.hits {
                if hits.is_empty() {
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new(format!("No sessions match \"{q}\".")).small().color(th.muted)));
                }
                for s in hits {
                    self.session_card(ui, x, &cache, s, 0.0, &q);
                }
            } else {
                for f in &cache.tree.folders {
                    self.folder_block(ui, x, &cache, f);
                }
                for s in &cache.tree.sessions {
                    self.session_card(ui, x, &cache, s, 0.0, "");
                }
                if cache.tree.folders.is_empty() && cache.tree.sessions.is_empty() {
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new("No sessions yet. Use + New to create one.").small().color(th.muted)));
                }
            }
            ui.add_space(10.0);
            // Keep dragging near the edge: scroll the list.
            if self.drag.as_ref().is_some_and(|d| !d.cancelled) {
                if let Some(p) = ui.input(|i| i.pointer.hover_pos()).filter(|p| area.x_range().contains(p.x)) {
                    if p.y < area.top() + EDGE_ZONE {
                        ui.scroll_with_delta(Vec2::new(0.0, 14.0));
                        ui.ctx().request_repaint();
                    } else if p.y > area.bottom() - EDGE_ZONE {
                        ui.scroll_with_delta(Vec2::new(0.0, -14.0));
                        ui.ctx().request_repaint();
                    }
                }
            }
        });
        self.cache = cache;
        self.update_drag(ui.ctx(), x, area);
    }

    fn folder_block(&mut self, ui: &mut Ui, x: &mut PanelCtx, c: &Cache, f: &TreeFolder) {
        let open = self.st.is_open(&f.folder.id);
        let head = self.folder_head(ui, x, f, open);
        let mut outer = head;
        if open && !f.sessions.is_empty() {
            let y0 = ui.cursor().top();
            for s in &f.sessions {
                self.session_card(ui, x, c, s, CHILD_INDENT, "");
            }
            let y1 = ui.cursor().top();
            ui.painter().line_segment([Pos2::new(head.left() + GUIDE_X, y0), Pos2::new(head.left() + GUIDE_X, y1)], Stroke::new(1.0, x.theme.ui.card_border));
            outer = outer.union(Rect::from_min_max(Pos2::new(head.left(), y0), Pos2::new(head.right(), y1)));
        }
        self.slots.push(Slot::Folder { id: f.folder.id.clone(), head, outer });
    }

    fn folder_head(&mut self, ui: &mut Ui, x: &mut PanelCtx, f: &TreeFolder, open: bool) -> Rect {
        let theme: &Theme = x.theme;
        let th = &theme.ui;
        let fid = f.folder.id.as_str();
        let w = ui.available_width();
        let (_, slot) = ui.allocate_space(Vec2::new(w, FOLDER_H));
        if self.scroll_to.as_deref() == Some(fid) {
            ui.scroll_to_rect(slot, None);
            self.scroll_to = None;
        }
        if !ui.is_rect_visible(slot) {
            return slot;
        }
        let card = Rect::from_min_max(Pos2::new(slot.left() + MARGIN, slot.top() + 2.0), Pos2::new(slot.right() - MARGIN, slot.bottom() - 2.0));
        let id = ui.id().with(("sb-row", fid));
        let resp = ui.interact(card.expand2(Vec2::new(0.0, 2.0)), id, Sense::CLICK | Sense::DRAG);
        let selected = self.sel.contains(fid);
        let hot = resp.contains_pointer() && self.drag.is_none();
        let dragging = self.drag.as_ref().is_some_and(|d| d.folder.as_deref() == Some(fid));
        let fade = if dragging { 0.45 } else { 1.0 };
        let p = ui.painter().clone();
        let (fill, stroke) = if selected { (th.folder_selected, th.accent) } else if hot { (th.hover_bg, th.card_border) } else { (th.card_bg, th.card_border) };
        p.rect(card, 6.0, fill.gamma_multiply(fade), Stroke::new(1.0, stroke.gamma_multiply(fade)), StrokeKind::Inside);
        if self.tree_focused && self.cursor.as_deref() == Some(fid) {
            p.rect_stroke(card, 6.0, Stroke::new(2.0, th.accent), StrokeKind::Inside);
        }
        let cy = card.center().y;
        // chevron: its own button (a single click toggles, §8.2 "Chevron toggle")
        let chev = Rect::from_center_size(Pos2::new(card.left() + 14.0, cy), Vec2::splat(20.0));
        let mut act = None;
        if glyph_button(ui, chev, id.with("chev"), G::Chevron { open }, th.muted, th.foreground, th.hover_bg, if open { "Collapse" } else { "Expand" }) {
            act = Some(Act::SetOpen(fid.to_string(), !open));
        }
        glyph(&p, G::Folder, Rect::from_center_size(Pos2::new(card.left() + 36.0, cy), Vec2::splat(16.0)), th.highlight);
        // right side: actions on hover, count pill otherwise
        let acts_w = if hot { 70.0 } else { 0.0 };
        let count = p.layout_no_wrap(f.sessions.len().to_string(), FontId::proportional(11.0), th.accent);
        let pill_w = (count.size().x + 12.0).max(20.0);
        let pill = Rect::from_min_size(Pos2::new(card.right() - 8.0 - acts_w - pill_w, cy - 8.0), Vec2::new(pill_w, 16.0));
        p.rect_filled(pill, 8.0, th.accent_dim);
        p.galley(pill.center() - count.size() / 2.0, count, th.accent);
        let tx = card.left() + 50.0;
        let name_w = (pill.left() - 6.0 - tx).max(10.0);
        let rename = self.renaming.as_mut().filter(|r| r.id == fid).map(|r| rename_edit(ui, Rect::from_min_size(Pos2::new(tx, cy - 11.0), Vec2::new(name_w, 22.0)), r));
        if rename.is_none() {
            let g = p.layout_job(job(&f.folder.name, &[], FontId::proportional(14.0), th.foreground, th.accent, th.accent_dim, name_w));
            p.galley(Pos2::new(tx, cy - g.size().y / 2.0), g, th.foreground);
        }
        if hot {
            let r3 = Rect::from_center_size(Pos2::new(card.right() - 18.0, cy), Vec2::splat(22.0));
            let r2 = r3.translate(Vec2::new(-24.0, 0.0));
            let r1 = r2.translate(Vec2::new(-24.0, 0.0));
            if glyph_button(ui, r1, id.with("add"), G::Plus, th.muted, th.foreground, th.hover_bg, "New session in folder") {
                act = Some(Act::NewSession(Some(fid.to_string())));
            }
            if glyph_button(ui, r2, id.with("ren"), G::Edit, th.muted, th.foreground, th.hover_bg, "Rename folder") {
                act = Some(Act::RenameFolder(fid.to_string()));
            }
            if glyph_button(ui, r3, id.with("del"), G::Trash, th.muted, th.error, th.hover_bg, "Delete folder — sessions in this folder will be moved to the root list") {
                act = Some(Act::DeleteFolder(fid.to_string()));
            }
        }
        if let Some(Some(commit)) = rename {
            self.finish_rename(commit, x);
        }
        let mods = ui.input(|i| i.modifiers);
        if resp.double_clicked() {
            act = Some(Act::SetOpen(fid.to_string(), !open));
        } else if resp.clicked() {
            self.pick(fid, mods.shift, mods.ctrl);
            self.focus_tree = true;
        }
        if resp.secondary_clicked() {
            if !self.sel.contains(fid) {
                self.set_sel(vec![fid.to_string()]);
            }
            self.cursor = Some(fid.to_string());
            self.focus_tree = true;
            self.open_menu(resp.interact_pointer_pos().unwrap_or(card.center()), MenuKind::Folder(fid.to_string()));
        }
        if resp.drag_started_by(PointerButton::Primary) && self.query.is_empty() && self.renaming.is_none() {
            self.drag = Some(Drag { folder: Some(fid.to_string()), sessions: vec![], label: f.folder.name.clone(), cancelled: false });
        }
        if resp.is_pointer_button_down_on() {
            self.focus_tree = true;
        }
        if let Some(a) = act {
            self.run(a, x);
        }
        slot
    }

    pub(super) fn session_card(&mut self, ui: &mut Ui, x: &mut PanelCtx, c: &Cache, s: &Session, indent: f32, q: &str) {
        let theme: &Theme = x.theme;
        let th = &theme.ui;
        let w = ui.available_width();
        let (_, slot) = ui.allocate_space(Vec2::new(w, SESSION_H));
        self.slots.push(Slot::Session { id: s.id.clone(), rect: slot });
        if self.scroll_to.as_deref() == Some(s.id.as_str()) {
            ui.scroll_to_rect(slot, None);
            self.scroll_to = None;
        }
        if !ui.is_rect_visible(slot) {
            return;
        }
        let card = Rect::from_min_max(Pos2::new(slot.left() + MARGIN + indent, slot.top() + 2.0), Pos2::new(slot.right() - MARGIN, slot.bottom() - 2.0));
        let id = ui.id().with(("sb-row", &s.id));
        let resp = ui.interact(Rect::from_min_max(Pos2::new(card.left(), slot.top()), Pos2::new(card.right(), slot.bottom())), id, Sense::CLICK | Sense::DRAG);
        let selected = self.sel.contains(&s.id);
        let hot = resp.contains_pointer() && self.drag.is_none();
        let dragging = self.drag.as_ref().is_some_and(|d| d.sessions.contains(&s.id));
        let fade = if dragging { 0.45 } else { 1.0 };
        let tag = parse_color(&s.color_tag);
        let p = ui.painter().clone();
        // tint = colour tag at 15% (21% hover, 28% selected), border = tag at 40% (§8.2)
        let a = if selected { 0.28 } else if hot { 0.21 } else { 0.15 };
        p.rect(card, 6.0, tag.gamma_multiply(a * fade), Stroke::new(1.0, tag.gamma_multiply(0.4 * fade)), StrokeKind::Inside);
        if self.tree_focused && self.cursor.as_deref() == Some(s.id.as_str()) {
            p.rect_stroke(card, 6.0, Stroke::new(2.0, th.accent), StrokeKind::Inside);
        }
        let cy = card.center().y;
        p.circle_filled(Pos2::new(card.left() + 10.0, cy), 3.5, tag.gamma_multiply(fade));
        // shell icon (the override first, then the executable's own)
        let icon = Rect::from_center_size(Pos2::new(card.left() + 32.0, cy), Vec2::splat(20.0));
        let src = if s.icon_override.trim().is_empty() { s.shell_path.as_str() } else { s.icon_override.as_str() };
        match x.icons.get(ui.ctx(), src).map(|t| t.id()) {
            Some(t) => {
                p.image(t, icon, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), egui::Color32::WHITE.gamma_multiply(fade));
            }
            None => paint::console(&p, icon, th.icon),
        }
        // right side: live dot, then the hover actions
        let live = x.live_sessions.contains(&s.id);
        let acts_w = if hot { 50.0 } else { 0.0 };
        let mut right = card.right() - 8.0 - acts_w;
        if live {
            p.circle_filled(Pos2::new(right - 4.0, cy), 6.5, th.success.gamma_multiply(0.25));
            p.circle_filled(Pos2::new(right - 4.0, cy), 3.5, th.success);
            right -= 14.0;
        }
        let tx = card.left() + 48.0;
        let max_w = (right - tx).max(10.0);
        let line = c.cmd.get(&s.id);
        // While filtering, the compact form if it holds the match, else the full text that was searched, so the
        // highlight is always visible.
        let cmd = line.map_or("", |l| if q.is_empty() || !tree::match_ranges(&l.short, q).is_empty() { l.short.as_str() } else { l.long.as_str() });
        let sub = if tree::only_in_description(s, line.map_or("", |l| l.long.as_str()), q) { s.description.lines().next().unwrap_or("") } else { cmd };
        let hl = |t: &str| tree::match_ranges(t, q);
        let body = FontId::proportional(14.0);
        let small = FontId::proportional(12.0);
        let g2 = p.layout_job(job(sub, &hl(sub), small, th.muted, th.accent, th.accent_dim, max_w));
        let rename_rect = |h1: f32| Rect::from_min_size(Pos2::new(tx, cy - (h1 + g2.size().y) / 2.0 - 1.0), Vec2::new(max_w, h1 + 2.0));
        let renaming = self.renaming.as_ref().is_some_and(|r| r.id == s.id);
        let g1 = p.layout_job(job(&s.name, &hl(&s.name), body, th.foreground, th.accent, th.accent_dim, max_w));
        let h1 = g1.size().y;
        let y0 = cy - (h1 + g2.size().y) / 2.0;
        let rename = if renaming { self.renaming.as_mut().map(|r| rename_edit(ui, rename_rect(h1), r)) } else { None };
        if !renaming {
            p.galley(Pos2::new(tx, y0), g1, th.foreground);
        }
        p.galley(Pos2::new(tx, y0 + h1), g2, th.muted);

        let mut act = None;
        if hot {
            let del = Rect::from_center_size(Pos2::new(card.right() - 18.0, cy), Vec2::splat(22.0));
            let edit = del.translate(Vec2::new(-24.0, 0.0));
            if glyph_button(ui, edit, id.with("edit"), G::Edit, th.muted, th.foreground, th.hover_bg, "Edit session") {
                act = Some(Act::Edit(s.id.clone()));
            }
            if glyph_button(ui, del, id.with("del"), G::Trash, th.muted, th.error, th.hover_bg, "Delete session") {
                act = Some(Act::Delete(vec![s.id.clone()]));
            }
        }
        if let Some(Some(commit)) = rename {
            self.finish_rename(commit, x);
        }
        let mods = ui.input(|i| i.modifiers);
        if resp.double_clicked() {
            // Double-click opens embedded; Ctrl+double-click runs as administrator (§8.2, §4.9).
            self.set_sel(vec![s.id.clone()]);
            act = Some(Act::Open { ids: vec![s.id.clone()], admin: mods.ctrl });
        } else if resp.clicked() {
            self.pick(&s.id, mods.shift, mods.ctrl);
            self.focus_tree = true;
        }
        if resp.secondary_clicked() {
            if !self.sel.contains(&s.id) {
                self.set_sel(vec![s.id.clone()]);
            }
            self.cursor = Some(s.id.clone());
            self.focus_tree = true;
            self.open_menu(resp.interact_pointer_pos().unwrap_or(card.center()), MenuKind::Session(s.id.clone()));
        }
        if resp.drag_started_by(PointerButton::Primary) && self.query.is_empty() && self.renaming.is_none() {
            let ids = if self.sel.contains(&s.id) { self.sel_sessions() } else { vec![s.id.clone()] };
            let label = if ids.len() > 1 { format!("{} sessions", ids.len()) } else { s.name.clone() };
            self.drag = Some(Drag { folder: None, sessions: ids, label, cancelled: false });
        }
        if resp.is_pointer_button_down_on() {
            self.focus_tree = true;
        }
        if resp.hovered() && self.drag.is_none() {
            resp.on_hover_ui(|ui| {
                ui.set_max_width(360.0);
                if !s.description.trim().is_empty() {
                    ui.label(&s.description);
                }
                ui.label(RichText::new(line.map_or("", |l| l.full.as_str())).small().monospace().color(th.muted));
            });
        }
        if let Some(a) = act {
            self.run(a, x);
        }
    }

    // ------------------------------------------------------------------------------------------ drag and drop

    /// Resolve the drop target under the pointer, draw the ghost and the indicator, commit on release.
    fn update_drag(&mut self, ctx: &Context, x: &mut PanelCtx, area: Rect) {
        let Some(d) = &mut self.drag else {
            self.drop = None;
            return;
        };
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            d.cancelled = true;
        }
        let pos = ctx.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()));
        let dragged = match &d.folder {
            Some(f) => Dragged::Folder(f),
            None => Dragged::Sessions(&d.sessions),
        };
        self.drop = if d.cancelled { None } else { pos.and_then(|p| tree::drop_at(&self.slots, area, p, dragged)) };
        let th = &x.theme.ui;
        if let Some(dr) = &self.drop {
            let lp = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("sb-drop"))).with_clip_rect(area);
            let line = |y: f32| {
                lp.rect_filled(Rect::from_min_max(Pos2::new(dr.rect.left() + MARGIN, y - 1.0), Pos2::new(dr.rect.right() - MARGIN, y + 1.0)), 1.0, th.accent);
            };
            match dr.mode {
                Mode::Before => line(dr.rect.top() + 1.0),
                Mode::After => line(dr.rect.bottom() - 1.0),
                Mode::Into => {
                    let r = Rect::from_min_max(Pos2::new(dr.rect.left() + MARGIN, dr.rect.top() + 2.0), Pos2::new(dr.rect.right() - MARGIN, dr.rect.bottom() - 2.0));
                    lp.rect(r, 6.0, th.accent_dim, Stroke::new(1.5, th.accent), StrokeKind::Inside);
                }
                Mode::Root => {
                    lp.rect_filled(Rect::from_min_max(Pos2::new(area.left() + MARGIN, area.bottom() - 4.0), Pos2::new(area.right() - MARGIN, area.bottom() - 1.0)), 1.0, th.accent);
                }
            }
        }
        if let (Some(p), false) = (pos, d.cancelled) {
            let lp = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("sb-ghost")));
            let g = lp.layout_no_wrap(d.label.clone(), FontId::proportional(13.0), th.foreground);
            let r = Rect::from_min_size(p + Vec2::new(14.0, 10.0), g.size() + Vec2::new(20.0, 8.0));
            lp.rect(r, 6.0, th.card_bg, Stroke::new(1.0, th.accent), StrokeKind::Inside);
            lp.galley(r.min + Vec2::new(10.0, 4.0), g, th.foreground);
        }
        ctx.request_repaint();
        if ctx.input(|i| i.pointer.primary_down()) {
            return;
        }
        // released
        let Some(d) = self.drag.take() else { return };
        let Some(dr) = self.drop.take().filter(|_| !d.cancelled) else { return };
        let moved = x.core.sessions.move_items(&d.sessions, d.folder.as_deref(), &dr.target, dr.half);
        // Sessions dropped into a folder reveal it (§8.2).
        if let (true, None, Target::Folder(f) | Target::FolderEdge(f)) = (moved, &d.folder, &dr.target) {
            self.set_open(f, true, x);
        }
        if moved {
            self.scroll_to = d.sessions.first().cloned().or(d.folder);
        }
    }

    pub(super) fn begin_rename(&mut self, id: &str, folder: bool, text: String) {
        self.renaming = Some(Rename { id: id.to_string(), folder, text, first: true });
        self.scroll_to = Some(id.to_string());
    }

    /// `true` = commit the text, `false` = cancel. Blank or unchanged names are ignored (folders cannot be empty).
    pub(super) fn finish_rename(&mut self, commit: bool, x: &mut PanelCtx) {
        let Some(r) = self.renaming.take() else { return };
        self.focus_tree = true;
        // The Enter / Esc that ended the edit is still in this frame's input: it must not also open or deselect.
        self.skip_keys = true;
        let name = r.text.trim();
        if !commit || name.is_empty() {
            return;
        }
        if r.folder {
            x.core.sessions.rename_folder(&r.id, name);
        } else if let Some(s) = x.core.sessions.get(&r.id).filter(|s| s.name != name) {
            if let Err(e) = x.core.sessions.update_session(Session { name: name.to_string(), ..s }) {
                x.toasts.push(e.to_string(), crate::kit::ToastKind::Error);
            }
        }
    }
}

/// Inline name editor placed over a row's name. `Some(true)` = Enter / focus lost, `Some(false)` = Esc.
fn rename_edit(ui: &mut Ui, rect: Rect, r: &mut Rename) -> Option<bool> {
    let id = Id::new(("sb-rename", &r.id));
    // A child Ui at an absolute rect: `Ui::put` would move the parent's cursor back up to the rect and shift every
    // row below it.
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
    let resp = child.add(TextEdit::singleline(&mut r.text).id(id).desired_width(rect.width()));
    if r.first {
        resp.request_focus();
        if let Some(mut st) = TextEdit::load_state(ui.ctx(), id) {
            st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(r.text.chars().count()))));
            st.store(ui.ctx(), id);
        } else {
            ui.ctx().request_repaint();
            return None;
        }
        r.first = false;
        return None;
    }
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        return Some(false);
    }
    resp.lost_focus().then_some(true)
}
