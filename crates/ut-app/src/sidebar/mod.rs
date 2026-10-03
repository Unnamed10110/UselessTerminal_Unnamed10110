//! The sessions sidebar (§8): header, search, folder/session tree with drag-and-drop, shells, snippets, the session
//! editor and the import/export flows. It owns no data: sessions, folders and snippets live in `core.sessions` (which
//! saves them debounced); the only thing persisted here is which folders are collapsed (`state`).
//!
//! * `tree`      pure logic (rows, selection ranges, drop-target maths), unit-tested
//! * `rows`      painting and pointer handling of folders and session cards, drag-and-drop
//! * `keys`      keyboard control of the tree
//! * `menus`     context menus and the actions behind them
//! * `editor`    the session edit dialog; `snippets` the snippet section and dialog
//! * `shells`    detected shells; `imports` export/import, Windows Terminal and SSH config

mod editor;
mod imports;
mod keys;
mod menus;
mod paint;
mod rows;
mod shells;
mod snippets;
mod state;
mod tree;

use crate::kit::{Dialog, DialogResult, ToastKind};
use crate::panels::{Cmd, PanelCtx};
use editor::{new_session, Outcome, SessionEditor};
use egui::{Align, Context, EventFilter, FontId, Id, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2};
use menus::{Act, Menu, MenuKind};
use paint::{glyph, glyph_button, G};
use rows::Drag;
use shells::Shells;
use snippets::SnippetEditor;
use state::UiState;
use std::collections::{HashMap, HashSet};
use tree::{DropAt, Kind, Row, Slot};
use ut_data::{ImportMode, Session, Snippet, Tree};
use ut_shell::ShellProfile;

/// Everything derived from the stores; rebuilt only when `SessionStore::version()` or the query changes.
struct Cache {
    version: Option<u64>,
    /// Trimmed query the hits were computed for.
    query: String,
    tree: Tree,
    /// `Some` while searching (flat, §8.2).
    hits: Option<Vec<Session>>,
    snippets: Vec<Snippet>,
    /// Session id -> the command line shown under its name.
    cmd: HashMap<String, CmdLine>,
}

impl Default for Cache {
    fn default() -> Self {
        Self { version: None, query: String::new(), tree: Tree { folders: vec![], sessions: vec![] }, hits: None, snippets: vec![], cmd: HashMap::new() }
    }
}

/// The three renderings of a session's command.
struct CmdLine {
    /// Tree: file name + arguments.
    short: String,
    /// While searching: what the search looks at.
    long: String,
    /// What the shell is given (tooltip).
    full: String,
}

/// An inline name editor open on a row.
struct Rename {
    id: String,
    folder: bool,
    text: String,
    /// First frame: grab focus and select the text.
    first: bool,
}

/// What a confirmation dialog will delete.
enum Confirmed {
    Sessions(Vec<String>),
    Folder(String),
    Snippet(String),
}

enum Modal {
    Confirm(Dialog, Confirmed),
    /// The text of the chosen sessions file, waiting for Merge / Replace.
    Import(Dialog, String),
}

pub struct Sidebar {
    st: UiState,
    query: String,
    sel: HashSet<String>,
    anchor: Option<String>,
    cursor: Option<String>,
    rows: Vec<Row>,
    cache: Cache,
    /// Where rows were drawn this frame (drop-target hit testing).
    slots: Vec<Slot>,
    drag: Option<Drag>,
    drop: Option<DropAt>,
    renaming: Option<Rename>,
    menu: Option<Menu>,
    modal: Option<Modal>,
    editor: Option<SessionEditor>,
    snippet_editor: Option<SnippetEditor>,
    ssh: Option<imports::SshImport>,
    shells: Shells,
    snip_sel: Option<String>,
    snip_scroll: Option<String>,
    tree_id: Id,
    search_id: Id,
    snip_id: Id,
    tree_rect: Rect,
    tree_focused: bool,
    search_had_focus: bool,
    want_search: bool,
    focus_tree: bool,
    focus_snippets: bool,
    refresh_shells: bool,
    scroll_to: Option<String>,
    /// A "new session" request that arrived without a context (keyboard action); handled in `show_windows`.
    pending_new: Option<Option<String>>,
    /// `ui` ran this frame (the sidebar is visible).
    shown: bool,
    /// Ignore this frame's key events in the tree (they were consumed by a rename that just ended).
    skip_keys: bool,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            st: UiState::load(),
            query: String::new(),
            sel: HashSet::new(),
            anchor: None,
            cursor: None,
            rows: vec![],
            cache: Cache::default(),
            slots: vec![],
            drag: None,
            drop: None,
            renaming: None,
            menu: None,
            modal: None,
            editor: None,
            snippet_editor: None,
            ssh: None,
            shells: Shells::default(),
            snip_sel: None,
            snip_scroll: None,
            tree_id: Id::new("sb-tree"),
            search_id: Id::new("sb-search"),
            snip_id: Id::new("sb-snippets-focus"),
            tree_rect: Rect::NOTHING,
            tree_focused: false,
            search_had_focus: false,
            want_search: false,
            focus_tree: false,
            focus_snippets: false,
            refresh_shells: false,
            scroll_to: None,
            pending_new: None,
            shown: false,
            skip_keys: false,
        }
    }

    /// True while a sidebar-owned modal or menu is open (the terminal must not receive keystrokes).
    pub fn modal_open(&self) -> bool {
        self.modal.is_some() || self.editor.is_some() || self.snippet_editor.is_some() || self.ssh.is_some() || self.menu.is_some()
    }

    /// "New session" from the keyboard action / palette: opens the editor on the default shell.
    pub fn new_session(&mut self) {
        self.pending_new = Some(None);
    }

    pub fn focus_search(&mut self) {
        self.want_search = true;
    }

    /// Draw the sidebar content into the left panel.
    pub fn ui(&mut self, ui: &mut Ui, x: &mut PanelCtx) {
        let ctx = ui.ctx().clone();
        self.tick_shells(&ctx, x);
        self.sync(x);
        ui.add_space(8.0);
        self.ui_header(ui, x);
        ui.add_space(6.0);
        self.ui_search(ui, x);
        ui.add_space(6.0);
        // Shells and snippets hug the bottom at their natural height; the tree takes the rest. The height is given
        // explicitly: an auto-sized panel only grows to what its (height-limited) scroll areas needed last frame.
        egui::Panel::bottom("sb-bottom").frame(egui::Frame::new()).exact_size(self.bottom_height(ui.available_height())).show(ui, |ui| {
            self.ui_shells(ui, x);
            self.ui_snippets(ui, x);
        });
        self.ui_tree(ui, x);
        self.handle_keys(&ctx, x);
        if std::mem::take(&mut self.focus_snippets) {
            ctx.memory_mut(|m| m.request_focus(self.snip_id));
        }
        self.repaint_if_stale(&ctx, x);
        self.shown = true;
    }

    /// Dialogs/windows owned by the sidebar (session editor, snippet editor, confirmations, menus).
    pub fn show_windows(&mut self, ctx: &Context, x: &mut PanelCtx) {
        self.tick_shells(ctx, x);
        if let Some(folder) = self.pending_new.take() {
            self.open_new_editor(x, folder);
        }
        self.show_menu(ctx, x);
        self.show_modal(ctx, x);
        self.show_editor(ctx, x);
        self.show_snippet_editor(ctx, x);
        self.show_ssh_import(ctx, x);
        // Only while the tree is on screen: a hidden sidebar does not sync, so it would never catch up.
        if std::mem::take(&mut self.shown) {
            self.repaint_if_stale(ctx, x);
        }
    }

    /// Height of the shells + snippets block: both headers plus the rows of the open sections (the lists scroll beyond
    /// 120 / 200 px), never more than 60% of the sidebar so the tree keeps room.
    fn bottom_height(&self, avail: f32) -> f32 {
        const HEAD: f32 = 28.0;
        let shells = HEAD + if self.st.shells_open { ((self.shells.list.len() + 1) as f32 * 24.0).min(120.0) } else { 0.0 };
        let snippets = HEAD + if !self.st.snippets_open { 0.0 } else if self.cache.snippets.is_empty() { 28.0 } else { (self.cache.snippets.len() as f32 * 40.0).min(200.0) };
        (shells + snippets + 8.0).min(avail * 0.6)
    }

    /// A change made this frame (rename, delete, import, …) is only drawn by the next one: make sure there is one.
    fn repaint_if_stale(&self, ctx: &Context, x: &PanelCtx) {
        if self.cache.version != Some(x.core.sessions.version()) {
            ctx.request_repaint();
        }
    }

    // ------------------------------------------------------------------------------------------ data

    fn tick_shells(&mut self, ctx: &Context, x: &PanelCtx) {
        let force = std::mem::take(&mut self.refresh_shells);
        let wanted = self.st.shells_open || self.editor.is_some();
        self.shells.tick(x.core, ctx, wanted, force);
    }

    /// Refresh what is derived from the stores, then recompute the visible rows and drop selections that vanished.
    fn sync(&mut self, x: &PanelCtx) {
        let store = &x.core.sessions;
        let q = self.query.trim().to_string();
        let v = store.version();
        if self.cache.version != Some(v) || self.cache.query != q {
            let tree = store.tree();
            let cmd = tree
                .folders
                .iter()
                .flat_map(|f| f.sessions.iter())
                .chain(tree.sessions.iter())
                .map(|s| (s.id.clone(), CmdLine { short: tree::short_command(s, |p| std::path::Path::new(p).is_file()), long: tree::long_command(s), full: s.full_command() }))
                .collect();
            let hits = (!q.is_empty()).then(|| store.search(&q));
            if self.st.forget_missing(|id| tree.folders.iter().any(|f| f.folder.id == id)) {
                self.st.save(&x.core.writer);
            }
            self.cache = Cache { version: Some(v), query: q, tree, hits, snippets: store.snippets(), cmd };
        }
        self.rows = tree::flatten(&self.cache.tree, |id| self.st.is_open(id), self.cache.hits.as_deref());
        let visible: HashSet<&str> = self.rows.iter().map(|r| r.id.as_str()).collect();
        self.sel.retain(|id| visible.contains(id.as_str()));
        if self.anchor.as_deref().is_some_and(|a| !visible.contains(a)) {
            self.anchor = None;
        }
        if self.cursor.as_deref().is_some_and(|c| !visible.contains(c)) {
            self.cursor = None;
        }
    }

    // ------------------------------------------------------------------------------------------ selection

    fn set_sel(&mut self, ids: Vec<String>) {
        self.anchor = ids.first().cloned();
        self.cursor = self.anchor.clone();
        self.sel = ids.into_iter().collect();
    }

    /// Selected sessions in row order.
    fn sel_sessions(&self) -> Vec<String> {
        self.rows.iter().filter(|r| r.kind == Kind::Session && self.sel.contains(&r.id)).map(|r| r.id.clone()).collect()
    }

    fn is_session(&self, id: &str) -> bool {
        tree::index_of(&self.rows, id).is_some_and(|i| self.rows[i].kind == Kind::Session)
    }

    /// Click / arrow semantics: plain = single, Ctrl = toggle, Shift = range from the anchor (sessions only; a folder
    /// never mixes into a multi-selection).
    fn pick(&mut self, id: &str, shift: bool, ctrl: bool) {
        let session = self.is_session(id);
        let anchor = self.anchor.clone().filter(|a| self.is_session(a));
        if let (true, true, Some(a)) = (shift, session, &anchor) {
            let range = tree::session_range(&self.rows, a, id);
            if ctrl {
                self.sel.extend(range);
            } else {
                self.sel = range.into_iter().collect();
            }
        } else if ctrl && session {
            let keep: HashSet<String> = self.sel.iter().filter(|s| self.is_session(s)).cloned().collect();
            self.sel = keep;
            if !self.sel.remove(id) {
                self.sel.insert(id.to_string());
            }
            self.anchor = Some(id.to_string());
        } else {
            self.sel = HashSet::from([id.to_string()]);
            self.anchor = Some(id.to_string());
        }
        self.cursor = Some(id.to_string());
        self.scroll_to = Some(id.to_string());
    }

    fn set_open(&mut self, id: &str, open: bool, x: &PanelCtx) {
        if self.st.set_open(id, open) {
            self.st.save(&x.core.writer);
        }
    }

    // ------------------------------------------------------------------------------------------ header and search

    fn ui_header(&mut self, ui: &mut Ui, x: &mut PanelCtx) {
        let theme = x.theme;
        let th = &theme.ui;
        egui::Frame::new().inner_margin(Margin::symmetric(10, 0)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Sessions").strong().size(14.0).color(th.accent));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(26.0, 24.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 4.0, th.hover_bg);
                    }
                    glyph(ui.painter(), G::More, Rect::from_center_size(r.center(), Vec2::splat(16.0)), if resp.hovered() { th.foreground } else { th.muted });
                    if resp.on_hover_text("More").clicked() {
                        self.open_menu(r.left_bottom() + Vec2::new(0.0, 2.0), MenuKind::Header);
                    }
                    // "+ New" pill: accent tint, add icon (§8.2)
                    let font = FontId::proportional(12.5);
                    let w = ui.painter().layout_no_wrap("New".into(), font.clone(), th.accent).size().x;
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(w + 34.0, 22.0), Sense::click());
                    let (fill, fg) = if resp.hovered() { (th.accent, th.tab_selected_fg) } else { (th.accent_dim, th.accent) };
                    ui.painter().rect(r, 11.0, fill, Stroke::new(1.0, th.accent), StrokeKind::Inside);
                    glyph(ui.painter(), G::Plus, Rect::from_center_size(Pos2::new(r.left() + 13.0, r.center().y), Vec2::splat(12.0)), fg);
                    let g = ui.painter().layout_no_wrap("New".into(), font, fg);
                    ui.painter().galley(Pos2::new(r.left() + 24.0, r.center().y - g.size().y / 2.0), g, fg);
                    let core = x.core;
                    let clicked = resp
                        .on_hover_ui(|ui| {
                            let chord = core.keys.lock().effective().get("newSession").and_then(|c| c.first()).cloned().unwrap_or_else(|| "Ctrl+Shift+N".into());
                            ui.label(format!("New Session ({chord})"));
                        })
                        .clicked();
                    if clicked {
                        self.run(Act::NewSession(None), x);
                    }
                });
            });
        });
    }

    fn ui_search(&mut self, ui: &mut Ui, x: &mut PanelCtx) {
        let theme = x.theme;
        let th = &theme.ui;
        let had_focus = self.search_had_focus;
        egui::Frame::new().inner_margin(Margin::symmetric(8, 0)).show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
            let focused = ui.memory(|m| m.has_focus(self.search_id));
            // The border turns accent-coloured on focus (§8.2).
            ui.painter().rect(rect, 6.0, th.input_bg, Stroke::new(1.0, if focused { th.accent } else { th.card_border }), StrokeKind::Inside);
            let clear_w = if self.query.is_empty() { 0.0 } else { 22.0 };
            let te = Rect::from_min_max(Pos2::new(rect.left() + 9.0, rect.top() + 2.0), Pos2::new(rect.right() - 6.0 - clear_w, rect.bottom() - 2.0));
            let mut field = ui.new_child(egui::UiBuilder::new().max_rect(te).layout(Layout::left_to_right(Align::Center)));
            let r = field.add(TextEdit::singleline(&mut self.query).id(self.search_id).hint_text("Search sessions").frame(egui::Frame::NONE).desired_width(te.width()).vertical_align(Align::Center));
            if r.changed() {
                ui.ctx().request_repaint();
            }
            if !self.query.is_empty() {
                let c = Rect::from_center_size(Pos2::new(rect.right() - 14.0, rect.center().y), Vec2::splat(20.0));
                if glyph_button(ui, c, self.search_id.with("clear"), G::Close, th.muted, th.foreground, th.hover_bg, "Clear search") {
                    self.query.clear();
                    r.request_focus();
                }
            }
            if std::mem::take(&mut self.want_search) {
                r.request_focus();
                if let Some(mut st) = TextEdit::load_state(ui.ctx(), self.search_id) {
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(self.query.chars().count()))));
                    st.store(ui.ctx(), self.search_id);
                }
            }
            if r.has_focus() {
                // Down / Enter / Esc are ours: Down moves into the list, Enter opens the first hit, Esc clears the text first.
                ui.ctx().memory_mut(|m| m.set_focus_lock_filter(self.search_id, EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: true }));
            }
            let events: Vec<(egui::Key, egui::Modifiers)> = ui.input(|i| i.events.iter().filter_map(|e| if let egui::Event::Key { key, pressed: true, modifiers, .. } = e { Some((*key, *modifiers)) } else { None }).collect());
            if had_focus || r.has_focus() {
                for (k, m) in events {
                    match k {
                        egui::Key::Escape if !self.query.is_empty() => {
                            self.query.clear();
                            r.request_focus();
                        }
                        egui::Key::Escape => {
                            ui.memory_mut(|mem| mem.surrender_focus(self.search_id));
                            x.cmds.push(Cmd::FocusTerminal);
                        }
                        egui::Key::ArrowDown if !self.rows.is_empty() => {
                            self.focus_tree = true;
                            let first = self.rows[0].id.clone();
                            self.pick(&first, false, false);
                        }
                        egui::Key::Enter if r.has_focus() && !self.query.trim().is_empty() => {
                            if let Some(first) = self.cache.hits.as_ref().and_then(|h| h.first()).map(|s| s.id.clone()) {
                                self.run(Act::Open { ids: vec![first], admin: m.ctrl }, x);
                            }
                        }
                        _ => {}
                    }
                }
            }
            self.search_had_focus = r.has_focus();
        });
    }

    // ------------------------------------------------------------------------------------------ actions

    fn default_shell(&self, x: &PanelCtx) -> Option<ShellProfile> {
        if !self.shells.loaded() {
            return x.core.default_profile();
        }
        let setting = x.core.cfg().shells.default_profile;
        ut_shell::resolve_default(&setting, &self.shells.list).or_else(|| self.shells.list.iter().find(|p| p.is_default)).or(self.shells.list.first()).cloned()
    }

    fn open_new_editor(&mut self, x: &PanelCtx, folder: Option<String>) {
        let shell = self.default_shell(x);
        self.editor = Some(SessionEditor::new(new_session(shell.as_ref(), folder), true, x.core.preset_names()));
    }

    fn confirm(&mut self, title: &str, msg: &str, what: Confirmed) {
        self.modal = Some(Modal::Confirm(Dialog::confirm(title, msg, "Delete", true), what));
    }

    fn run(&mut self, a: Act, x: &mut PanelCtx) {
        let core = x.core;
        let store = &core.sessions;
        match a {
            Act::Open { ids, admin } => {
                for id in ids {
                    if let Some(s) = store.get(&id) {
                        // `runAsAdmin` is the session's default launch mode (§8.1); Ctrl / the menu entry force it.
                        x.open_session(&s, admin || s.run_as_admin);
                    }
                }
            }
            Act::NewSession(folder) => self.open_new_editor(x, folder),
            Act::Edit(id) => {
                if let Some(s) = store.get(&id) {
                    self.editor = Some(SessionEditor::new(s, false, core.preset_names()));
                }
            }
            Act::Duplicate(id) => {
                if let Some(copy) = store.duplicate_session(&id) {
                    let named = Session { name: format!("{} (copy)", copy.name), ..copy };
                    let shown = store.update_session(named.clone()).unwrap_or(named);
                    self.set_sel(vec![shown.id.clone()]);
                    self.scroll_to = Some(shown.id);
                }
            }
            Act::CopyCommand(id) => {
                if let Some(s) = store.get(&id) {
                    crate::clipboard::set_text(&s.full_command());
                    x.toasts.push("Command line copied.", ToastKind::Success);
                }
            }
            Act::Delete(ids) => {
                let names: Vec<String> = ids.iter().filter_map(|i| store.get(i)).map(|s| s.name).collect();
                match names.as_slice() {
                    [] => {}
                    [one] => self.confirm("Delete session", &format!("Delete session \"{one}\"?"), Confirmed::Sessions(ids)),
                    many => self.confirm("Delete sessions", &format!("Delete {} sessions?", many.len()), Confirmed::Sessions(ids)),
                }
            }
            Act::SetColor(ids, color) => {
                for s in ids.iter().filter_map(|i| store.get(i)) {
                    if let Err(e) = store.update_session(Session { color_tag: color.clone(), ..s }) {
                        x.toasts.push(e.to_string(), ToastKind::Error);
                    }
                }
            }
            Act::NewFolder => {
                let f = store.add_folder("New Folder");
                self.set_sel(vec![f.id.clone()]);
                self.begin_rename(&f.id, true, f.name);
            }
            Act::RenameFolder(id) => {
                if let Some(f) = store.folders().into_iter().find(|f| f.id == id) {
                    self.set_sel(vec![id.clone()]);
                    self.begin_rename(&id, true, f.name);
                }
            }
            Act::RenameSession(id) => {
                if let Some(s) = store.get(&id) {
                    self.set_sel(vec![id.clone()]);
                    self.begin_rename(&id, false, s.name);
                }
            }
            Act::MoveFolder { id, up } => {
                if up {
                    store.move_folder_up(&id);
                } else {
                    store.move_folder_down(&id);
                }
            }
            Act::SetOpen(id, open) => self.set_open(&id, open, x),
            Act::DeleteFolder(id) => {
                if let Some(f) = store.folders().into_iter().find(|f| f.id == id) {
                    let n = store.tree().folders.iter().find(|t| t.folder.id == id).map_or(0, |t| t.sessions.len());
                    let extra = if n > 0 { "\nSessions in this folder will be moved to the root list." } else { "" };
                    self.confirm("Delete folder", &format!("Delete folder \"{}\"?{extra}", f.name), Confirmed::Folder(id));
                }
            }
            Act::Export => self.export_sessions(x),
            Act::Import => self.import_sessions(x),
            Act::ImportWt => self.import_wt(x),
            Act::ImportSsh => self.import_ssh(x),
            Act::QuickConnect => x.cmds.push(Cmd::Action("quickConnect".into())),
            Act::RefreshShells => self.refresh_shells = true,
            Act::OpenShell(id) => {
                if let Some(p) = self.shells.find(&id) {
                    x.cmds.push(Cmd::OpenProfile(p.clone()));
                }
            }
            Act::SaveShell(id) => {
                if let Some(p) = self.shells.find(&id).cloned() {
                    let s = Session { name: p.name, shell_path: p.path, arguments: p.arguments, color_tag: p.color, ..Default::default() };
                    match store.add_session(s) {
                        Ok(s) => {
                            self.scroll_to = Some(s.id.clone());
                            x.toasts.push(format!("Saved session \"{}\".", s.name), ToastKind::Success);
                        }
                        Err(e) => x.toasts.push(e.to_string(), ToastKind::Error),
                    }
                }
            }
            Act::NewSnippet => self.snippet_editor = Some(SnippetEditor::new(None)),
            Act::EditSnippet(id) => {
                if let Some(s) = store.snippets().into_iter().find(|s| s.id == id) {
                    self.snippet_editor = Some(SnippetEditor::new(Some(s)));
                }
            }
            Act::RunSnippet(id) => {
                if let Some(s) = store.snippets().into_iter().find(|s| s.id == id) {
                    x.cmds.push(Cmd::RunSnippet(s));
                }
            }
            Act::DeleteSnippet(id) => {
                if let Some(s) = store.snippets().into_iter().find(|s| s.id == id) {
                    self.confirm("Delete snippet", &format!("Delete snippet \"{}\"?", s.name), Confirmed::Snippet(id));
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------ windows

    fn show_modal(&mut self, ctx: &Context, x: &mut PanelCtx) {
        let Some(m) = self.modal.take() else { return };
        match m {
            Modal::Confirm(mut d, what) => match d.show(ctx, x.theme) {
                None => self.modal = Some(Modal::Confirm(d, what)),
                Some(r) => {
                    self.focus_tree = true;
                    if matches!(r, DialogResult::Button(1, _)) {
                        let store = &x.core.sessions;
                        match what {
                            Confirmed::Sessions(ids) => ids.iter().for_each(|id| {
                                store.delete_session(id);
                            }),
                            Confirmed::Folder(id) => {
                                store.delete_folder(&id);
                            }
                            Confirmed::Snippet(id) => {
                                store.delete_snippet(&id);
                            }
                        }
                    }
                }
            },
            Modal::Import(mut d, text) => match d.show(ctx, x.theme) {
                None => self.modal = Some(Modal::Import(d, text)),
                Some(r) => {
                    self.focus_tree = true;
                    match r {
                        DialogResult::Button(1, _) => self.do_import(x, &text, ImportMode::Replace),
                        DialogResult::Button(2, _) => self.do_import(x, &text, ImportMode::Merge),
                        _ => {}
                    }
                }
            },
        }
    }

    fn show_editor(&mut self, ctx: &Context, x: &mut PanelCtx) {
        let Some(mut ed) = self.editor.take() else { return };
        match ed.show(ctx, x.theme, &x.core.sessions, &self.shells.list) {
            Outcome::Open => self.editor = Some(ed),
            Outcome::Saved(s) => {
                if let Some(f) = &s.folder_id {
                    self.set_open(f, true, x);
                }
                if ed.is_new {
                    // A new session must not be hidden by the filter that was typed before it existed.
                    self.query.clear();
                }
                self.set_sel(vec![s.id.clone()]);
                self.scroll_to = Some(s.id);
                self.focus_tree = true;
            }
            Outcome::Cancelled => self.focus_tree = true,
        }
    }
}
