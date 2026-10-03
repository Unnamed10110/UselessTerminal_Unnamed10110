//! Pane runtime: PTY + scanner + IPC batching (§3.4, §3.5) and the `pty_*` commands.

use crate::app::emit;
use crate::state::AppState;
use crate::taps::{tap_path, LogOptions, Tap};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::{AppHandle, Manager, State};
use ut_shell::ShellKind;
use ut_pty::{CloseMode, Pty, Sink, SpawnConfig};
use ut_vt_scan::{CastHeader, Config as ScanConfig, Event, Phase, Scanner};

// ------------------------------------------------------------------ batching

/// Adaptive output batching: the reader appends, a sender thread ships whatever accumulated while the
/// previous IPC message was in flight. Idle keystroke echo goes out immediately (nothing to wait for);
/// a flood coalesces into large messages instead of one IPC call per ConPTY line.
struct Batch {
    buf: Mutex<Vec<u8>>,
    cv: Condvar,
    closed: AtomicBool,
    pushed: AtomicU64,
}

impl Batch {
    fn push(&self, d: &[u8]) {
        self.buf.lock().extend_from_slice(d);
        self.pushed.fetch_add(d.len() as u64, Ordering::AcqRel);
        self.cv.notify_one();
    }

    fn run(&self, out: Channel<InvokeResponseBody>) {
        let mut spare: Vec<u8> = Vec::new();
        loop {
            {
                let mut g = self.buf.lock();
                while g.is_empty() {
                    if self.closed.load(Ordering::Acquire) {
                        return;
                    }
                    self.cv.wait(&mut g);
                }
                std::mem::swap(&mut *g, &mut spare);
            }
            let msg = std::mem::take(&mut spare);
            let cap = msg.capacity().min(1 << 20);
            if out.send(InvokeResponseBody::Raw(msg)).is_err() {
                return; // webview gone
            }
            spare = Vec::with_capacity(cap);
        }
    }
}

// ----------------------------------------------------------------- pane state

#[derive(Clone, Debug, Default)]
pub struct PaneInfo {
    pub cwd: Option<String>,
    pub cwd_host: Option<String>,
    pub cwd_local: bool,
    pub title: String,
    pub last_exit: Option<i32>,
    pub phase: Phase,
    pub branch: Option<String>,
}

pub struct PaneSink {
    id: String,
    app: AppHandle,
    scanner: Mutex<(Scanner, Vec<Event>)>,
    pub info: Mutex<PaneInfo>,
    batch: Arc<Batch>,
    pub log: Mutex<Option<Tap>>,
    pub cast: Mutex<Option<Tap>>,
    pty: Mutex<Option<Arc<dyn Pty>>>,
    start: Instant,
    first_output: AtomicBool,
    last_output_ms: AtomicU64,
    osc52: ut_vt_scan::Osc52,
    branch_pending: AtomicBool,
    /// Cached SSH target of this pane (§10.4); invalidated on title change and command end.
    pub ssh_cache: Mutex<Option<(Instant, Option<ut_ssh::target::SshTarget>)>>,
}

impl PaneSink {
    /// Resolve the git branch off the reader thread (file reads), coalescing bursts (§7.6, §19.2).
    fn refresh_branch(&self) {
        if self.branch_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let (app, id) = (self.app.clone(), self.id.clone());
        tauri::async_runtime::spawn_blocking(move || {
            let st = app.state::<AppState>();
            let Some(pane) = st.pane(&id) else { return };
            pane.sink.branch_pending.store(false, Ordering::Release);
            let (cwd, local) = {
                let i = pane.sink.info.lock();
                (i.cwd.clone(), i.cwd_local)
            };
            let branch = cwd.filter(|_| local).and_then(|c| st.git.branch_for(&c));
            let mut i = pane.sink.info.lock();
            if i.branch != branch {
                i.branch = branch.clone();
                drop(i);
                emit(&app, "pane:branch", serde_json::json!({ "paneId": id, "branch": branch }));
            }
        });
    }

    fn handle(&self, ev: Event) {
        match ev {
            Event::Title { title } => {
                let mut i = self.info.lock();
                if i.title != title {
                    i.title = title.clone();
                    drop(i);
                    *self.ssh_cache.lock() = None;
                    emit(&self.app, "pane:title", serde_json::json!({ "paneId": self.id, "title": title }));
                }
            }
            Event::Cwd { path, host, local } => {
                let mut i = self.info.lock();
                if i.cwd.as_deref() != Some(&path) || i.cwd_host.as_deref() != Some(&host) {
                    i.cwd = Some(path.clone());
                    i.cwd_host = Some(host.clone());
                    i.cwd_local = local;
                    drop(i);
                    emit(
                        &self.app,
                        "pane:cwd",
                        serde_json::json!({ "paneId": self.id, "path": path, "host": host, "local": local }),
                    );
                    if local {
                        self.refresh_branch();
                    }
                }
            }
            Event::Shell { kind, exit_code, duration_ms } => {
                {
                    let mut i = self.info.lock();
                    i.phase = match kind {
                        ut_vt_scan::ShellKind::PromptStart => Phase::Prompt,
                        ut_vt_scan::ShellKind::InputStart => Phase::Input,
                        ut_vt_scan::ShellKind::CommandStart => Phase::Running,
                        ut_vt_scan::ShellKind::CommandEnd => Phase::Done,
                    };
                    if exit_code.is_some() {
                        i.last_exit = exit_code;
                    }
                }
                if kind == ut_vt_scan::ShellKind::CommandEnd {
                    *self.ssh_cache.lock() = None;
                }
                // `git checkout` etc. change HEAD without a cwd change: re-resolve at prompt/command end.
                if matches!(kind, ut_vt_scan::ShellKind::PromptStart | ut_vt_scan::ShellKind::CommandEnd) {
                    self.refresh_branch();
                }
                emit(
                    &self.app,
                    "pane:shell",
                    serde_json::json!({ "paneId": self.id, "kind": kind, "exitCode": exit_code, "durationMs": duration_ms }),
                );
            }
            Event::Bell => emit(&self.app, "pane:bell", serde_json::json!({ "paneId": self.id })),
            Event::Osc52Write { data } => crate::clipboard::set_text(&String::from_utf8_lossy(&data)),
            Event::Osc52ReadRequest => {
                if self.osc52 == ut_vt_scan::Osc52::ReadWrite {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD
                        .encode(crate::clipboard::get_text().unwrap_or_default());
                    if let Some(p) = self.pty.lock().as_ref() {
                        p.write(format!("\x1b]52;c;{b64}\x1b\\").as_bytes());
                    }
                }
            }
            Event::Progress { state, value } => {
                emit(&self.app, "pane:progress", serde_json::json!({ "paneId": self.id, "state": state, "value": value }))
            }
            Event::Notify { title, body } => {
                emit(&self.app, "pane:notify", serde_json::json!({ "paneId": self.id, "title": title, "body": body }))
            }
            Event::AltScreen { .. } | Event::Output => {}
        }
    }
}

impl Sink for PaneSink {
    fn frame(&self, data: &[u8]) {
        // Ship first (latency), then extract state.
        self.batch.push(data);
        if let Some(t) = self.log.lock().as_ref() {
            t.push(data);
        }
        if let Some(t) = self.cast.lock().as_ref() {
            t.push(data);
        }
        self.first_output.store(true, Ordering::Release);
        self.last_output_ms.store(self.start.elapsed().as_millis() as u64, Ordering::Release);
        let events = {
            let mut g = self.scanner.lock();
            let (scanner, buf) = &mut *g;
            buf.clear();
            scanner.feed(data, Instant::now(), buf);
            std::mem::take(buf)
        };
        for ev in events.iter().cloned() {
            self.handle(ev);
        }
        self.scanner.lock().1 = events;
    }

    fn exited(&self, exit_code: Option<u32>) {
        self.batch.closed.store(true, Ordering::Release);
        self.batch.cv.notify_all();
        if let Some(t) = self.log.lock().take() {
            t.stop();
        }
        if let Some(t) = self.cast.lock().take() {
            t.stop();
        }
        let code = exit_code.map(|c| c as i32);
        // `totalBytes` lets the UI hold the exit notice until all output frames have been parsed:
        // channel messages and events travel separately and are not ordered against each other.
        emit(
            &self.app,
            "pane:exited",
            serde_json::json!({
                "paneId": self.id, "exitCode": code, "totalBytes": self.batch.pushed.load(Ordering::Acquire)
            }),
        );
    }
}

pub struct Pane {
    pub id: String,
    pub pty: Arc<dyn Pty>,
    pub sink: Arc<PaneSink>,
    pub command_line: String,
    pub shell_kind: ShellKind,
    pub size: Mutex<(u16, u16)>,
    pub start_cwd: PathBuf,
}

// ------------------------------------------------------------------ delivery

/// Timed writes into the PTY after spawn (integration init lines, starting commands; §6.5).
#[derive(Clone, Debug)]
pub enum Delivery {
    AfterDelay { ms: u64, bytes: Vec<u8> },
    /// Wait for the first output, then `quiet_ms` without output, but never longer than `cap_ms` in total.
    AfterFirstOutputQuiet { quiet_ms: u64, cap_ms: u64, bytes: Vec<u8> },
}

fn run_deliveries(sink: Arc<PaneSink>, pty: Arc<dyn Pty>, items: Vec<Delivery>) {
    for d in items {
        match d {
            Delivery::AfterDelay { ms, bytes } => {
                std::thread::sleep(Duration::from_millis(ms));
                pty.write(&bytes);
            }
            Delivery::AfterFirstOutputQuiet { quiet_ms, cap_ms, bytes } => {
                let t0 = Instant::now();
                while pty.is_alive() && t0.elapsed() < Duration::from_millis(cap_ms) {
                    if sink.first_output.load(Ordering::Acquire) {
                        let idle = sink.start.elapsed().as_millis() as u64 - sink.last_output_ms.load(Ordering::Acquire);
                        if idle >= quiet_ms {
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                pty.write(&bytes);
            }
        }
    }
}

// ------------------------------------------------------------------ commands

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub command: Option<String>,
    pub profile_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SpawnReq {
    pub pane_id: String,
    #[serde(default)]
    pub launch: Launch,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    pub starting_command: Option<String>,
    /// "auto" | "off"
    pub integration: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SpawnInfo {
    pub pid: u32,
    pub shell_kind: ShellKind,
    pub injected: bool,
    /// Colour typed input in xterm itself at OSC 133;B (§6.6 mechanism 2).
    pub local_input_color: bool,
    pub session_name: Option<String>,
    pub command_line: String,
    pub cwd: String,
    /// User-facing notes (e.g. "integration skipped: command line too long").
    pub notes: Vec<String>,
}

/// What a launch request resolves to before the PTY exists.
pub struct Resolved {
    pub command_line: String,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub deliveries: Vec<Delivery>,
    pub shell_kind: ShellKind,
    pub notes: Vec<String>,
    pub injected: bool,
    pub local_input_color: bool,
    pub session_name: Option<String>,
}

#[tauri::command]
pub async fn pty_spawn(
    app: AppHandle,
    state: State<'_, AppState>,
    req: SpawnReq,
    output: Channel<InvokeResponseBody>,
) -> Result<SpawnInfo, String> {
    let resolved = crate::launch::resolve(&state, &req)?;
    let batch = Arc::new(Batch {
        buf: Mutex::new(Vec::new()),
        cv: Condvar::new(),
        closed: AtomicBool::new(false),
        pushed: AtomicU64::new(0),
    });
    let osc52 = state.osc52();
    let sink = Arc::new(PaneSink {
        id: req.pane_id.clone(),
        app,
        scanner: Mutex::new((
            Scanner::new(ScanConfig { computer_name: std::env::var("COMPUTERNAME").unwrap_or_default(), osc52 }),
            Vec::new(),
        )),
        info: Mutex::new(PaneInfo::default()),
        batch: batch.clone(),
        log: Mutex::new(None),
        cast: Mutex::new(None),
        pty: Mutex::new(None),
        start: Instant::now(),
        first_output: AtomicBool::new(false),
        last_output_ms: AtomicU64::new(0),
        osc52,
        branch_pending: AtomicBool::new(false),
        ssh_cache: Mutex::new(None),
    });
    let b = batch.clone();
    std::thread::Builder::new().name("pty-send".into()).spawn(move || b.run(output)).map_err(|e| e.to_string())?;

    let mut cfg = SpawnConfig::new(resolved.command_line.clone(), req.cols.max(1), req.rows.max(1));
    cfg.cwd = Some(resolved.cwd.clone());
    cfg.env = resolved.env.clone();
    cfg.conpty_dll = state.conpty_dll();
    let pty = match ut_pty::spawn(cfg, sink.clone()) {
        Ok(p) => p,
        Err(e) => {
            batch.closed.store(true, Ordering::Release);
            batch.cv.notify_all();
            return Err(e.to_string());
        }
    };
    *sink.pty.lock() = Some(pty.clone());
    if !resolved.deliveries.is_empty() {
        let (s, p, d) = (sink.clone(), pty.clone(), resolved.deliveries.clone());
        std::thread::Builder::new().name("pty-deliver".into()).spawn(move || run_deliveries(s, p, d)).ok();
    }
    let info = SpawnInfo {
        pid: pty.pid(),
        shell_kind: resolved.shell_kind,
        injected: resolved.injected,
        local_input_color: resolved.local_input_color,
        session_name: resolved.session_name,
        command_line: resolved.command_line.clone(),
        cwd: resolved.cwd.to_string_lossy().into_owned(),
        notes: resolved.notes,
    };
    let pane = Arc::new(Pane {
        id: req.pane_id.clone(),
        pty,
        sink,
        command_line: resolved.command_line,
        shell_kind: resolved.shell_kind,
        size: Mutex::new((req.cols, req.rows)),
        start_cwd: resolved.cwd.clone(),
    });
    state.panes.write().insert(req.pane_id, pane);
    Ok(info)
}

/// Raw-body command: bytes are the payload, the pane id travels in the `paneid` header (§3.4.2).
#[tauri::command]
pub async fn pty_write(state: State<'_, AppState>, request: Request<'_>) -> Result<(), String> {
    let id = request
        .headers()
        .get("paneid")
        .and_then(|v| v.to_str().ok())
        .ok_or("missing paneid header")?
        .to_string();
    let InvokeBody::Raw(bytes) = request.body() else { return Err("expected a raw body".into()) };
    if let Some(p) = state.pane(&id) {
        p.pty.write(bytes);
        if let Some(t) = p.sink.cast.lock().as_ref() {
            t.push_input(bytes);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn pty_resize(state: State<'_, AppState>, pane_id: String, cols: u16, rows: u16) -> Result<(), String> {
    if let Some(p) = state.pane(&pane_id) {
        let mut s = p.size.lock();
        if *s != (cols, rows) && cols > 0 && rows > 0 {
            *s = (cols, rows);
            p.pty.resize(cols, rows);
            if let Some(t) = p.sink.cast.lock().as_ref() {
                t.resize(cols, rows);
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn pty_ack(state: State<'_, AppState>, pane_id: String, bytes: usize) -> Result<(), String> {
    if let Some(p) = state.pane(&pane_id) {
        p.pty.ack(bytes);
    }
    Ok(())
}

#[tauri::command]
pub async fn pty_close(state: State<'_, AppState>, pane_id: String, mode: Option<String>) -> Result<(), String> {
    let pane = state.panes.write().remove(&pane_id);
    if let Some(p) = pane {
        let mode = if mode.as_deref() == Some("force") { CloseMode::Force } else { CloseMode::Graceful };
        let kill_tree = state.kill_console_tree();
        // Graceful close waits ~100 ms; keep it off the async runtime threads.
        tauri::async_runtime::spawn_blocking(move || p.pty.close(mode, kill_tree));
    }
    Ok(())
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PaneInfoDto {
    pub pid: u32,
    pub alive: bool,
    pub exit_code: Option<u32>,
    pub cwd: Option<String>,
    pub cwd_host: Option<String>,
    pub title: String,
    pub last_exit: Option<i32>,
    pub shell_kind: ShellKind,
    pub command_line: String,
    pub unacked: usize,
    pub branch: Option<String>,
    pub foreground: Option<String>,
}

#[tauri::command]
pub async fn pane_info(
    state: State<'_, AppState>,
    pane_id: String,
    foreground: Option<bool>,
) -> Result<Option<PaneInfoDto>, String> {
    let want_fg = foreground.unwrap_or(false);
    Ok(state.pane(&pane_id).map(|p| {
        let i = p.sink.info.lock().clone();
        let alive = p.pty.is_alive();
        PaneInfoDto {
            pid: p.pty.pid(),
            alive,
            exit_code: p.pty.exit_code(),
            cwd: i.cwd,
            cwd_host: i.cwd_host,
            title: i.title,
            last_exit: i.last_exit,
            shell_kind: p.shell_kind,
            command_line: p.command_line.clone(),
            unacked: p.pty.unacked(),
            branch: i.branch,
            foreground: if want_fg && alive { ut_pty::proc::foreground_name(p.pty.pid()) } else { None },
        }
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TapState {
    pub active: bool,
    pub path: Option<String>,
}

/// Toggle session logging on a pane. `line_prefix` is the pane index label for multi-pane tabs.
#[tauri::command]
pub async fn log_toggle(
    state: State<'_, AppState>,
    pane_id: String,
    title: Option<String>,
    line_prefix: Option<String>,
) -> Result<TapState, String> {
    let p = state.pane(&pane_id).ok_or("unknown pane")?;
    let mut slot = p.sink.log.lock();
    if let Some(t) = slot.take() {
        let path = t.stop();
        return Ok(TapState { active: false, path: Some(path.to_string_lossy().into_owned()) });
    }
    let raw = state.log_raw();
    let dir = state.log_dir();
    let path = tap_path(&dir, title.as_deref().unwrap_or(""), "session", "log");
    let tap = Tap::log(path.clone(), LogOptions { line_prefix, raw }).map_err(|e| e.to_string())?;
    *slot = Some(tap);
    Ok(TapState { active: true, path: Some(path.to_string_lossy().into_owned()) })
}

#[tauri::command]
pub async fn record_toggle(
    state: State<'_, AppState>,
    pane_id: String,
    title: Option<String>,
) -> Result<TapState, String> {
    let p = state.pane(&pane_id).ok_or("unknown pane")?;
    let mut slot = p.sink.cast.lock();
    if let Some(t) = slot.take() {
        let path = t.stop();
        return Ok(TapState { active: false, path: Some(path.to_string_lossy().into_owned()) });
    }
    let (cols, rows) = *p.size.lock();
    let shell_name = p
        .command_line
        .trim_start_matches('"')
        .split(['"', ' '])
        .next()
        .and_then(|s| std::path::Path::new(s).file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let header = CastHeader {
        cols,
        rows,
        timestamp: chrono::Utc::now().timestamp(),
        title: title.clone().unwrap_or_default(),
        shell_name,
    };
    let path = tap_path(&state.recordings_dir(), title.as_deref().unwrap_or(""), "recording", "cast");
    let tap = Tap::cast(path.clone(), header, state.record_input()).map_err(|e| e.to_string())?;
    *slot = Some(tap);
    Ok(TapState { active: true, path: Some(path.to_string_lossy().into_owned()) })
}
