//! App assembly: plugins, main window (restored bounds), tray, hotkey, command registration.

use crate::state::{AppState, StartArgs};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};
use ut_core::{clamp_bounds, Rect};

/// Emit to the main webview only — never to the browser-panel webview.
pub fn emit<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit_to("main", event, payload);
}

pub fn main_hwnd(app: &AppHandle) -> Option<isize> {
    app.get_webview_window("main").and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize)
}

/// The UI finished loading: reveal the window (no white flash), install the native hooks.
#[tauri::command]
fn app_ready(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let state = app.state::<AppState>();
        if *state.maximized.lock() && !w.is_maximized().unwrap_or(false) {
            let _ = w.maximize();
        }
        let _ = w.show();
        let _ = w.set_focus();
    }
    // WebView2's child windows exist once the page has loaded: subclass them for the ShareX scroll bridge.
    crate::win::install_scroll_bridge(&app);
    let state = app.state::<AppState>();
    crate::win::apply_settings_delta(&app, &state, &serde_json::json!({ "quake": {}, "ui": {} }));
}

/// Monitor work areas in physical pixels, for restoring the window onto a connected screen (§16.3).
fn work_areas(app: &AppHandle) -> (Vec<Rect>, usize) {
    let mons = app.available_monitors().unwrap_or_default();
    let primary = app.primary_monitor().ok().flatten();
    let rects: Vec<Rect> = mons
        .iter()
        .map(|m| {
            let wa = m.work_area();
            Rect { x: wa.position.x, y: wa.position.y, width: wa.size.width as i32, height: wa.size.height as i32 }
        })
        .collect();
    let idx = primary
        .and_then(|p| mons.iter().position(|m| m.position() == p.position()))
        .unwrap_or(0);
    (rects, idx)
}

fn create_main_window(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<AppState>();
    let (ws, _) = state.load_window_state();
    *state.maximized.lock() = ws.maximized;

    // Another process (e.g. the previous WPF version) may hold the default profile folder with different
    // environment options, which makes WebView2 fail with "resource not in the correct state": fall back to a
    // second profile folder instead of failing to start.
    let build = |dir: &str| {
        WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("Useless Terminal")
            .inner_size(1200.0, 800.0)
            .min_inner_size(480.0, 360.0)
            .decorations(false)
            .shadow(true)
            .visible(false)
            .center()
            .background_color(tauri::window::Color(11, 15, 20, 255))
            .data_directory(ut_fs::local_data_dir().join(dir))
            .build()
    };
    let w = build("WebView2").or_else(|e| {
        tracing::warn!("WebView2 profile folder unavailable ({e}); using WebView2-alt");
        build("WebView2-alt")
    })?;

    // Restore bounds clamped to the connected monitors (never an off-screen window).
    if let Some(saved) = ws.bounds {
        let (monitors, primary) = work_areas(app);
        if !monitors.is_empty() {
            let r = clamp_bounds(saved, &monitors, primary);
            let _ = w.set_position(PhysicalPosition::new(r.x, r.y));
            let _ = w.set_size(PhysicalSize::new(r.width.max(1) as u32, r.height.max(1) as u32));
            *state.normal_bounds.lock() = Some(r);
        }
    }
    // Never leave a hidden window if the UI fails to report ready.
    let w2 = w.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(6));
        let _ = w2.show();
    });
    Ok(())
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("UT_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    // Opt-in developer aid: `UT_DEBUG_PORT=9222` exposes the main webview over the DevTools protocol.
    if let Ok(port) = std::env::var("UT_DEBUG_PORT") {
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", format!("--remote-debugging-port={port}"));
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            // A second launch forwards its arguments to the running instance, which opens a new tab (§16.2).
            let parsed = StartArgs::parse(&args);
            crate::app::emit(app, "app:second-instance", serde_json::json!({ "args": parsed, "cwd": cwd }));
            crate::win::show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(AppState::new())
        .register_asynchronous_uri_scheme_protocol("utasset", crate::assets::asset_protocol)
        .register_asynchronous_uri_scheme_protocol("uticon", crate::assets::icon_protocol)
        .on_window_event(crate::win::on_window_event)
        .setup(|app| {
            let h = app.handle().clone();
            *h.state::<AppState>().args.lock() = StartArgs::parse(&std::env::args().collect::<Vec<_>>());
            create_main_window(&h)?;
            crate::win::setup_tray(&h)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_ready,
            crate::pane::pty_spawn,
            crate::pane::pty_write,
            crate::pane::pty_resize,
            crate::pane::pty_ack,
            crate::pane::pty_close,
            crate::pane::pane_info,
            crate::pane::log_toggle,
            crate::pane::record_toggle,
            crate::clipboard::clipboard_read,
            crate::clipboard::clipboard_write,
            crate::cmd_settings::settings_get,
            crate::cmd_settings::settings_patch,
            crate::cmd_settings::settings_reset_with_backup,
            crate::cmd_settings::settings_open_file,
            crate::cmd_settings::theme_preview,
            crate::cmd_settings::fonts_monospace,
            crate::cmd_settings::keybindings_get,
            crate::cmd_settings::keybindings_set,
            crate::cmd_settings::keybindings_reset,
            crate::cmd_settings::keybindings_keymap,
            crate::cmd_data::sessions_snapshot,
            crate::cmd_data::sessions_search,
            crate::cmd_data::sessions_clear_banner,
            crate::cmd_data::session_add,
            crate::cmd_data::session_update,
            crate::cmd_data::session_delete,
            crate::cmd_data::session_duplicate,
            crate::cmd_data::folder_add,
            crate::cmd_data::folder_rename,
            crate::cmd_data::folder_delete,
            crate::cmd_data::folder_move,
            crate::cmd_data::items_move,
            crate::cmd_data::snippet_add,
            crate::cmd_data::snippet_update,
            crate::cmd_data::snippet_delete,
            crate::cmd_data::sessions_export,
            crate::cmd_data::sessions_import,
            crate::cmd_data::import_wt,
            crate::cmd_data::import_ssh_config,
            crate::cmd_data::workspaces_list,
            crate::cmd_data::workspace_save,
            crate::cmd_data::workspace_rename,
            crate::cmd_data::workspace_delete,
            crate::cmd_data::shells_detect,
            crate::cmd_data::window_state_load,
            crate::cmd_data::window_state_save,
            crate::sys::open_external,
            crate::sys::open_path,
            crate::sys::export_buffer,
            crate::sys::notify_attention,
            crate::sys::run_elevated,
            crate::sys::pane_busy,
            crate::sys::app_quit,
            crate::sys::app_info,
            crate::sys::diagnostics,
            crate::sys::key_label,
            crate::sys::pick_file,
            crate::sys::pick_folder,
            crate::win::quake_set_hotkey,
            crate::drops::pane_ssh,
            crate::drops::quick_connect,
            crate::drops::ssh_history,
            crate::drops::quote_paths,
            crate::drops::files_drop,
            crate::drops::drop_resolve,
            crate::drops::drop_cancel,
            crate::assets::icon_for,
            crate::assets::background_url,
            crate::browser::browser_toggle,
            crate::browser::browser_set_bounds,
            crate::browser::browser_navigate,
            crate::browser::browser_nav,
            crate::browser::browser_hide,
            crate::browser::browser_paste,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Useless Terminal");

    app.run(|handle, ev| {
        if let tauri::RunEvent::Exit = ev {
            let s = handle.state::<AppState>();
            s.closing.store(true, std::sync::atomic::Ordering::Release);
            s.flush_all(); // every pending write reaches disk on exit (§16.2)
        }
    });
}
