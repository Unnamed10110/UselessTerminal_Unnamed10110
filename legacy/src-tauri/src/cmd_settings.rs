//! Settings, theme and keybinding commands.

use crate::app::emit;
use crate::state::AppState;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use tauri::{AppHandle, State};
use ut_core::{effective_with, ui_css_vars, EffectiveTheme, LoadStatus, Settings, ThemeRef};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    pub settings: Settings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<EffectiveTheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub css_vars: Option<Vec<(String, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presets: Option<Vec<String>>,
    pub load_error: Option<String>,
}

pub fn preset_list(state: &AppState) -> Vec<String> {
    let mut v: Vec<String> = ut_core::preset_names().into_iter().map(String::from).collect();
    v.extend(state.user_themes.read().iter().map(|p| p.name.clone()));
    v
}

pub fn theme_for(state: &AppState, s: &Settings) -> (EffectiveTheme, Vec<(String, String)>) {
    let t = effective_with(&s.theme, &state.user_themes.read());
    let vars = ui_css_vars(&t, &s.ui);
    (t, vars)
}

pub fn load_error(state: &AppState) -> Option<String> {
    match state.settings.status() {
        LoadStatus::Corrupt { error } => Some(error),
        _ => None,
    }
}

/// `full` = include the theme/css vars/preset list (always on first load; deltas only when they changed).
pub fn bundle(state: &AppState, full: bool) -> Bundle {
    let settings = state.cfg();
    let (theme, css_vars, presets) = if full {
        let (t, v) = theme_for(state, &settings);
        (Some(t), Some(v), Some(preset_list(state)))
    } else {
        (None, None, None)
    };
    Bundle { settings, theme, css_vars, presets, load_error: load_error(state) }
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> Result<Bundle, String> {
    Ok(bundle(&state, true))
}

#[tauri::command]
pub async fn settings_patch(app: AppHandle, state: State<'_, AppState>, patch: Value) -> Result<Bundle, String> {
    let (_, delta) = state.settings.patch(patch)?;
    let visual = delta.get("theme").is_some() || delta.get("ui").is_some();
    crate::win::apply_settings_delta(&app, &state, &delta);
    Ok(bundle(&state, visual))
}

#[tauri::command]
pub async fn settings_reset_with_backup(app: AppHandle, state: State<'_, AppState>) -> Result<Bundle, String> {
    state.settings.reset_with_backup()?;
    crate::win::apply_settings_delta(&app, &state, &serde_json::json!({ "quake": {}, "ui": {} }));
    Ok(bundle(&state, true))
}

#[tauri::command]
pub async fn settings_open_file(state: State<'_, AppState>) -> Result<(), String> {
    crate::sys::shell_open(&state.settings.path().to_string_lossy())
}

/// Effective theme for a preset without saving it (live preview on hover, §14.3 / §7.10).
#[tauri::command]
pub async fn theme_preview(state: State<'_, AppState>, preset: String, overrides: Option<Value>) -> Result<Value, String> {
    let mut s = state.cfg();
    s.theme = ThemeRef { preset, overrides: overrides.and_then(|o| serde_json::from_value(o).ok()).unwrap_or_default() };
    let (theme, vars) = theme_for(&state, &s);
    Ok(serde_json::json!({ "theme": theme, "cssVars": vars }))
}

#[tauri::command]
pub async fn fonts_monospace() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(crate::sys::monospace_fonts).await.unwrap_or_default()
}

// ---------------------------------------------------------------- keybindings

fn effective(state: &AppState) -> BTreeMap<String, Vec<String>> {
    state.keys.lock().effective()
}

fn persist_keys(app: &AppHandle, state: &AppState) -> Result<BTreeMap<String, Vec<String>>, String> {
    state.keys.lock().save(&state.keys_path).map_err(|e| format!("cannot save keybindings: {e}"))?;
    let eff = effective(state);
    emit(app, "keybindings:changed", &eff);
    Ok(eff)
}

#[tauri::command]
pub async fn keybindings_get(state: State<'_, AppState>) -> Result<BTreeMap<String, Vec<String>>, String> {
    Ok(effective(&state))
}

/// `chords = null` unbinds the action (the key then passes through to the shell).
#[tauri::command]
pub async fn keybindings_set(
    app: AppHandle,
    state: State<'_, AppState>,
    action: String,
    chords: Option<Vec<String>>,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    {
        let mut k = state.keys.lock();
        match chords {
            Some(c) => k.set_binding(&action, &c)?,
            None => k.unbind(&action)?,
        };
    }
    persist_keys(&app, &state)
}

#[tauri::command]
pub async fn keybindings_reset(
    app: AppHandle,
    state: State<'_, AppState>,
    action: Option<String>,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    state.keys.lock().reset(action.as_deref());
    persist_keys(&app, &state)
}

/// Apply a built-in keymap. `default` resets everything, `shellSafe` = Windows-Terminal-style (§15.2).
#[tauri::command]
pub async fn keybindings_keymap(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    {
        let mut k = state.keys.lock();
        k.reset(None);
        if name == "shellSafe" {
            k.apply_shell_safe();
        }
    }
    persist_keys(&app, &state)
}
