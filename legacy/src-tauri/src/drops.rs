//! SSH helpers and file drag-and-drop (§10, §11): OS drops are forwarded to the UI (which picks the pane),
//! `files_drop` does the copy/upload off the UI thread and reports through `drop:status` events.

use crate::app::emit;
use crate::pane::Pane;
use crate::state::AppState;
use parking_lot::Mutex;
use serde_json::json;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, DragDropEvent, Manager, State};
use ut_shell::ShellKind;
use ut_ssh::target::{remote_dest, title_cwd};
use ut_ssh::transfer::{Destination, DropPlan, Resolution};
use ut_ssh::target::SshTarget;
use ut_ssh::CancelToken;

/// One in-flight drop per pane: cancel token + the channel the conflict dialog answers through.
pub struct DropJob {
    pub cancel: CancelToken,
    resolution: Mutex<Option<Sender<Resolution>>>,
}

// --------------------------------------------------------- OS drag events

/// Forward native drag-drop events (physical coordinates) to the UI, which finds the pane under the cursor.
pub fn on_drag_drop(app: &AppHandle, ev: &DragDropEvent) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    // Shift at drop time selects "paste paths" instead of "copy files" (§11.2 [P1]).
    let shift = unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) } < 0;
    let payload = match ev {
        DragDropEvent::Enter { paths, position } => json!({ "type": "enter", "paths": paths, "x": position.x, "y": position.y }),
        DragDropEvent::Over { position } => json!({ "type": "over", "x": position.x, "y": position.y }),
        DragDropEvent::Drop { paths, position } => json!({ "type": "drop", "paths": paths, "x": position.x, "y": position.y, "shift": shift }),
        _ => json!({ "type": "leave" }),
    };
    emit(app, "app:drag", payload);
}

// ----------------------------------------------------------- ssh discovery

/// Where is ssh in this pane? (§10.4) own command → process tree → OSC 7 host. Cached for 2 s and
/// invalidated on title / command-end, never recomputed per drag-over event (§24 #24).
pub fn locate_ssh(pane: &Pane) -> Option<SshTarget> {
    {
        let c = pane.sink.ssh_cache.lock();
        if let Some((at, t)) = c.as_ref() {
            if at.elapsed() < Duration::from_secs(2) {
                return t.clone();
            }
        }
    }
    let found = locate_uncached(pane);
    *pane.sink.ssh_cache.lock() = Some((Instant::now(), found.clone()));
    found
}

fn locate_uncached(pane: &Pane) -> Option<SshTarget> {
    if let Some(mut t) = ut_ssh::target::parse_argv(&pane.command_line) {
        t.is_primary_shell = true;
        return Some(t);
    }
    for p in ut_pty::proc::descendants(pane.pty.pid()) {
        if let Some(t) = ut_pty::proc::command_line(p.pid).and_then(|c| ut_ssh::target::parse_argv(&c)) {
            return Some(t);
        }
    }
    let i = pane.sink.info.lock();
    match (&i.cwd_host, i.cwd_local) {
        (Some(h), false) if !h.is_empty() => Some(SshTarget::from_host(h)),
        _ => None,
    }
}

#[tauri::command]
pub async fn pane_ssh(state: State<'_, AppState>, pane_id: String) -> Result<Option<SshTarget>, String> {
    let Some(p) = state.pane(&pane_id) else { return Ok(None) };
    tauri::async_runtime::spawn_blocking(move || locate_ssh(&p)).await.map_err(|e| e.to_string())
}

// -------------------------------------------------------------- quick connect

#[tauri::command]
pub async fn quick_connect(state: State<'_, AppState>, input: String) -> Result<Option<serde_json::Value>, String> {
    let Some(ssh) = ut_ssh::locate::find_ssh() else { return Err("ssh.exe was not found.".into()) };
    let Some(cl) = ut_ssh::quick::quick_command_line(&ssh, &input) else { return Ok(None) };
    state.remember_ssh(input.trim());
    Ok(Some(json!({ "commandLine": cl, "title": format!("SSH: {}", input.trim()) })))
}

#[tauri::command]
pub async fn ssh_history(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state.ssh_history.lock().clone())
}

/// A path list quoted for the pane's shell kind (§11.6) — "paste paths" drops.
#[tauri::command]
pub async fn quote_paths(state: State<'_, AppState>, pane_id: String, paths: Vec<String>) -> Result<String, String> {
    let kind = state.pane(&pane_id).map(|p| p.shell_kind).unwrap_or(ShellKind::Unknown);
    Ok(ut_shell::quote_paths(kind, &paths))
}

// ------------------------------------------------------------------ the drop

fn status(app: &AppHandle, pane: &str, state: &str, title: &str, detail: &str, severity: &str, hide_ms: u64) {
    emit(
        app,
        "drop:status",
        json!({ "paneId": pane, "state": state, "title": title, "detail": detail, "severity": severity, "hideMs": hide_ms }),
    );
}

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

/// `mode = "paste"` writes the quoted paths into the shell (Shift-drop / `drop.defaultAction`), otherwise the
/// dropped items are copied into the pane's current directory (local) or uploaded to the remote one (SSH).
#[tauri::command]
pub async fn files_drop(app: AppHandle, state: State<'_, AppState>, pane_id: String, paths: Vec<String>, mode: String) -> Result<(), String> {
    let pane = state.pane(&pane_id).ok_or("unknown pane")?;
    if paths.is_empty() {
        return Ok(());
    }
    if mode == "paste" {
        let q = ut_shell::quote_paths(pane.shell_kind, &paths);
        pane.pty.write(format!("{q} ").as_bytes());
        return Ok(());
    }
    let job = Arc::new(DropJob { cancel: CancelToken::new(), resolution: Mutex::new(None) });
    state.drops.lock().insert(pane_id.clone(), job.clone());
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        run_drop(&app2, &pane_id, &pane, paths, &job);
        app2.state::<AppState>().drops.lock().remove(&pane_id);
    });
    Ok(())
}

fn run_drop(app: &AppHandle, id: &str, pane: &Pane, paths: Vec<PathBuf>, job: &Arc<DropJob>) {
    let (cwd, local_host, title) = {
        let i = pane.sink.info.lock();
        (i.cwd.clone(), i.cwd_local || i.cwd_host.as_deref().unwrap_or("").is_empty(), i.title.clone())
    };
    let target = locate_ssh(pane);
    let label = if paths.len() == 1 {
        paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        format!("{} items", paths.len())
    };

    let (dest, dest_label) = match &target {
        Some(t) => {
            // Remote cwd: OSC 7 with a non-local host, else the title fallback (until OSC 7 is seen), else `~`.
            let osc7_remote = !local_host;
            let remote_cwd = if osc7_remote { cwd } else { title_cwd(&title) };
            let path = remote_dest(remote_cwd.as_deref(), !osc7_remote);
            let label = format!("{}:{}", t.host, path);
            (Destination::Remote { target: t.clone(), path }, label)
        }
        None => {
            let c = cwd.or_else(|| Some(pane.start_cwd.to_string_lossy().into_owned()));
            let d = ut_ssh::transfer::resolve_local_dest(c.as_deref());
            let label = d.to_string_lossy().into_owned();
            (Destination::Local(d), label)
        }
    };

    status(app, id, "checking", "Checking…", &format!("{label} → {dest_label}"), "info", 0);
    let plan: DropPlan = ut_ssh::transfer::plan_drop(&paths, dest);
    let mut resolution = Resolution::Replace; // irrelevant without conflicts
    if !plan.conflicts.is_empty() {
        let (tx, rx): (Sender<Resolution>, Receiver<Resolution>) = channel();
        *job.resolution.lock() = Some(tx);
        emit(app, "drop:conflict", json!({ "paneId": id, "names": plan.conflicts, "dest": dest_label }));
        resolution = rx.recv_timeout(Duration::from_secs(600)).unwrap_or(Resolution::Cancel);
        if resolution == Resolution::Cancel {
            status(app, id, "cancelled", "Cancelled", "", "info", 1500);
            return;
        }
    }
    let total = plan.items.len();
    let (a2, id2) = (app.clone(), id.to_string());
    let progress = move |p: ut_ssh::Progress| {
        let title = if total == 1 { "Copying…".to_string() } else { format!("Copying {total} items…") };
        status(&a2, &id2, "copying", &title, &format!("Copying {} of {}… {}", p.index, p.total, p.name), "info", 0);
    };
    let r = ut_ssh::transfer::execute_drop(&plan, resolution, &job.cancel, &progress);
    let mut failed = r.failed.clone();
    failed.extend(plan.invalid.iter().cloned());
    if r.cancelled {
        status(app, id, "cancelled", "Cancelled", "", "info", 1500);
    } else if failed.is_empty() && !r.copied.is_empty() {
        let t = if r.copied.len() == 1 { "Copied".to_string() } else { format!("Copied {} items", r.copied.len()) };
        status(app, id, "success", &t, &names_line(&r.copied), "success", 3200);
    } else if !r.copied.is_empty() {
        status(app, id, "partial", &format!("Copied {}, {} failed", r.copied.len(), failed.len()), &first_errors(&failed), "warning", 5000);
    } else if failed.is_empty() {
        status(app, id, "failed", "Nothing was copied.", "", "info", 3000);
    } else {
        status(app, id, "failed", "Copy failed", &first_errors(&failed), "error", 5000);
    }
}

#[tauri::command]
pub async fn drop_resolve(state: State<'_, AppState>, pane_id: String, resolution: Resolution) -> Result<(), String> {
    if let Some(j) = state.drops.lock().get(&pane_id) {
        if let Some(tx) = j.resolution.lock().take() {
            let _ = tx.send(resolution);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn drop_cancel(state: State<'_, AppState>, pane_id: String) -> Result<(), String> {
    if let Some(j) = state.drops.lock().get(&pane_id) {
        j.cancel.cancel();
        if let Some(tx) = j.resolution.lock().take() {
            let _ = tx.send(Resolution::Cancel);
        }
    }
    Ok(())
}
