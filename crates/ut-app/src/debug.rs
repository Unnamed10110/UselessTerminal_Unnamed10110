//! Developer hooks, active only when `UT_DUMP=<txt>` is set (scripted end-to-end checks):
//! * the active pane's stats + buffer are written to that file about once a second,
//! * creating `<txt>.shot` makes the app save `<txt>.png` (a screenshot of the window) and delete the trigger,
//! * creating `<txt>.quit` closes the app, `<txt>.toggle` does what the Quake hotkey does.

use crate::app::App;
use egui::{Context, Event, ViewportCommand};
use std::path::PathBuf;
use std::time::Duration;

/// Appends a line to `<UT_DUMP>.log` (no-op unless `UT_DUMP` is set).
pub fn log(msg: &str) {
    use std::io::Write;
    let Some(mut p) = std::env::var_os("UT_DUMP") else { return };
    p.push(".log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%H:%M:%S%.3f"));
    }
}

impl App {
    /// `<UT_DUMP>.toggle` does what the Quake hotkey does. Runs from `logic()`, which keeps ticking while the window is hidden.
    pub fn debug_toggle(&mut self, ctx: &Context) {
        let Some(mut p) = std::env::var_os("UT_DUMP") else { return };
        p.push(".toggle");
        let p = PathBuf::from(p);
        if p.exists() {
            let _ = std::fs::remove_file(p);
            log(&format!("toggle: hidden={} focused={} minimized={}", self.hidden, self.window_focused, self.minimized));
            self.toggle_window(ctx);
        }
    }

    pub fn debug_hooks(&mut self, ctx: &Context) {
        let Some(dump) = std::env::var_os("UT_DUMP").map(PathBuf::from) else { return };
        let with = |ext: &str| {
            let mut s = dump.clone().into_os_string();
            s.push(ext);
            PathBuf::from(s)
        };
        // `UT_DUMP_MS` (min 10) speeds the dump up for latency measurements (bench/throughput.ps1).
        let every = Duration::from_millis(std::env::var("UT_DUMP_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(500).max(10));
        if self.debug_last_dump.is_none_or(|t| t.elapsed() >= every) {
            self.debug_last_dump = Some(std::time::Instant::now());
            if let Some(s) = self.focused_session() {
                let flags = format!("window_focused={} term_focus={} refocus={} focused_id={:?} view_id={:?} router={} modal={} sidebar={}", self.window_focused, self.debug_term_focus(ctx), self.refocus, ctx.memory(|m| m.focused()), self.tabs.get(self.active).and_then(|t| t.focused_pane()).map(|p| p.view.id()), self.router.enabled.load(std::sync::atomic::Ordering::Relaxed), self.any_modal(), self.sidebar_open);
                let _ = std::fs::write(&dump, format!("{flags}\n{}", dump_text(&s, every >= Duration::from_millis(100))));
            }
            let _ = std::fs::write(with(".browser"), format!("open={} {}", self.browser_open, self.browser.debug_state()));
            if with(".quit").exists() {
                let _ = std::fs::remove_file(with(".quit"));
                self.quit();
            }
            if with(".shot").exists() {
                let _ = std::fs::remove_file(with(".shot"));
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
            }
        }
        for ev in ctx.input(|i| i.events.clone()) {
            if let Event::Screenshot { image, .. } = ev {
                let [w, h] = image.size;
                let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                let _ = image::save_buffer(with(".png"), &rgba, w as u32, h as u32, image::ColorType::Rgba8);
            }
        }
        ctx.request_repaint_after(every.min(Duration::from_millis(250)));
    }
}

impl App {
    fn debug_term_focus(&self, ctx: &Context) -> bool {
        self.tabs.get(self.active).and_then(|t| t.focused_pane()).is_some_and(|p| ctx.memory(|m| m.has_focus(p.view.id())))
    }
}

/// `full`: the whole scrollback; the fast dump of the benchmark (every < 100 ms) writes the live screen only, otherwise
/// serialising 10k lines under the terminal lock would be the thing being measured.
fn dump_text(s: &ut_term::TermSession, full: bool) -> String {
    let inf = s.info();
    let rx = s.inner.rx_bytes.load(std::sync::atomic::Ordering::Relaxed);
    let fr = s.inner.frames.load(std::sync::atomic::Ordering::Relaxed);
    format!(
        "# pid={} rx={rx} frames={fr} size={:?} grid={:?} resizes={:?} vtop={} alive={} title={:?} cwd={:?} phase={:?} last_exit={:?}\n{}",
        s.pid(),
        s.size(),
        s.grid_size(),
        s.resize_history(),
        s.view_top_abs().0,
        s.is_alive(),
        inf.title,
        inf.cwd,
        inf.phase,
        inf.last_exit,
        if full { s.buffer_text() } else { s.screen_text() }
    )
}
