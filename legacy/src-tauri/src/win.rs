//! Window-level features: tray, Quake hotkey, ShareX scroll bridge (WM_VSCROLL), backdrop, window events.

use crate::app::{emit, main_hwnd};
use crate::state::AppState;
use serde_json::Value;
use std::sync::atomic::Ordering;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewWindow, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use ut_core::Rect;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, WM_VSCROLL};

// -------------------------------------------------------------------- window

/// Show + restore + focus (Quake "show" path and tray / second instance).
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Quake toggle (§7.8): visible AND active → hide; otherwise show, restore, activate.
pub fn toggle_main(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    let visible = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
    if visible && w.is_focused().unwrap_or(false) {
        app.state::<AppState>().flush_all(); // hiding to the tray: persist everything
        let _ = w.hide();
    } else {
        show_main(app);
        emit(app, "app:quake-toggle", Value::Null);
    }
}

/// Track Normal-state bounds only (§16.3: a hidden/minimised/maximised window reports junk or the wrong rect).
fn track_bounds(w: &WebviewWindow, state: &AppState) {
    let (min, max, vis) = (w.is_minimized().unwrap_or(true), w.is_maximized().unwrap_or(false), w.is_visible().unwrap_or(false));
    *state.maximized.lock() = max;
    if min || max || !vis {
        return;
    }
    if let (Ok(p), Ok(s)) = (w.outer_position(), w.outer_size()) {
        if s.width > 0 && s.height > 0 {
            *state.normal_bounds.lock() = Some(Rect { x: p.x, y: p.y, width: s.width as i32, height: s.height as i32 });
        }
    }
}

pub fn on_window_event(w: &tauri::Window, ev: &WindowEvent) {
    let app = w.app_handle();
    let state = app.state::<AppState>();
    match ev {
        WindowEvent::CloseRequested { api, .. } if w.label() == "main" => {
            // The UI asks for confirmation when something is running, then calls `app_quit`.
            api.prevent_close();
            if state.panes.read().is_empty() {
                state.closing.store(true, Ordering::Release);
                state.flush_all();
                app.exit(0);
            } else {
                emit(app, "app:close-requested", Value::Null);
                show_main(app);
            }
        }
        WindowEvent::Moved(_) | WindowEvent::Resized(_) if w.label() == "main" => {
            if let Some(ww) = app.get_webview_window("main") {
                track_bounds(&ww, &state);
            }
        }
        WindowEvent::Focused(f) if w.label() == "main" => {
            emit(app, "app:focus", serde_json::json!({ "focused": f }));
        }
        WindowEvent::DragDrop(d) if w.label() == "main" => crate::drops::on_drag_drop(app, d),
        _ => {}
    }
}

// ---------------------------------------------------------------------- tray

pub fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "tray-toggle", "Show / Hide", true, None::<&str>)?,
            &MenuItem::with_id(app, "tray-new", "New Tab", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "tray-settings", "Settings", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "tray-exit", "Exit", true, None::<&str>)?,
        ],
    )?;
    let mut b = TrayIconBuilder::with_id("main")
        .tooltip("Useless Terminal")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "tray-toggle" => toggle_main(app),
            "tray-new" => {
                show_main(app);
                emit(app, "app:action", serde_json::json!({ "action": "newTab" }));
            }
            "tray-settings" => {
                show_main(app);
                emit(app, "app:action", serde_json::json!({ "action": "settings" }));
            }
            "tray-exit" => {
                // Exit goes through the same confirmation as closing the window.
                show_main(app);
                emit(app, "app:close-requested", Value::Null);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
            | TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e
            {
                // Double-click shows and focuses; a single click is handled by the Click arm only on
                // platforms that do not deliver DoubleClick.
                if matches!(e, TrayIconEvent::DoubleClick { .. }) {
                    show_main(tray.app_handle());
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    Ok(())
}

// -------------------------------------------------------------------- hotkey

/// `Win+Backquote` → plugin accelerator syntax (`Super+Backquote`).
fn accelerator(hotkey: &str) -> String {
    hotkey
        .split('+')
        .map(|p| match p.trim().to_ascii_lowercase().as_str() {
            "win" | "windows" | "meta" => "Super".to_string(),
            "ctrl" | "control" => "Control".to_string(),
            other if other.is_empty() => String::new(),
            _ => p.trim().to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// (Re)register the global Quake hotkey. On failure the caller shows a non-blocking warning.
pub fn register_quake(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let sc: Shortcut = accelerator(hotkey).parse().map_err(|e| format!("Invalid hotkey '{hotkey}': {e}"))?;
    gs.on_shortcut(sc, |app, _, ev| {
        if ev.state == ShortcutState::Pressed {
            toggle_main(app);
        }
    })
    .map_err(|e| format!("Could not register {hotkey}: {e}"))
}

#[tauri::command]
pub async fn quake_set_hotkey(app: AppHandle, state: tauri::State<'_, AppState>, hotkey: String) -> Result<String, String> {
    register_quake(&app, &hotkey)?;
    state.settings.patch(serde_json::json!({ "quake": { "hotkey": hotkey } }))?;
    Ok(hotkey)
}

/// React to the parts of a settings change the backend owns (hotkey, backdrop).
pub fn apply_settings_delta(app: &AppHandle, state: &AppState, delta: &Value) {
    if delta.get("quake").is_some_and(|q| q.get("hotkey").is_some() || q.is_object()) {
        let hk = state.cfg().quake.hotkey;
        if let Err(e) = register_quake(app, &hk) {
            emit(app, "app:banner", serde_json::json!({ "kind": "warning", "text": e }));
        }
    }
    if delta.get("ui").is_some_and(|u| u.get("backdrop").is_some() || u.is_object()) {
        apply_backdrop(app, state);
    }
}

/// `ui.backdrop` (§7.1): Mica needs Windows 11 (build ≥ 22000), otherwise it falls back to none.
pub fn apply_backdrop(app: &AppHandle, state: &AppState) {
    use tauri::utils::config::WindowEffectsConfig;
    use tauri::window::{Effect, EffectsBuilder};
    use ut_core::settings::Backdrop;
    let Some(w) = app.get_webview_window("main") else { return };
    let build = crate::sys::os_build();
    let eff: Option<WindowEffectsConfig> = match state.cfg().ui.backdrop {
        Backdrop::Mica if build >= 22000 => Some(EffectsBuilder::new().effect(Effect::Mica).build()),
        Backdrop::Acrylic => Some(EffectsBuilder::new().effect(Effect::Acrylic).build()),
        _ => None,
    };
    let _ = w.set_effects(eff);
}

// ----------------------------------------------------- ShareX scroll bridge

const SB_LINEUP: usize = 0;
const SB_LINEDOWN: usize = 1;
const SB_PAGEUP: usize = 2;
const SB_PAGEDOWN: usize = 3;
const SB_TOP: usize = 6;
const SB_BOTTOM: usize = 7;

unsafe extern "system" fn scroll_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, data: usize) -> LRESULT {
    if msg == WM_VSCROLL {
        let action = match wp.0 & 0xFFFF {
            SB_LINEUP => Some("lineUp"),
            SB_LINEDOWN => Some("lineDown"),
            SB_PAGEUP => Some("pageUp"),
            SB_PAGEDOWN => Some("pageDown"),
            SB_TOP => Some("top"),
            SB_BOTTOM => Some("bottom"),
            _ => None,
        };
        if let Some(a) = action {
            // `data` is a leaked Box<AppHandle>.
            let app = &*(data as *const AppHandle);
            emit(app, "app:scroll", serde_json::json!({ "action": a }));
            return LRESULT(0);
        }
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

unsafe extern "system" fn subclass_child(hwnd: HWND, lp: LPARAM) -> BOOL {
    let _ = SetWindowSubclass(hwnd, Some(scroll_proc), 0xA11CE, lp.0 as usize);
    BOOL(1)
}

/// Subclass the top-level window AND the WebView2 host child windows so ShareX's "Windows message"
/// scrolling capture (WM_VSCROLL) scrolls the focused terminal pane (§7.11).
pub fn install_scroll_bridge(app: &AppHandle) {
    let Some(h) = main_hwnd(app) else { return };
    let data = Box::into_raw(Box::new(app.clone())) as usize;
    unsafe {
        let top = HWND(h as *mut _);
        let _ = SetWindowSubclass(top, Some(scroll_proc), 0xA11CE, data);
        let _ = EnumChildWindows(Some(top), Some(subclass_child), LPARAM(data as isize));
    }
}
