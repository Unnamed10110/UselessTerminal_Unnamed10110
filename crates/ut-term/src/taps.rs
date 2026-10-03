//! Log / asciicast taps (§12.1, §12.2). They are fed by a channel from the reader thread, never
//! blocking it: the queue is byte-budgeted at 16 MiB and drops (flagging "truncated") when full.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use ut_vt_scan::{system_clock, CastHeader, CastWriter, LogStripper};

const QUEUE_CAP: usize = 16 * 1024 * 1024;
const FLUSH_EVERY: Duration = Duration::from_secs(1);
const FLUSH_BYTES: usize = 32 * 1024;

enum Msg {
    Data(Vec<u8>),
    Input(Vec<u8>),
    Resize(u16, u16),
    Stop,
}

pub struct Tap {
    pub path: PathBuf,
    tx: Sender<Msg>,
    queued: Arc<AtomicUsize>,
    truncated: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

pub struct LogOptions {
    /// Prefix every line (pane index when the tab has several panes, §12.1).
    pub line_prefix: Option<String>,
    pub raw: bool,
}

impl Tap {
    fn start(
        path: PathBuf,
        run: impl FnOnce(Receiver<Msg>, Arc<AtomicUsize>) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let q = queued.clone();
        let handle = std::thread::Builder::new().name("ut-tap".into()).spawn(move || run(rx, q))?;
        Ok(Self { path, tx, queued, truncated: Arc::new(AtomicBool::new(false)), handle: Some(handle) })
    }

    /// Plain-text (ANSI-stripped) or raw log file.
    pub fn log(path: PathBuf, opts: LogOptions) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = File::create(&path)?;
        Self::start(path, move |rx, queued| {
            let mut out = BufWriter::new(file);
            let mut stripper = LogStripper::new();
            let mut at_line_start = true;
            let mut since_flush = 0usize;
            let mut last_flush = Instant::now();
            let _ = writeln!(out, "--- Session log started: {} ---", ut_fs::now_iso());
            let mut text = String::new();
            loop {
                match rx.recv_timeout(FLUSH_EVERY) {
                    Ok(Msg::Data(d)) => {
                        queued.fetch_sub(d.len(), Ordering::Relaxed);
                        if opts.raw {
                            let _ = out.write_all(&d);
                        } else {
                            text.clear();
                            stripper.feed(&d, &mut text);
                            write_prefixed(&mut out, &text, opts.line_prefix.as_deref(), &mut at_line_start);
                        }
                        since_flush += d.len();
                    }
                    // Logs never capture keyboard input (§18.3).
                    Ok(Msg::Input(d)) => {
                        queued.fetch_sub(d.len(), Ordering::Relaxed);
                    }
                    Ok(Msg::Resize(..)) => {}
                    Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if since_flush >= FLUSH_BYTES || last_flush.elapsed() >= FLUSH_EVERY {
                    let _ = out.flush();
                    since_flush = 0;
                    last_flush = Instant::now();
                }
            }
            text.clear();
            stripper.finish(&mut text);
            write_prefixed(&mut out, &text, opts.line_prefix.as_deref(), &mut at_line_start);
            if !at_line_start {
                let _ = writeln!(out);
            }
            let _ = writeln!(out, "--- Session log ended: {} ---", ut_fs::now_iso());
            let _ = out.flush();
        })
    }

    /// asciicast v2 recording.
    pub fn cast(path: PathBuf, header: CastHeader, capture_input: bool) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = BufWriter::new(File::create(&path)?);
        let mut w = CastWriter::new(file, header, system_clock())?;
        Self::start(path, move |rx, queued| {
            loop {
                match rx.recv_timeout(FLUSH_EVERY) {
                    Ok(Msg::Data(d)) => {
                        queued.fetch_sub(d.len(), Ordering::Relaxed);
                        let _ = w.output(&d);
                    }
                    Ok(Msg::Input(d)) => {
                        queued.fetch_sub(d.len(), Ordering::Relaxed);
                        if capture_input {
                            let _ = w.input(&d);
                        }
                    }
                    Ok(Msg::Resize(c, r)) => {
                        let _ = w.resize(c, r);
                    }
                    Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
            let _ = w.finish();
        })
    }

    pub fn push(&self, data: &[u8]) {
        if self.queued.load(Ordering::Relaxed) + data.len() > QUEUE_CAP {
            self.truncated.store(true, Ordering::Relaxed);
            return;
        }
        self.queued.fetch_add(data.len(), Ordering::Relaxed);
        let _ = self.tx.send(Msg::Data(data.to_vec()));
    }

    pub fn push_input(&self, data: &[u8]) {
        if self.queued.load(Ordering::Relaxed) + data.len() <= QUEUE_CAP {
            self.queued.fetch_add(data.len(), Ordering::Relaxed);
            let _ = self.tx.send(Msg::Input(data.to_vec()));
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(Msg::Resize(cols, rows));
    }

    pub fn was_truncated(&self) -> bool {
        self.truncated.load(Ordering::Relaxed)
    }

    /// Finish the file and wait for the writer thread.
    pub fn stop(mut self) -> PathBuf {
        let _ = self.tx.send(Msg::Stop);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.path.clone()
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

fn write_prefixed(out: &mut impl Write, text: &str, prefix: Option<&str>, at_line_start: &mut bool) {
    let Some(prefix) = prefix else {
        let _ = out.write_all(text.as_bytes());
        if let Some(c) = text.chars().last() {
            *at_line_start = c == '\n';
        }
        return;
    };
    for chunk in text.split_inclusive('\n') {
        if *at_line_start {
            let _ = out.write_all(prefix.as_bytes());
        }
        let _ = out.write_all(chunk.as_bytes());
        *at_line_start = chunk.ends_with('\n');
    }
}

/// `<dir>\<sanitized title or fallback>-yyyyMMdd-HHmmss.<ext>` (§12.1/§12.2).
pub fn tap_path(dir: &Path, title: &str, fallback: &str, ext: &str) -> PathBuf {
    dir.join(format!("{}-{}.{}", ut_fs::sanitize_file_name(title, fallback), ut_fs::now_stamp(), ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_log_strips_osc_and_prefixes() {
        let dir = std::env::temp_dir().join(format!("ut tap test {}", std::process::id()));
        let path = dir.join("a b.log");
        let tap = Tap::log(path.clone(), LogOptions { line_prefix: Some("[1] ".into()), raw: false }).unwrap();
        tap.push(b"\x1b]0;title\x07hello \x1b[31mred\x1b[0m\r\nsecond");
        tap.push(b" line\r\n");
        tap.stop();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.starts_with("--- Session log started: "), "{s}");
        assert!(s.contains("[1] hello red\n[1] second line\n"), "{s}");
        assert!(!s.contains("title") && !s.contains('\x07') && !s.contains('\x1b'), "{s}");
        assert!(s.trim_end().ends_with(" ---") && s.contains("--- Session log ended: "));
    }

    #[test]
    fn cast_has_header_events_and_resize() {
        let dir = std::env::temp_dir().join(format!("ut tap cast {}", std::process::id()));
        let path = dir.join("x.cast");
        let h = CastHeader { cols: 100, rows: 30, timestamp: 1, title: "t".into(), shell_name: "pwsh".into() };
        let tap = Tap::cast(path.clone(), h, false).unwrap();
        tap.push("héllo".as_bytes());
        tap.resize(120, 40);
        tap.stop();
        let s = std::fs::read_to_string(&path).unwrap();
        let mut lines = s.lines();
        assert!(lines.next().unwrap().contains("\"width\":100"));
        assert!(lines.any(|l| l.contains("\"r\"") && l.contains("120x40")));
    }
}
