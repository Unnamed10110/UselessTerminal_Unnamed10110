//! SSH helpers (spec §8.7, §10, §11.1-11.5). Everything is synchronous: call from worker threads.
//!
//! * [`locate`]: `find_ssh()`, `find_scp(ssh)`, `find_sftp(ssh)` (§10.1).
//! * [`quick`]: `parse_quick_connect(input)`, `quick_command_line(ssh, input)` (§10.2, IPv6 aware).
//! * [`target`]: `SshTarget`, `parse_argv(cmdline)` (§10.4), `resolve_alias` (`ssh -G`, 60 s cache),
//!   `title_cwd`, `is_remote_cwd_usable`, `remote_dest` (§10.6 / §11.2).
//! * [`config`]: `parse_ssh_config(text)`, `read_ssh_config(path)` (follows `Include`),
//!   `host_to_command_line`, `host_description` (§8.7).
//! * [`mux`]: `probe_multiplexing(ssh)`, `mux_dir()`, `inject_connection_reuse(cmdline, mode, ssh)` (§10.5).
//! * [`transfer`]: `resolve_local_dest(cwd)`, `plan_drop(paths, dest)` -> `DropPlan{items, conflicts}`,
//!   `execute_drop(plan, resolution, cancel, progress)` -> `DropResult{copied, failed, ..}` (§11.2-11.5),
//!   plus the pure command builders (quoting, scripts, scp/sftp argv, error formatting).
//!
//! Long operations take a [`CancelToken`] and a `&dyn Fn(Progress)` callback.

pub mod config;
pub mod locate;
pub mod mux;
pub mod quick;
pub mod target;
pub mod transfer;

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Cooperative cancellation flag, cheap to clone and share between threads.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Progress of a drop: item `index` (1-based) of `total`; bytes are over the whole drop
/// (`bytes_total` is the sum of the local source sizes).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub index: usize,
    pub total: usize,
    pub name: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

pub(crate) struct Out {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) fn hide_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Run `cmd` to completion, capturing output. `stdin` (if any) is fed from a thread. On timeout or
/// cancel the whole process tree is killed (`taskkill /F /T`, §11.5).
pub(crate) fn run(
    cmd: &mut Command,
    stdin: Option<&[u8]>,
    timeout: Duration,
    cancel: Option<&CancelToken>,
) -> io::Result<Out> {
    hide_window(cmd)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = si.write_all(&data);
        });
    }
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());

    let start = Instant::now();
    let (mut timed_out, mut cancelled) = (false, false);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        cancelled = cancel.is_some_and(CancelToken::is_cancelled);
        timed_out = !cancelled && start.elapsed() >= timeout;
        if cancelled || timed_out {
            kill_tree(&mut child);
            break None;
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    // A surviving grandchild (e.g. a ControlPersist master) can keep the pipes open: don't wait forever.
    let grace = Duration::from_millis(1500);
    let text = |rx: mpsc::Receiver<Vec<u8>>| String::from_utf8_lossy(&rx.recv_timeout(grace).unwrap_or_default()).into_owned();
    Ok(Out {
        code: status.and_then(|s| s.code()),
        stdout: text(out_rx),
        stderr: text(err_rx),
        timed_out,
        cancelled,
    })
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    match r {
        Some(mut r) => {
            std::thread::spawn(move || {
                let mut v = Vec::new();
                let _ = r.read_to_end(&mut v);
                let _ = tx.send(v);
            });
        }
        None => {
            let _ = tx.send(Vec::new());
        }
    }
    rx
}

fn kill_tree(child: &mut Child) {
    let mut tk = Command::new("taskkill");
    tk.args(["/F", "/T", "/PID", &child.id().to_string()]).stdout(Stdio::null()).stderr(Stdio::null());
    let _ = hide_window(&mut tk).status();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_c(script: &str) -> Command {
        let mut c = Command::new("cmd");
        c.args(["/C", script]);
        c
    }

    #[test]
    fn run_captures_output_and_code() {
        let o = run(&mut cmd_c("echo hi & echo err 1>&2 & exit 3"), None, Duration::from_secs(20), None).unwrap();
        assert_eq!(o.code, Some(3));
        assert!(o.stdout.contains("hi") && o.stderr.contains("err"));
        assert!(!o.timed_out && !o.cancelled);
    }

    #[test]
    fn run_feeds_stdin() {
        let o = run(Command::new("findstr").arg("x"), Some(b"axb\r\nzzz\r\n"), Duration::from_secs(20), None).unwrap();
        assert_eq!(o.stdout.trim(), "axb");
    }

    #[test]
    fn timeout_kills_the_tree() {
        let t = Instant::now();
        let o = run(&mut cmd_c("ping -n 30 127.0.0.1 >nul"), None, Duration::from_millis(300), None).unwrap();
        assert!(o.timed_out && o.code.is_none());
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn cancel_stops_the_process() {
        let c = CancelToken::new();
        let c2 = c.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c2.cancel();
        });
        let t = Instant::now();
        let o = run(&mut cmd_c("ping -n 30 127.0.0.1 >nul"), None, Duration::from_secs(60), Some(&c)).unwrap();
        assert!(o.cancelled && !o.timed_out);
        assert!(t.elapsed() < Duration::from_secs(10));
    }
}
