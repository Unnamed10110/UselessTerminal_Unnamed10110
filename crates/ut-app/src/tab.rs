//! Tabs and panes: a binary split tree per tab, panes spawned lazily once they have a stable size (§5.4: a
//! shell started before its real size is known caches a wrong width).

use crate::core::{Core, LaunchReq, Resolved};
use crate::fonts::Fonts;
use crate::theme::Theme;
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ut_core::Settings;
use ut_shell::ShellKind;
use ut_term::{Link, Match, PaneEvent, SearchOpts, TermSession, TermView, ViewOptions};
use ut_vt_scan::{Phase, ShellKind as Mark};

pub const MAX_PANES: usize = 16;
const SPLITTER: f32 = 2.0;
const MIN_PANE: f32 = 60.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    /// `a` left, `b` right.
    Right,
    /// `a` top, `b` bottom.
    Down,
}

#[derive(Clone, Debug)]
pub enum Layout {
    Leaf(u64),
    Split { dir: SplitDir, ratio: f32, a: Box<Layout>, b: Box<Layout> },
}

impl Layout {
    pub fn leaves(&self, out: &mut Vec<u64>) {
        match self {
            Layout::Leaf(i) => out.push(*i),
            Layout::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    fn replace(&mut self, id: u64, by: Layout) -> bool {
        match self {
            Layout::Leaf(i) if *i == id => {
                *self = by;
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { a, b, .. } => a.replace(id, by.clone()) || b.replace(id, by),
        }
    }

    /// Remove a leaf; its parent collapses into the sibling. `None` when nothing is left.
    fn remove(self, id: u64) -> Option<Layout> {
        match self {
            Layout::Leaf(i) => (i != id).then_some(Layout::Leaf(i)),
            Layout::Split { dir, ratio, a, b } => match (a.remove(id), b.remove(id)) {
                (Some(a), Some(b)) => Some(Layout::Split { dir, ratio, a: Box::new(a), b: Box::new(b) }),
                (x, y) => x.or(y),
            },
        }
    }

    fn node_mut(&mut self, path: &[bool]) -> &mut Layout {
        match (self, path.split_first()) {
            (Layout::Split { a, b, .. }, Some((go_b, rest))) => if *go_b { b.node_mut(rest) } else { a.node_mut(rest) },
            (n, _) => n,
        }
    }
}

struct Splitter {
    rect: Rect,
    dir: SplitDir,
    parent: Rect,
    path: Vec<bool>,
}

fn place(n: &Layout, r: Rect, path: &mut Vec<bool>, panes: &mut Vec<(u64, Rect)>, sp: &mut Vec<Splitter>) {
    match n {
        Layout::Leaf(i) => panes.push((*i, r)),
        Layout::Split { dir, ratio, a, b } => {
            let (span, min) = if *dir == SplitDir::Right { (r.width(), MIN_PANE.min(r.width() / 2.0)) } else { (r.height(), MIN_PANE.min(r.height() / 2.0)) };
            let first = (((span - SPLITTER) * ratio).round()).clamp(min, (span - min - SPLITTER).max(min));
            let (ra, rb, rs) = if *dir == SplitDir::Right {
                (
                    Rect::from_min_size(r.min, Vec2::new(first, r.height())),
                    Rect::from_min_max(Pos2::new(r.min.x + first + SPLITTER, r.min.y), r.max),
                    Rect::from_min_size(Pos2::new(r.min.x + first, r.min.y), Vec2::new(SPLITTER, r.height())),
                )
            } else {
                (
                    Rect::from_min_size(r.min, Vec2::new(r.width(), first)),
                    Rect::from_min_max(Pos2::new(r.min.x, r.min.y + first + SPLITTER), r.max),
                    Rect::from_min_size(Pos2::new(r.min.x, r.min.y + first), Vec2::new(r.width(), SPLITTER)),
                )
            };
            sp.push(Splitter { rect: rs, dir: *dir, parent: r, path: path.clone() });
            path.push(false);
            place(a, ra, path, panes, sp);
            path.pop();
            path.push(true);
            place(b, rb, path, panes, sp);
            path.pop();
        }
    }
}

#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub query: String,
    pub opts: SearchOpts,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    focus: bool,
    computed: Option<(String, SearchOpts)>,
    changed_at: Option<Instant>,
}

pub struct DropOverlay {
    pub title: String,
    pub detail: String,
    pub severity: Severity,
    pub until: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

pub struct Pane {
    pub id: u64,
    pub session: Option<Arc<TermSession>>,
    pub error: Option<String>,
    pub view: TermView,
    pub req: LaunchReq,
    pub kind: ShellKind,
    pub injected: bool,
    pub local_input_color: bool,
    pub command_line: String,
    pub title: String,
    pub cwd: Option<String>,
    pub cwd_host: String,
    pub cwd_local: bool,
    pub branch: Option<String>,
    pub foreground: Option<String>,
    pub last_exit: Option<i32>,
    pub last_duration_ms: Option<u64>,
    pub phase: Phase,
    pub exited: bool,
    pub read_only: bool,
    pub bell_until: Option<Instant>,
    pub search: Search,
    pub drop_overlay: Option<DropOverlay>,
    pub start_cwd: Option<String>,
    stable_frames: u32,
    last_size: (u32, u32),
    pub branch_pending: bool,
    pub notes_shown: bool,
    pub font_override: f32,
    pub theme_bg: Option<Color32>,
    pub seen_frames: u64,
}

/// What the tab/pane layer asks the app to do.
pub enum TabOut {
    ContextMenu { pane: u64, pos: Pos2 },
    LinkClick { pane: u64, link: Link },
    Zoom { pane: u64, dir: i32 },
    Focus { pane: u64 },
    Event { pane: u64, ev: PaneEvent },
    Spawned { pane: u64 },
    SearchAll { query: String, opts: SearchOpts },
}

pub struct PaneEnv<'a> {
    pub core: &'a Arc<Core>,
    pub theme: &'a Theme,
    pub fonts: &'a Fonts,
    pub cfg: &'a Settings,
    pub repaint: Arc<dyn Fn() + Send + Sync>,
    pub bg_texture: Option<&'a egui::TextureHandle>,
    pub bg_opacity: f32,
    pub tab_active: bool,
    pub out: &'a mut Vec<TabOut>,
}

impl Pane {
    pub fn new(id: u64, req: LaunchReq) -> Pane {
        Pane {
            id,
            session: None,
            error: None,
            view: TermView::new(egui::Id::new(("term", id))),
            start_cwd: req.cwd.clone(),
            req,
            kind: ShellKind::Unknown,
            injected: false,
            local_input_color: false,
            command_line: String::new(),
            title: String::new(),
            cwd: None,
            cwd_host: String::new(),
            cwd_local: true,
            branch: None,
            foreground: None,
            last_exit: None,
            last_duration_ms: None,
            phase: Phase::None,
            exited: false,
            read_only: false,
            bell_until: None,
            search: Search::default(),
            drop_overlay: None,
            stable_frames: 0,
            last_size: (0, 0),
            branch_pending: false,
            notes_shown: false,
            font_override: 0.0,
            theme_bg: None,
            seen_frames: 0,
        }
    }

    pub fn started(&self) -> bool {
        self.session.is_some()
    }

    fn font_pts(&self, cfg: &Settings) -> f32 {
        if self.font_override >= 8.0 { self.font_override } else { cfg.terminal.font_size as f32 }
    }

    /// Spawn the shell once the pane has had the same sensible size for two frames.
    fn try_spawn(&mut self, ctx: &egui::Context, rect: Rect, env: &mut PaneEnv) {
        if self.session.is_some() || self.error.is_some() {
            return;
        }
        let font = self.font_for(env);
        let (cw, ch) = ut_term::cell_size(ctx, &font, env.cfg.terminal.line_height as f32, env.cfg.terminal.letter_spacing as f32);
        let (cols, rows) = ut_term::cells_that_fit(rect.size(), ut_term::PADDING, true, env.cfg.terminal.minimap, (cw, ch));
        let size = (cols.max(0) as u32, rows.max(0) as u32);
        if cols < 4 || rows < 2 {
            self.stable_frames = 0;
            return;
        }
        self.stable_frames = if size == self.last_size { self.stable_frames + 1 } else { 0 };
        self.last_size = size;
        if self.stable_frames < 2 {
            ctx.request_repaint();
            return;
        }
        match env.core.resolve_launch(&self.req) {
            Ok(r) => self.spawn_with(env, r, cols as u16, rows as u16),
            Err(e) => self.error = Some(e),
        }
    }

    fn spawn_with(&mut self, env: &mut PaneEnv, r: Resolved, cols: u16, rows: u16) {
        let cfg = env.core.session_config(&r, cols, rows, env.theme.palette.clone(), env.repaint.clone());
        match TermSession::spawn(cfg) {
            Ok(s) => {
                self.kind = r.kind;
                self.injected = r.injected;
                self.local_input_color = r.local_input_color;
                self.command_line = r.command_line.clone();
                self.start_cwd = Some(r.cwd.to_string_lossy().into_owned());
                for n in &r.notes {
                    s.feed_local(format!("\x1b[2m[{n}]\x1b[0m\r\n").as_bytes());
                }
                self.session = Some(s);
                env.out.push(TabOut::Spawned { pane: self.id });
            }
            // §4.1: a failed spawn shows inside the pane — red text plus the command line that failed.
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Keep an inactive tab's pane alive without drawing: poll events, optionally spawn (lazy restore, §16.3), and
    /// report output as tab activity.
    pub fn background_tick(&mut self, ctx: &egui::Context, rect: Rect, env: &mut PaneEnv, allow_spawn: bool) -> bool {
        if allow_spawn {
            self.try_spawn(ctx, rect, env);
        }
        self.poll_events(env);
        let mut output = false;
        if let Some(s) = &self.session {
            let f = s.inner.frames.load(std::sync::atomic::Ordering::Relaxed);
            output = f != self.seen_frames;
            self.seen_frames = f;
        }
        output
    }

    pub fn restart(&mut self, env: &mut PaneEnv) {
        let Some(s) = self.session.clone() else { return };
        if !self.exited {
            return;
        }
        s.feed_local(b"\r\n");
        if let Err(e) = s.respawn() {
            s.feed_local(format!("\x1b[31m{e}\x1b[0m\r\n").as_bytes());
        } else {
            self.exited = false;
            self.phase = Phase::None;
            env.out.push(TabOut::Spawned { pane: self.id });
        }
    }

    fn font_for(&self, env: &PaneEnv) -> egui::FontId {
        let mut f = env.fonts.regular.clone();
        f.size = self.font_pts(env.cfg);
        f
    }

    pub fn poll_events(&mut self, env: &mut PaneEnv) {
        let Some(s) = self.session.clone() else { return };
        for ev in s.drain_events() {
            match &ev {
                PaneEvent::Title(t) => self.title = t.clone(),
                PaneEvent::Cwd { path, host, local } => {
                    self.cwd = Some(path.clone());
                    self.cwd_host = host.clone();
                    self.cwd_local = *local;
                }
                PaneEvent::Shell { kind, exit_code, duration_ms } => {
                    self.phase = match kind {
                        Mark::PromptStart => Phase::Prompt,
                        Mark::InputStart => Phase::Input,
                        Mark::CommandStart => Phase::Running,
                        Mark::CommandEnd => Phase::Done,
                    };
                    if *kind == Mark::CommandEnd {
                        if exit_code.is_some() {
                            self.last_exit = *exit_code;
                        }
                        self.last_duration_ms = *duration_ms;
                    }
                    // Typed-input colour for every shell but PowerShell (whose script colours PSReadLine, §6.6).
                    if self.local_input_color {
                        match kind {
                            Mark::InputStart => s.feed_local(ut_shell::typed_input_escape(&env.cfg.terminal.typed_input_color).as_bytes()),
                            Mark::CommandStart | Mark::CommandEnd => s.feed_local(ut_shell::RESET_FG.as_bytes()),
                            _ => {}
                        }
                    }
                }
                PaneEvent::Bell => {
                    if env.cfg.terminal.bell.visual {
                        self.bell_until = Some(Instant::now() + Duration::from_millis(150));
                    }
                }
                PaneEvent::Exited { .. } => self.exited = true,
                _ => {}
            }
            env.out.push(TabOut::Event { pane: self.id, ev });
        }
    }

    /// Draw the pane into `rect`. `focused`: the tab's focused pane; `multi`: the tab has several panes.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, env: &mut PaneEnv, focused: bool, multi: bool, flags: Flags) {
        let ctx = ui.ctx().clone();
        let theme = env.theme;
        self.try_spawn(&ctx, rect, env);
        self.poll_events(env);
        let bg = self.theme_bg.unwrap_or(theme.term_bg);
        let painter = ui.painter_at(rect);
        if env.bg_texture.is_none() {
            painter.rect_filled(rect, 0.0, bg);
        } else if let Some(tex) = env.bg_texture {
            // cover + centre, then the theme background at 62 % over it (§5.12)
            let ts = tex.size_vec2();
            let scale = (rect.width() / ts.x).max(rect.height() / ts.y);
            let sz = ts * scale;
            let r = Rect::from_center_size(rect.center(), sz);
            painter.with_clip_rect(rect).image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::from_white_alpha((env.bg_opacity * 255.0) as u8));
            painter.rect_filled(rect, 0.0, bg.gamma_multiply(0.62));
        }
        let Some(s) = self.session.clone() else {
            let text = match &self.error {
                Some(e) => format!("{e}"),
                None => "Starting…".to_string(),
            };
            let color = if self.error.is_some() { theme.ui.error } else { theme.ui.muted };
            painter.text(rect.min + Vec2::new(12.0, 12.0), Align2::LEFT_TOP, text, egui::FontId::monospace(13.0), color);
            return;
        };
        let font = self.font_for(env);
        let mk = |f: &egui::FontId| {
            let mut x = f.clone();
            x.size = font.size;
            x
        };
        let t = &env.cfg.terminal;
        let matches = self.search.matches.clone();
        let opts = ViewOptions {
            font: font.clone(),
            bold: Some(mk(&env.fonts.bold)),
            italic: Some(mk(&env.fonts.italic)),
            bold_italic: Some(mk(&env.fonts.bold_italic)),
            line_height: t.line_height as f32,
            letter_spacing: t.letter_spacing as f32,
            padding: ut_term::PADDING,
            palette: &theme.palette,
            bg_color: bg,
            transparent_bg: true, // the pane background was painted above
            selection_bg: theme.selection_bg,
            selection_fg: theme.selection_fg,
            cursor_color: theme.cursor,
            cursor_style: match t.cursor_style {
                ut_core::settings::CursorStyle::Bar => ut_term::CursorSetting::Bar,
                ut_core::settings::CursorStyle::Block => ut_term::CursorSetting::Block,
                ut_core::settings::CursorStyle::Underline => ut_term::CursorSetting::Underline,
            },
            cursor_blink: t.cursor_blink,
            focused,
            bold_is_bright: true,
            copy_on_select: t.copy_on_select,
            matches: &matches,
            current_match: self.search.current,
            scrollbar: true,
            minimap: t.minimap,
            crt: t.crt,
        };
        let out = ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| self.view.show(ui, &s, &opts)).inner;
        if out.clicked {
            env.out.push(TabOut::Focus { pane: self.id });
        }
        if let Some(pos) = out.context_menu {
            env.out.push(TabOut::ContextMenu { pane: self.id, pos });
        }
        if let Some(link) = out.link_click {
            env.out.push(TabOut::LinkClick { pane: self.id, link });
        }
        if out.zoom != 0 {
            env.out.push(TabOut::Zoom { pane: self.id, dir: out.zoom });
        }

        // ---- overlays
        if multi && !focused {
            painter.rect_filled(rect, 0.0, theme.ui.chrome_bg.gamma_multiply(0.5));
        }
        if multi && focused {
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, theme.ui.accent), StrokeKind::Inside);
        }
        if self.bell_until.is_some_and(|u| Instant::now() < u) {
            painter.rect_filled(rect, 0.0, Color32::from_white_alpha(46));
            ctx.request_repaint_after(Duration::from_millis(160));
        }
        let mut x = rect.right() - 18.0;
        let badge = |text: &str, bg: Color32, fg: Color32, x: &mut f32| {
            let g = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(10.0), fg);
            let r = Rect::from_min_size(Pos2::new(*x - g.size().x - 10.0, rect.top() + 4.0), g.size() + Vec2::new(10.0, 3.0));
            painter.rect_filled(r, 3.0, bg);
            painter.galley(r.min + Vec2::new(5.0, 1.5), g, fg);
            *x = r.left() - 4.0;
        };
        if flags.broadcast {
            badge("BROADCAST", theme.ui.warning, Color32::BLACK, &mut x);
        }
        if self.read_only {
            badge("READ-ONLY", Color32::from_rgb(255, 170, 0), Color32::BLACK, &mut x);
        }
        if s.recording() {
            badge("● REC", Color32::from_rgb(220, 40, 40), Color32::WHITE, &mut x);
        }
        if self.exited {
            let g = painter.layout_no_wrap("Process exited — Enter to restart, Ctrl+W to close".into(), egui::FontId::proportional(12.0), theme.ui.foreground);
            let r = Rect::from_center_size(Pos2::new(rect.center().x, rect.bottom() - 22.0), g.size() + Vec2::new(24.0, 8.0));
            painter.rect_filled(r, 4.0, theme.ui.card_bg);
            painter.rect_stroke(r, 4.0, Stroke::new(1.0, theme.ui.card_border), StrokeKind::Inside);
            painter.galley(r.min + Vec2::new(12.0, 4.0), g, theme.ui.foreground);
        }
        if let Some(o) = &self.drop_overlay {
            if o.until.is_some_and(|u| Instant::now() > u) {
                self.drop_overlay = None;
            } else {
                draw_drop_overlay(&painter, rect, o, theme);
            }
        }
        self.search_bar(ui, rect, &s, env);
    }

    // ----------------------------------------------------------------------- search
    pub fn open_search(&mut self, s: Option<&TermSession>) {
        self.search.open = true;
        self.search.focus = true;
        if let Some(sel) = s.and_then(|s| s.selection_text()).filter(|t| !t.contains('\n')) {
            self.search.query = sel;
        }
    }

    pub fn close_search(&mut self) {
        self.search.open = false;
        self.search.matches.clear();
        self.search.current = None;
        self.search.computed = None;
    }

    pub fn refresh_search(&mut self, s: &TermSession, jump: bool) {
        let key = (self.search.query.clone(), self.search.opts);
        self.search.matches = s.search(&key.0, key.1);
        self.search.computed = Some(key);
        if self.search.matches.is_empty() {
            self.search.current = None;
            return;
        }
        if jump || self.search.current.is_none_or(|c| c >= self.search.matches.len()) {
            let (top, _) = s.view_top_abs();
            let idx = self.search.matches.iter().position(|m| m.abs >= top).unwrap_or(self.search.matches.len() - 1);
            self.search.current = Some(idx);
            s.reveal_abs(self.search.matches[idx].abs);
        }
    }

    pub fn search_step(&mut self, s: &TermSession, dir: i32) {
        if self.search.matches.is_empty() {
            return;
        }
        let n = self.search.matches.len() as i32;
        let cur = self.search.current.map_or(if dir > 0 { -1 } else { 0 }, |c| c as i32);
        let next = (cur + dir).rem_euclid(n) as usize;
        self.search.current = Some(next);
        s.reveal_abs(self.search.matches[next].abs);
    }

    fn search_bar(&mut self, ui: &mut Ui, rect: Rect, s: &Arc<TermSession>, env: &mut PaneEnv) {
        if !self.search.open {
            return;
        }
        let w = 420.0f32.min(rect.width() - 16.0);
        let bar = Rect::from_min_size(Pos2::new(rect.right() - w - 14.0, rect.top() + 6.0), Vec2::new(w, 30.0));
        let mut close = false;
        let mut step = 0;
        let mut all = false;
        ui.scope_builder(UiBuilder::new().max_rect(bar), |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let te = ui.add(egui::TextEdit::singleline(&mut self.search.query).hint_text("Find").desired_width(150.0));
                    if self.search.focus {
                        te.request_focus();
                        self.search.focus = false;
                    }
                    if te.has_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            step = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
                            te.request_focus();
                        }
                    }
                    let mut tog = |ui: &mut Ui, v: &mut bool, label: &str, tip: &str| {
                        if ui.add(egui::Button::new(label).selected(*v).small()).on_hover_text(tip).clicked() {
                            *v = !*v;
                            true
                        } else {
                            false
                        }
                    };
                    let mut changed = te.changed();
                    changed |= tog(ui, &mut self.search.opts.regex, ".*", "Regular expression");
                    changed |= tog(ui, &mut self.search.opts.case_sensitive, "Aa", "Match case");
                    changed |= tog(ui, &mut self.search.opts.whole_word, "ab", "Whole word");
                    if changed {
                        self.search.changed_at = Some(Instant::now());
                    }
                    let count = if self.search.query.is_empty() {
                        String::new()
                    } else if self.search.matches.is_empty() {
                        "No results".into()
                    } else {
                        format!("{} of {}", self.search.current.map_or(0, |c| c + 1), self.search.matches.len())
                    };
                    ui.label(egui::RichText::new(count).small().color(env.theme.ui.muted));
                    if ui.small_button("▲").on_hover_text("Previous (Shift+Enter)").clicked() {
                        step = -1;
                    }
                    if ui.small_button("▼").on_hover_text("Next (Enter)").clicked() {
                        step = 1;
                    }
                    all = ui.small_button("All").on_hover_text("Search in all tabs").clicked();
                    if ui.small_button("✕").on_hover_text("Close (Esc)").clicked() {
                        close = true;
                    }
                });
            });
        });
        if close {
            self.close_search();
            env.out.push(TabOut::Focus { pane: self.id });
            return;
        }
        // Debounced incremental search (50 ms).
        if let Some(t) = self.search.changed_at {
            if t.elapsed() >= Duration::from_millis(50) {
                self.search.changed_at = None;
                self.refresh_search(s, true);
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(55));
            }
        }
        if step != 0 {
            if self.search.computed.as_ref() != Some(&(self.search.query.clone(), self.search.opts)) {
                self.refresh_search(s, true);
            } else {
                self.search_step(s, step);
            }
        }
        if all {
            env.out.push(TabOut::SearchAll { query: self.search.query.clone(), opts: self.search.opts });
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Flags {
    pub broadcast: bool,
}

fn draw_drop_overlay(p: &egui::Painter, rect: Rect, o: &DropOverlay, theme: &Theme) {
    let border = match o.severity {
        Severity::Info => theme.ui.accent,
        Severity::Success => theme.ui.success,
        Severity::Warning => theme.ui.warning,
        Severity::Error => theme.ui.error,
    };
    let title = p.layout_no_wrap(o.title.clone(), egui::FontId::proportional(14.0), theme.ui.foreground);
    let detail = p.layout(o.detail.clone(), egui::FontId::proportional(11.0), theme.ui.muted, (rect.width() * 0.7).max(120.0));
    let w = title.size().x.max(detail.size().x).max(200.0) + 32.0;
    let h = title.size().y + detail.size().y + 28.0;
    let r = Rect::from_center_size(rect.center(), Vec2::new(w, h));
    p.rect_filled(r, 8.0, theme.ui.card_bg);
    p.rect_stroke(r, 8.0, Stroke::new(2.0, border), StrokeKind::Inside);
    p.galley(Pos2::new(r.center().x - title.size().x / 2.0, r.top() + 10.0), title.clone(), theme.ui.foreground);
    p.galley(Pos2::new(r.center().x - detail.size().x / 2.0, r.top() + 14.0 + title.size().y), detail, theme.ui.muted);
}

// =========================================================================================== Tab

pub struct Tab {
    pub id: u64,
    pub title: String,
    pub title_locked: bool,
    pub color: Option<Color32>,
    pub pinned: bool,
    pub group: String,
    pub read_only: bool,
    pub broadcast: bool,
    /// Original command line (profile / session / quick connect) — saved for restore and "Save as session".
    pub command: String,
    pub session_id: Option<String>,
    pub starting_command: Option<String>,
    pub cwd: Option<String>,
    pub activity: bool,
    pub layout: Layout,
    pub panes: Vec<Pane>,
    pub focused: u64,
    pub logging: bool,
    pub font_override: f32,
    pub theme_bg: Option<Color32>,
    rects: Vec<(u64, Rect)>,
    next_pane: u64,
}

impl Tab {
    pub fn new(id: u64, title: String, command: String, req: LaunchReq, first_pane: u64) -> Tab {
        let mut p = Pane::new(first_pane, req.clone());
        p.start_cwd = req.cwd.clone();
        Tab {
            id,
            title,
            title_locked: false,
            color: None,
            pinned: false,
            group: String::new(),
            read_only: false,
            broadcast: false,
            command,
            session_id: req.session_id.clone(),
            starting_command: req.starting_command.clone(),
            cwd: req.cwd.clone(),
            activity: false,
            layout: Layout::Leaf(first_pane),
            panes: vec![p],
            focused: first_pane,
            logging: false,
            font_override: 0.0,
            theme_bg: None,
            rects: vec![],
            next_pane: first_pane + 1,
        }
    }

    pub fn pane(&self, id: u64) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    /// The pane whose last laid-out rect contains `p` (egui points).
    pub fn pane_at(&self, p: Pos2) -> Option<u64> {
        self.rects.iter().find(|(_, r)| r.contains(p)).map(|(id, _)| *id)
    }

    pub fn pane_mut(&mut self, id: u64) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.pane(self.focused).or(self.panes.first())
    }

    pub fn focused_pane_mut(&mut self) -> Option<&mut Pane> {
        let f = self.focused;
        if self.panes.iter().any(|p| p.id == f) { self.pane_mut(f) } else { self.panes.first_mut() }
    }

    pub fn alloc_pane_id(&mut self, global: &mut u64) -> u64 {
        *global += 1;
        *global
    }

    /// Split `from` (default: focused). The new pane inherits the live LOCAL cwd and the tab's flags (§7.4).
    pub fn split(&mut self, dir: SplitDir, from: Option<u64>, new_id: u64) -> Option<u64> {
        if self.panes.len() >= MAX_PANES {
            return None;
        }
        let from = from.unwrap_or(self.focused);
        let src = self.pane(from)?;
        let mut req = src.req.clone();
        req.cwd = src.cwd.clone().filter(|_| src.cwd_local).or_else(|| src.start_cwd.clone()).or(req.cwd);
        let mut p = Pane::new(new_id, req);
        p.read_only = self.read_only;
        p.font_override = self.font_override;
        p.theme_bg = self.theme_bg;
        self.layout.replace(from, Layout::Split { dir, ratio: 0.5, a: Box::new(Layout::Leaf(from)), b: Box::new(Layout::Leaf(new_id)) });
        self.panes.push(p);
        self.focused = new_id;
        Some(new_id)
    }

    /// Close a pane; `true` when it was the last one (the tab should close).
    pub fn close_pane(&mut self, id: u64, kill_tree: bool) -> bool {
        if let Some(i) = self.panes.iter().position(|p| p.id == id) {
            let p = self.panes.remove(i);
            if let Some(s) = &p.session {
                let s = s.clone();
                std::thread::spawn(move || s.close(ut_pty::CloseMode::Graceful, kill_tree));
            }
            let order = i.min(self.panes.len().saturating_sub(1));
            match std::mem::replace(&mut self.layout, Layout::Leaf(0)).remove(id) {
                Some(l) => self.layout = l,
                None => return true,
            }
            if self.focused == id {
                self.focused = self.panes.get(order).map_or(0, |p| p.id);
            }
        }
        self.panes.is_empty()
    }

    pub fn unsplit_all(&mut self, kill_tree: bool) {
        let keep = self.panes.first().map(|p| p.id);
        let ids: Vec<u64> = self.panes.iter().skip(1).map(|p| p.id).collect();
        for id in ids {
            self.close_pane(id, kill_tree);
        }
        if let Some(k) = keep {
            self.focused = k;
            self.layout = Layout::Leaf(k);
        }
    }

    /// Geometric focus move (§7.4). `false` when there is no pane that way (the key then passes through).
    pub fn move_focus(&mut self, dx: i32, dy: i32) -> bool {
        let Some((_, cur)) = self.rects.iter().find(|(i, _)| *i == self.focused).cloned() else { return false };
        let mut best: Option<(u64, f32)> = None;
        for (id, r) in &self.rects {
            if *id == self.focused {
                continue;
            }
            let ox = r.right().min(cur.right()) - r.left().max(cur.left());
            let oy = r.bottom().min(cur.bottom()) - r.top().max(cur.top());
            let d = match (dx, dy) {
                (1, 0) if r.left() >= cur.right() - 1.0 && oy > 0.0 => Some(r.left() - cur.right()),
                (-1, 0) if r.right() <= cur.left() + 1.0 && oy > 0.0 => Some(cur.left() - r.right()),
                (0, 1) if r.top() >= cur.bottom() - 1.0 && ox > 0.0 => Some(r.top() - cur.bottom()),
                (0, -1) if r.bottom() <= cur.top() + 1.0 && ox > 0.0 => Some(cur.top() - r.bottom()),
                _ => None,
            };
            if let Some(d) = d {
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((*id, d));
                }
            }
        }
        match best {
            Some((id, _)) => {
                self.focused = id;
                true
            }
            None => false,
        }
    }

    /// See [`Pane::background_tick`]; returns true when any pane produced output.
    pub fn background_tick(&mut self, ctx: &egui::Context, area: Rect, env: &mut PaneEnv, allow_spawn: bool) {
        let mut panes = Vec::new();
        place(&self.layout, area, &mut vec![], &mut panes, &mut Vec::new());
        self.rects = panes.clone();
        let mut out = false;
        for (id, r) in panes {
            if let Some(p) = self.panes.iter_mut().find(|p| p.id == id) {
                out |= p.background_tick(ctx, r, env, allow_spawn);
            }
        }
        if out {
            self.activity = true;
        }
    }

    pub fn layout_rects(&mut self, area: Rect) -> &[(u64, Rect)] {
        let mut panes = Vec::new();
        let mut sp = Vec::new();
        place(&self.layout, area, &mut vec![], &mut panes, &mut sp);
        self.rects = panes;
        &self.rects
    }

    /// Draw the tab's panes and splitters into `area`.
    pub fn show(&mut self, ui: &mut Ui, area: Rect, env: &mut PaneEnv, window_focused: bool) {
        let mut panes = Vec::new();
        let mut sp = Vec::new();
        place(&self.layout, area, &mut vec![], &mut panes, &mut sp);
        self.rects = panes.clone();
        let multi = panes.len() > 1;
        let flags = Flags { broadcast: self.broadcast };
        for (id, r) in &panes {
            let focused = *id == self.focused;
            let ro = self.read_only;
            if let Some(pane) = self.panes.iter_mut().find(|p| p.id == *id) {
                pane.read_only = ro;
                pane.show(ui, *r, env, focused && (window_focused || multi), multi, flags);
            }
        }
        // splitters
        for s in sp {
            let id = egui::Id::new(("split", self.id, &s.path));
            let hit = s.rect.expand(3.0);
            let resp = ui.interact(hit, id, Sense::drag());
            let col = if resp.hovered() || resp.dragged() { env.theme.ui.accent } else { env.theme.ui.splitter };
            ui.painter().rect_filled(s.rect, 0.0, col);
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(if s.dir == SplitDir::Right { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
            }
            if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (pos, span, start) = if s.dir == SplitDir::Right { (p.x, s.parent.width() - SPLITTER, s.parent.left()) } else { (p.y, s.parent.height() - SPLITTER, s.parent.top()) };
                    let lo = (MIN_PANE / span).min(0.45);
                    if let Layout::Split { ratio, .. } = self.layout.node_mut(&s.path) {
                        *ratio = ((pos - start) / span).clamp(lo, 1.0 - lo);
                    }
                }
            }
        }
    }

    pub fn any_running(&self) -> usize {
        self.panes.iter().filter(|p| p.session.is_some() && !p.exited && (p.phase == Phase::Running || p.kind == ShellKind::Ssh)).count()
    }

    pub fn live_cwd(&self) -> Option<String> {
        self.focused_pane().and_then(|p| p.cwd.clone().filter(|_| p.cwd_local))
    }
}
