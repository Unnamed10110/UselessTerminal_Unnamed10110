//! ConPTY wrapper (§4). The rest of the app only sees the [`Pty`] trait, so a Unix PTY can be added later.
//!
//! Threads per pane: reader (blocking ReadFile → [`Sink::frame`], with credit-based backpressure),
//! writer (queue → WriteFile, also applies throttled resizes) and an exit watcher (process handle →
//! exit code → `ClosePseudoConsole` while the reader is still draining → [`Sink::exited`]).

mod env;
mod flow;
#[cfg(windows)]
mod conpty;
#[cfg(windows)]
pub mod elevate;
#[cfg(windows)]
pub mod proc;

pub use env::build_env_block;
pub use flow::{HIGH_WATER, LOW_WATER};

use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct SpawnConfig {
    /// Passed verbatim to `CreateProcessW` (never split and re-joined, §4.2).
    pub command_line: String,
    pub cwd: Option<PathBuf>,
    /// Fully layered environment (see `ut-data::env`); sorted/serialised here.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    /// Backpressure thresholds in bytes (defaults: [`HIGH_WATER`] / [`LOW_WATER`]).
    pub high_water: usize,
    pub low_water: usize,
    /// `Some(path)` → load that `conpty.dll` (bundled ConPTY, §2.1) and fall back to kernel32 if it fails.
    pub conpty_dll: Option<PathBuf>,
}

impl SpawnConfig {
    pub fn new(command_line: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            command_line: command_line.into(),
            cwd: None,
            env: Vec::new(),
            cols,
            rows,
            high_water: HIGH_WATER,
            low_water: LOW_WATER,
            conpty_dll: None,
        }
    }
}

/// Receives output on the reader thread. `frame` may do cheap work (scan, log queue, IPC send) but must
/// not block for long: blocking the reader is what applies backpressure, and only via the flow window.
pub trait Sink: Send + Sync + 'static {
    fn frame(&self, data: &[u8]);
    /// Called exactly once, after the process exited and the reader reached EOF (or timed out).
    fn exited(&self, exit_code: Option<u32>);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseMode {
    /// `ClosePseudoConsole`, 100 ms grace, then kill leftover console-subsystem processes (§4.8).
    Graceful,
    /// Kill every process of the job immediately.
    Force,
}

pub trait Pty: Send + Sync {
    fn pid(&self) -> u32;
    /// Non-blocking enqueue. A write after disposal is silently dropped (§4.5).
    fn write(&self, bytes: &[u8]);
    /// Deduplicated and throttled to one `ResizePseudoConsole` per 16 ms with the trailing value (§4.6).
    fn resize(&self, cols: u16, rows: u16);
    /// Credit-based flow control: bytes the UI has *parsed* (§3.5).
    fn ack(&self, bytes: usize);
    fn close(&self, mode: CloseMode, kill_console_tree: bool);
    fn is_alive(&self) -> bool;
    fn exit_code(&self) -> Option<u32>;
    /// Bytes delivered to the sink that the UI has not acked yet (diagnostics page, tests).
    fn unacked(&self) -> usize;
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("ConPTY is not available on this system: {0}")]
    Unsupported(String),
    #[error("{stage} failed: {message}")]
    Os { stage: &'static str, message: String },
}

/// Spawn `cfg.command_line` in a new pseudo-console.
#[cfg(windows)]
pub fn spawn(cfg: SpawnConfig, sink: Arc<dyn Sink>) -> Result<Arc<dyn Pty>, SpawnError> {
    conpty::spawn(cfg, sink).map(|p| p as Arc<dyn Pty>)
}

#[cfg(not(windows))]
pub fn spawn(_cfg: SpawnConfig, _sink: Arc<dyn Sink>) -> Result<Arc<dyn Pty>, SpawnError> {
    Err(SpawnError::Unsupported("only Windows ConPTY is implemented".into()))
}
