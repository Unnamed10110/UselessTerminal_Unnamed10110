//! File drops (§11): the hover card, copy / upload off the UI thread, the "File already exists" dialog and the
//! paste-paths mode (Shift, or `drop.defaultAction = "paste"`). Planning and transfer live in `ut_ssh::transfer`.

use crate::app::{App, DialogId};
use crate::kit::{Button, ButtonKind, Dialog};
use crate::tab::{DropOverlay, Severity};
use egui::{Context, Pos2};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use ut_ssh::target::{parse_argv, remote_dest, title_cwd, SshTarget};
use ut_ssh::transfer::{self, Destination, Resolution};
use ut_ssh::{CancelToken, Progress};

/// What the worker thread tells the UI.
enum Msg {
    Status { pane: u64, title: String, detail: String, sev: Severity, hide_ms: u64 },
    Conflict { names: Vec<String>, dest: String, reply: Sender<Resolution> },
}

pub struct Drops {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Conflicts waiting for the dialog (one is shown at a time).
    queue: VecDeque<(Vec<String>, String, Sender<Resolution>)>,
    /// Reply channel of the conflict dialog on screen.
    open: Option<Sender<Resolution>>,
}

impl Drops {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, queue: VecDeque::new(), open: None }
    }

    /// The conflict dialog was answered.
    pub fn resolve(&mut self, r: Resolution) {
        if let Some(tx) = self.open.take() {
            let _ = tx.send(r);
        }
    }
}

/// Everything the worker needs from a pane, captured on the UI thread.
struct Snapshot {
    pane: u64,
    pid: u32,
    command_line: String,
    cwd: Option<String>,
    cwd_host: String,
    osc7_local: bool,
    title: String,
    start_cwd: Option<String>,
}

impl App {
    /// Called once per frame: OS drag hover/drop events in, worker messages out.
    pub fn drops_frame(&mut self, ctx: &Context) {
        let (hovered, dropped) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>()));
        if hovered || !dropped.is_empty() {
            crate::debug::log(&format!("drag event: hovered={hovered} dropped={dropped:?}"));
            let target = self.pane_under_cursor(ctx).or_else(|| self.tabs.get(self.active).map(|t| t.focused));
            if let Some(pane) = target {
                if !dropped.is_empty() {
                    self.drop_files(ctx, pane, dropped);
                } else {
                    self.hover_card(pane);
                    ctx.request_repaint_after(Duration::from_millis(60));
                }
            }
        }
        while let Ok(m) = self.drops.rx.try_recv() {
            match m {
                Msg::Status { pane, title, detail, sev, hide_ms } => {
                    let until = (hide_ms > 0).then(|| Instant::now() + Duration::from_millis(hide_ms));
                    if let Some(p) = self.tabs.iter_mut().find_map(|t| t.pane_mut(pane)) {
                        p.drop_overlay = Some(DropOverlay { title, detail, severity: sev, until });
                    }
                    if hide_ms > 0 {
                        ctx.request_repaint_after(Duration::from_millis(hide_ms + 50));
                    }
                }
                Msg::Conflict { names, dest, reply } => self.drops.queue.push_back((names, dest, reply)),
            }
        }
        if self.drops.open.is_none() && self.dialog.is_none() {
            if let Some((names, dest, reply)) = self.drops.queue.pop_front() {
                self.drops.open = Some(reply);
                let mut d = Dialog::new(
                    "File already exists",
                    &conflict_message(&names, &dest),
                    vec![Button::new("Cancel", ButtonKind::Normal), Button::new("Rename", ButtonKind::Primary), Button::new("Replace", ButtonKind::Danger)],
                );
                d.default = 1;
                self.dialog = Some((DialogId::DropConflict, d));
            }
        }
    }

    /// The pane under the OS cursor (egui gets no pointer events during an OS file drag).
    fn pane_under_cursor(&self, ctx: &Context) -> Option<u64> {
        use windows::Win32::Foundation::{HWND, POINT};
        use windows::Win32::Graphics::Gdi::ScreenToClient;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let hwnd = HWND(self.hwnd? as *mut _);
        let mut p = POINT::default();
        unsafe {
            GetCursorPos(&mut p).ok()?;
            if !ScreenToClient(hwnd, &mut p).as_bool() {
                return None;
            }
        }
        let ppp = ctx.pixels_per_point();
        self.tabs.get(self.active)?.pane_at(Pos2::new(p.x as f32 / ppp, p.y as f32 / ppp))
    }

    fn hover_card(&mut self, pane: u64) {
        let paste = paste_mode() != (self.cfg.drop.default_action == ut_core::settings::DropAction::Paste);
        let Some(p) = self.tabs.get_mut(self.active).and_then(|t| t.pane_mut(pane)) else { return };
        let (title, detail) = if paste {
            ("Drop to paste paths".to_string(), "The paths are typed at the prompt".to_string())
        } else if parse_argv(&p.command_line).is_some() {
            ("Drop to upload".to_string(), "Release to upload into the remote folder".to_string())
        } else {
            ("Drop to copy".to_string(), p.cwd.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| "Release to copy into this folder".into()))
        };
        p.drop_overlay = Some(DropOverlay { title, detail, severity: Severity::Info, until: Some(Instant::now() + Duration::from_millis(250)) });
    }

    fn drop_files(&mut self, ctx: &Context, pane: u64, paths: Vec<PathBuf>) {
        let paste = paste_mode() != (self.cfg.drop.default_action == ut_core::settings::DropAction::Paste);
        let read_only = self.tabs.get(self.active).is_some_and(|t| t.read_only);
        let Some(p) = self.tabs.get_mut(self.active).and_then(|t| t.pane_mut(pane)) else { return };
        p.drop_overlay = None;
        let Some(session) = p.session.clone().filter(|_| !p.exited) else {
            p.drop_overlay = Some(DropOverlay {
                title: "Cannot copy".into(),
                detail: "The current shell directory is not available yet.".into(),
                severity: Severity::Error,
                until: Some(Instant::now() + Duration::from_secs(4)),
            });
            return;
        };
        if read_only || p.read_only {
            return;
        }
        if paste {
            let q = ut_shell::quote_paths(p.kind, &paths.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>());
            session.write(format!("{q} ").as_bytes());
            return;
        }
        let snap = Snapshot {
            pane,
            pid: session.pid(),
            command_line: p.command_line.clone(),
            cwd: p.cwd.clone(),
            cwd_host: p.cwd_host.clone(),
            osc7_local: p.cwd_local || p.cwd_host.is_empty(),
            title: p.title.clone(),
            start_cwd: p.start_cwd.clone(),
        };
        crate::debug::log(&format!("drop: copy job pane={pane} files={paths:?}"));
        let (ctx, tx) = (ctx.clone(), self.drops.tx.clone());
        std::thread::spawn(move || run(&ctx, &tx, &snap, paths));
    }
}

/// Shift held at drop time selects the opposite of the configured default (§11.2).
fn paste_mode() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) < 0 }
}

// ------------------------------------------------------------------------------------------- worker

/// Where is ssh in this pane? (§10.4) own command → process tree → OSC 7 host. Runs on the worker only (process snapshot).
fn locate_ssh(s: &Snapshot) -> Option<SshTarget> {
    if let Some(mut t) = parse_argv(&s.command_line) {
        t.is_primary_shell = true;
        return Some(t);
    }
    for p in ut_pty::proc::descendants(s.pid) {
        if let Some(t) = ut_pty::proc::command_line(p.pid).and_then(|c| parse_argv(&c)) {
            return Some(t);
        }
    }
    (!s.osc7_local).then(|| SshTarget::from_host(&s.cwd_host))
}

fn run(ctx: &Context, tx: &Sender<Msg>, s: &Snapshot, paths: Vec<PathBuf>) {
    let status = |title: &str, detail: &str, sev: Severity, hide_ms: u64| {
        let _ = tx.send(Msg::Status { pane: s.pane, title: title.into(), detail: detail.into(), sev, hide_ms });
        ctx.request_repaint();
    };
    let label = match paths.as_slice() {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        many => format!("{} items", many.len()),
    };
    status("Checking…", &format!("{label} → …"), Severity::Info, 0);

    let (dest, dest_label) = match locate_ssh(s) {
        Some(t) => {
            // Remote cwd: OSC 7 with a non-local host, else the title fallback (until OSC 7 is seen), else `~`.
            let remote_cwd = if s.osc7_local { title_cwd(&s.title) } else { s.cwd.clone() };
            let path = remote_dest(remote_cwd.as_deref(), s.osc7_local);
            let label = format!("{}:{}", t.host, path);
            (Destination::Remote { target: t, path }, label)
        }
        None => {
            let c = s.cwd.clone().or_else(|| s.start_cwd.clone());
            let d = transfer::resolve_local_dest(c.as_deref());
            let label = d.to_string_lossy().into_owned();
            (Destination::Local(d), label)
        }
    };
    status("Checking…", &format!("{label} → {dest_label}"), Severity::Info, 0);

    let plan = transfer::plan_drop(&paths, dest);
    let mut resolution = Resolution::Replace; // irrelevant without conflicts
    if !plan.conflicts.is_empty() {
        let (reply, answer) = channel();
        let _ = tx.send(Msg::Conflict { names: plan.conflicts.clone(), dest: dest_label, reply });
        ctx.request_repaint();
        resolution = answer.recv_timeout(Duration::from_secs(600)).unwrap_or(Resolution::Cancel);
        if resolution == Resolution::Cancel {
            status("Cancelled", "", Severity::Info, 1500);
            return;
        }
    }
    let total = plan.items.len();
    let progress = |p: Progress| {
        let title = if total == 1 { "Copying…".to_string() } else { format!("Copying {total} items…") };
        status(&title, &format!("Copying {} of {}… {}", p.index, p.total, p.name), Severity::Info, 0);
    };
    let r = transfer::execute_drop(&plan, resolution, &CancelToken::new(), &progress);
    if r.cancelled {
        status("Cancelled", "", Severity::Info, 1500);
    } else if r.failed.is_empty() && !r.copied.is_empty() {
        let t = if r.copied.len() == 1 { "Copied".to_string() } else { format!("Copied {} items", r.copied.len()) };
        status(&t, &names_line(&r.copied), Severity::Success, 3200);
    } else if !r.copied.is_empty() {
        status(&format!("Copied {}, {} failed", r.copied.len(), r.failed.len()), &first_errors(&r.failed), Severity::Warning, 5000);
    } else if r.failed.is_empty() {
        status("Nothing was copied.", "", Severity::Info, 3000);
    } else {
        status("Copy failed", &first_errors(&r.failed), Severity::Error, 5000);
    }
}

// --------------------------------------------------------------------------------------------- text

/// Up to four names, then "… and N more" (§11.3).
fn names_line(names: &[String]) -> String {
    let mut s = names.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 4 {
        s.push_str(&format!(" … and {} more", names.len() - 4));
    }
    s
}

fn first_errors(failed: &[(String, String)]) -> String {
    failed.iter().take(4).map(|(n, e)| format!("{n}: {e}")).collect::<Vec<_>>().join("\n")
}

/// The "File already exists" text (§11.4): headline, destination, up to 6 names (else 5 + "… and N more"), hint.
fn conflict_message(names: &[String], dest: &str) -> String {
    let head = if names.len() == 1 { "A file with this name already exists.".to_string() } else { format!("{} items already exist in the destination.", names.len()) };
    let listed = if names.len() <= 6 {
        names.join("\n")
    } else {
        format!("{}\n… and {} more", names[..5].join("\n"), names.len() - 5)
    };
    let example = names.first().map_or(String::new(), |n| format!(" (e.g. \"{}\")", renamed(n)));
    format!("{head}\n\nDestination: {dest}\n\n{listed}\n\nRename keeps both copies{example}. Replace overwrites the existing item(s).")
}

/// `"{stem} (1){ext}"` (§11.4).
fn renamed(name: &str) -> String {
    match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => format!("{} (1){}", &name[..i], &name[i..]),
        None => format!("{name} (1)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(c: usize) -> Vec<String> {
        (1..=c).map(|i| format!("f{i}.txt")).collect()
    }

    #[test]
    fn names_are_capped_at_four() {
        assert_eq!(names_line(&n(2)), "f1.txt, f2.txt");
        assert_eq!(names_line(&n(6)), "f1.txt, f2.txt, f3.txt, f4.txt … and 2 more");
    }

    #[test]
    fn conflict_text_lists_six_or_five_plus_more() {
        let one = conflict_message(&n(1), "C:\\d");
        assert!(one.starts_with("A file with this name already exists."));
        let six = conflict_message(&n(6), "C:\\d");
        assert!(six.contains("6 items already exist") && six.contains("f6.txt") && !six.contains("more"));
        let seven = conflict_message(&n(7), "C:\\d");
        assert!(seven.contains("f5.txt") && !seven.contains("f6.txt") && seven.contains("… and 2 more"));
    }

    #[test]
    fn rename_example_keeps_the_extension() {
        assert_eq!(renamed("a.txt"), "a (1).txt");
        assert_eq!(renamed("a.tar.gz"), "a.tar (1).gz");
        assert_eq!(renamed(".env"), ".env (1)");
        assert_eq!(renamed("dir"), "dir (1)");
        assert!(conflict_message(&n(1), "C:\\d").contains("e.g. \"f1 (1).txt\""));
    }

    #[test]
    fn errors_show_the_first_four() {
        let f: Vec<_> = (0..6).map(|i| (format!("a{i}"), "boom".to_string())).collect();
        assert_eq!(first_errors(&f).lines().count(), 4);
    }
}
