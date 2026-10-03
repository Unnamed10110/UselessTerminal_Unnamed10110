//! Command palette (§7.10): fuzzy match, recents first, shortcut labels read from the LIVE keymap, themes preview
//! on selection.

use crate::panels::{Cmd, PanelCtx};
use egui::{Align2, Context, Key, RichText, Vec2};

struct Entry {
    id: String,
    label: String,
    action: Option<String>,
    cmd: EntryCmd,
}

enum EntryCmd {
    Action(String),
    Session(String),
    Snippet(String),
    Workspace(String),
    Tab(u64),
    Theme(String),
    SettingsFile,
    Diagnostics,
}

const BASE: [(&str, &str, bool); 28] = [
    ("newTab", "New Tab", true),
    ("closePane", "Close Tab / Pane", true),
    ("togglePanel", "Toggle Sessions Panel", true),
    ("toggleBrowser", "Toggle Browser Panel", true),
    ("settings", "Open Settings", true),
    ("duplicateTab", "Duplicate Tab", true),
    ("newSession", "New Saved Session", true),
    ("addPane", "Add Pane (Split)", false),
    ("splitRight", "Split Pane Right", true),
    ("splitDown", "Split Pane Down", true),
    ("unsplitAll", "Unsplit All Panes", false),
    ("renameTab", "Rename Tab", false),
    ("pinTab", "Pin / Unpin Tab", false),
    ("broadcastToggle", "Toggle Broadcast Input", false),
    ("nextTab", "Next Tab", true),
    ("prevTab", "Previous Tab", true),
    ("closeOthers", "Close Other Tabs", false),
    ("closeRight", "Close Tabs to the Right", false),
    ("quake", "Toggle Window (Quake Mode)", true),
    ("quickConnect", "Quick SSH Connect", true),
    ("toggleLog", "Toggle Session Logging", false),
    ("toggleReadOnly", "Toggle Read-Only Mode", false),
    ("toggleRecording", "Toggle Recording (asciicast)", false),
    ("toggleCrt", "Toggle Retro CRT Mode", false),
    ("findAllTabs", "Find in All Tabs", false),
    ("toggleMinimap", "Toggle Minimap Scrollbar", false),
    ("saveWorkspace", "Save Current Tabs as Workspace", false),
    ("sendToBrowser", "Send Selection to Browser", false),
];

/// Subsequence score: bonus for word starts and consecutive runs, penalty for gaps. `None` = no match.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut score, mut ti, mut prev) = (0i32, 0usize, -2i64);
    for qc in query.to_lowercase().chars() {
        let i = t[ti..].iter().position(|&c| c == qc)? + ti;
        score += 10;
        if i as i64 == prev + 1 {
            score += 15;
        }
        if i == 0 || matches!(t[i - 1], ' ' | '-' | '_' | '/' | ':') {
            score += 12;
        }
        score -= (i - ti).min(8) as i32;
        prev = i as i64;
        ti = i + 1;
    }
    Some(score - (t.len() / 8) as i32)
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    query: String,
    sel: usize,
    recent: Vec<String>,
    focus: bool,
    last_preview: Option<String>,
}

impl Palette {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.sel = 0;
        self.focus = true;
    }

    fn entries(&self, x: &PanelCtx) -> Vec<Entry> {
        let mut v: Vec<Entry> = BASE.iter().map(|(id, label, key)| Entry { id: (*id).into(), label: (*label).into(), action: key.then(|| (*id).into()), cmd: EntryCmd::Action((*id).into()) }).collect();
        for w in x.core.workspaces.list() {
            v.push(Entry { id: format!("ws:{}", w.id), label: format!("Open Workspace: {}", w.name), action: None, cmd: EntryCmd::Workspace(w.id) });
        }
        for s in x.core.sessions.list() {
            v.push(Entry { id: format!("session:{}", s.id), label: format!("Open Session: {}", s.name), action: None, cmd: EntryCmd::Session(s.id) });
        }
        for s in x.core.sessions.snippets() {
            v.push(Entry { id: format!("snippet:{}", s.id), label: format!("Run Snippet: {}", s.name), action: None, cmd: EntryCmd::Snippet(s.id) });
        }
        for (i, t) in x.tabs.iter().enumerate() {
            v.push(Entry { id: format!("tab:{}", t.id), label: format!("Go to Tab: {} {}", i + 1, t.title), action: None, cmd: EntryCmd::Tab(t.id) });
        }
        for n in x.core.preset_names() {
            v.push(Entry { id: format!("theme:{n}"), label: format!("Theme: {n}"), action: None, cmd: EntryCmd::Theme(n) });
        }
        v.push(Entry { id: "diagnostics".into(), label: "Developer: Diagnostics".into(), action: None, cmd: EntryCmd::Diagnostics });
        v.push(Entry { id: "settingsFile".into(), label: "Open settings.json".into(), action: None, cmd: EntryCmd::SettingsFile });
        v
    }

    fn run(&mut self, e: &Entry, x: &mut PanelCtx) {
        self.recent.retain(|r| r != &e.id);
        self.recent.insert(0, e.id.clone());
        self.recent.truncate(12);
        match &e.cmd {
            EntryCmd::Action(a) => x.cmds.push(Cmd::Action(a.clone())),
            EntryCmd::Session(id) => x.cmds.push(Cmd::OpenSession { id: id.clone(), admin: false }),
            EntryCmd::Snippet(id) => {
                if let Some(s) = x.core.sessions.snippets().into_iter().find(|s| &s.id == id) {
                    x.cmds.push(Cmd::RunSnippet(s));
                }
            }
            EntryCmd::Workspace(id) => {
                if let Some(w) = x.core.workspaces.get(id) {
                    x.cmds.push(Cmd::OpenWorkspace(w));
                }
            }
            EntryCmd::Tab(id) => x.cmds.push(Cmd::Action(format!("goTab:{id}"))),
            EntryCmd::Theme(n) => {
                let _ = x.core.settings.patch(serde_json::json!({ "theme": { "preset": n, "overrides": null } })); // null (not {}) clears: a merge patch with {} is a no-op
            }
            EntryCmd::SettingsFile => x.cmds.push(Cmd::Action("openSettingsFile".into())),
            EntryCmd::Diagnostics => x.cmds.push(Cmd::Action("diagnostics".into())),
        }
    }

    pub fn show(&mut self, ctx: &Context, x: &mut PanelCtx) {
        if !self.open {
            return;
        }
        let mut entries = self.entries(x);
        let q = self.query.trim().to_string();
        let mut scored: Vec<(i32, Entry)> = entries
            .drain(..)
            .filter_map(|e| {
                let s = fuzzy(&q, &e.label)?;
                let r = self.recent.iter().position(|r| r == &e.id).map_or(0, |r| 40 - 3 * r as i32);
                Some((s + r, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
        scored.truncate(60);
        self.sel = self.sel.min(scored.len().saturating_sub(1));

        let mut chosen: Option<usize> = None;
        let mut close = false;
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            close = true;
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowDown)) && !scored.is_empty() {
            self.sel = (self.sel + 1) % scored.len();
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowUp)) && !scored.is_empty() {
            self.sel = (self.sel + scored.len() - 1) % scored.len();
        }
        if ctx.input(|i| i.key_pressed(Key::Enter)) && !scored.is_empty() {
            chosen = Some(self.sel);
        }
        let keymap = x.core.keys.lock().effective();
        egui::Window::new("palette")
            .title_bar(false)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 70.0))
            .fixed_size(Vec2::new(480.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let te = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Type a command…").desired_width(f32::INFINITY).font(egui::TextStyle::Heading));
                if self.focus {
                    te.request_focus();
                    self.focus = false;
                }
                if te.changed() {
                    self.sel = 0;
                }
                ui.separator();
                egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, (_, e)) in scored.iter().enumerate() {
                        let selected = i == self.sel;
                        let hint = e.action.as_ref().and_then(|a| keymap.get(a)).and_then(|c| c.first()).cloned().unwrap_or_default();
                        let r = ui.horizontal(|ui| {
                            ui.set_width(ui.available_width());
                            let resp = ui.selectable_label(selected, &e.label);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(hint).small().weak()));
                            resp
                        });
                        if selected && ctx.input(|i| i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowUp)) {
                            r.inner.scroll_to_me(None);
                        }
                        if r.inner.clicked() {
                            chosen = Some(i);
                        }
                        if r.inner.hovered() && ctx.input(|i| i.pointer.delta().length() > 0.0) {
                            self.sel = i;
                        }
                    }
                });
            });
        // Theme live preview of the selected entry (reverts on close).
        let want = scored.get(self.sel).and_then(|(_, e)| if let EntryCmd::Theme(n) = &e.cmd { Some(n.clone()) } else { None });
        if want != self.last_preview {
            self.last_preview = want.clone();
            x.cmds.push(Cmd::PreviewTheme(want.map(|n| ut_core::effective_with(&ut_core::ThemeRef { preset: n, overrides: Default::default() }, &x.core.user_themes.read()))));
        }
        if let Some(i) = chosen {
            let (_, e) = scored.remove(i);
            self.open = false;
            x.cmds.push(Cmd::PreviewTheme(None));
            self.last_preview = None;
            self.run(&e, x);
            x.cmds.push(Cmd::FocusTerminal);
        } else if close || ctx.input(|i| i.viewport().focused == Some(false)) {
            self.open = false;
            self.last_preview = None;
            x.cmds.push(Cmd::PreviewTheme(None));
            x.cmds.push(Cmd::FocusTerminal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_word_starts_and_runs() {
        let a = fuzzy("nt", "New Tab").unwrap();
        let b = fuzzy("nt", "Open Settings").unwrap_or(-999);
        assert!(a > b);
        assert!(fuzzy("zzz", "New Tab").is_none());
        assert!(fuzzy("new", "New Tab").unwrap() > fuzzy("nta", "New Tab").unwrap());
        assert_eq!(fuzzy("", "anything"), Some(0));
    }
}
