//! Keyboard control of the tree (§8.2 [P1]): arrows move and (with Shift) extend, Enter opens, Ctrl+Enter opens as
//! administrator, F2 renames, Del deletes (after confirmation), Left/Right collapse and expand.

use super::menus::Act;
use super::tree::{self, Kind, Step};
use super::Sidebar;
use crate::panels::PanelCtx;
use egui::{Context, Event, EventFilter, Key, Modifiers};

impl Sidebar {
    pub(super) fn handle_keys(&mut self, ctx: &Context, x: &mut PanelCtx) {
        if std::mem::take(&mut self.focus_tree) && !self.modal_open() {
            ctx.memory_mut(|m| m.request_focus(self.tree_id));
        }
        self.tree_focused = ctx.memory(|m| m.has_focus(self.tree_id));
        if std::mem::take(&mut self.skip_keys) {
            return;
        }
        if !self.tree_focused || self.modal_open() || self.renaming.is_some() || self.drag.is_some() {
            return;
        }
        // The arrows are ours; without this egui would move focus to a neighbouring widget.
        ctx.memory_mut(|m| m.set_focus_lock_filter(self.tree_id, EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false }));
        let keys: Vec<(Key, Modifiers)> = ctx.input(|i| i.events.iter().filter_map(|e| if let Event::Key { key, pressed: true, modifiers, .. } = e { Some((*key, *modifiers)) } else { None }).collect());
        for (key, m) in keys {
            if m.alt {
                continue;
            }
            let cur = self.cursor.clone();
            let at = cur.as_deref().and_then(|c| tree::index_of(&self.rows, c));
            let row = at.map(|i| self.rows[i].clone());
            let step = match key {
                Key::ArrowDown => Some(Step::Down),
                Key::ArrowUp => Some(Step::Up),
                Key::Home => Some(Step::Home),
                Key::End => Some(Step::End),
                _ => None,
            };
            if let Some(s) = step {
                if let Some(i) = tree::step(&self.rows, cur.as_deref(), s) {
                    let id = self.rows[i].id.clone();
                    self.pick(&id, m.shift, false);
                }
                continue;
            }
            match (key, row) {
                (Key::ArrowLeft, Some(r)) if r.kind == Kind::Folder && self.st.is_open(&r.id) => self.set_open(&r.id, false, x),
                (Key::ArrowLeft, Some(r)) if r.kind == Kind::Session && self.query.is_empty() => {
                    if let Some(parent) = r.parent {
                        self.pick(&parent, false, false);
                    }
                }
                (Key::ArrowRight, Some(r)) if r.kind == Kind::Folder => {
                    if !self.st.is_open(&r.id) {
                        self.set_open(&r.id, true, x);
                    } else if let Some(next) = at.and_then(|i| self.rows.get(i + 1)).filter(|n| n.parent.as_deref() == Some(r.id.as_str())).map(|n| n.id.clone()) {
                        self.pick(&next, false, false);
                    }
                }
                (Key::Enter, Some(r)) if r.kind == Kind::Folder => {
                    let open = self.st.is_open(&r.id);
                    self.set_open(&r.id, !open, x);
                }
                (Key::Enter, Some(_)) => {
                    let ids = self.sel_sessions();
                    if !ids.is_empty() {
                        self.run(Act::Open { ids, admin: m.ctrl }, x);
                    }
                }
                (Key::F2, Some(r)) => self.run(if r.kind == Kind::Folder { Act::RenameFolder(r.id) } else { Act::RenameSession(r.id) }, x),
                (Key::Delete, Some(r)) if r.kind == Kind::Folder => self.run(Act::DeleteFolder(r.id), x),
                (Key::Delete, Some(_)) => {
                    let ids = self.sel_sessions();
                    if !ids.is_empty() {
                        self.run(Act::Delete(ids), x);
                    }
                }
                (Key::A, _) if m.ctrl => {
                    self.sel = self.rows.iter().filter(|r| r.kind == Kind::Session).map(|r| r.id.clone()).collect();
                }
                _ => {}
            }
        }
    }
}
