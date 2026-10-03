//! Browser panel (§17): a SEPARATE child webview with its own data folder and NO IPC capability — the
//! "browser" label is deliberately absent from `capabilities/main.json`, so remote pages cannot call a
//! single command or listen to a single event. Created lazily on first open.

use crate::app::emit;
use crate::state::AppState;
use serde::Deserialize;
use tauri::webview::WebviewBuilder;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, State, Url, WebviewUrl};

const LABEL: &str = "browser";
const DEFAULT_URL: &str = "https://chatgpt.com";

#[derive(Deserialize, Default)]
struct BrowserState {
    #[serde(rename = "lastUrl")]
    last_url: Option<String>,
}

fn state_path() -> std::path::PathBuf {
    ut_fs::app_data_dir().join("browser.json")
}

fn last_url() -> String {
    match ut_fs::read_json::<BrowserState>(&state_path()) {
        ut_fs::ReadJson::Ok(s) => s.last_url.filter(|u| u.starts_with("http")).unwrap_or_else(|| DEFAULT_URL.into()),
        _ => DEFAULT_URL.into(),
    }
}

fn remember(app: &AppHandle, url: &str) {
    if url.starts_with("http") {
        app.state::<AppState>().writer.queue_json(
            state_path(),
            std::time::Duration::from_millis(800),
            &serde_json::json!({ "lastUrl": url }),
        );
    }
}

fn ensure(app: &AppHandle, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    if app.get_webview(LABEL).is_some() {
        return Ok(());
    }
    let main = app.get_webview_window("main").ok_or("main window missing")?;
    let url: Url = last_url().parse().map_err(|e: url::ParseError| e.to_string())?;
    let make = |dir: &str| {
        let (a1, a2) = (app.clone(), app.clone());
        WebviewBuilder::new(LABEL, WebviewUrl::External(url.clone()))
        .data_directory(ut_fs::local_data_dir().join(dir)) // separate profile: logins persist here only
        .on_navigation(move |u| {
            emit(&a1, "browser:navigated", serde_json::json!({ "url": u.as_str() }));
            remember(&a1, u.as_str());
            true
        })
        // window.open / target=_blank navigate in the same view.
        .on_new_window(move |u, _| {
            if let Some(b) = a2.get_webview(LABEL) {
                let _ = b.navigate(u);
            }
            tauri::webview::NewWindowResponse::Deny
        })
    };
    let win = main.as_ref().window();
    let (pos, size) = (LogicalPosition::new(x, y), LogicalSize::new(w.max(1.0), h.max(1.0)));
    // Same fallback as the main window when another process holds the default profile folder.
    win.add_child(make("WebView2Browser"), pos, size)
        .or_else(|_| win.add_child(make("WebView2Browser-alt"), pos, size))
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn browser_toggle(app: AppHandle, open: bool) -> Result<(), String> {
    if open {
        if let Some(b) = app.get_webview(LABEL) {
            let _ = b.show();
        } // first creation happens with the real bounds in `browser_set_bounds`
    } else if let Some(b) = app.get_webview(LABEL) {
        let _ = b.hide();
    }
    Ok(())
}

/// Bounds in logical pixels, from a ResizeObserver on the panel element.
#[tauri::command]
pub async fn browser_set_bounds(app: AppHandle, x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
    ensure(&app, x, y, width, height)?;
    if let Some(b) = app.get_webview(LABEL) {
        let _ = b.set_position(LogicalPosition::new(x, y));
        let _ = b.set_size(LogicalSize::new(width.max(1.0), height.max(1.0)));
    }
    Ok(())
}

#[tauri::command]
pub async fn browser_navigate(app: AppHandle, url: String) -> Result<(), String> {
    let u: Url = url.parse().map_err(|e: url::ParseError| e.to_string())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("Only http(s) addresses can be opened in the browser panel.".into());
    }
    app.get_webview(LABEL).ok_or("browser panel is not open")?.navigate(u).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn browser_nav(app: AppHandle, action: String) -> Result<(), String> {
    let js = match action.as_str() {
        "back" => "history.back()",
        "forward" => "history.forward()",
        "reload" => "location.reload()",
        _ => return Err("unknown action".into()),
    };
    app.get_webview(LABEL).ok_or("browser panel is not open")?.eval(js).map_err(|e| e.to_string())
}

/// Airspace (§17.1): the child webview paints above the main one, so it is hidden while a modal is open.
#[tauri::command]
pub async fn browser_hide(app: AppHandle, hidden: bool) -> Result<(), String> {
    if let Some(b) = app.get_webview(LABEL) {
        if hidden { let _ = b.hide(); } else { let _ = b.show(); }
    }
    Ok(())
}

/// "Send selection to browser" (§17.2): clipboard only — put the text on the clipboard, focus the page and
/// press Ctrl+V. No script is injected into the remote page.
#[tauri::command]
pub async fn browser_paste(app: AppHandle, text: String) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL,
    };
    crate::clipboard::set_text(&text);
    let b = app.get_webview(LABEL).ok_or("browser panel is not open")?;
    b.set_focus().map_err(|e| e.to_string())?;
    std::thread::sleep(std::time::Duration::from_millis(120));
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, ..Default::default() },
        },
    };
    let v = VIRTUAL_KEY(0x56);
    let seq = [key(VK_CONTROL, false), key(v, false), key(v, true), key(VK_CONTROL, true)];
    unsafe { SendInput(&seq, size_of::<INPUT>() as i32) };
    Ok(())
}

#[allow(dead_code)]
fn _unused(_: State<'_, AppState>) {}
