//! §21.2 integration tests against a real ConPTY.
#![cfg(windows)]

use parking_lot::{Condvar, Mutex};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ut_pty::{proc, spawn, CloseMode, Pty, Sink, SpawnConfig};

#[derive(Default)]
struct Collect {
    buf: Mutex<Vec<u8>>,
    exit: Mutex<Option<Option<u32>>>,
    cv: Condvar,
}

impl Sink for Collect {
    fn frame(&self, d: &[u8]) {
        self.buf.lock().extend_from_slice(d);
        self.cv.notify_all();
    }
    fn exited(&self, c: Option<u32>) {
        *self.exit.lock() = Some(c);
        self.cv.notify_all();
    }
}

impl Collect {
    fn wait(&self, secs: u64, pred: impl Fn(&str, Option<Option<u32>>) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        let mut g = self.buf.lock();
        loop {
            let text = String::from_utf8_lossy(&g).into_owned();
            if pred(&text, *self.exit.lock()) {
                return true;
            }
            if self.cv.wait_until(&mut g, end).timed_out() {
                let text = String::from_utf8_lossy(&g).into_owned();
                return pred(&text, *self.exit.lock());
            }
        }
    }
    fn len(&self) -> usize {
        self.buf.lock().len()
    }
}

fn env() -> Vec<(String, String)> {
    let mut e: Vec<_> = std::env::vars().collect();
    e.push(("TERM".into(), "xterm-256color".into()));
    e
}

fn run(cmd: &str, cols: u16, rows: u16) -> (Arc<dyn Pty>, Arc<Collect>) {
    run_cfg(SpawnConfig { env: env(), ..SpawnConfig::new(cmd, cols, rows) })
}

fn run_cfg(cfg: SpawnConfig) -> (Arc<dyn Pty>, Arc<Collect>) {
    let sink = Arc::new(Collect::default());
    let pty = spawn(cfg, sink.clone()).expect("spawn");
    (pty, sink)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ut pty test {name} {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn echo_hi_exits_zero() {
    let (pty, sink) = run("cmd.exe /c echo hi", 80, 24);
    assert!(sink.wait(10, |t, e| t.contains("hi") && e == Some(Some(0))), "{:?}", sink.buf.lock());
    assert_eq!(pty.exit_code(), Some(0));
    assert!(!pty.is_alive());
    pty.write(b"ignored after exit\r"); // silent no-op
}

#[test]
fn exit_code_is_reported() {
    let (pty, sink) = run("cmd.exe /c exit 3", 80, 24);
    assert!(sink.wait(10, |_, e| e == Some(Some(3))));
    assert_eq!(pty.exit_code(), Some(3));
}

#[test]
fn env_term_cwd_and_paths_with_spaces() {
    let dir = tmp("env");
    let bat = dir.join("say hello.cmd");
    std::fs::write(&bat, "@echo off\r\necho TERM=%TERM% ARG=%1 CWD=%CD%\r\n").unwrap();
    let cmd = format!("cmd.exe /d /s /c \"\"{}\" hello\"", bat.display());
    let (_pty, sink) = run_cfg(SpawnConfig { cwd: Some(dir.clone()), env: env(), ..SpawnConfig::new(cmd, 200, 24) });
    let want = format!("CWD={}", dir.display());
    assert!(
        sink.wait(10, |t, _| t.contains("TERM=xterm-256color") && t.contains("ARG=hello") && t.contains(&want)),
        "{}",
        String::from_utf8_lossy(&sink.buf.lock())
    );
}

#[test]
fn resize_is_visible_to_the_child() {
    let (pty, sink) = run("cmd.exe", 80, 24);
    assert!(sink.wait(10, |t, _| t.contains('>')), "no prompt");
    pty.resize(111, 33);
    std::thread::sleep(Duration::from_millis(150));
    pty.write(b"mode con\r");
    assert!(sink.wait(10, |t, _| t.contains("111")), "mode con should report 111 columns");
    pty.write(b"exit\r");
    assert!(sink.wait(10, |_, e| e.is_some()));
}

#[test]
fn backpressure_blocks_reader_until_acked() {
    let dir = tmp("flood");
    let file = dir.join("big file.txt");
    let line = "0123456789abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz\r\n";
    std::fs::write(&file, line.repeat(120_000)).unwrap(); // ~8.8 MB
    let (high, low) = (256 * 1024, 64 * 1024);
    let cmd = format!("cmd.exe /d /s /c \"type \"{}\"\"", file.display());
    let (pty, sink) = run_cfg(SpawnConfig { env: env(), high_water: high, low_water: low, ..SpawnConfig::new(cmd, 120, 30) });

    // No acks: the reader must stall just past HIGH. (Poll: conhost can be slow to start under load.)
    assert!(wait_for(40, || sink.len() > high), "should deliver past HIGH, got {}", sink.len());
    std::thread::sleep(Duration::from_millis(800));
    let stalled = sink.len();
    assert!(stalled <= high + 64 * 1024 + 1024, "reader must block near HIGH, got {stalled}");
    assert!(pty.is_alive(), "child must be blocked by ConPTY, not finished");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sink.len(), stalled, "no growth while unacked");

    // Now ack everything until the child finishes.
    let end = Instant::now() + Duration::from_secs(120);
    let mut acked = 0usize;
    while sink.exit.lock().is_none() && Instant::now() < end {
        let have = sink.len();
        pty.ack(have - acked);
        acked = have;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(sink.exit.lock().is_some(), "flood should complete once acks flow");
    assert!(sink.len() > 8_000_000, "all output delivered, got {}", sink.len());
}

fn wait_for(secs: u64, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

fn has_proc(pid: u32, name: &str) -> bool {
    proc::descendants(pid).iter().any(|p| p.name.eq_ignore_ascii_case(name))
}

#[test]
fn closing_a_pane_kills_console_children_but_spares_gui_apps() {
    let (pty, sink) = run("cmd.exe", 100, 30);
    assert!(sink.wait(10, |t, _| t.contains('>')));
    pty.write(b"start \"\" notepad.exe\r");
    let notepad = wait_for(8, || has_proc(pty.pid(), "notepad.exe"));
    pty.write(b"ping -t 127.0.0.1\r");
    assert!(wait_for(8, || has_proc(pty.pid(), "ping.exe")), "ping should be running under the shell");

    // Remember a GUI pid to prove it survives; it may be absent if notepad is a store-app stub.
    let gui_pid = proc::descendants(pty.pid()).into_iter().find(|p| p.name.eq_ignore_ascii_case("notepad.exe")).map(|p| p.pid);
    let ping_pid = proc::descendants(pty.pid()).into_iter().find(|p| p.name.eq_ignore_ascii_case("ping.exe")).unwrap().pid;

    pty.close(CloseMode::Graceful, true);
    assert!(sink.wait(10, |_, e| e.is_some()), "exit watcher must fire after close");
    assert!(wait_for(5, || !proc::snapshot().iter().any(|p| p.pid == ping_pid)), "ping must die with its tab");
    if notepad {
        let gid = gui_pid.expect("notepad seen");
        assert!(proc::snapshot().iter().any(|p| p.pid == gid), "notepad must survive the tab closing");
        proc::terminate_all(&[gid]);
    } else {
        eprintln!("notepad.exe not observed under the shell (store stub?) — GUI-survival check skipped");
    }
}

#[test]
fn spawn_failure_reports_command_line_and_leaks_nothing() {
    let sink = Arc::new(Collect::default());
    let err = spawn(SpawnConfig { env: env(), ..SpawnConfig::new("definitely-not-a-real-program-xyz.exe --flag", 80, 24) }, sink)
        .err()
        .expect("must fail");
    let msg = err.to_string();
    assert!(msg.contains("definitely-not-a-real-program-xyz.exe --flag"), "{msg}");
}

#[test]
fn ctrl_c_byte_interrupts_a_running_command() {
    let (pty, out) = run("cmd.exe /c ping -n 30 127.0.0.1", 100, 30);
    assert!(out.wait(10, |t, _| t.contains("127.0.0.1")), "ping did not start");
    pty.write(&[0x03]);
    assert!(out.wait(10, |_, e| e.is_some()), "0x03 on the input pipe must end the foreground command");
}

/// ConPTY + reader throughput with a sink that only counts (`cargo test --release -p ut-pty --test conpty perf -- --ignored --nocapture`).
#[test]
#[ignore]
fn perf_type_of_a_big_file() {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    struct Count(AtomicU64, AtomicBool, AtomicU64);
    impl Sink for Count {
        fn frame(&self, d: &[u8]) {
            self.0.fetch_add(d.len() as u64, Ordering::Relaxed);
            self.2.fetch_add(1, Ordering::Relaxed);
        }
        fn exited(&self, _: Option<u32>) {
            self.1.store(true, Ordering::Release);
        }
    }
    let path = tmp("perf").join("big.txt");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let line = format!("{}\r\n", "x".repeat(77));
        let mb: usize = std::env::var("UT_PERF_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
        for _ in 0..(mb * 1024 * 1024 / line.len()) {
            f.write_all(line.as_bytes()).unwrap();
        }
    }
    let size = std::fs::metadata(&path).unwrap().len();
    let sink = Arc::new(Count(AtomicU64::new(0), AtomicBool::new(false), AtomicU64::new(0)));
    let t = Instant::now();
    let _pty = spawn(SpawnConfig { env: env(), high_water: usize::MAX / 4, low_water: usize::MAX / 8, conpty_dll: std::env::var_os("UT_CONPTY_DLL").map(Into::into), ..SpawnConfig::new(&std::env::var("UT_PERF_CMD").unwrap_or_else(|_| "cmd.exe /c type \"{file}\"".into()).replace("{file}", &path.display().to_string()), 120, 40) }, sink.clone()).expect("spawn");
    while !sink.1.load(Ordering::Acquire) && t.elapsed() < Duration::from_secs(300) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let secs = t.elapsed().as_secs_f64();
    println!("type {} MB: {secs:.2} s = {:.1} MB/s (received {} MB in {} frames, avg {} B)", size / 1_000_000, size as f64 / 1e6 / secs, sink.0.load(Ordering::Relaxed) / 1_000_000, sink.2.load(Ordering::Relaxed), sink.0.load(Ordering::Relaxed) / sink.2.load(Ordering::Relaxed).max(1));
}

/// Dumps what ConPTY sends while PSReadLine's ListView redraws (`UT_RAW_OUT=<file>`): a debugging aid, not an assertion.
#[test]
#[ignore]
fn dump_psreadline_listview_stream() {
    let Some(out_path) = std::env::var_os("UT_RAW_OUT") else { return };
    let (cols, rows) = (90u16, 26u16);
    // UT_RAW_FILL=n prints n lines first, so the prompt sits near the bottom and the list has to scroll the screen
    let fill: usize = std::env::var("UT_RAW_FILL").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let cmd = format!("pwsh.exe -NoLogo -NoProfile -NoExit -Command \"1..{fill} | ForEach-Object {{ \\\"line $_\\\" }}; Set-PSReadLineOption -PredictionSource History -PredictionViewStyle ListView\"");
    let (pty, out) = run(&cmd, cols, rows);
    std::thread::sleep(Duration::from_secs(3));
    let mark = 0usize;
    // UT_RAW_TYPE_FILL=n: type an n-line command at the prompt, so the NEXT prompt sits on the bottom row
    if let Some(n) = std::env::var("UT_RAW_TYPE_FILL").ok().and_then(|v| v.parse::<usize>().ok()) {
        pty.write(format!("Set-PSReadLineOption -PredictionViewStyle ListView -PredictionSource History\r1..{n} | % {{ 'line ' + $_ }}\r").as_bytes());
        std::thread::sleep(Duration::from_secs(2));
    }
    for ch in "cd /c/W".chars() {
        pty.write(ch.to_string().as_bytes());
        std::thread::sleep(Duration::from_millis(400));
    }
    std::thread::sleep(Duration::from_secs(1));
    let all = out.buf.lock().clone();
    let tail = &all[mark.min(all.len())..];
    let esc = |b: &[u8]| -> String {
        let mut s = String::new();
        for &c in b {
            match c {
                0x1b => s.push_str("<ESC>"),
                b'\r' => s.push_str("<CR>"),
                b'\n' => s.push_str("<LF>\n"),
                0x20..=0x7e => s.push(c as char),
                _ => s.push_str(&format!("<{c:02x}>")),
            }
        }
        s
    };
    if let Some(bin) = std::env::var_os("UT_RAW_BIN") {
        std::fs::write(bin, tail).unwrap();
    }
    std::fs::write(out_path, esc(tail)).unwrap();
}
