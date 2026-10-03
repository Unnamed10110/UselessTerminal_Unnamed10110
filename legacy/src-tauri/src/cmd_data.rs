//! Sessions, folders, snippets, workspaces, shells, imports/exports and window-state commands.

use crate::app::emit;
use crate::state::AppState;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use ut_core::WindowState;
use ut_data::{Folder, Half, ImportMode, Resolver, Session, Snippet, Target, Workspace};
use ut_shell::ShellProfile;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    folders: Vec<Folder>,
    sessions: Vec<Session>,
    snippets: Vec<Snippet>,
    banner: Option<String>,
    version: u64,
}

fn snapshot(state: &AppState) -> Snapshot {
    let f = state.sessions.snapshot();
    Snapshot { folders: f.folders, sessions: f.sessions, snippets: f.snippets, banner: state.sessions.banner(), version: state.sessions.version() }
}

/// Push the new state to the UI (event-driven live dots / lists — no polling, §24 #27).
fn changed(app: &AppHandle, state: &AppState) {
    emit(app, "sessions:changed", snapshot(state));
}

fn workspaces_changed(app: &AppHandle, state: &AppState) {
    emit(app, "workspaces:changed", state.workspaces.list());
}

#[tauri::command]
pub async fn sessions_snapshot(state: State<'_, AppState>) -> Result<Snapshot, String> {
    Ok(snapshot(&state))
}

#[tauri::command]
pub async fn sessions_search(state: State<'_, AppState>, query: String) -> Result<Vec<Session>, String> {
    Ok(state.sessions.search(&query))
}

#[tauri::command]
pub async fn sessions_clear_banner(state: State<'_, AppState>) -> Result<(), String> {
    state.sessions.clear_banner();
    Ok(())
}

#[tauri::command]
pub async fn session_add(app: AppHandle, state: State<'_, AppState>, session: Value) -> Result<Session, String> {
    let s: Session = serde_json::from_value(session).map_err(|e| e.to_string())?;
    let s = state.sessions.add_session(s).map_err(|e| e.to_string())?;
    changed(&app, &state);
    Ok(s)
}

#[tauri::command]
pub async fn session_update(app: AppHandle, state: State<'_, AppState>, session: Value) -> Result<Session, String> {
    let s: Session = serde_json::from_value(session).map_err(|e| e.to_string())?;
    let s = state.sessions.update_session(s).map_err(|e| e.to_string())?;
    changed(&app, &state);
    Ok(s)
}

#[tauri::command]
pub async fn session_delete(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.sessions.delete_session(&id);
    changed(&app, &state);
    Ok(())
}

#[tauri::command]
pub async fn session_duplicate(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<Session, String> {
    let s = state.sessions.duplicate_session(&id).ok_or("Unknown session")?;
    changed(&app, &state);
    Ok(s)
}

#[tauri::command]
pub async fn folder_add(app: AppHandle, state: State<'_, AppState>, name: String) -> Result<Folder, String> {
    let f = state.sessions.add_folder(&name);
    changed(&app, &state);
    Ok(f)
}

#[tauri::command]
pub async fn folder_rename(app: AppHandle, state: State<'_, AppState>, id: String, name: String) -> Result<(), String> {
    state.sessions.rename_folder(&id, &name);
    changed(&app, &state);
    Ok(())
}

#[tauri::command]
pub async fn folder_delete(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.sessions.delete_folder(&id);
    changed(&app, &state);
    Ok(())
}

#[tauri::command]
pub async fn folder_move(app: AppHandle, state: State<'_, AppState>, id: String, dir: String) -> Result<(), String> {
    if dir == "up" { state.sessions.move_folder_up(&id) } else { state.sessions.move_folder_down(&id) };
    changed(&app, &state);
    Ok(())
}

/// The whole §8.2 drop table: `session_ids` and/or the dragged `folder_id` dropped on `target`.
#[tauri::command]
pub async fn items_move(
    app: AppHandle,
    state: State<'_, AppState>,
    session_ids: Vec<String>,
    folder_id: Option<String>,
    target: Target,
    half: Half,
) -> Result<bool, String> {
    let moved = state.sessions.move_items(&session_ids, folder_id.as_deref(), &target, half);
    if moved {
        changed(&app, &state);
    }
    Ok(moved)
}

#[tauri::command]
pub async fn snippet_add(app: AppHandle, state: State<'_, AppState>, snippet: Value) -> Result<Snippet, String> {
    let s: Snippet = serde_json::from_value(snippet).map_err(|e| e.to_string())?;
    let s = state.sessions.add_snippet(s).map_err(|e| e.to_string())?;
    changed(&app, &state);
    Ok(s)
}

#[tauri::command]
pub async fn snippet_update(app: AppHandle, state: State<'_, AppState>, snippet: Value) -> Result<Snippet, String> {
    let s: Snippet = serde_json::from_value(snippet).map_err(|e| e.to_string())?;
    let s = state.sessions.update_snippet(s).map_err(|e| e.to_string())?;
    changed(&app, &state);
    Ok(s)
}

#[tauri::command]
pub async fn snippet_delete(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.sessions.delete_snippet(&id);
    changed(&app, &state);
    Ok(())
}

// ------------------------------------------------------------ import / export

#[tauri::command]
pub async fn sessions_export(app: AppHandle, state: State<'_, AppState>) -> Result<Option<String>, String> {
    let text = ut_data::export_json(&state.sessions);
    let a = app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        a.dialog().file().add_filter("Sessions", &["json"]).set_file_name("useless-terminal-sessions.json").blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(p) = path.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    std::fs::write(&p, text).map_err(|e| e.to_string())?;
    Ok(Some(p.to_string_lossy().into_owned()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    added: usize,
    skipped: usize,
    message: String,
}

#[tauri::command]
pub async fn sessions_import(app: AppHandle, state: State<'_, AppState>, mode: ImportMode) -> Result<Option<ImportResult>, String> {
    let a = app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || a.dialog().file().add_filter("Sessions", &["json"]).blocking_pick_file())
        .await
        .map_err(|e| e.to_string())?;
    let Some(p) = path.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    let r = ut_data::import_json(&state.sessions, &text, mode).map_err(|e| e.to_string())?;
    changed(&app, &state);
    Ok(Some(ImportResult { added: r.added, skipped: r.skipped, message: format!("Imported {} session(s).", r.added) }))
}

#[tauri::command]
pub async fn import_wt(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let profiles = state.shells.get(false);
    let resolver = Resolver {
        pwsh_path: profiles.iter().find(|p| p.id == "pwsh").map(|p| p.path.clone()),
        wsl_exe: profiles.iter().find(|p| p.id == "wsl").map(|p| p.path.clone()),
    };
    if ut_data::find_wt_settings().is_none() {
        return Ok(serde_json::json!({ "added": 0, "found": 0, "message": "Windows Terminal settings.json was not found." }));
    }
    let r = ut_data::wt_import(&state.sessions, &resolver).map_err(|e| e.to_string())?;
    changed(&app, &state);
    // §8.6: report the number actually added, not the number found.
    Ok(serde_json::json!({
        "added": r.added, "found": r.added + r.skipped,
        "message": format!("Imported {} Windows Terminal profile(s).", r.added),
    }))
}

#[tauri::command]
pub async fn import_ssh_config(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let ssh = ut_ssh::locate::find_ssh().ok_or("ssh.exe was not found.")?;
    let hosts = ut_ssh::config::read_ssh_config(None).map_err(|e| e.to_string())?;
    if hosts.is_empty() {
        return Ok(serde_json::json!({ "added": 0, "message": "No SSH hosts found in ~/.ssh/config." }));
    }
    let sessions: Vec<Session> = hosts
        .iter()
        .map(|h| Session {
            name: format!("[SSH] {}", h.alias),
            description: ut_ssh::config::host_description(h),
            shell_path: ssh.to_string_lossy().into_owned(),
            arguments: h.alias.clone(), // `ssh <alias>`: ssh applies the user's whole config itself (§8.7)
            color_tag: "#6be5ff".into(),
            ..Session::default()
        })
        .collect();
    let r = ut_data::add_imported(&state.sessions, sessions);
    changed(&app, &state);
    Ok(serde_json::json!({ "added": r.added, "message": format!("Imported {} SSH host(s) from ~/.ssh/config.", r.added) }))
}

// ---------------------------------------------------------------- workspaces

#[tauri::command]
pub async fn workspaces_list(state: State<'_, AppState>) -> Result<Vec<Workspace>, String> {
    Ok(state.workspaces.list())
}

#[tauri::command]
pub async fn workspace_save(app: AppHandle, state: State<'_, AppState>, workspace: Value) -> Result<Workspace, String> {
    let w: Workspace = serde_json::from_value(workspace).map_err(|e| e.to_string())?;
    let w = state.workspaces.add(w).map_err(|e| e.to_string())?;
    workspaces_changed(&app, &state);
    Ok(w)
}

#[tauri::command]
pub async fn workspace_rename(app: AppHandle, state: State<'_, AppState>, id: String, name: String) -> Result<(), String> {
    state.workspaces.rename(&id, &name);
    workspaces_changed(&app, &state);
    Ok(())
}

#[tauri::command]
pub async fn workspace_delete(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.workspaces.delete(&id);
    workspaces_changed(&app, &state);
    Ok(())
}

// -------------------------------------------------------------------- shells

#[tauri::command]
pub async fn shells_detect(app: AppHandle, force: Option<bool>) -> Result<Vec<ShellProfile>, String> {
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        app.state::<AppState>().shells.get(force)
    })
    .await
    .map_err(|e| e.to_string())
}

// --------------------------------------------------------------- window state

#[tauri::command]
pub async fn window_state_load(state: State<'_, AppState>) -> Result<Value, String> {
    let (ws, _) = state.load_window_state();
    serde_json::to_value(ws).map_err(|e| e.to_string())
}

/// The UI sends tabs/panel state; the bounds come from what the backend tracked while the window was in
/// the Normal state (§16.3: never from a hidden or minimised window).
#[tauri::command]
pub async fn window_state_save(state: State<'_, AppState>, state_json: Value) -> Result<(), String> {
    if state.closing.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(()); // close has started: an autosave queued earlier must not save an empty tab list
    }
    let mut ws = WindowState::from_value(&state_json);
    ws.bounds = *state.normal_bounds.lock();
    ws.maximized = *state.maximized.lock();
    ws.sanitize();
    state.queue_window_state(&ws);
    Ok(())
}
