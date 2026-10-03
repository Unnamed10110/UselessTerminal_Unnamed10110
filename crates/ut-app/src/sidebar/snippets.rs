//! Snippets section at the bottom of the sidebar (§8.2): collapsible list (max 200 px), add/edit dialog, run on the
//! focused pane through `Cmd::RunSnippet` (the app handles bracketed paste, Enter and broadcast).

use super::editor::{dialog_button, error_label, field_label};
use super::menus::{Act, Menu, MenuKind};
use super::paint::{glyph_button, section_header, G};
use super::Sidebar;
use crate::panels::PanelCtx;
use crate::theme::Theme;
use egui::text::{CCursor, CCursorRange};
use egui::{Align, Context, Event, FontId, Id, Key, Layout, Modifiers, Pos2, Rect, RichText, Sense, Stroke, TextEdit, Ui, Vec2};
use ut_data::{Snippet, SessionStore};

const ITEM_H: f32 = 40.0;

/// What the list shows under the name: the first command line, with a marker when there are more.
pub fn preview(command: &str) -> String {
    let mut lines = command.lines();
    let first = lines.next().unwrap_or("").trim_end().to_string();
    if lines.next().is_some() {
        format!("{first}  ↵…")
    } else {
        first
    }
}

pub struct SnippetEditor {
    existing: Option<Snippet>,
    name: String,
    command: String,
    append_enter: bool,
    error: String,
    focus: u8,
    submit: bool,
}

pub enum SnipOutcome {
    Open,
    Saved(Snippet),
    Cancelled,
}

impl SnippetEditor {
    pub fn new(existing: Option<Snippet>) -> Self {
        let (name, command, append_enter) = existing.as_ref().map_or((String::new(), String::new(), true), |s| (s.name.clone(), s.command.clone(), s.append_enter));
        Self { existing, name, command, append_enter, error: String::new(), focus: 2, submit: false }
    }

    fn try_save(&mut self, store: &SessionStore) -> Option<Snippet> {
        let s = Snippet { name: self.name.trim().to_string(), command: self.command.clone(), append_enter: self.append_enter, ..self.existing.clone().unwrap_or_default() };
        // The store requires both; saying which one is missing is friendlier than its combined message.
        self.error = match (s.name.is_empty(), s.command.trim().is_empty()) {
            (true, _) => "Name is required.".into(),
            (_, true) => "Command is required.".into(),
            _ => String::new(),
        };
        if !self.error.is_empty() {
            return None;
        }
        let r = if self.existing.is_some() { store.update_snippet(s) } else { store.add_snippet(s) };
        match r {
            Ok(saved) => Some(saved),
            Err(e) => {
                self.error = e.to_string();
                None
            }
        }
    }

    pub fn show(&mut self, ctx: &Context, theme: &Theme, store: &SessionStore) -> SnipOutcome {
        let th = &theme.ui;
        let mut cancel = false;
        let modal = egui::Modal::new(Id::new("sb-snippet-editor")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 40.0).clamp(300.0, 480.0));
            ui.label(RichText::new(if self.existing.is_some() { "Edit Snippet" } else { "New Snippet" }).strong().size(15.0));
            ui.add_space(6.0);
            let w = ui.available_width();
            field_label(ui, th, "Name");
            let r = ui.add(TextEdit::singleline(&mut self.name).desired_width(w));
            if self.focus > 0 {
                r.request_focus();
                if let Some(mut st) = TextEdit::load_state(ui.ctx(), r.id) {
                    st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(self.name.chars().count()))));
                    st.store(ui.ctx(), r.id);
                }
                self.focus -= 1;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.submit = true;
            }
            field_label(ui, th, "Command");
            ui.add(TextEdit::multiline(&mut self.command).font(egui::TextStyle::Monospace).desired_rows(5).desired_width(w));
            ui.checkbox(&mut self.append_enter, "Press Enter after sending");
            error_label(ui, th, &self.error);
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if dialog_button(ui, th, "Save", true) {
                    self.submit = true;
                }
                if dialog_button(ui, th, "Cancel", false) {
                    cancel = true;
                }
            });
            if ui.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Enter)) {
                self.submit = true;
            }
        });
        if cancel || (modal.is_top_modal && !modal.any_popup_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))) {
            return SnipOutcome::Cancelled;
        }
        if std::mem::take(&mut self.submit) {
            if let Some(s) = self.try_save(store) {
                return SnipOutcome::Saved(s);
            }
        }
        SnipOutcome::Open
    }
}

impl Sidebar {
    pub(super) fn ui_snippets(&mut self, ui: &mut Ui, x: &mut PanelCtx) {
        let theme = x.theme;
        let th = &theme.ui;
        let (toggle, add) = section_header(ui, th, "sb-snippets", "Snippets", Some(self.cache.snippets.len()), self.st.snippets_open, Some("New snippet"));
        if toggle {
            self.st.snippets_open = !self.st.snippets_open;
            self.st.save(&x.core.writer);
        }
        if add {
            self.run(Act::NewSnippet, x);
        }
        if !self.st.snippets_open {
            return;
        }
        let snippets = self.cache.snippets.clone();
        let mut act: Option<Act> = None;
        if snippets.is_empty() {
            ui.label(RichText::new("No snippets yet. Use + to add one.").small().color(th.muted));
            ui.add_space(8.0);
        }
        let focused = ui.memory(|m| m.has_focus(self.snip_id));
        let list = egui::ScrollArea::vertical().id_salt("sb-snippets-list").max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for s in &snippets {
                let w = ui.available_width();
                let (_, slot) = ui.allocate_space(Vec2::new(w, ITEM_H));
                if self.snip_scroll.as_deref() == Some(s.id.as_str()) {
                    ui.scroll_to_rect(slot, None);
                    self.snip_scroll = None;
                }
                if !ui.is_rect_visible(slot) {
                    continue;
                }
                let r = Rect::from_min_max(Pos2::new(slot.left() + 8.0, slot.top() + 1.0), Pos2::new(slot.right() - 8.0, slot.bottom() - 1.0));
                let id = ui.id().with(("sb-snip", &s.id));
                let resp = ui.interact(r, id, Sense::click());
                let selected = self.snip_sel.as_deref() == Some(s.id.as_str());
                let hot = resp.contains_pointer();
                let p = ui.painter().clone();
                if selected {
                    p.rect(r, 6.0, th.accent_dim, Stroke::new(1.0, th.accent), egui::StrokeKind::Inside);
                } else if hot {
                    p.rect_filled(r, 6.0, th.hover_bg);
                }
                if selected && focused {
                    p.rect_stroke(r, 6.0, Stroke::new(2.0, th.accent), egui::StrokeKind::Inside);
                }
                let max_w = (r.width() - 20.0 - if hot { 48.0 } else { 0.0 }).max(10.0);
                let name = p.layout(s.name.clone(), FontId::proportional(13.0), th.foreground, max_w);
                let cmd = p.layout(preview(&s.command), FontId::monospace(11.5), th.muted, max_w);
                let y0 = r.center().y - (name.size().y + cmd.size().y) / 2.0;
                let h1 = name.size().y;
                p.galley(Pos2::new(r.left() + 10.0, y0), name, th.foreground);
                p.galley(Pos2::new(r.left() + 10.0, y0 + h1), cmd, th.muted);
                if hot {
                    let del = Rect::from_center_size(Pos2::new(r.right() - 16.0, r.center().y), Vec2::splat(22.0));
                    let edit = del.translate(Vec2::new(-24.0, 0.0));
                    if glyph_button(ui, edit, id.with("edit"), G::Edit, th.muted, th.foreground, th.hover_bg, "Edit snippet") {
                        act = Some(Act::EditSnippet(s.id.clone()));
                    }
                    if glyph_button(ui, del, id.with("del"), G::Trash, th.muted, th.error, th.hover_bg, "Delete snippet") {
                        act = Some(Act::DeleteSnippet(s.id.clone()));
                    }
                }
                if resp.double_clicked() {
                    self.snip_sel = Some(s.id.clone());
                    act = Some(Act::RunSnippet(s.id.clone()));
                } else if resp.clicked() {
                    self.snip_sel = Some(s.id.clone());
                }
                if resp.is_pointer_button_down_on() {
                    ui.memory_mut(|m| m.request_focus(self.snip_id));
                }
                if resp.secondary_clicked() {
                    self.snip_sel = Some(s.id.clone());
                    self.menu = Some(Menu { pos: resp.interact_pointer_pos().unwrap_or(r.center()), kind: MenuKind::Snippet(Some(s.id.clone())) });
                }
                if resp.hovered() {
                    resp.on_hover_text(format!("{}\n\nDouble-click to run", s.command));
                }
            }
        });
        // Registered after the rows so it never takes their clicks; rows hand it the focus themselves.
        ui.interact(list.inner_rect, self.snip_id, Sense::focusable_noninteractive());
        if focused && !snippets.is_empty() && !self.modal_open() {
            ui.ctx().memory_mut(|m| m.set_focus_lock_filter(self.snip_id, egui::EventFilter { tab: false, horizontal_arrows: false, vertical_arrows: true, escape: false }));
            let keys: Vec<Key> = ui.input(|i| i.events.iter().filter_map(|e| if let Event::Key { key, pressed: true, modifiers, .. } = e { (!modifiers.ctrl && !modifiers.alt).then_some(*key) } else { None }).collect());
            let at = self.snip_sel.as_ref().and_then(|id| snippets.iter().position(|s| &s.id == id));
            for k in keys {
                match (k, at) {
                    (Key::ArrowDown, i) => self.snip_sel = snippets.get(i.map_or(0, |i| (i + 1).min(snippets.len() - 1))).map(|s| s.id.clone()),
                    (Key::ArrowUp, i) => self.snip_sel = snippets.get(i.map_or(0, |i| i.saturating_sub(1))).map(|s| s.id.clone()),
                    (Key::Enter, Some(i)) => act = Some(Act::RunSnippet(snippets[i].id.clone())),
                    (Key::F2, Some(i)) => act = Some(Act::EditSnippet(snippets[i].id.clone())),
                    (Key::Delete, Some(i)) => act = Some(Act::DeleteSnippet(snippets[i].id.clone())),
                    _ => continue,
                }
                self.snip_scroll = self.snip_sel.clone();
            }
        }
        if let Some(a) = act {
            self.run(a, x);
        }
    }

    pub(super) fn show_snippet_editor(&mut self, ctx: &Context, x: &mut PanelCtx) {
        let Some(mut ed) = self.snippet_editor.take() else { return };
        match ed.show(ctx, x.theme, &x.core.sessions) {
            SnipOutcome::Open => self.snippet_editor = Some(ed),
            SnipOutcome::Saved(s) => {
                self.snip_sel = Some(s.id.clone());
                self.snip_scroll = Some(s.id);
                self.focus_snippets = true;
            }
            SnipOutcome::Cancelled => self.focus_snippets = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_shows_the_first_line_and_marks_the_rest() {
        assert_eq!(preview("ls -la"), "ls -la");
        assert_eq!(preview("cd /x\r\nls"), "cd /x  ↵…");
        assert_eq!(preview(""), "");
        assert_eq!(preview("one\n"), "one", "a trailing newline is not 'more'");
    }

    #[test]
    fn editor_requires_both_fields_and_round_trips_through_the_store() {
        let d = std::env::temp_dir().join(format!("ut sidebar snip {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let w = std::sync::Arc::new(ut_fs::DebouncedWriter::new(|p, e| panic!("{p:?}: {e}")));
        let store = SessionStore::open(d.join("sessions.json"), w);
        let mut ed = SnippetEditor::new(None);
        assert!(ed.try_save(&store).is_none());
        assert_eq!(ed.error, "Name is required.");
        ed.name = "  deploy ".into();
        assert!(ed.try_save(&store).is_none());
        assert_eq!(ed.error, "Command is required.");
        ed.command = "make\nmake deploy".into();
        ed.append_enter = false;
        let s = ed.try_save(&store).unwrap();
        assert_eq!((s.name.as_str(), s.append_enter), ("deploy", false));
        // editing keeps the id (and position)
        let mut ed = SnippetEditor::new(Some(s.clone()));
        ed.name = "deploy!".into();
        let u = ed.try_save(&store).unwrap();
        assert_eq!(u.id, s.id);
        assert_eq!(store.snippets().len(), 1);
        assert_eq!(store.snippets()[0].name, "deploy!");
    }
}
