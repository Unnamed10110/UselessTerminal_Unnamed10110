//! The application: state, per-frame orchestration and window-state persistence. Chrome drawing is in `chrome`,
//! actions/commands in `actions`.

use crate::core::{Banner, BannerKind, Core, LaunchReq};
use crate::fonts::{self, Fonts};
use crate::icons::IconCache;
use crate::kit::{Dialog, Toasts};
use crate::palette::Palette;
use crate::panels::{Cmd, TabInfo};
use crate::settings_ui::SettingsUi;
use crate::sidebar::Sidebar;
use crate::tab::{Layout, SplitDir, Tab, TabOut, PaneEnv};
use crate::theme::{parse_color, Theme};
use crate::winhooks::{self, Hotkey, Tray};
use egui::{Color32, Context, Pos2, Rect, Vec2};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ut_core::windowstate::{PaneLayout, SplitDir as WsDir, TabState, WindowState};
use ut_core::Settings;
use ut_term::input::Router;

#[derive(Clone, Debug, Default)]
pub struct StartArgs {
    pub cwd: Option<String>,
    pub session: Option<String>,
    pub workspace: Option<String>,
    pub command: Vec<String>,
}

impl StartArgs {
    /// `--cwd <dir> --session <name|id> --workspace <name> -- <command…>` (§16.2).
    pub fn parse(args: &[String]) -> Self {
        let mut a = StartArgs::default();
        let mut it = args.iter().skip(1);
        while let Some(x) = it.next() {
            match x.as_str() {
                "--cwd" => a.cwd = it.next().cloned(),
                "--session" => a.session = it.next().cloned(),
                "--workspace" => a.workspace = it.next().cloned(),
                "--" => a.command = it.by_ref().cloned().collect(),
                _ => {}
            }
        }
        a
    }
}

/// What a dialog was opened for; results come back to [`App::on_dialog`].
pub enum DialogId {
    CloseWindow,
    CloseTab(u64),
    CloseOthers(u64),
    CloseRight(u64),
    RenameTab(u64),
    SetGroup(u64),
    PasteMulti { pane: u64, text: String },
    WorkspaceName,
    ReplaceTabs(String),
    SaveSession(u64),
    QuickConnect,
    OpenWorkspace(String),
    NewTabName { command: String, color: String },
    OpenLink(String),
    DropConflict,
}

pub struct MenuState {
    pub pos: Pos2,
    pub kind: MenuKind,
}

pub enum MenuKind {
    Tab(u64),
    Pane { tab: u64, pane: u64 },
    Shells,
}

pub struct NewTab {
    pub title: String,
    pub locked: bool,
    pub color: Option<Color32>,
    pub command: String,
    pub req: LaunchReq,
    pub pinned: bool,
    pub group: String,
    pub read_only: bool,
    pub font_override: f32,
    pub theme_bg: Option<Color32>,
    pub layout: Option<PaneLayout>,
}

impl NewTab {
    pub fn simple(title: &str, command: &str, req: LaunchReq) -> Self {
        Self { title: title.into(), locked: false, color: None, command: command.into(), req, pinned: false, group: String::new(), read_only: false, font_override: 0.0, theme_bg: None, layout: None }
    }
}

pub struct BannerItem {
    pub kind: BannerKind,
    pub text: String,
    pub action: Option<BannerAction>,
}

#[derive(Clone, Copy)]
pub enum BannerAction {
    OpenSettingsFile,
    ResetSettings,
}

pub struct App {
    pub core: Arc<Core>,
    pub cfg: Settings,
    pub theme: Theme,
    pub saved_theme: Option<Theme>,
    pub fonts: Fonts,
    pub font_key: (String, String, u32, u32),
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub next_tab: u64,
    pub next_pane: u64,
    pub router: Arc<Router>,
    pub hwnd: Option<isize>,
    pub window_focused: bool,
    pub maximized: bool,
    pub minimized: bool,
    pub normal_bounds: Option<ut_core::Rect>,
    pub close_confirmed: bool,
    pub dialog: Option<(DialogId, Dialog)>,
    pub drops: crate::drops::Drops,
    pub quake_slide: Option<crate::quake::Slide>,
    /// Presented as the Quake drop-down (not saved as the window's normal bounds).
    pub quake_on: bool,
    pub quake_was_focused: bool,
    pub menu: Option<MenuState>,
    pub toasts: Toasts,
    pub banners: Vec<BannerItem>,
    pub palette: Palette,
    pub settings_ui: SettingsUi,
    pub sidebar: Sidebar,
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub browser_open: bool,
    pub browser_width: f32,
    pub browser: crate::browser::Browser,
    pub icons: IconCache,
    pub bg_texture: Option<egui::TextureHandle>,
    bg_key: String,
    pub hotkey: Hotkey,
    pub tray: Tray,
    pub branch_tx: Sender<(u64, Option<String>)>,
    pub branch_rx: Receiver<(u64, Option<String>)>,
    pub refocus: bool,
    pub restored: bool,
    pub closing: bool,
    pub last_saved_json: String,
    pub dirty_since: Option<Instant>,
    pub tab_rects: Vec<(u64, Rect)>,
    pub tab_drag: Option<u64>,
    pub scroll_to_active: bool,
    pub pane_area: Rect,
    pub start_args: StartArgs,
    pub replacing: bool,
    pub pending_cmds: Vec<Cmd>,
    pub ctx: Context,
    pub anchor: Mutex<Option<i64>>,
    pub diag_open: bool,
    pub started_at: Instant,
    pub bg_started: bool,
    pub first_frame_done: bool,
    pub fg_poll: Instant,
    pub hidden: bool,
    pub debug_last_dump: Option<Instant>,
    pub last_title: String,
    pub second_instance_rx: Option<Receiver<Vec<String>>>,
    pub burst: std::collections::HashMap<u64, (u32, Instant)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, core: Arc<Core>, args: StartArgs) -> App {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let ctx = cc.egui_ctx.clone();
        let cfg = core.cfg();
        let theme = Theme::from(&core.theme());
        theme.apply(&ctx);
        let fonts = fonts::install(&ctx, &cfg.terminal.font_family, &cfg.ui.font_family, cfg.terminal.font_weight as u16, cfg.terminal.font_size as f32);
        let hwnd = cc.window_handle().ok().and_then(|h| if let RawWindowHandle::Win32(w) = h.as_raw() { Some(w.hwnd.get()) } else { None });
        let rc = ctx.clone();
        let repaint = move || rc.request_repaint();
        let router = Router::new(repaint.clone());
        if let Some(h) = hwnd {
            router.install(h);
            winhooks::install_scroll_bridge(h, repaint.clone());
        }
        let (branch_tx, branch_rx) = channel();
        let mut app = App {
            font_key: (cfg.terminal.font_family.clone(), cfg.ui.font_family.clone(), cfg.terminal.font_weight, cfg.ui.font_size.to_bits() as u32),
            cfg,
            theme,
            saved_theme: None,
            fonts,
            tabs: vec![],
            active: 0,
            next_tab: 1,
            next_pane: 1,
            router,
            hwnd,
            window_focused: true,
            maximized: false,
            minimized: false,
            normal_bounds: None,
            close_confirmed: false,
            dialog: None,
            drops: crate::drops::Drops::new(),
            quake_slide: None,
            quake_on: false,
            quake_was_focused: false,
            menu: None,
            toasts: Toasts::default(),
            banners: vec![],
            palette: Palette::default(),
            settings_ui: SettingsUi::new(),
            sidebar: Sidebar::new(),
            sidebar_open: true,
            sidebar_width: 260.0,
            browser_open: false,
            browser_width: 500.0,
            browser: Default::default(),
            icons: IconCache::default(),
            bg_texture: None,
            bg_key: String::new(),
            hotkey: Hotkey::new(repaint.clone()),
            tray: Tray::new(repaint),
            branch_tx,
            branch_rx,
            refocus: true,
            restored: false,
            closing: false,
            last_saved_json: String::new(),
            dirty_since: None,
            tab_rects: vec![],
            tab_drag: None,
            scroll_to_active: true,
            pane_area: Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0)),
            start_args: args,
            replacing: false,
            pending_cmds: vec![],
            ctx,
            anchor: Mutex::new(None),
            diag_open: false,
            started_at: Instant::now(),
            bg_started: false,
            first_frame_done: false,
            fg_poll: Instant::now(),
            hidden: false,
            debug_last_dump: None,
            last_title: String::new(),
            second_instance_rx: None,
            burst: Default::default(),
            core,
        };
        for Banner { kind, text } in std::mem::take(&mut *app.core.startup.lock()) {
            app.banners.push(BannerItem { kind, text, action: None });
        }
        if matches!(app.core.settings.status(), ut_core::LoadStatus::Corrupt { .. }) {
            app.banners.push(BannerItem { kind: BannerKind::Info, text: "Open the file or reset it (a backup is created).".into(), action: Some(BannerAction::OpenSettingsFile) });
            app.banners.push(BannerItem { kind: BannerKind::Info, text: "Reset settings to defaults (backup created).".into(), action: Some(BannerAction::ResetSettings) });
        }
        if let Err(e) = app.hotkey.register(&app.cfg.quake.hotkey) {
            app.toasts.push(e, crate::kit::ToastKind::Warning);
        }
        app.apply_keymap();
        app.restore_state();
        app.restored = true;
        app
    }

    // --------------------------------------------------------------------------------- tabs

    pub fn alloc_tab_id(&mut self) -> u64 {
        self.next_tab += 1;
        self.next_tab
    }

    pub fn alloc_pane_id(&mut self) -> u64 {
        self.next_pane += 1;
        self.next_pane
    }

    pub fn add_tab(&mut self, nt: NewTab, activate: bool) -> u64 {
        let tid = self.alloc_tab_id();
        let first = self.alloc_pane_id();
        let mut tab = Tab::new(tid, nt.title.clone(), nt.command.clone(), nt.req.clone(), first);
        tab.title_locked = nt.locked;
        tab.color = nt.color;
        tab.pinned = nt.pinned;
        tab.group = nt.group.clone();
        tab.read_only = nt.read_only;
        tab.font_override = nt.font_override;
        tab.theme_bg = nt.theme_bg;
        if let Some(p) = tab.panes.first_mut() {
            p.font_override = nt.font_override;
            p.theme_bg = nt.theme_bg;
            p.read_only = nt.read_only;
        }
        if let Some(pl) = &nt.layout {
            // Restored split layout: every leaf becomes a pane that starts in its last cwd.
            let mut panes = Vec::new();
            let layout = self.from_pane_layout(pl, &nt.req, &mut panes, &nt);
            if !panes.is_empty() {
                tab.focused = panes[0].id;
                tab.layout = layout;
                tab.panes = panes;
            }
        }
        self.tabs.push(tab);
        if activate {
            self.active = self.tabs.len() - 1;
            self.refocus = true;
            self.scroll_to_active = true;
        }
        self.mark_dirty();
        tid
    }

    fn from_pane_layout(&mut self, pl: &PaneLayout, base: &LaunchReq, panes: &mut Vec<crate::tab::Pane>, nt: &NewTab) -> Layout {
        match pl {
            PaneLayout::Leaf { cwd, .. } => {
                let id = self.alloc_pane_id();
                let mut req = base.clone();
                if let Some(c) = cwd.clone().filter(|c| !c.is_empty()) {
                    req.cwd = Some(c);
                }
                let mut p = crate::tab::Pane::new(id, req);
                p.font_override = nt.font_override;
                p.theme_bg = nt.theme_bg;
                p.read_only = nt.read_only;
                panes.push(p);
                Layout::Leaf(id)
            }
            PaneLayout::Split { dir, ratio, a, b } => Layout::Split {
                dir: if matches!(dir, WsDir::Right) { SplitDir::Right } else { SplitDir::Down },
                ratio: *ratio as f32,
                a: Box::new(self.from_pane_layout(a, base, panes, nt)),
                b: Box::new(self.from_pane_layout(b, base, panes, nt)),
            },
        }
    }

    pub fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn activate(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.active = i;
            self.tabs[i].activity = false;
            self.refocus = true;
            self.scroll_to_active = true;
            self.mark_dirty();
        }
    }

    // ---------------------------------------------------------------------------- settings reaction

    /// Re-apply what depends on settings after `core.settings` changed (the settings window, palette themes
    /// and external edits only patch the store).
    pub fn sync_settings(&mut self, ctx: &Context) {
        let new = self.core.cfg();
        if new == self.cfg {
            return;
        }
        let prev = std::mem::replace(&mut self.cfg, new);
        let cfg = &self.cfg;
        if prev.theme != cfg.theme || prev.ui != cfg.ui {
            self.theme = Theme::from(&self.core.theme());
            self.theme.apply(ctx);
            self.push_palette();
        }
        let key = (cfg.terminal.font_family.clone(), cfg.ui.font_family.clone(), cfg.terminal.font_weight, cfg.ui.font_size.to_bits() as u32);
        if key != self.font_key {
            self.font_key = key;
            self.fonts = fonts::install(ctx, &cfg.terminal.font_family, &cfg.ui.font_family, cfg.terminal.font_weight as u16, cfg.terminal.font_size as f32);
        }
        if prev.ui.scale != cfg.ui.scale {
            ctx.set_zoom_factor(cfg.ui.scale.clamp(0.75, 2.0) as f32);
        }
        if prev.quake != cfg.quake {
            if let Err(e) = self.hotkey.register(&cfg.quake.hotkey) {
                self.toasts.push(e, crate::kit::ToastKind::Warning);
            }
        }
        if prev.ui.backdrop != cfg.ui.backdrop {
            self.apply_backdrop();
        }
        if prev.terminal.background_image != cfg.terminal.background_image {
            self.bg_key.clear();
        }
    }

    pub fn push_palette(&self) {
        for t in &self.tabs {
            for p in &t.panes {
                if let Some(s) = &p.session {
                    s.set_palette(self.theme.palette.clone());
                }
            }
        }
    }

    pub fn apply_backdrop(&self) {
        // Mica/acrylic need Windows 11 (build ≥ 22000) / window-vibrancy; wired in a later step.
    }

    pub fn ensure_bg_texture(&mut self, ctx: &Context) {
        let b = &self.cfg.terminal.background_image;
        let key = format!("{}|{}", b.path, std::fs::metadata(&b.path).and_then(|m| m.modified()).ok().map(|t| format!("{t:?}")).unwrap_or_default());
        if key == self.bg_key {
            return;
        }
        self.bg_key = key;
        self.bg_texture = None;
        let path = b.path.trim().to_string();
        if path.is_empty() {
            return;
        }
        const MAX: u64 = 15 * 1024 * 1024;
        let ext_ok = std::path::Path::new(&path).extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"));
        let size_ok = std::fs::metadata(&path).map(|m| m.is_file() && m.len() <= MAX).unwrap_or(false);
        if !ext_ok || !size_ok {
            self.toasts.push("The background image is missing, too large (>15 MiB) or not a supported format.", crate::kit::ToastKind::Warning);
            return;
        }
        match image::open(&path) {
            Ok(img) => {
                let img = img.thumbnail(2560, 1600).into_rgba8();
                let (w, h) = img.dimensions();
                self.bg_texture = Some(ctx.load_texture("bg", egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw()), egui::TextureOptions::LINEAR));
            }
            Err(e) => self.toasts.push(format!("Could not load the background image: {e}"), crate::kit::ToastKind::Warning),
        }
    }

    // ------------------------------------------------------------------------- window state

    pub fn mark_dirty(&mut self) {
        if self.restored && !self.closing && self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
    }

    pub fn to_pane_layout(&self, tab: &Tab, l: &Layout) -> PaneLayout {
        match l {
            Layout::Leaf(id) => {
                let p = tab.pane(*id);
                let cwd = p.and_then(|p| p.cwd.clone().filter(|_| p.cwd_local).or_else(|| p.start_cwd.clone()));
                PaneLayout::Leaf { pane_id: id.to_string(), profile: None, command: None, cwd }
            }
            Layout::Split { dir, ratio, a, b } => PaneLayout::Split {
                dir: if *dir == SplitDir::Right { WsDir::Right } else { WsDir::Down },
                ratio: *ratio as f64,
                a: Box::new(self.to_pane_layout(tab, a)),
                b: Box::new(self.to_pane_layout(tab, b)),
            },
        }
    }

    pub fn serialize_state(&self) -> WindowState {
        let tabs = self
            .tabs
            .iter()
            .map(|t| TabState {
                title: t.title.clone(),
                title_locked: t.title_locked,
                command: t.command.clone(),
                session_id: t.session_id.clone(),
                cwd: t.live_cwd().or_else(|| t.cwd.clone()),
                starting_command: t.starting_command.clone(),
                color: t.color.map(|c| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())),
                pinned: t.pinned,
                group: Some(t.group.clone()).filter(|g| !g.is_empty()),
                read_only: t.read_only,
                layout: Some(self.to_pane_layout(t, &t.layout)),
            })
            .collect();
        let mut ws = WindowState {
            bounds: self.normal_bounds,
            maximized: self.maximized,
            sidebar_open: self.sidebar_open,
            sidebar_width: self.sidebar_width as u32,
            browser_open: self.browser_open,
            browser_width: self.browser_width as u32,
            active_tab_index: self.active,
            tabs,
            ..WindowState::default()
        };
        ws.sanitize();
        ws
    }

    /// Autosave every 15 s only when something changed (so a force-kill still leaves a usable session);
    /// never before restore finished, never after close started (§16.3).
    pub fn save_state(&mut self, force: bool) {
        if !self.restored || (self.closing && !force) {
            return;
        }
        let ws = self.serialize_state();
        let json = serde_json::to_string(&ws).unwrap_or_default();
        if !force && json == self.last_saved_json {
            return;
        }
        self.last_saved_json = json;
        self.core.queue_window_state(&ws);
        if force {
            self.core.flush_all();
        }
        self.dirty_since = None;
    }

    fn restore_state(&mut self) {
        let (ws, _) = self.core.load_window_state();
        self.sidebar_open = ws.sidebar_open;
        self.sidebar_width = ws.sidebar_width as f32;
        self.browser_open = ws.browser_open;
        self.browser_width = ws.browser_width as f32;
        self.normal_bounds = ws.bounds;
        self.maximized = ws.maximized;
        let startup = self.cfg.startup.clone();
        let mut made = 0;
        if startup.mode == ut_core::settings::StartupMode::Workspace {
            if let Some(w) = startup.workspace_id.as_ref().and_then(|id| self.core.workspaces.get(id)) {
                self.open_workspace(&w, false);
                made = self.tabs.len();
            }
        } else if startup.mode == ut_core::settings::StartupMode::RestoreLastSession {
            // Each tab restores on its own: one bad tab must not drop the others (§16.3).
            for ts in &ws.tabs {
                let session = ts.session_id.as_ref().and_then(|id| self.core.sessions.get(id));
                let command = if !ts.command.is_empty() { ts.command.clone() } else { session.as_ref().map(|s| s.full_command()).unwrap_or_default() };
                if command.is_empty() && session.is_none() {
                    continue;
                }
                let req = LaunchReq {
                    command: Some(command.clone()),
                    session_id: session.as_ref().map(|s| s.id.clone()),
                    cwd: ts.cwd.clone(),
                    starting_command: if session.is_some() { None } else { ts.starting_command.clone() },
                    ..Default::default()
                };
                let nt = NewTab {
                    title: if ts.title.is_empty() { "Terminal".into() } else { ts.title.clone() },
                    locked: ts.title_locked,
                    color: ts.color.as_deref().map(parse_color),
                    command,
                    req,
                    pinned: ts.pinned,
                    group: ts.group.clone().unwrap_or_default(),
                    read_only: ts.read_only,
                    font_override: session.as_ref().map_or(0.0, |s| s.font_size as f32),
                    theme_bg: session.as_ref().and_then(|s| Some(s.theme_background.clone()).filter(|b| !b.is_empty())).map(|b| parse_color(&b)),
                    layout: ts.layout.clone(),
                };
                self.add_tab(nt, false);
                made += 1;
            }
            self.active = ws.active_tab_index.min(self.tabs.len().saturating_sub(1));
        }
        if made == 0 || self.tabs.is_empty() {
            self.new_default_tab();
        }
    }

    pub fn new_default_tab(&mut self) {
        let prof = self.core.default_profile();
        let command = prof.as_ref().map(|p| p.command.clone()).unwrap_or_else(|| "cmd.exe".into());
        let req = LaunchReq { command: Some(command.clone()), cwd: Some(ut_fs::home_dir().to_string_lossy().into_owned()), ..Default::default() };
        let mut nt = NewTab::simple("Terminal", &command, req);
        nt.color = prof.as_ref().map(|p| parse_color(&p.color));
        self.add_tab(nt, true);
    }

    // ---------------------------------------------------------------------------------- frame

    pub fn pane_env<'a>(&'a self, out: &'a mut Vec<TabOut>, tab_active: bool) -> PaneEnv<'a> {
        let rc = self.ctx.clone();
        PaneEnv {
            core: &self.core,
            theme: &self.theme,
            fonts: &self.fonts,
            cfg: &self.cfg,
            repaint: Arc::new(move || rc.request_repaint()),
            bg_texture: self.bg_texture.as_ref(),
            bg_opacity: self.cfg.terminal.background_image.opacity as f32,
            tab_active,
            out,
        }
    }

    /// Run the panels' commands.
    pub fn panel_ctx_cmds(&mut self, cmds: Vec<Cmd>) {
        for c in cmds {
            self.run_cmd(c);
        }
    }

    pub fn live_sessions(&self) -> HashSet<String> {
        self.tabs.iter().filter_map(|t| t.session_id.clone()).collect()
    }

    pub fn tab_infos(&self) -> Vec<TabInfo> {
        self.tabs.iter().map(|t| TabInfo { id: t.id, title: t.title.clone() }).collect()
    }

    pub fn any_modal(&self) -> bool {
        self.dialog.is_some() || self.palette.open || self.settings_ui.open || self.sidebar.modal_open() || self.menu.is_some()
    }

    /// Everything the app does around a frame, before panels draw.
    pub fn begin_frame(&mut self, ctx: &Context) {
        self.sync_settings(ctx);
        self.ensure_bg_texture(ctx);
        let vp = ctx.input(|i| i.viewport().clone());
        self.maximized = vp.maximized.unwrap_or(false);
        self.minimized = vp.minimized.unwrap_or(false);
        let focused = vp.focused.unwrap_or(true);
        // egui drops widget focus when the OS window deactivates; take it back when the window returns.
        if focused && !self.window_focused {
            self.refocus = true;
        }
        self.window_focused = focused;
        if !self.maximized && !self.minimized && !self.quake_on {
            if let (Some(r), ppp) = (vp.outer_rect, ctx.pixels_per_point()) {
                if r.width() > 50.0 && r.height() > 50.0 && r.min.x.is_finite() && r.min.y.is_finite() {
                    self.normal_bounds = Some(ut_core::Rect { x: (r.min.x * ppp) as i32, y: (r.min.y * ppp) as i32, width: (r.width() * ppp) as i32, height: (r.height() * ppp) as i32 });
                }
            }
        }
        while let Ok((pane, branch)) = self.branch_rx.try_recv() {
            for t in &mut self.tabs {
                if let Some(p) = t.pane_mut(pane) {
                    p.branch = branch.clone();
                    p.branch_pending = false;
                }
            }
        }
        self.apply_router_actions();
        self.drops_frame(ctx);
        self.quake_tick(ctx);
        self.debug_hooks(ctx);
    }

    pub fn end_frame(&mut self, ctx: &Context) {
        // Tell the keyboard router where keys go (focused pane + broadcast targets) and whether it may take them.
        let mut targets = Vec::new();
        let mut readonly = false;
        let mut term_focus = false;
        if let Some(t) = self.tabs.get(self.active) {
            if let Some(p) = t.focused_pane() {
                term_focus = ctx.memory(|m| m.has_focus(p.view.id()));
                readonly = p.read_only || t.read_only;
                if let Some(s) = p.session.clone().filter(|_| !p.exited) {
                    targets.push(s);
                    if t.broadcast {
                        for o in &t.panes {
                            if o.id != p.id && !o.read_only && !o.exited {
                                if let Some(s) = &o.session {
                                    targets.push(s.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        self.router.set_targets(targets, readonly);
        self.router.set_enabled(term_focus && self.window_focused && !self.any_modal());
        self.router.set_shortcuts_only(!term_focus && self.window_focused && !self.any_modal());
        // The terminal owns keyboard focus unless a text field (search, rename, dialog) holds it or a modal is up;
        // a clicked-away or freshly activated window would otherwise leave nothing focused and the keys unrouted.
        if self.window_focused && !self.any_modal() && (self.refocus || ctx.memory(|m| m.focused().is_none())) {
            if let Some(p) = self.tabs.get(self.active).and_then(|t| t.focused_pane()) {
                ctx.memory_mut(|m| m.request_focus(p.view.id()));
            }
        }
        if term_focus || self.any_modal() {
            self.refocus = false;
        }
        if let Some(t) = self.dirty_since {
            if t.elapsed() >= Duration::from_secs(15) {
                self.save_state(false);
            } else {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
        // lazy restore: start the other tabs 2 s after startup
        if !self.bg_started && self.started_at.elapsed() >= Duration::from_secs(2) {
            self.bg_started = true;
        }
    }

    pub fn apply_router_actions(&mut self) {
        for (action, vk, mods) in self.router.take_actions() {
            self.run_action_vk(&action, Some(vk), mods);
        }
    }
}

impl App {
    /// Visible (not hidden to the tray / Quake).
    pub fn hidden_flag(&self) -> bool {
        !self.hidden
    }

    pub fn set_hidden(&mut self, h: bool) {
        self.hidden = h;
    }

    /// A second launch forwards its arguments (`--cwd`, `--session`, `--workspace`, `-- cmd`) to this instance.
    pub fn poll_second_instance(&mut self, ctx: &Context) {
        let Some(rx) = &self.second_instance_rx else { return };
        let mut got = Vec::new();
        while let Ok(a) = rx.try_recv() {
            got.push(a);
        }
        for a in got {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.hidden = false;
            self.open_start_args(&StartArgs::parse(&a));
        }
    }

    pub fn open_start_args(&mut self, a: &StartArgs) {
        if let Some(name) = &a.session {
            let s = self.core.sessions.list().into_iter().find(|s| &s.id == name || s.name.eq_ignore_ascii_case(name));
            if let Some(s) = s {
                return self.open_session(&s.id, false);
            }
        }
        if let Some(name) = &a.workspace {
            let w = self.core.workspaces.list().into_iter().find(|w| &w.id == name || w.name.eq_ignore_ascii_case(name));
            if let Some(w) = w {
                return self.open_workspace(&w, false);
            }
        }
        if !a.command.is_empty() {
            let cmd = a.command.iter().map(|x| if x.contains(char::is_whitespace) { format!("\"{x}\"") } else { x.clone() }).collect::<Vec<_>>().join(" ");
            let req = LaunchReq { command: Some(cmd.clone()), cwd: a.cwd.clone().or_else(|| Some(ut_fs::home_dir().to_string_lossy().into_owned())), ..Default::default() };
            self.add_tab(NewTab::simple(a.command.first().map(String::as_str).unwrap_or("Terminal"), &cmd, req), true);
        } else if a.cwd.is_some() {
            let prof = self.core.default_profile();
            let command = prof.map(|p| p.command).unwrap_or_else(|| "cmd.exe".into());
            let req = LaunchReq { command: Some(command.clone()), cwd: a.cwd.clone(), ..Default::default() };
            self.add_tab(NewTab::simple("Terminal", &command, req), true);
        }
    }

    /// Close every shell (console processes die with their pane) and flush all files.
    pub fn shutdown(&mut self) {
        self.save_state(true);
        self.browser_shutdown();
        let kill = self.cfg.processes.kill_console_tree_on_close;
        let sessions: Vec<_> = self.tabs.iter().flat_map(|t| t.panes.iter().filter_map(|p| p.session.clone())).collect();
        let handles: Vec<_> = sessions.into_iter().map(|s| std::thread::spawn(move || s.close(ut_pty::CloseMode::Graceful, kill))).collect();
        let t0 = Instant::now();
        for h in handles {
            while !h.is_finished() && t0.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.core.flush_all();
    }
}
