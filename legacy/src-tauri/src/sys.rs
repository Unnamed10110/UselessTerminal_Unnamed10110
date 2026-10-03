//! OS-facing commands: opening links/paths (§5.10, §18.2), dialogs, attention, elevation, diagnostics.

use crate::app::main_hwnd;
use crate::state::AppState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{FlashWindowEx, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY, SW_SHOWNORMAL};

/// ShellExecute "open" — for URLs and for files the user explicitly asked to open.
pub fn shell_open(target: &str) -> Result<(), String> {
    let r = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    if r.0 as usize > 32 { Ok(()) } else { Err(format!("Could not open {target} (code {})", r.0 as usize)) }
}

fn explorer(arg: &str) {
    let _ = std::process::Command::new("explorer.exe").raw_arg_compat(arg).spawn();
}

trait RawArgCompat {
    fn raw_arg_compat(&mut self, a: &str) -> &mut Self;
}
impl RawArgCompat for std::process::Command {
    fn raw_arg_compat(&mut self, a: &str) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.raw_arg(a)
    }
}

const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OpenResult {
    Opened,
    NeedsConfirmation,
}

/// Only http/https/mailto open silently; any other scheme (file:, ms-settings:, custom protocols) needs the
/// user to confirm the full URL first (§18.2) — the UI shows the dialog and calls again with `confirmed`.
#[tauri::command]
pub async fn open_external(url: String, confirmed: Option<bool>) -> Result<OpenResult, String> {
    let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
    let plain = url.contains(':') && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    if !plain || url.chars().any(|c| c.is_control()) {
        return Err("Not a valid URL.".into());
    }
    if !ALLOWED_SCHEMES.contains(&scheme.as_str()) && confirmed != Some(true) {
        return Ok(OpenResult::NeedsConfirmation);
    }
    shell_open(&url)?;
    Ok(OpenResult::Opened)
}

const EXECUTABLE_EXTS: [&str; 14] = [
    "exe", "bat", "cmd", "ps1", "vbs", "lnk", "com", "msi", "scr", "js", "jse", "wsf", "hta", "reg",
];

/// Open a clicked file path (§5.10): folders → Explorer; missing file → its parent; executables are never
/// run (the parent folder opens with the file selected); anything else → the editor, else the default app.
#[tauri::command]
pub async fn open_path(state: State<'_, AppState>, path: String, line: Option<u32>, cwd: Option<String>) -> Result<(), String> {
    let mut p = PathBuf::from(path.trim().trim_matches('"'));
    if p.is_relative() {
        if let Some(c) = cwd.as_ref().filter(|c| Path::new(c).is_dir()) {
            p = Path::new(c).join(&p);
        }
    }
    let editor = state.cfg().links.editor_command;
    tauri::async_runtime::spawn_blocking(move || {
        let s = p.to_string_lossy().into_owned();
        if s.contains('"') || s.contains('\n') {
            return Err("Unsupported path.".to_string());
        }
        if p.is_dir() {
            explorer(&format!("\"{s}\""));
            return Ok(());
        }
        if !p.exists() {
            let parent = p.parent().filter(|d| d.is_dir()).ok_or("The path does not exist.")?;
            explorer(&format!("\"{}\"", parent.display()));
            return Ok(());
        }
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if EXECUTABLE_EXTS.contains(&ext.as_str()) {
            explorer(&format!("/select,\"{s}\""));
            return Ok(());
        }
        if !run_editor(&editor, &s, line.unwrap_or(1)) {
            shell_open(&s)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Run the `links.editorCommand` template (`code --goto "{file}:{line}"`) without a shell: tokenise, substitute,
/// resolve the program on PATH; `.cmd`/`.bat` shims (code.cmd) go through `cmd.exe /d /c`.
fn run_editor(template: &str, file: &str, line: u32) -> bool {
    use std::os::windows::process::CommandExt;
    let mut toks: Vec<String> = Vec::new();
    let (mut cur, mut quoted, mut any) = (String::new(), false, false);
    for c in template.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        toks.push(cur);
    }
    let toks: Vec<String> = toks.into_iter().map(|t| t.replace("{file}", file).replace("{line}", &line.to_string())).collect();
    let Some((prog, args)) = toks.split_first() else { return false };
    let Some(exe) = ut_shell::resolve_exe(prog) else { return false };
    const NO_WINDOW: u32 = 0x0800_0000;
    let ext = exe.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let mut cmd;
    if ext == "cmd" || ext == "bat" {
        cmd = std::process::Command::new("cmd.exe");
        cmd.args(["/d", "/c"]).arg(&exe).args(args);
    } else {
        cmd = std::process::Command::new(&exe);
        cmd.args(args);
    }
    cmd.creation_flags(NO_WINDOW).spawn().is_ok()
}

#[tauri::command]
pub async fn export_buffer(app: AppHandle, text: String, suggested_name: Option<String>) -> Result<Option<String>, String> {
    let name = suggested_name.unwrap_or_else(|| "terminal-output.txt".into());
    let a = app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        a.dialog()
            .file()
            .add_filter("Text", &["txt"])
            .add_filter("Log", &["log"])
            .add_filter("All files", &["*"])
            .set_file_name(name)
            .blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(p) = path.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    std::fs::write(&p, text).map_err(|e| e.to_string())?;
    Ok(Some(p.to_string_lossy().into_owned()))
}

/// Flash the taskbar button until the window is foregrounded (§12.3) and/or show a toast.
#[tauri::command]
pub async fn notify_attention(app: AppHandle, kind: String, title: Option<String>, body: Option<String>) -> Result<(), String> {
    if kind == "toast" {
        let _ = app
            .notification()
            .builder()
            .title(title.unwrap_or_else(|| "Useless Terminal".into()))
            .body(body.unwrap_or_default())
            .show();
    } else if let Some(h) = main_hwnd(&app) {
        let fi = FLASHWINFO {
            cbSize: size_of::<FLASHWINFO>() as u32,
            hwnd: HWND(h as *mut _),
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        unsafe {
            let _ = FlashWindowEx(&fi);
        }
    }
    Ok(())
}

/// "Run as administrator" while the app is not elevated: a separate elevated console (§4.9).
#[tauri::command]
pub async fn run_elevated(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let s = state.sessions.get(&session_id).ok_or("Unknown session")?;
    let (exe, args) = ut_shell::split_exe_and_args(&s.full_command());
    let cwd = s.resolved_cwd(&[]);
    tauri::async_runtime::spawn_blocking(move || ut_pty::elevate::run_elevated(&exe, &args, Some(&cwd)))
        .await
        .map_err(|e| e.to_string())?
}

/// §7.9: does the shell have a non-console-host child (something running, without OSC 133)?
#[tauri::command]
pub async fn pane_busy(state: State<'_, AppState>, pane_id: String) -> Result<bool, String> {
    let Some(p) = state.pane(&pane_id) else { return Ok(false) };
    let pid = p.pty.pid();
    tauri::async_runtime::spawn_blocking(move || ut_pty::proc::has_busy_children(pid)).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn app_quit(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.closing.store(true, std::sync::atomic::Ordering::Release);
    state.flush_all();
    app.exit(0);
    Ok(())
}

// ------------------------------------------------------------------ app info

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupMessage {
    kind: &'static str,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: &'static str,
    elevated: bool,
    os_build: u32,
    computer_name: String,
    webview2_version: Option<String>,
    home_dir: String,
    data_dir: String,
    args: crate::state::StartArgs,
    startup: Vec<StartupMessage>,
}

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo, String> {
    // A booting UI knows nothing about earlier panes (webview reload / crash): close the orphans instead of
    // leaking their shells. (A 1 MiB replay buffer for re-attaching is the [P2] alternative, §3.5.)
    let orphans: Vec<_> = state.panes.write().drain().map(|(_, p)| p).collect();
    if !orphans.is_empty() {
        let kill = state.kill_console_tree();
        tauri::async_runtime::spawn_blocking(move || {
            for p in orphans {
                p.pty.close(ut_pty::CloseMode::Graceful, kill);
            }
        });
    }
    let startup = std::mem::take(&mut *state.startup.lock())
        .into_iter()
        .map(|b| StartupMessage { kind: b.kind, text: b.text })
        .collect();
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        elevated: state.elevated,
        os_build: os_build(),
        computer_name: std::env::var("COMPUTERNAME").unwrap_or_default(),
        webview2_version: tauri::webview_version().ok(),
        home_dir: ut_fs::home_dir().to_string_lossy().into_owned(),
        data_dir: ut_fs::app_data_dir().to_string_lossy().into_owned(),
        args: state.args.lock().clone(),
        startup,
    })
}

/// The real OS build number (xterm's `windowsPty.buildNumber`, §5.2).
pub fn os_build() -> u32 {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let mut buf = [0u16; 32];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    };
    if r.is_err() {
        return 0;
    }
    String::from_utf16_lossy(&buf).trim_end_matches('\0').trim().parse().unwrap_or(0)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneDiag {
    pane_id: String,
    unacked: usize,
    pid: u32,
    alive: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    working_set_bytes: u64,
    panes: Vec<PaneDiag>,
}

#[tauri::command]
pub async fn diagnostics(state: State<'_, AppState>) -> Result<Diagnostics, String> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    let ws = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb).map(|_| c.WorkingSetSize as u64).unwrap_or(0) };
    let panes = state
        .panes
        .read()
        .iter()
        .map(|(id, p)| PaneDiag { pane_id: id.clone(), unacked: p.pty.unacked(), pid: p.pty.pid(), alive: p.pty.is_alive() })
        .collect();
    Ok(Diagnostics { working_set_bytes: ws, panes })
}

/// Label of a physical key on the CURRENT keyboard layout (VK_OEM_3 is "Ñ" on es-ES, §7.8).
#[tauri::command]
pub async fn key_label(code: String) -> String {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyNameTextW, MapVirtualKeyW, MAPVK_VK_TO_VSC};
    let vk: u32 = match code.as_str() {
        "Backquote" => 0xC0,
        "Minus" => 0xBD,
        "Equal" => 0xBB,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "Semicolon" => 0xBA,
        "Quote" => 0xDE,
        "BracketLeft" => 0xDB,
        "BracketRight" => 0xDD,
        "Backslash" => 0xDC,
        _ => return code,
    };
    unsafe {
        let sc = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
        let mut buf = [0u16; 32];
        let n = GetKeyNameTextW((sc << 16) as i32, &mut buf);
        if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { code }
    }
}

/// Monospace families via DirectWrite's `IsMonospacedFont` (§14.3), sorted.
pub fn monospace_fonts() -> Vec<String> {
    use windows::core::Interface;
    use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory, IDWriteFont1, DWRITE_FACTORY_TYPE_SHARED};
    let mut out = Vec::new();
    unsafe {
        let Ok(f) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return out };
        let mut coll = None;
        if f.GetSystemFontCollection(&mut coll, false).is_err() {
            return out;
        }
        let Some(coll) = coll else { return out };
        for i in 0..coll.GetFontFamilyCount() {
            let Ok(fam) = coll.GetFontFamily(i) else { continue };
            let Ok(font) = fam.GetFont(0) else { continue };
            let Ok(f1) = font.cast::<IDWriteFont1>() else { continue };
            if !f1.IsMonospacedFont().as_bool() {
                continue;
            }
            let Ok(names) = fam.GetFamilyNames() else { continue };
            let (mut idx, mut exists) = (0u32, windows::core::BOOL(0));
            let _ = names.FindLocaleName(w!("en-us"), &mut idx, &mut exists);
            if !exists.as_bool() {
                idx = 0;
            }
            let Ok(len) = names.GetStringLength(idx) else { continue };
            let mut buf = vec![0u16; len as usize + 1];
            if names.GetString(idx, &mut buf).is_ok() {
                out.push(String::from_utf16_lossy(&buf[..len as usize]));
            }
        }
    }
    out.sort_by_key(|s| s.to_lowercase());
    out.dedup();
    out
}

#[derive(serde::Deserialize)]
pub struct FileFilter {
    name: String,
    extensions: Vec<String>,
}

/// Native "open file" dialog for the settings / session editors (shell path, background image).
#[tauri::command]
pub async fn pick_file(app: AppHandle, title: Option<String>, filters: Option<Vec<FileFilter>>) -> Result<Option<String>, String> {
    let r = tauri::async_runtime::spawn_blocking(move || {
        let mut d = app.dialog().file();
        if let Some(t) = title {
            d = d.set_title(t);
        }
        for f in filters.unwrap_or_default() {
            let exts: Vec<&str> = f.extensions.iter().map(String::as_str).collect();
            d = d.add_filter(f.name, &exts);
        }
        d.blocking_pick_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(r.and_then(|p| p.into_path().ok()).map(|p| p.to_string_lossy().into_owned()))
}

/// Native folder picker (working directory).
#[tauri::command]
pub async fn pick_folder(app: AppHandle, title: Option<String>) -> Result<Option<String>, String> {
    let r = tauri::async_runtime::spawn_blocking(move || {
        let mut d = app.dialog().file();
        if let Some(t) = title {
            d = d.set_title(t);
        }
        d.blocking_pick_folder()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(r.and_then(|p| p.into_path().ok()).map(|p| p.to_string_lossy().into_owned()))
}
