//! Actions: shortcut/palette commands, tab and pane operations, clipboard, dialogs and native events.

use crate::app::{App, BannerAction, DialogId, MenuKind, MenuState, NewTab};
use crate::clipboard;
use crate::core::{BannerKind, LaunchReq};
use crate::kit::{Button, ButtonKind, Dialog, DialogResult, ToastKind};
use crate::panels::Cmd;
use crate::tab::{SplitDir, TabOut};
use crate::theme::{parse_color, Theme};
use crate::winhooks::{ScrollAction, TrayCmd};
use ut_term::{Scroll, TermMode};
use egui::{Context, ViewportCommand};
use std::sync::Arc;
use ut_term::input::{encode_key, Mods};
use ut_term::PaneEvent;

const FUNNY: [&str; 24] = [
    "Quantum Potato", "Void Chicken", "Turbo Waffle", "Cosmic Pickle", "Neon Walrus", "Spicy Nebula", "Gravity Noodle", "Laser Badger",
    "Pixel Llama", "Atomic Muffin", "Hyper Teapot", "Rocket Cabbage", "Sonic Biscuit", "Plasma Penguin", "Glitch Otter", "Orbital Taco",
    "Fuzzy Kernel", "Binary Banana", "Crispy Daemon", "Lunar Pancake", "Stealth Pretzel", "Mighty Mango", "Cyber Koala", "Zen Toaster",
];

pub fn funny_name() -> &'static str {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as usize).unwrap_or(0);
    FUNNY[t % FUNNY.len()]
}

const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;

impl App {
    // ------------------------------------------------------------------------------ panel commands

    pub fn run_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::OpenSession { id, admin } => self.open_session(&id, admin),
            Cmd::OpenProfile(p) => {
                let mut d = Dialog::prompt("New tab", "Tab name", funny_name(), true);
                d.width = 360.0;
                self.dialog = Some((DialogId::NewTabName { command: p.command, color: p.color }, d));
            }
            Cmd::OpenCommand { command, title } => {
                let req = LaunchReq { command: Some(command.clone()), cwd: Some(ut_fs::home_dir().to_string_lossy().into_owned()), ..Default::default() };
                self.add_tab(NewTab::simple(&title, &command, req), true);
            }
            Cmd::RunSnippet(s) => self.run_snippet(&s.command, s.append_enter),
            Cmd::OpenWorkspace(w) => self.open_workspace_ask(&w),
            Cmd::Action(a) => self.run_action(&a),
            Cmd::PreviewTheme(Some(t)) => {
                self.saved_theme.get_or_insert_with(|| Theme::from(&self.core.theme()));
                self.theme = Theme::from(&t);
                self.theme.apply(&self.ctx);
                self.push_palette();
            }
            Cmd::PreviewTheme(None) => {
                if self.saved_theme.take().is_some() {
                    self.theme = Theme::from(&self.core.theme());
                    self.theme.apply(&self.ctx);
                    self.push_palette();
                }
            }
            Cmd::FocusTerminal => self.refocus = true,
            Cmd::SendSelectionToBrowser => self.browser_send_selection(),
            Cmd::RefreshShells => {
                let core = self.core.clone();
                std::thread::spawn(move || {
                    core.shells.get(true);
                });
            }
            Cmd::ReloadKeymap => self.apply_keymap(),
        }
    }

    /// Push the live keymap (`core.keys`, defaults + keybindings.json) into the keyboard router as Win32 virtual keys.
    /// `Win+…` chords are global hotkeys (settings.quake.hotkey), not router shortcuts.
    pub fn apply_keymap(&self) {
        let eff = self.core.keys.lock().effective();
        let mut table = Vec::new();
        for (action, chords) in &eff {
            for c in chords.iter().filter_map(|c| c.parse::<ut_core::Chord>().ok()).filter(|c| !c.win) {
                table.push(ut_term::input::Binding::new(c.ctrl, c.shift, c.alt, &c.key, action.clone()));
            }
        }
        self.browser.set_shortcuts(&table);
        self.router.set_shortcuts(table);
    }

    // ------------------------------------------------------------------------------ actions

    pub fn run_action(&mut self, action: &str) {
        self.run_action_vk(action, None, Mods::default());
    }

    /// Shortcut actions queued by the keyboard router: keys the action declines are written to the shell.
    pub fn run_action_vk(&mut self, action: &str, vk: Option<u16>, m: Mods) {
        if !self.do_action(action, vk) {
            if let (Some(vk), Some(t)) = (vk, self.tabs.get(self.active)) {
                if let Some(s) = t.focused_pane().and_then(|p| p.session.clone()) {
                    if let Some(b) = encode_key(vk, m, s.mode()) {
                        s.write(&b);
                    }
                }
            }
        }
    }

    /// `true` = consumed.
    fn do_action(&mut self, action: &str, vk: Option<u16>) -> bool {
        if let Some(id) = action.strip_prefix("goTab:") {
            if let Some(i) = id.parse::<u64>().ok().and_then(|id| self.tab_index(id)) {
                self.activate(i);
            }
            return true;
        }
        if let Some(n) = action.strip_prefix("selectTabNumpad").and_then(|n| n.parse::<usize>().ok()) {
            let i = if n == 0 { 9 } else { n - 1 };
            return self.select_tab_n(i);
        }
        if let Some(n) = action.strip_prefix("selectTab").and_then(|n| n.parse::<usize>().ok()) {
            return self.select_tab_n(n - 1);
        }
        let cur = self.active;
        match action {
            "newTab" => self.new_default_tab(),
            "closePane" => self.close_focused_pane(),
            "togglePanel" => {
                self.sidebar_open = !self.sidebar_open;
                if self.sidebar_open {
                    self.sidebar.focus_search();
                } else {
                    self.refocus = true;
                }
                self.mark_dirty();
            }
            "toggleBrowser" => self.toggle_browser(),
            "settings" => self.settings_ui.open(),
            "nextTab" | "prevTab" => {
                if self.tabs.len() > 1 {
                    let n = self.tabs.len();
                    let i = if action == "nextTab" { (cur + 1) % n } else { (cur + n - 1) % n };
                    self.activate(i);
                }
            }
            "newSession" => {
                self.sidebar_open = true;
                self.sidebar.new_session();
            }
            "duplicateTab" => self.duplicate_tab(cur),
            "commandPalette" => self.palette.open(),
            "quickConnect" => {
                let mut d = Dialog::new("Quick SSH Connect", "", vec![Button::new("Cancel", ButtonKind::Normal), Button::new("Save as session", ButtonKind::Normal), Button::new("Connect", ButtonKind::Primary)]);
                d.input = Some(String::new());
                d.label = "Quick SSH Connect (user@host or user@host:port)".into();
                d.width = 460.0;
                self.dialog = Some((DialogId::QuickConnect, d));
            }
            "movePaneFocus" => {
                let (dx, dy) = match vk {
                    Some(VK_LEFT) => (-1, 0),
                    Some(VK_RIGHT) => (1, 0),
                    Some(VK_UP) => (0, -1),
                    Some(VK_DOWN) => (0, 1),
                    _ => return false,
                };
                // Passes through to the shell when there is no pane that way (PSReadLine word selection).
                let moved = self.tabs.get_mut(cur).is_some_and(|t| t.move_focus(dx, dy));
                if moved {
                    self.refocus = true;
                }
                return moved;
            }
            "prevCommand" | "nextCommand" => {
                let dir = if action == "prevCommand" { -1 } else { 1 };
                if let Some(p) = self.tabs.get_mut(cur).and_then(|t| t.focused_pane_mut()) {
                    if let Some(s) = p.session.clone() {
                        let mut anchor = *self.anchor.lock();
                        s.goto_mark(dir, &mut anchor);
                        *self.anchor.lock() = anchor;
                    }
                }
            }
            "search" | "findAllTabs" => {
                if let Some(p) = self.tabs.get_mut(cur).and_then(|t| t.focused_pane_mut()) {
                    let s = p.session.clone();
                    p.open_search(s.as_deref());
                }
            }
            "exportBuffer" => self.export_buffer(),
            "copy" => return self.copy_selection(true),
            "paste" => self.paste_into_focused(),
            "splitRight" => self.split_focused(SplitDir::Right),
            "splitDown" => self.split_focused(SplitDir::Down),
            "addPane" => self.add_pane(),
            "unsplitAll" => {
                let kill = self.cfg.processes.kill_console_tree_on_close;
                if let Some(t) = self.tabs.get_mut(cur) {
                    t.unsplit_all(kill);
                }
                self.mark_dirty();
            }
            "zoomIn" => self.zoom_focused(1),
            "zoomOut" => self.zoom_focused(-1),
            "zoomReset" => self.zoom_focused(0),
            "scrollPageUp" | "scrollPageDown" => {
                if let Some(s) = self.focused_session() {
                    s.scroll(if action == "scrollPageUp" { Scroll::PageUp } else { Scroll::PageDown });
                }
            }
            "renameTab" => {
                if let Some(t) = self.tabs.get(cur) {
                    let mut d = Dialog::prompt("Rename tab", "", &t.title, true);
                    d.label = "Tab name (empty = automatic)".into();
                    self.dialog = Some((DialogId::RenameTab(t.id), d));
                }
            }
            "pinTab" => self.toggle_pin(cur),
            "broadcastToggle" => {
                if let Some(t) = self.tabs.get_mut(cur) {
                    t.broadcast = !t.broadcast;
                }
            }
            "closeOthers" => self.close_others(cur),
            "closeRight" => self.close_right(cur),
            "quake" => self.toggle_window(&self.ctx.clone()),
            "toggleLog" => self.toggle_logging(cur),
            "toggleReadOnly" => {
                if let Some(t) = self.tabs.get_mut(cur) {
                    t.read_only = !t.read_only;
                    for p in &mut t.panes {
                        p.read_only = t.read_only;
                    }
                }
                self.mark_dirty();
            }
            "toggleRecording" => self.toggle_recording(cur),
            "toggleCrt" => {
                let v = !self.cfg.terminal.crt;
                let _ = self.core.settings.patch(serde_json::json!({ "terminal": { "crt": v } }));
            }
            "toggleMinimap" => {
                let v = !self.cfg.terminal.minimap;
                let _ = self.core.settings.patch(serde_json::json!({ "terminal": { "minimap": v } }));
            }
            "saveWorkspace" => {
                self.dialog = Some((DialogId::WorkspaceName, Dialog::prompt("Save current tabs as workspace", "Workspace name", "My Workspace", false)));
            }
            "sendToBrowser" => self.run_cmd(Cmd::SendSelectionToBrowser),
            "restartPane" => self.restart_focused(),
            "openSettingsFile" => {
                let _ = crate::sys::shell_open(&self.core.settings.path().to_string_lossy());
            }
            "diagnostics" => self.diag_open = true,
            _ => return false,
        }
        true
    }

    fn select_tab_n(&mut self, i: usize) -> bool {
        if i < self.tabs.len() {
            self.activate(i);
            true
        } else {
            false // consumed only if that tab exists
        }
    }

    pub fn focused_session(&self) -> Option<Arc<ut_term::TermSession>> {
        self.tabs.get(self.active).and_then(|t| t.focused_pane()).and_then(|p| p.session.clone())
    }

    // ------------------------------------------------------------------------------ panes

    fn split_focused(&mut self, dir: SplitDir) {
        let id = self.alloc_pane_id();
        if let Some(t) = self.tabs.get_mut(self.active) {
            if t.split(dir, None, id).is_some() {
                self.refocus = true;
                self.mark_dirty();
            }
        }
    }

    /// P0 parity layout: "Add Pane" fills 1→2→3→4 in the fixed arrangement, then keeps splitting right.
    fn add_pane(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let ids: Vec<u64> = t.panes.iter().map(|p| p.id).collect();
        let (dir, from) = match ids.len() {
            1 => (SplitDir::Right, ids[0]),
            2 => (SplitDir::Down, ids[0]),
            3 => (SplitDir::Right, ids[2]),
            _ => (SplitDir::Right, t.focused),
        };
        let id = self.alloc_pane_id();
        if let Some(t) = self.tabs.get_mut(self.active) {
            if t.split(dir, Some(from), id).is_some() {
                self.refocus = true;
                self.mark_dirty();
            }
        }
    }

    fn close_focused_pane(&mut self) {
        let kill = self.cfg.processes.kill_console_tree_on_close;
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if t.pinned && t.panes.len() == 1 {
            return; // Ctrl+W does nothing on a pinned single-pane tab
        }
        if t.panes.len() == 1 {
            self.remove_tab(self.active);
        } else {
            let f = t.focused;
            t.close_pane(f, kill);
            self.refocus = true;
            self.mark_dirty();
        }
    }

    fn restart_focused(&mut self) {
        let mut out = Vec::new();
        let idx = self.active;
        let Some(tab) = self.tabs.get(idx) else { return };
        let (fid, active) = (tab.focused, true);
        // Temporarily take the pane out so `pane_env` can borrow `self` immutably.
        let Some(mut pane) = self.tabs[idx].panes.iter().position(|p| p.id == fid).map(|i| std::mem::replace(&mut self.tabs[idx].panes[i], crate::tab::Pane::new(fid, LaunchReq::default()))) else { return };
        {
            let mut env = self.pane_env(&mut out, active);
            pane.restart(&mut env);
        }
        if let Some(i) = self.tabs[idx].panes.iter().position(|p| p.id == fid) {
            self.tabs[idx].panes[i] = pane;
        }
    }

    // ------------------------------------------------------------------------------ tabs

    pub fn duplicate_tab(&mut self, i: usize) {
        let Some(t) = self.tabs.get(i) else { return };
        // Copies EVERYTHING about the source: environment, theme and font overrides, starting command, session id.
        let req = LaunchReq {
            command: Some(t.command.clone()),
            session_id: t.session_id.clone(),
            cwd: t.live_cwd().or_else(|| t.cwd.clone()),
            starting_command: t.starting_command.clone(),
            ..t.focused_pane().map(|p| p.req.clone()).unwrap_or_default()
        };
        let nt = NewTab {
            title: t.title.clone(),
            locked: t.title_locked,
            color: t.color,
            command: t.command.clone(),
            req: LaunchReq { cwd: t.live_cwd().or_else(|| t.cwd.clone()), ..req },
            pinned: false,
            group: t.group.clone(),
            read_only: t.read_only,
            font_override: t.font_override,
            theme_bg: t.theme_bg,
            layout: None,
        };
        self.add_tab(nt, true);
    }

    pub fn toggle_pin(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let was_active = self.tabs[self.active].id;
        let mut t = self.tabs.remove(i);
        t.pinned = !t.pinned;
        // Pinning moves the tab to just after the existing pinned tabs.
        let at = self.tabs.iter().filter(|x| x.pinned).count();
        self.tabs.insert(at, t);
        self.active = self.tabs.iter().position(|x| x.id == was_active).unwrap_or(0);
        self.mark_dirty();
    }

    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || from == to {
            return;
        }
        let active = self.tabs[self.active].id;
        let t = self.tabs.remove(from);
        self.tabs.insert(to.min(self.tabs.len()), t);
        self.active = self.tabs.iter().position(|x| x.id == active).unwrap_or(0);
        self.mark_dirty();
    }

    pub fn remove_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let kill = self.cfg.processes.kill_console_tree_on_close;
        let active_id = self.tabs[self.active].id;
        let mut t = self.tabs.remove(i);
        let ids: Vec<u64> = t.panes.iter().map(|p| p.id).collect();
        for id in ids {
            t.close_pane(id, kill);
        }
        self.active = if self.tabs.is_empty() { 0 } else if t.id == active_id { i.min(self.tabs.len() - 1) } else { self.tabs.iter().position(|x| x.id == active_id).unwrap_or(0) };
        self.refocus = true;
        self.scroll_to_active = true;
        self.mark_dirty();
        // Closing the last tab closes the window; the saved state has `tabs: []` so the next launch opens a default tab.
        if self.tabs.is_empty() && !self.replacing {
            self.quit();
        }
    }

    fn running_count(&self, idx: &[usize]) -> usize {
        idx.iter().filter_map(|&i| self.tabs.get(i)).map(|t| t.any_running()).sum()
    }

    /// Confirm only when something is running (§7.9).
    fn confirm_close(&mut self, running: usize, what: &str, id: DialogId) -> bool {
        use ut_core::settings::CloseConfirm::*;
        let mode = self.cfg.processes.close_confirm;
        if mode == Never || (mode == WhenRunning && running == 0) {
            return true;
        }
        let msg = if running == 0 { format!("Close {what}?") } else { format!("{running} terminal session{} still running. Close anyway?", if running == 1 { " is" } else { "s are" }) };
        self.dialog = Some((id, Dialog::confirm("Close", &msg, "Close", true)));
        false
    }

    pub fn close_tab(&mut self, i: usize, force: bool) {
        let Some(t) = self.tabs.get(i) else { return };
        let (id, multi) = (t.id, t.panes.len() > 1);
        if !force && multi && !self.confirm_close(self.running_count(&[i]), "this tab", DialogId::CloseTab(id)) {
            return;
        }
        self.remove_tab(i);
    }

    pub fn close_others(&mut self, keep: usize) {
        let Some(id) = self.tabs.get(keep).map(|t| t.id) else { return };
        let idx: Vec<usize> = (0..self.tabs.len()).filter(|&i| i != keep && !self.tabs[i].pinned).collect();
        let what = format!("{} tab{}", idx.len(), if idx.len() == 1 { "" } else { "s" });
        if idx.is_empty() || !self.confirm_close(self.running_count(&idx), &what, DialogId::CloseOthers(id)) {
            return;
        }
        self.close_ids(idx);
    }

    pub fn close_right(&mut self, of: usize) {
        let Some(id) = self.tabs.get(of).map(|t| t.id) else { return };
        let idx: Vec<usize> = (of + 1..self.tabs.len()).filter(|&i| !self.tabs[i].pinned).collect();
        let what = format!("{} tab{}", idx.len(), if idx.len() == 1 { "" } else { "s" });
        if idx.is_empty() || !self.confirm_close(self.running_count(&idx), &what, DialogId::CloseRight(id)) {
            return;
        }
        self.close_ids(idx);
    }

    fn close_ids(&mut self, mut idx: Vec<usize>) {
        idx.sort_unstable_by(|a, b| b.cmp(a));
        for i in idx {
            self.remove_tab(i);
        }
    }

    pub fn open_session(&mut self, id: &str, admin: bool) {
        let Some(s) = self.core.sessions.get(id) else { return };
        if admin && !self.core.elevated {
            let (exe, args) = ut_shell::split_exe_and_args(&s.full_command());
            let cwd = s.resolved_cwd(&[]);
            match ut_pty::elevate::run_elevated(&exe, &args, Some(&cwd)) {
                Ok(()) => self.toasts.push("Opened in a separate elevated console. The starting command, environment, theme and shell integration are not applied.", ToastKind::Info),
                Err(e) => self.toasts.push(e, ToastKind::Error),
            }
            return;
        }
        let req = LaunchReq { command: Some(s.full_command()), session_id: Some(s.id.clone()), ..Default::default() };
        let nt = NewTab {
            title: s.name.clone(),
            locked: true,
            color: Some(parse_color(&s.color_tag)),
            command: s.full_command(),
            req,
            pinned: false,
            group: String::new(),
            read_only: false,
            font_override: s.font_size as f32,
            theme_bg: Some(s.theme_background.clone()).filter(|b| !b.is_empty()).map(|b| parse_color(&b)),
            layout: None,
        };
        self.add_tab(nt, true);
    }

    pub fn open_workspace_ask(&mut self, w: &ut_data::Workspace) {
        if self.tabs.is_empty() {
            return self.open_workspace(w, false);
        }
        let d = Dialog::new(
            "Open workspace",
            &format!("Open \"{}\" by replacing the current tabs or adding to this window?", w.name),
            vec![Button::new("Cancel", ButtonKind::Normal), Button::new("Add to window", ButtonKind::Normal), Button::new("Replace current tabs", ButtonKind::Primary)],
        );
        self.dialog = Some((DialogId::OpenWorkspace(w.id.clone()), d));
    }

    pub fn open_workspace(&mut self, w: &ut_data::Workspace, replace: bool) {
        if replace {
            self.replacing = true; // do not quit when the last tab goes away
            while !self.tabs.is_empty() {
                self.remove_tab(0);
            }
            self.replacing = false;
        }
        let first = self.tabs.len();
        for wt in &w.tabs {
            let session = wt.session_id.as_ref().and_then(|id| self.core.sessions.get(id));
            let command = if !wt.command.is_empty() { wt.command.clone() } else { session.as_ref().map(|s| s.full_command()).unwrap_or_default() };
            let req = LaunchReq {
                command: Some(command.clone()),
                session_id: session.as_ref().map(|s| s.id.clone()),
                cwd: wt.cwd.clone(),
                starting_command: if session.is_some() { None } else { wt.starting_command.clone() },
                ..Default::default()
            };
            let mut nt = NewTab::simple(if wt.title.is_empty() { "Terminal" } else { &wt.title }, &command, req);
            nt.locked = true;
            nt.color = wt.color.as_deref().map(parse_color).or_else(|| session.as_ref().map(|s| parse_color(&s.color_tag)));
            if let Some(s) = &session {
                nt.font_override = s.font_size as f32;
            }
            nt.layout = wt.layout.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok());
            self.add_tab(nt, false);
        }
        if first < self.tabs.len() {
            self.activate(first);
        }
    }

    fn save_workspace(&mut self, name: &str) {
        let tabs = self
            .tabs
            .iter()
            .map(|t| ut_data::WorkspaceTab {
                session_id: t.session_id.clone(),
                title: t.title.clone(),
                command: t.command.clone(),
                cwd: t.live_cwd().or_else(|| t.cwd.clone()),
                starting_command: t.starting_command.clone(),
                color: t.color.map(|c| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())),
                layout: serde_json::to_value(self.to_pane_layout(t, &t.layout)).ok(),
            })
            .collect();
        let w = ut_data::Workspace { id: String::new(), name: name.to_string(), tabs };
        match self.core.workspaces.add(w) {
            Ok(_) => self.toasts.push(format!("Workspace \"{name}\" saved."), ToastKind::Success),
            Err(e) => self.toasts.push(e.to_string(), ToastKind::Error),
        }
    }

    // ------------------------------------------------------------------------------ clipboard

    pub fn copy_selection(&mut self, clear: bool) -> bool {
        let Some(s) = self.focused_session() else { return false };
        let Some(mut t) = s.selection_text() else { return false };
        if self.cfg.terminal.trim_trailing_whitespace_on_copy {
            t = t.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n");
        }
        clipboard::set_text(&t);
        if clear && self.cfg.terminal.clear_selection_on_copy {
            s.clear_selection();
        }
        true
    }

    pub fn paste_into_focused(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(p) = t.focused_pane() else { return };
        if p.read_only || t.read_only || p.exited {
            return;
        }
        let (pane, kind) = (p.id, p.kind);
        let Some(s) = p.session.clone() else { return };
        let c = clipboard::read();
        if let Some(text) = c.text {
            use ut_core::settings::MultiLinePasteWarning::*;
            let multi = text.trim_end_matches(['\r', '\n']).contains(['\r', '\n']);
            let bracketed = s.mode().contains(TermMode::BRACKETED_PASTE);
            let warn = match self.cfg.terminal.multi_line_paste_warning {
                Always => multi,
                Auto => multi && !bracketed,
                Never => false,
            };
            if warn {
                let lines: Vec<&str> = text.lines().collect();
                let mut preview = lines.iter().take(5).copied().collect::<Vec<_>>().join("\n");
                if lines.len() > 5 {
                    preview.push_str(&format!("\n… ({} more lines)", lines.len() - 5));
                }
                let mut d = Dialog::confirm("Paste multiple lines", &format!("Paste {} lines? Each line may run as a command.", lines.len()), "Paste", false);
                d.preview = Some(preview);
                d.default = 0;
                self.dialog = Some((DialogId::PasteMulti { pane, text }, d));
            } else {
                self.write_paste(pane, &text);
            }
        } else if !c.files.is_empty() {
            let q = ut_shell::quote_paths(kind, &c.files);
            self.write_paste(pane, &format!("{q} "));
        } else if c.has_image {
            // Clipboard-aware CLIs (Claude Code, Codex) read the image from the OS clipboard on Ctrl+V (§5.6).
            s.write(&[0x16]);
        }
    }

    pub fn write_paste(&mut self, pane: u64, text: &str) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(p) = t.pane(pane) else { return };
        let Some(s) = p.session.clone() else { return };
        let bytes = clipboard::paste_bytes(text, s.mode().contains(TermMode::BRACKETED_PASTE));
        self.write_to_targets(pane, &bytes);
    }

    /// Bytes to `pane` and, when broadcast is on, to every other writable pane of the tab.
    fn write_to_targets(&mut self, pane: u64, bytes: &[u8]) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(s) = t.pane(pane).and_then(|p| p.session.clone()) else { return };
        s.write(bytes);
        if t.broadcast {
            for o in t.panes.iter().filter(|o| o.id != pane && !o.read_only && !o.exited) {
                if let Some(os) = &o.session {
                    os.write(bytes);
                }
            }
        }
    }

    pub fn run_snippet(&mut self, command: &str, enter: bool) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(p) = t.focused_pane() else { return };
        let pane = p.id;
        // `write_paste` honours bracketed paste and fans out to every broadcast target.
        self.write_paste(pane, command);
        if enter {
            // §8.2: the Enter goes AFTER the paste; inside bracketed paste a shell only inserts a newline.
            self.write_to_targets(pane, b"\r");
        }
        self.refocus = true;
    }

    fn export_buffer(&mut self) {
        let Some(s) = self.focused_session() else { return };
        let text = s.buffer_text();
        let f = rfd::FileDialog::new().set_file_name("terminal-output.txt").add_filter("Text", &["txt"]).add_filter("Log", &["log"]).add_filter("All files", &["*"]).save_file();
        if let Some(p) = f {
            match std::fs::write(&p, text) {
                Ok(()) => self.toasts.push(format!("Saved {}", p.display()), ToastKind::Success),
                Err(e) => self.toasts.push(e.to_string(), ToastKind::Error),
            }
        }
    }

    // ------------------------------------------------------------------------------ zoom / taps

    pub fn zoom_focused(&mut self, dir: i32) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let Some(p) = t.focused_pane_mut() else { return };
        let cur = if p.font_override >= 8.0 { p.font_override } else { self.cfg.terminal.font_size as f32 };
        let next = if dir == 0 { 14.0 } else { (cur + dir as f32).clamp(8.0, 32.0) };
        if next == cur {
            return;
        }
        if self.cfg.terminal.zoom_scope == ut_core::settings::ZoomScope::Pane || p.font_override >= 8.0 {
            p.font_override = next;
        } else {
            // Global: only `{fontSize}` is ever patched (saved with a 500 ms debounce by the store, §5.9).
            let _ = self.core.settings.patch(serde_json::json!({ "terminal": { "fontSize": next } }));
        }
    }

    pub fn ui_zoom(&mut self, dir: i32) {
        let s = (((self.cfg.ui.scale + dir as f64 * 0.05) * 20.0).round() / 20.0).clamp(0.75, 2.0);
        let _ = self.core.settings.patch(serde_json::json!({ "ui": { "scale": s } }));
    }

    fn toggle_logging(&mut self, i: usize) {
        let (dir, raw) = (self.core.log_dir(), self.cfg.logging.format == ut_core::settings::LogFormat::Raw);
        let Some(t) = self.tabs.get_mut(i) else { return };
        let multi = t.panes.len() > 1;
        let want = !t.logging;
        let mut note = None;
        for (n, p) in t.panes.iter().enumerate() {
            if let Some(s) = p.session.as_ref().filter(|_| !p.exited) {
                if s.logging() == want {
                    continue;
                }
                match s.toggle_log(&dir, &t.title, multi.then(|| format!("[{}] ", n + 1)), raw) {
                    Ok((true, path)) if n == 0 => note = Some(format!("Logging to {}", path.display())),
                    Err(e) => note = Some(format!("Logging failed: {e}")),
                    _ => {}
                }
            }
        }
        t.logging = want;
        if let Some(n) = note {
            self.toasts.push(n, ToastKind::Info);
        }
    }

    fn toggle_recording(&mut self, i: usize) {
        let dir = self.core.recordings_dir();
        let capture = self.cfg.recording.capture_input;
        let Some(t) = self.tabs.get(i) else { return };
        let Some(s) = t.focused_pane().and_then(|p| p.session.clone()) else { return };
        match s.toggle_record(&dir, &t.title, capture) {
            Ok((on, path)) => self.toasts.push(if on { format!("Recording to {}", path.display()) } else { format!("Saved {}", path.display()) }, ToastKind::Info),
            Err(e) => self.toasts.push(format!("Recording failed: {e}"), ToastKind::Error),
        }
    }

    // ------------------------------------------------------------------------------ dialogs

    pub fn show_dialog(&mut self, ctx: &Context) {
        let Some((id, mut d)) = self.dialog.take() else { return };
        match d.show(ctx, &self.theme) {
            None => self.dialog = Some((id, d)),
            Some(r) => {
                self.refocus = true;
                self.on_dialog(id, r);
            }
        }
    }

    fn on_dialog(&mut self, id: DialogId, r: DialogResult) {
        let (btn, text) = match r {
            DialogResult::Button(i, t) => (i as i32, t),
            DialogResult::Dismissed => (-1, String::new()),
        };
        match id {
            DialogId::CloseWindow if btn == 1 => self.quit(),
            DialogId::CloseTab(tid) if btn == 1 => {
                if let Some(i) = self.tab_index(tid) {
                    self.remove_tab(i);
                }
            }
            DialogId::CloseOthers(tid) if btn == 1 => {
                if let Some(keep) = self.tab_index(tid) {
                    let idx: Vec<usize> = (0..self.tabs.len()).filter(|&i| i != keep && !self.tabs[i].pinned).collect();
                    self.close_ids(idx);
                }
            }
            DialogId::CloseRight(tid) if btn == 1 => {
                if let Some(of) = self.tab_index(tid) {
                    let idx: Vec<usize> = (of + 1..self.tabs.len()).filter(|&i| !self.tabs[i].pinned).collect();
                    self.close_ids(idx);
                }
            }
            DialogId::RenameTab(tid) if btn == 1 => {
                if let Some(t) = self.tab_index(tid).and_then(|i| self.tabs.get_mut(i)) {
                    if text.is_empty() {
                        t.title_locked = false; // empty unlocks the title again
                        t.title = t.focused_pane().map(|p| p.title.clone()).filter(|x| !x.is_empty()).unwrap_or_else(|| "Terminal".into());
                    } else {
                        t.title = text;
                        t.title_locked = true;
                    }
                }
                self.mark_dirty();
            }
            DialogId::SetGroup(tid) if btn == 1 => {
                if let Some(t) = self.tab_index(tid).and_then(|i| self.tabs.get_mut(i)) {
                    t.group = text;
                }
                self.mark_dirty();
            }
            DialogId::PasteMulti { pane, text } if btn == 1 => self.write_paste(pane, &text),
            DialogId::WorkspaceName if btn == 1 && !text.is_empty() => self.save_workspace(&text),
            DialogId::OpenWorkspace(wid) if btn >= 1 => {
                if let Some(w) = self.core.workspaces.get(&wid) {
                    self.open_workspace(&w, btn == 2);
                }
            }
            DialogId::SaveSession(tid) if btn == 1 && !text.is_empty() => {
                if let Some(t) = self.tab_index(tid).and_then(|i| self.tabs.get(i)) {
                    let s = ut_data::Session {
                        name: text,
                        shell_path: t.command.clone(),
                        working_directory: t.live_cwd().or_else(|| t.cwd.clone()).unwrap_or_default(),
                        starting_command: t.starting_command.clone().unwrap_or_default(),
                        color_tag: t.color.map(|c| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())).unwrap_or_else(|| "#00ff44".into()),
                        ..ut_data::Session::default()
                    };
                    match self.core.sessions.add_session(s) {
                        Ok(s) => self.toasts.push(format!("Saved session \"{}\".", s.name), ToastKind::Success),
                        Err(e) => self.toasts.push(e.to_string(), ToastKind::Error),
                    }
                }
            }
            DialogId::NewTabName { command, color } if btn == 1 => {
                let req = LaunchReq { command: Some(command.clone()), cwd: Some(ut_fs::home_dir().to_string_lossy().into_owned()), ..Default::default() };
                let mut nt = NewTab::simple(if text.is_empty() { "Terminal" } else { &text }, &command, req);
                nt.locked = !text.is_empty();
                nt.color = Some(parse_color(&color));
                self.add_tab(nt, true);
            }
            DialogId::QuickConnect if btn >= 1 && !text.is_empty() => self.quick_connect(&text, btn == 1),
            DialogId::DropConflict => self.drops.resolve(match btn {
                1 => ut_ssh::transfer::Resolution::Rename,
                2 => ut_ssh::transfer::Resolution::Replace,
                _ => ut_ssh::transfer::Resolution::Cancel,
            }),
            DialogId::OpenLink(url) if btn == 1 => {
                if let Err(e) = crate::sys::shell_open(&url) {
                    self.toasts.push(e, ToastKind::Error);
                }
            }
            _ => {}
        }
    }

    fn quick_connect(&mut self, input: &str, save: bool) {
        let Some(ssh) = ut_ssh::locate::find_ssh() else {
            self.toasts.push("ssh.exe was not found.", ToastKind::Error);
            return;
        };
        let Some(cl) = ut_ssh::quick::quick_command_line(&ssh, input) else {
            self.toasts.push("That is not a valid SSH target.", ToastKind::Error);
            return;
        };
        self.core.remember_ssh(input);
        let title = format!("SSH: {input}");
        if save {
            let s = ut_data::Session { name: title.clone(), shell_path: cl.clone(), color_tag: "#6be5ff".into(), ..ut_data::Session::default() };
            if let Ok(s) = self.core.sessions.add_session(s) {
                self.toasts.push(format!("Saved session \"{}\".", s.name), ToastKind::Success);
            }
        }
        let req = LaunchReq { command: Some(cl.clone()), cwd: Some(ut_fs::home_dir().to_string_lossy().into_owned()), ..Default::default() };
        let mut nt = NewTab::simple(&title, &cl, req);
        nt.color = Some(parse_color("#6be5ff"));
        self.add_tab(nt, true);
    }

    pub fn show_banners_actions(&mut self, a: BannerAction) {
        match a {
            BannerAction::OpenSettingsFile => {
                let _ = crate::sys::shell_open(&self.core.settings.path().to_string_lossy());
            }
            BannerAction::ResetSettings => match self.core.settings.reset_with_backup() {
                Ok(_) => self.toasts.push("Settings were reset (a backup was kept).", ToastKind::Success),
                Err(e) => self.toasts.push(e, ToastKind::Error),
            },
        }
    }

    // ------------------------------------------------------------------------------ close / window

    pub fn request_close(&mut self) {
        if self.closing {
            return;
        }
        let all: Vec<usize> = (0..self.tabs.len()).collect();
        let n = self.running_count(&all);
        if self.confirm_close(n, "the window", DialogId::CloseWindow) {
            self.quit();
        }
    }

    pub fn quit(&mut self) {
        if self.closing {
            return;
        }
        // Save BEFORE disposing panes, otherwise the tab list would be empty (§16.3). A last tab closed by the
        // user already left `tabs: []`, so the next launch opens one default tab.
        self.save_state(true);
        self.closing = true;
        self.close_confirmed = true;
        self.ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    pub fn toggle_window(&mut self, ctx: &Context) {
        let visible = self.hidden_flag();
        if !visible || !self.window_focused || self.minimized {
            if !(self.cfg.quake.dropdown && self.quake_show(ctx)) {
                self.quake_leave(ctx);
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            self.set_hidden(false);
            self.refocus = true;
        } else {
            self.hide_window(ctx);
        }
    }

    /// Hide to the tray / Quake.
    pub fn hide_window(&mut self, ctx: &Context) {
        self.save_state(true); // hiding to the tray: persist everything
        self.quake_leave(ctx);
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.set_hidden(true);
    }

    /// Hotkey, tray, ShareX scroll bridge, window close requests.
    pub fn handle_native(&mut self, ctx: &Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_confirmed {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            self.set_hidden(false);
            self.request_close();
        }
        if self.hotkey.pressed() {
            self.toggle_window(ctx);
        }
        for c in self.tray.poll() {
            match c {
                TrayCmd::Toggle => self.toggle_window(ctx),
                TrayCmd::Show => {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.set_hidden(false);
                }
                TrayCmd::NewTab => {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.set_hidden(false);
                    self.new_default_tab();
                }
                TrayCmd::Settings => {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.set_hidden(false);
                    self.settings_ui.open();
                }
                TrayCmd::Exit => {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    self.set_hidden(false);
                    self.request_close();
                }
            }
        }
        for a in crate::winhooks::take_scroll_actions() {
            if let Some(s) = self.focused_session() {
                s.scroll(match a {
                    ScrollAction::LineUp => Scroll::Delta(1),
                    ScrollAction::LineDown => Scroll::Delta(-1),
                    ScrollAction::PageUp => Scroll::PageUp,
                    ScrollAction::PageDown => Scroll::PageDown,
                    ScrollAction::Top => Scroll::Top,
                    ScrollAction::Bottom => Scroll::Bottom,
                });
            }
        }
    }

    // ------------------------------------------------------------------------------ pane events

    /// Apply what the tab layer reported this frame.
    pub fn handle_outs(&mut self, tab_id: u64, outs: Vec<TabOut>) {
        for o in outs {
            match o {
                TabOut::Focus { pane } => {
                    if let Some(t) = self.tab_index(tab_id).and_then(|i| self.tabs.get_mut(i)) {
                        t.focused = pane;
                    }
                    self.refocus = true;
                }
                TabOut::ContextMenu { pane, pos } => {
                    if let Some(t) = self.tab_index(tab_id).and_then(|i| self.tabs.get_mut(i)) {
                        t.focused = pane;
                    }
                    self.menu = Some(MenuState { pos, kind: MenuKind::Pane { tab: tab_id, pane } });
                }
                TabOut::LinkClick { pane, link } => self.open_link(tab_id, pane, link),
                TabOut::Zoom { pane, dir } => {
                    if let Some(t) = self.tab_index(tab_id).and_then(|i| self.tabs.get_mut(i)) {
                        t.focused = pane;
                    }
                    self.zoom_focused(dir);
                }
                TabOut::Spawned { pane } => {
                    self.mark_dirty();
                    self.refresh_branch(tab_id, pane);
                }
                TabOut::SearchAll { query, opts } => {
                    for t in &mut self.tabs {
                        for p in &mut t.panes {
                            if let Some(s) = p.session.clone() {
                                p.search.open = true;
                                p.search.query = query.clone();
                                p.search.opts = opts;
                                p.refresh_search(&s, true);
                            }
                        }
                    }
                }
                TabOut::Event { pane, ev } => self.on_pane_event(tab_id, pane, ev),
            }
        }
    }

    fn on_pane_event(&mut self, tab_id: u64, pane: u64, ev: PaneEvent) {
        let active = self.tabs.get(self.active).is_some_and(|t| t.id == tab_id);
        let Some(ti) = self.tab_index(tab_id) else { return };
        match ev {
            PaneEvent::Title(t) => {
                let tab = &mut self.tabs[ti];
                if !tab.title_locked && tab.focused == pane && !t.is_empty() {
                    tab.title = t;
                }
            }
            PaneEvent::Cwd { local, .. } => {
                if local {
                    self.refresh_branch(tab_id, pane);
                }
                self.mark_dirty();
            }
            PaneEvent::Bell => {
                let b = self.cfg.terminal.bell.clone();
                if !active {
                    self.tabs[ti].activity = true;
                }
                if b.flash_taskbar && !self.window_focused {
                    if let Some(h) = self.hwnd {
                        crate::winhooks::flash(h);
                    }
                }
            }
            PaneEvent::Shell { kind, exit_code, duration_ms } => {
                use ut_vt_scan::ShellKind::*;
                if matches!(kind, PromptStart | CommandEnd) {
                    self.refresh_branch(tab_id, pane);
                }
                if kind == CommandEnd {
                    self.command_finished(ti, exit_code, duration_ms);
                }
            }
            PaneEvent::Exited { code } => {
                use ut_core::settings::CloseOnExit::*;
                let close = match self.cfg.terminal.close_on_exit {
                    Always => true,
                    Graceful => code == Some(0),
                    Never => false,
                };
                if close {
                    let kill = self.cfg.processes.kill_console_tree_on_close;
                    let last = self.tabs[ti].close_pane(pane, kill);
                    if last {
                        self.remove_tab(ti);
                    }
                }
            }
            PaneEvent::Progress { .. } | PaneEvent::Notify { .. } => {}
        }
    }

    /// §12.3: a notification fires when the command took ≥ `minDurationSec` and the window is unfocused or the
    /// pane's tab is not active.
    fn command_finished(&mut self, ti: usize, code: Option<i32>, dur: Option<u64>) {
        let cfg = &self.cfg.notifications.command_finished;
        let Some(dur) = dur else { return };
        if !cfg.enabled || dur < cfg.min_duration_sec as u64 * 1000 {
            return;
        }
        let active = ti == self.active;
        if self.window_focused && active {
            return;
        }
        if let Some(h) = self.hwnd {
            crate::winhooks::flash(h);
        }
        if !active {
            self.tabs[ti].activity = true;
        }
        let _ = (code, cfg.toast);
    }

    fn refresh_branch(&mut self, tab_id: u64, pane: u64) {
        let Some(p) = self.tab_index(tab_id).and_then(|i| self.tabs.get_mut(i)).and_then(|t| t.pane_mut(pane)) else { return };
        if p.branch_pending || !p.cwd_local {
            return;
        }
        let Some(cwd) = p.cwd.clone() else { return };
        p.branch_pending = true;
        let (core, tx, ctx) = (self.core.clone(), self.branch_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let b = core.git.branch_for(&cwd);
            let _ = tx.send((pane, b));
            ctx.request_repaint();
        });
    }

    fn open_link(&mut self, tab_id: u64, pane: u64, link: ut_term::Link) {
        match link.kind {
            ut_term::LinkKind::Url => {
                if crate::sys::is_safe_url(&link.text) || link.text.starts_with("www.") {
                    let u = if link.text.starts_with("www.") { format!("https://{}", link.text) } else { link.text.clone() };
                    if let Err(e) = crate::sys::shell_open(&u) {
                        self.toasts.push(e, ToastKind::Error);
                    }
                } else {
                    let d = Dialog::confirm("Open link", &format!("This link uses a non-web protocol. Open it anyway?\n\n{}", link.text), "Open", false);
                    self.dialog = Some((DialogId::OpenLink(link.text), d));
                }
            }
            ut_term::LinkKind::Path { line, .. } => {
                let cwd = self.tab_index(tab_id).and_then(|i| self.tabs.get(i)).and_then(|t| t.pane(pane)).and_then(|p| p.cwd.clone().filter(|_| p.cwd_local));
                if let Err(e) = crate::sys::open_path(&link.text, line, cwd.as_deref(), &self.cfg.links.editor_command) {
                    self.toasts.push(e, ToastKind::Error);
                }
            }
        }
    }

    pub fn banner_kind_color(&self, k: BannerKind) -> egui::Color32 {
        match k {
            BannerKind::Info => self.theme.ui.accent,
            BannerKind::Warning => self.theme.ui.warning,
            BannerKind::Error => self.theme.ui.error,
        }
    }
}
