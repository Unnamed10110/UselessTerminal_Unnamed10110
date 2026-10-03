//! One terminal session: ConPTY (`ut-pty`) → backend scanner (`ut-vt-scan`) → emulator grid
//! (`alacritty_terminal`). Everything the UI needs is read from here; the reader thread is the only writer
//! of the grid besides resizes and selection.
//!
//! Prompt rows (OSC 133) are tracked as *absolute* row numbers: `abs = a_top + grid_line`, where `a_top` is
//! the absolute index of the screen's top row. `a_top` grows by exactly the number of lines pushed into
//! history (history never saturates because the grid's limit is `scrollback + SLACK` and we trim it
//! ourselves, so `history_size` deltas are exact).

use crate::taps::{tap_path, LogOptions, Tap};
use alacritty_terminal::event::{Event as TEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Osc52 as TOsc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, Rgb};
use parking_lot::{Mutex, RwLock};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use ut_pty::{CloseMode, Pty, Sink, SpawnConfig, SpawnError};
use ut_vt_scan::{CastHeader, Config as ScanConfig, Event as VtEvent, Osc52, Phase, Scanner};

/// Lines kept above the user's scrollback before we trim (hysteresis for [`TermSession::trim_history`]).
const SLACK: usize = 1000;
const MAX_MARKS: usize = 512;

/// Timed writes into the PTY after spawn (integration init lines, starting commands; §6.5).
#[derive(Clone, Debug)]
pub enum Delivery {
    /// Wait for the first output, then `quiet_ms` without output, but never longer than `cap_ms` in total.
    AfterFirstOutputQuiet { quiet_ms: u64, cap_ms: u64, bytes: Vec<u8> },
}

pub struct SessionConfig {
    pub command_line: String,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    pub scrollback: usize,
    pub conpty_dll: Option<PathBuf>,
    pub osc52: Osc52,
    pub computer_name: String,
    pub deliveries: Vec<Delivery>,
    /// Wakes the UI (egui `request_repaint`). Called from the reader thread, rate-limited by the session.
    pub repaint: Arc<dyn Fn() + Send + Sync>,
    pub palette: Palette,
    pub cursor: CursorSetting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorSetting {
    Bar,
    Block,
    Underline,
}

/// Colours the emulator needs to answer OSC 4/10/11 queries and the renderer to resolve cells.
#[derive(Clone, Debug)]
pub struct Palette {
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub cursor: [u8; 3],
    pub ansi: [[u8; 3]; 16],
}

impl Default for Palette {
    fn default() -> Self {
        let a = [
            [0, 0, 0], [205, 49, 49], [13, 188, 121], [229, 229, 16], [36, 114, 200], [188, 63, 188], [17, 168, 205], [229, 229, 229],
            [102, 102, 102], [241, 76, 76], [35, 209, 139], [245, 245, 67], [59, 142, 234], [214, 112, 214], [41, 184, 219], [255, 255, 255],
        ];
        Self { fg: [229, 229, 229], bg: [0, 0, 0], cursor: [255, 255, 255], ansi: a }
    }
}

impl Palette {
    /// 256-colour + special entries of the xterm palette (`index` < 256), honouring the theme's 16 ANSI colours.
    pub fn indexed(&self, i: usize) -> [u8; 3] {
        match i {
            0..=15 => self.ansi[i],
            16..=231 => {
                let n = i - 16;
                let v = |c: usize| if c == 0 { 0 } else { (55 + 40 * c) as u8 };
                [v(n / 36), v((n / 6) % 6), v(n % 6)]
            }
            232..=255 => {
                let g = (8 + 10 * (i - 232)) as u8;
                [g, g, g]
            }
            256 => self.fg,
            257 => self.bg,
            258 => self.cursor,
            _ => self.fg,
        }
    }
}

// ----------------------------------------------------------------------------------- events for the UI

#[derive(Clone, Debug)]
pub enum PaneEvent {
    Title(String),
    Cwd { path: String, host: String, local: bool },
    Shell { kind: ut_vt_scan::ShellKind, exit_code: Option<i32>, duration_ms: Option<u64> },
    Bell,
    Progress { state: u8, value: u8 },
    Notify { title: String, body: String },
    Exited { code: Option<i32> },
}

#[derive(Clone, Debug)]
pub struct Mark {
    /// Absolute row of the prompt line.
    pub abs: i64,
    pub exit: Option<i32>,
    pub duration_ms: Option<u64>,
    pub done: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Info {
    pub title: String,
    pub cwd: Option<String>,
    pub cwd_host: String,
    pub cwd_local: bool,
    pub last_exit: Option<i32>,
    pub last_duration_ms: Option<u64>,
    pub phase: Phase,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub alt_screen: bool,
}

pub(crate) struct Track {
    pub(crate) a_top: i64,
    last_h: usize,
    cap: usize,
}

pub struct Shared {
    /// Set once the PTY exists; escape-sequence replies (device status, OSC queries) go through it.
    pty: OnceLock<Arc<dyn Pty>>,
    palette: RwLock<Palette>,
    cell: Mutex<(f32, f32)>,
    osc52: Osc52,
}

#[derive(Clone)]
pub struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, e: TEvent) {
        let write = |s: String| {
            if let Some(p) = self.0.pty.get() {
                p.write(s.as_bytes());
            }
        };
        match e {
            TEvent::PtyWrite(s) => write(s),
            TEvent::ColorRequest(i, fmt) => {
                let c = self.0.palette.read().indexed(i);
                write(fmt(Rgb { r: c[0], g: c[1], b: c[2] }));
            }
            TEvent::TextAreaSizeRequest(fmt) => {
                let (w, h) = *self.0.cell.lock();
                write(fmt(WindowSize { num_lines: 0, num_cols: 0, cell_width: w as u16, cell_height: h as u16 }));
            }
            // Title/bell/clipboard come from the backend scanner (size caps, read policy), not from here.
            _ => {}
        }
    }
}

struct Size {
    cols: usize,
    lines: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

struct ScanState {
    scanner: Scanner,
    events: Vec<VtEvent>,
    offs: Vec<usize>,
}

pub struct Inner {
    pub term: FairMutex<Term<Listener>>,
    proc: Mutex<Processor>,
    scan: Mutex<ScanState>,
    pub(crate) track: Mutex<Track>,
    shared: Arc<Shared>,
    pub(crate) info: Mutex<Info>,
    pub(crate) marks: Mutex<VecDeque<Mark>>,
    events: Mutex<Vec<PaneEvent>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    dirty: AtomicBool,
    pub log: Mutex<Option<Tap>>,
    pub cast: Mutex<Option<Tap>>,
    start: Instant,
    first_output: AtomicBool,
    last_output_ms: AtomicU64,
    /// Bytes received in total (diagnostics / throughput).
    pub rx_bytes: AtomicU64,
    pub frames: AtomicU64,
    size: Mutex<(u16, u16)>,
    /// Every size the session had since it started, with the time (ms): diagnostics for resize-related glitches.
    resizes: Mutex<Vec<(u16, u16, u64)>>,
}

pub struct TermSession {
    pub inner: Arc<Inner>,
    pty: RwLock<Arc<dyn Pty>>,
    pub command_line: String,
    start_req: Mutex<SpawnConfig>,
    pub start_cwd: Option<PathBuf>,
}

impl Inner {
    pub(crate) fn wake(&self) {
        // One pending repaint request at a time: a flood must not call into egui per frame.
        if !self.dirty.swap(true, Ordering::AcqRel) {
            (self.repaint)();
        }
    }

    /// Called by the UI once per frame before it reads the grid.
    pub fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    fn track_scroll(&self, term: &Term<Listener>) {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let mut t = self.track.lock();
        let h = term.grid().history_size();
        if h >= t.last_h {
            t.a_top += (h - t.last_h) as i64;
        } // else: history was cleared (ED 3): the screen's absolute row is unchanged
        t.last_h = h;
    }

    /// Absolute row of grid line `l` (valid for the primary screen).
    fn abs_of(&self, l: i32) -> i64 {
        self.track.lock().a_top + l as i64
    }

    fn push_event(&self, e: PaneEvent) {
        self.events.lock().push(e);
    }

    /// Shell-integration marks and the pane info, from one scanner event. `term` is the grid advanced up to
    /// exactly this event, so the cursor row is the prompt's row.
    fn on_marker(&self, term: &Term<Listener>, kind: ut_vt_scan::ShellKind, exit: Option<i32>, dur: Option<u64>) {
        use ut_vt_scan::ShellKind::*;
        let mut marks = self.marks.lock();
        match kind {
            PromptStart => {
                let abs = self.abs_of(term.grid().cursor.point.line.0);
                if marks.back().is_none_or(|m| m.abs != abs) {
                    marks.push_back(Mark { abs, exit: None, duration_ms: None, done: false });
                    while marks.len() > MAX_MARKS {
                        marks.pop_front();
                    }
                }
            }
            CommandEnd => {
                if let Some(m) = marks.iter_mut().rev().find(|m| !m.done) {
                    m.done = true;
                    m.exit = exit;
                    m.duration_ms = dur;
                }
            }
            InputStart | CommandStart => {}
        }
    }
}

impl Sink for InnerSink {
    fn frame(&self, data: &[u8]) {
        let i = &*self.0;
        i.rx_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        i.frames.fetch_add(1, Ordering::Relaxed);
        i.first_output.store(true, Ordering::Release);
        i.last_output_ms.store(i.start.elapsed().as_millis() as u64, Ordering::Release);
        if let Some(t) = i.log.lock().as_ref() {
            t.push(data);
        }
        if let Some(t) = i.cast.lock().as_ref() {
            t.push(data);
        }

        let mut scan = i.scan.lock();
        let ScanState { scanner, events, offs } = &mut *scan;
        events.clear();
        offs.clear();
        scanner.feed_at(data, Instant::now(), events, Some(offs));

        {
            // Advance the grid in segments so shell markers sample the cursor exactly where the OSC 133
            // sequence ended (usually one segment; a prompt is two or three).
            let mut term = i.term.lock();
            let mut proc = i.proc.lock();
            let mut prev = 0;
            for (ev, &off) in events.iter().zip(offs.iter()) {
                if let VtEvent::Shell { kind, exit_code, duration_ms } = ev {
                    let off = off.clamp(prev, data.len());
                    proc.advance(&mut *term, &data[prev..off]);
                    prev = off;
                    i.track_scroll(&term);
                    i.on_marker(&term, *kind, *exit_code, *duration_ms);
                }
            }
            if prev < data.len() {
                proc.advance(&mut *term, &data[prev..]);
            }
            i.track_scroll(&term);
            i.trim_history(&mut term);
        }

        for ev in events.drain(..) {
            self.handle(ev);
        }
        i.wake();
    }

    fn exited(&self, code: Option<u32>) {
        let i = &*self.0;
        let code = code.map(|c| c as i32);
        {
            let mut term = i.term.lock();
            let msg = format!("\r\n\x1b[2m[process exited with code {}]\x1b[0m\r\n", code.map_or("?".into(), |c| c.to_string()));
            i.proc.lock().advance(&mut *term, msg.as_bytes());
            i.track_scroll(&term);
        }
        if let Some(t) = i.log.lock().take() {
            t.stop();
        }
        if let Some(t) = i.cast.lock().take() {
            t.stop();
        }
        {
            let mut inf = i.info.lock();
            inf.exited = true;
            inf.exit_code = code;
        }
        i.push_event(PaneEvent::Exited { code });
        i.wake();
    }
}

impl Inner {
    /// Keep history at `cap` lines (+ SLACK of hysteresis) so the grid never saturates (see module docs).
    fn trim_history(&self, term: &mut Term<Listener>) {
        let mut t = self.track.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let h = term.grid().history_size();
        if h > t.cap + SLACK {
            let g = term.grid_mut();
            g.update_history(t.cap);
            g.update_history(t.cap + SLACK + SLACK);
            t.last_h = g.history_size();
        }
    }
}

pub(crate) struct InnerSink(pub(crate) Arc<Inner>);

impl InnerSink {
    fn handle(&self, ev: VtEvent) {
        let i = &*self.0;
        match ev {
            VtEvent::Title { title } => {
                i.info.lock().title = title.clone();
                i.push_event(PaneEvent::Title(title));
            }
            VtEvent::Cwd { path, host, local } => {
                {
                    let mut inf = i.info.lock();
                    inf.cwd = Some(path.clone());
                    inf.cwd_host = host.clone();
                    inf.cwd_local = local;
                }
                i.push_event(PaneEvent::Cwd { path, host, local });
            }
            VtEvent::Shell { kind, exit_code, duration_ms } => {
                {
                    let mut inf = i.info.lock();
                    inf.phase = match kind {
                        ut_vt_scan::ShellKind::PromptStart => Phase::Prompt,
                        ut_vt_scan::ShellKind::InputStart => Phase::Input,
                        ut_vt_scan::ShellKind::CommandStart => Phase::Running,
                        ut_vt_scan::ShellKind::CommandEnd => Phase::Done,
                    };
                    if kind == ut_vt_scan::ShellKind::CommandEnd {
                        if exit_code.is_some() {
                            inf.last_exit = exit_code;
                        }
                        inf.last_duration_ms = duration_ms;
                    }
                }
                i.push_event(PaneEvent::Shell { kind, exit_code, duration_ms });
            }
            VtEvent::Bell => i.push_event(PaneEvent::Bell),
            VtEvent::Osc52Write { data } => {
                if let Ok(mut c) = arboard::Clipboard::new() {
                    let _ = c.set_text(String::from_utf8_lossy(&data).into_owned());
                }
            }
            VtEvent::Osc52ReadRequest => {
                if i.shared.osc52 == Osc52::ReadWrite {
                    use std::fmt::Write;
                    let text = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()).unwrap_or_default();
                    let mut b64 = String::new();
                    b64_encode(text.as_bytes(), &mut b64);
                    let mut s = String::new();
                    let _ = write!(s, "\x1b]52;c;{b64}\x1b\\");
                    if let Some(p) = i.shared.pty.get() {
                        p.write(s.as_bytes());
                    }
                }
            }
            VtEvent::Progress { state, value } => i.push_event(PaneEvent::Progress { state, value }),
            VtEvent::Notify { title, body } => i.push_event(PaneEvent::Notify { title, body }),
            VtEvent::AltScreen { active } => i.info.lock().alt_screen = active,
            VtEvent::Output => {}
        }
    }
}

fn b64_encode(data: &[u8], out: &mut String) {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
}

impl TermSession {
    pub fn spawn(cfg: SessionConfig) -> Result<Arc<TermSession>, SpawnError> {
        let (cols, rows) = (cfg.cols.max(2), cfg.rows.max(1));
        let (inner, shared) = new_inner(&cfg);
        let mut sc = SpawnConfig::new(cfg.command_line.clone(), cols, rows);
        sc.cwd = cfg.cwd.clone();
        sc.env = cfg.env.clone();
        sc.conpty_dll = cfg.conpty_dll.clone();
        // The reader parses synchronously into the grid, so there is no UI-side queue to protect: credit-based
        // flow control is off (the emulator's parse speed is the natural back-pressure).
        sc.high_water = usize::MAX / 4;
        sc.low_water = usize::MAX / 8;
        let pty = ut_pty::spawn(sc.clone(), Arc::new(InnerSink(inner.clone())))?;
        let _ = shared.pty.set(pty.clone());
        if !cfg.deliveries.is_empty() {
            let (i2, p2, d) = (inner.clone(), pty.clone(), cfg.deliveries.clone());
            std::thread::Builder::new().name("pty-deliver".into()).spawn(move || run_deliveries(i2, p2, d)).ok();
        }
        Ok(Arc::new(TermSession { inner, pty: RwLock::new(pty), command_line: cfg.command_line, start_req: Mutex::new(sc), start_cwd: cfg.cwd }))
    }

    /// Start a new process in this same grid (Enter after the process exited, §4.7).
    pub fn respawn(&self) -> Result<(), SpawnError> {
        let i = &self.inner;
        let (c, r) = *i.size.lock();
        let mut sc = self.start_req.lock().clone();
        sc.cols = c;
        sc.rows = r;
        {
            let mut inf = i.info.lock();
            inf.exited = false;
            inf.exit_code = None;
            inf.phase = Phase::None;
        }
        let pty = ut_pty::spawn(sc, Arc::new(InnerSink(i.clone())))?;
        // `OnceLock` cannot be replaced: replies go through the CURRENT pty held by the session, and the
        // listener keeps the first one — acceptable because the listener only answers terminal queries and a
        // stale handle silently drops writes.
        *self.pty.write() = pty;
        Ok(())
    }

    /// Feed bytes into the emulator only (never to the PTY): local typed-input colouring (§6.6) and notices.
    pub fn feed_local(&self, bytes: &[u8]) {
        let mut term = self.inner.term.lock();
        self.inner.proc.lock().advance(&mut *term, bytes);
        drop(term);
        self.inner.wake();
    }

    pub fn pty(&self) -> Arc<dyn Pty> {
        self.pty.read().clone()
    }

    pub fn pid(&self) -> u32 {
        self.pty().pid()
    }

    pub fn is_alive(&self) -> bool {
        self.pty().is_alive()
    }

    pub fn write(&self, bytes: &[u8]) {
        self.pty().write(bytes);
        if let Some(t) = self.inner.cast.lock().as_ref() {
            t.push_input(bytes);
        }
    }

    pub fn close(&self, mode: CloseMode, kill_console_tree: bool) {
        self.pty().close(mode, kill_console_tree);
    }

    pub fn info(&self) -> Info {
        self.inner.info.lock().clone()
    }

    pub fn drain_events(&self) -> Vec<PaneEvent> {
        std::mem::take(&mut *self.inner.events.lock())
    }

    pub fn set_palette(&self, p: Palette) {
        *self.inner.shared.palette.write() = p;
    }

    pub fn set_cell_size(&self, w: f32, h: f32) {
        *self.inner.shared.cell.lock() = (w, h);
    }

    pub fn mode(&self) -> TermMode {
        *self.inner.term.lock().mode()
    }

    pub fn size(&self) -> (u16, u16) {
        *self.inner.size.lock()
    }

    /// Resize the emulator and the console. Marks survive vertical resizes (the cursor row is the anchor);
    /// a width change reflows lines, so marks are dropped then.
    pub fn resize(&self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        let i = &self.inner;
        {
            let mut sz = i.size.lock();
            if *sz == (cols, rows) {
                return;
            }
            let reflow = sz.0 != cols;
            *sz = (cols, rows);
            {
                let mut r = i.resizes.lock();
                if r.len() < 64 {
                    r.push((cols, rows, i.start.elapsed().as_millis() as u64));
                }
            }
            let mut term = i.term.lock();
            let alt = term.mode().contains(TermMode::ALT_SCREEN);
            let before = i.track.lock().a_top + term.grid().cursor.point.line.0 as i64;
            term.resize(Size { cols: cols as usize, lines: rows as usize });
            if !alt {
                let mut t = i.track.lock();
                t.a_top = before - term.grid().cursor.point.line.0 as i64;
                t.last_h = term.grid().history_size();
            }
            if reflow {
                i.marks.lock().clear();
            }
        }
        self.pty().resize(cols, rows);
        if let Some(t) = i.cast.lock().as_ref() {
            t.resize(cols, rows);
        }
        i.wake();
    }

    // ---------------------------------------------------------------- scrollback / marks

    pub fn scroll(&self, s: Scroll) {
        self.inner.term.lock().scroll_display(s);
        self.inner.wake();
    }

    pub fn scroll_lines(&self, n: i32) {
        self.scroll(Scroll::Delta(n));
    }

    pub fn clear_scrollback(&self) {
        let mut term = self.inner.term.lock();
        term.grid_mut().clear_history();
        self.inner.marks.lock().clear();
        self.inner.track.lock().last_h = 0;
        drop(term);
        self.inner.wake();
    }

    pub fn marks(&self) -> Vec<Mark> {
        self.inner.marks.lock().iter().cloned().collect()
    }

    /// Absolute row of the top visible row and the viewport height.
    pub fn view_top_abs(&self) -> (i64, usize) {
        let term = self.inner.term.lock();
        let t = self.inner.track.lock();
        (t.a_top - term.grid().display_offset() as i64, term.screen_lines())
    }

    /// Ctrl+Alt+Up/Down (§6.4): scroll the viewport only. `anchor` is the previous jump target (None = view top + 1).
    pub fn goto_mark(&self, dir: i32, anchor: &mut Option<i64>) -> bool {
        let (top, _rows) = self.view_top_abs();
        let a = anchor.unwrap_or(top + 1);
        let marks = self.inner.marks.lock();
        let target = if dir < 0 { marks.iter().rev().map(|m| m.abs).find(|&l| l < a) } else { marks.iter().map(|m| m.abs).find(|&l| l > a) };
        drop(marks);
        let Some(t) = target else { return false };
        *anchor = Some(t);
        let mut term = self.inner.term.lock();
        let a_top = self.inner.track.lock().a_top;
        // Want `t - 1` at the top of the viewport (one line of context), clamped to the scrollable range.
        let want_top = t - 1;
        let h = term.grid().history_size() as i64;
        let offset = (a_top - want_top).clamp(0, h);
        let cur = term.grid().display_offset() as i32;
        term.scroll_display(Scroll::Delta(offset as i32 - cur));
        drop(term);
        self.inner.wake();
        true
    }

    // ---------------------------------------------------------------- text access

    pub fn selection_text(&self) -> Option<String> {
        self.inner.term.lock().selection_to_string().filter(|s| !s.is_empty())
    }

    pub fn has_selection(&self) -> bool {
        self.inner.term.lock().selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn clear_selection(&self) {
        self.inner.term.lock().selection = None;
        self.inner.wake();
    }

    pub fn select_all(&self) {
        let mut term = self.inner.term.lock();
        let top = Point::new(Line(-(term.grid().history_size() as i32)), Column(0));
        let bottom = Point::new(Line(term.screen_lines() as i32 - 1), Column(term.columns() - 1));
        let mut sel = Selection::new(SelectionType::Simple, top, alacritty_terminal::index::Side::Left);
        sel.update(bottom, alacritty_terminal::index::Side::Right);
        term.selection = Some(sel);
        drop(term);
        self.inner.wake();
    }

    /// The whole buffer (history + screen) as text, one `\n` per row (export, §12.5).
    /// `(columns, rows, ms since start)` of the spawn size and of every resize after it.
    pub fn resize_history(&self) -> Vec<(u16, u16, u64)> {
        self.inner.resizes.lock().clone()
    }

    /// The emulator's own dimensions `(columns, screen lines, history lines)`: they must always equal [`size`](Self::size).
    pub fn grid_size(&self) -> (usize, usize, usize) {
        let t = self.inner.term.lock();
        (t.columns(), t.screen_lines(), t.grid().history_size())
    }

    /// The live screen only (no scrollback), trailing blanks trimmed: cheap enough for a benchmark's fast dump.
    pub fn screen_text(&self) -> String {
        let term = self.inner.term.lock();
        let top = Point::new(Line(0), Column(0));
        let bottom = Point::new(Line(term.screen_lines() as i32 - 1), Column(term.columns() - 1));
        let s = term.bounds_to_string(top, bottom);
        s.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("
")
    }

    pub fn buffer_text(&self) -> String {
        let term = self.inner.term.lock();
        let top = Point::new(Line(-(term.grid().history_size() as i32)), Column(0));
        let bottom = Point::new(Line(term.screen_lines() as i32 - 1), Column(term.columns() - 1));
        let s = term.bounds_to_string(top, bottom);
        let mut lines: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    // ---------------------------------------------------------------- taps

    pub fn toggle_log(&self, dir: &std::path::Path, title: &str, prefix: Option<String>, raw: bool) -> std::io::Result<(bool, PathBuf)> {
        let mut slot = self.inner.log.lock();
        if let Some(t) = slot.take() {
            return Ok((false, t.stop()));
        }
        let path = tap_path(dir, title, "session", "log");
        *slot = Some(Tap::log(path.clone(), LogOptions { line_prefix: prefix, raw })?);
        Ok((true, path))
    }

    pub fn toggle_record(&self, dir: &std::path::Path, title: &str, capture_input: bool) -> std::io::Result<(bool, PathBuf)> {
        let mut slot = self.inner.cast.lock();
        if let Some(t) = slot.take() {
            return Ok((false, t.stop()));
        }
        let (cols, rows) = self.size();
        let shell = self.command_line.trim_start_matches('"').split(['"', ' ']).next().and_then(|s| std::path::Path::new(s).file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let header = CastHeader { cols, rows, timestamp: chrono_now(), title: title.to_string(), shell_name: shell };
        let path = tap_path(dir, title, "recording", "cast");
        *slot = Some(Tap::cast(path.clone(), header, capture_input)?);
        Ok((true, path))
    }

    pub fn logging(&self) -> bool {
        self.inner.log.lock().is_some()
    }

    pub fn recording(&self) -> bool {
        self.inner.cast.lock().is_some()
    }
}

pub(crate) fn new_inner(cfg: &SessionConfig) -> (Arc<Inner>, Arc<Shared>) {
    let (cols, rows) = (cfg.cols.max(2), cfg.rows.max(1));
        let shared = Arc::new(Shared {
            pty: OnceLock::new(),
            palette: RwLock::new(cfg.palette.clone()),
            cell: Mutex::new((8.0, 16.0)),
            osc52: cfg.osc52,
        });
        let tcfg = TermConfig {
            scrolling_history: cfg.scrollback + SLACK + SLACK,
            default_cursor_style: cursor_style(cfg.cursor),
            osc52: TOsc52::Disabled, // handled by the scanner (caps + read policy)
            ..TermConfig::default()
        };
        let term = Term::new(tcfg, &Size { cols: cols as usize, lines: rows as usize }, Listener(shared.clone()));
        let inner = Arc::new(Inner {
            term: FairMutex::new(term),
            proc: Mutex::new(Processor::new()),
            scan: Mutex::new(ScanState {
                scanner: Scanner::new(ScanConfig { computer_name: cfg.computer_name.clone(), osc52: cfg.osc52 }),
                events: Vec::new(),
                offs: Vec::new(),
            }),
            track: Mutex::new(Track { a_top: 0, last_h: 0, cap: cfg.scrollback }),
            shared: shared.clone(),
            info: Mutex::new(Info { cwd_local: true, ..Info::default() }),
            marks: Mutex::new(VecDeque::new()),
            events: Mutex::new(Vec::new()),
            repaint: cfg.repaint.clone(),
            dirty: AtomicBool::new(false),
            log: Mutex::new(None),
            cast: Mutex::new(None),
            start: Instant::now(),
            first_output: AtomicBool::new(false),
            last_output_ms: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            size: Mutex::new((cols, rows)),
            resizes: Mutex::new(vec![(cols, rows, 0)]),
        });
    (inner, shared)
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn cursor_style(c: CursorSetting) -> alacritty_terminal::vte::ansi::CursorStyle {
    use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle};
    CursorStyle {
        shape: match c {
            CursorSetting::Bar => CursorShape::Beam,
            CursorSetting::Block => CursorShape::Block,
            CursorSetting::Underline => CursorShape::Underline,
        },
        blinking: false,
    }
}

fn run_deliveries(i: Arc<Inner>, pty: Arc<dyn Pty>, items: Vec<Delivery>) {
    for d in items {
        let Delivery::AfterFirstOutputQuiet { quiet_ms, cap_ms, bytes } = d;
        let t0 = Instant::now();
        while pty.is_alive() && t0.elapsed() < Duration::from_millis(cap_ms) {
            if i.first_output.load(Ordering::Acquire) {
                let idle = (i.start.elapsed().as_millis() as u64).saturating_sub(i.last_output_ms.load(Ordering::Acquire));
                if idle >= quiet_ms {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        pty.write(&bytes);
    }
}

