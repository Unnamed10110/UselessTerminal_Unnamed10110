use crate::session::*;
use alacritty_terminal::grid::Dimensions;
use std::sync::Arc;
use ut_pty::Sink;
use ut_vt_scan::Osc52;

fn cfg(rows: u16, scrollback: usize) -> SessionConfig {
    SessionConfig {
        command_line: String::new(),
        cwd: None,
        env: vec![],
        cols: 40,
        rows,
        scrollback,
        conpty_dll: None,
        osc52: Osc52::Write,
        computer_name: "X".into(),
        deliveries: vec![],
        repaint: Arc::new(|| {}),
        palette: Palette::default(),
        cursor: CursorSetting::Bar,
    }
}

fn feed(i: &Arc<Inner>, bytes: &[u8]) {
    InnerSink(i.clone()).frame(bytes);
}

fn lines(n: usize, prefix: &str) -> Vec<u8> {
    (0..n).flat_map(|k| format!("{prefix}{k}\r\n").into_bytes()).collect()
}

/// Prompt rows are exact even when OSC 133;A is in the MIDDLE of a chunk, after the grid scrolled, and after
/// the history was trimmed many times (§6.4 navigation must never drift).
#[test]
fn marks_track_absolute_rows_through_scrolling_and_trim() {
    let (inner, _) = new_inner(&cfg(5, 20));
    // First prompt on row 0, inside a chunk that keeps writing (and scrolling) after the marker.
    let mut chunk = b"\x1b]133;A\x07$ cmd\r\n".to_vec();
    chunk.extend(lines(12, "out"));
    feed(&inner, &chunk);
    feed(&inner, b"\x1b]133;D;3\x07\x1b]133;A\x07$ ");

    let marks: Vec<Mark> = inner.marks.lock().iter().cloned().collect();
    assert_eq!(marks.len(), 2);
    assert_eq!(marks[0].abs, 0, "the first prompt is absolute row 0");
    assert_eq!(marks[0].exit, Some(3));
    assert_eq!(marks[1].abs, 13, "13 lines were written before the second prompt");

    feed(&inner, &lines(5000, "more")); // history overflows cap + SLACK several times and is trimmed
    let m: Vec<Mark> = inner.marks.lock().iter().cloned().collect();
    assert_eq!(m[1].abs, 13, "absolute rows never drift when the grid trims its history");
    let term = inner.term.lock();
    assert!(term.grid().history_size() <= 20 + 2100, "history stays bounded, got {}", term.grid().history_size());
}

#[test]
fn split_osc_sequence_across_frames_still_marks() {
    let (inner, _) = new_inner(&cfg(5, 20));
    feed(&inner, b"abc\r\n\x1b]13");
    feed(&inner, b"3;A\x07$ ");
    let m: Vec<Mark> = inner.marks.lock().iter().cloned().collect();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].abs, 1);
}

#[test]
fn osc7_title_and_exit_events_reach_the_ui() {
    let (inner, _) = new_inner(&cfg(5, 20));
    feed(&inner, b"\x1b]0;my title\x07\x1b]7;file:///C:/Users/x%20y\x07");
    let info = inner.info.lock().clone();
    assert_eq!(info.title, "my title");
    assert_eq!(info.cwd.as_deref(), Some("C:\\Users\\x y"));
    assert!(info.cwd_local);
}

/// Parser + grid + scanner throughput without ConPTY or UI (`cargo test --release -p ut-term perf -- --ignored --nocapture`).
/// The §19 budget is >= 10 MB/s for the whole app; this stage alone should be far above it.
#[test]
#[ignore]
fn perf_sink_throughput() {
    let (inner, _) = new_inner(&cfg(44, 10_000));
    let chunk: Vec<u8> = (0..1500).flat_map(|k| format!("{}\r\n", 1_000_000 + k).into_bytes()).collect(); // ~12 KB of short lines
    let mut colored = Vec::new();
    for k in 0..500 {
        colored.extend_from_slice(format!("\x1b[3{}mline {k} ", k % 8).as_bytes());
        colored.extend_from_slice(&[b'x'; 90]);
        colored.extend_from_slice(b"\x1b[0m\r\n");
    }
    for (name, data) in [("short lines", &chunk), ("colored 100-col lines", &colored)] {
        let total = 64 * 1024 * 1024;
        let t = std::time::Instant::now();
        let mut fed = 0;
        while fed < total {
            feed(&inner, data);
            fed += data.len();
        }
        let secs = t.elapsed().as_secs_f64();
        println!("{name}: {:.1} MB in {secs:.2} s = {:.1} MB/s", fed as f64 / 1e6, fed as f64 / 1e6 / secs);
    }
}

/// Replays a captured ConPTY stream (`UT_RAW_BIN`) into the grid and prints the screen: a debugging aid
/// (`UT_RAW_CHUNK=n` feeds n bytes per frame; default: everything at once).
#[test]
#[ignore]
fn replay_captured_stream() {
    let Some(path) = std::env::var_os("UT_RAW_BIN") else { return };
    let data = std::fs::read(path).unwrap();
    let mut c = cfg(26, 10_000);
    c.cols = 90;
    let (inner, _) = new_inner(&c);
    let chunk: usize = std::env::var("UT_RAW_CHUNK").ok().and_then(|v| v.parse().ok()).unwrap_or(data.len().max(1));
    for part in data.chunks(chunk) {
        feed(&inner, part);
    }
    let term = inner.term.lock();
    let top = alacritty_terminal::index::Point::new(alacritty_terminal::index::Line(0), alacritty_terminal::index::Column(0));
    let bottom = alacritty_terminal::index::Point::new(alacritty_terminal::index::Line(term.screen_lines() as i32 - 1), alacritty_terminal::index::Column(term.columns() - 1));
    let text = term.bounds_to_string(top, bottom);
    for l in text.lines() {
        println!("|{}", l.trim_end());
    }
    println!("history={} cursor={:?}", term.grid().history_size(), term.grid().cursor.point);
}
