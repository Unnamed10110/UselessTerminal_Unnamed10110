//! Settings (§14): schema with defaults, ONE `sanitize()`, merge-patch deltas, and the store
//! with its debounced atomic save, corrupt-file protection and legacy migration.

use crate::color::normalize_hex;
use crate::keybindings::Chord;
use crate::theme::ThemeRef;
use crate::{diff, lenient, merge_patch, to_json, LoadStatus};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use ut_fs::{DebouncedWriter, ReadJson};

pub const SCHEMA_VERSION: u32 = 3;
/// §14.1: writes are debounced by 500 ms.
pub const SAVE_DELAY: Duration = Duration::from_millis(500);

/// String enums: unknown strings fail to deserialize, which `lenient` turns into the default.
macro_rules! str_enum {
    ($($name:ident { $($(#[$m:meta])* $v:ident),+ $(,)? })+) => {$(
        #[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
        #[serde(rename_all = "camelCase")]
        pub enum $name { $($(#[$m])* $v),+ }
    )+};
}

str_enum! {
    CursorStyle { #[default] Bar, Block, Underline }
    Renderer { #[default] Webgl, Dom }
    RendererRepair { #[default] Auto, On, Off }
    RightClick { #[default] Menu, Paste, CopyPaste }
    MultiLinePasteWarning { #[default] Auto, Always, Never }
    PasteImages { #[default] PassThroughCtrlV, InlinePreview }
    Osc52 { #[default] Write, #[serde(rename = "readwrite")] ReadWrite, Off }
    ZoomScope { #[default] Global, Pane }
    CloseOnExit { #[default] Never, Graceful, Always }
    ConptyImplementation { #[default] Auto, Bundled, System }
    Backdrop { #[default] #[serde(rename = "none")] Off, Mica, Acrylic }
    StartupMode { #[default] RestoreLastSession, Workspace, DefaultTab }
    CloseConfirm { #[default] WhenRunning, Always, Never }
    ConnectionReuse { #[default] Auto, Always, Never }
    DropAction { #[default] Copy, Paste }
    LogFormat { #[default] Plain, Raw }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Bell {
    pub visual: bool,
    pub audible: bool,
    pub flash_taskbar: bool,
}
impl Default for Bell {
    fn default() -> Self {
        Self { visual: true, audible: false, flash_taskbar: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct BackgroundImage {
    pub path: String,
    pub opacity: f64,
}
impl Default for BackgroundImage {
    fn default() -> Self {
        Self { path: String::new(), opacity: 0.52 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TerminalSettings {
    pub font_family: String,
    pub font_size: f64,
    pub font_weight: u32,
    pub line_height: f64,
    pub letter_spacing: f64,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    pub scrollback: u32,
    pub renderer: Renderer,
    pub renderer_repair: RendererRepair,
    /// Personal setting, never part of a theme (§13.2).
    pub typed_input_color: String,
    pub override_ps_read_line_colors: bool,
    pub copy_on_select: bool,
    pub clear_selection_on_copy: bool,
    pub trim_trailing_whitespace_on_copy: bool,
    pub right_click: RightClick,
    pub multi_line_paste_warning: MultiLinePasteWarning,
    pub paste_images: PasteImages,
    pub osc52: Osc52,
    pub zoom_scope: ZoomScope,
    pub close_on_exit: CloseOnExit,
    pub refresh_environment: bool,
    pub conpty_implementation: ConptyImplementation,
    pub bell: Bell,
    pub background_image: BackgroundImage,
    pub crt: bool,
    pub minimap: bool,
}
impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: "'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace".into(),
            font_size: 14.0,
            font_weight: 400,
            line_height: 1.0,
            letter_spacing: 0.0,
            cursor_style: CursorStyle::Bar,
            cursor_blink: true,
            scrollback: 10_000,
            renderer: Renderer::Webgl,
            renderer_repair: RendererRepair::Auto,
            typed_input_color: "#ffffff".into(),
            override_ps_read_line_colors: true,
            copy_on_select: false,
            clear_selection_on_copy: true,
            trim_trailing_whitespace_on_copy: true,
            right_click: RightClick::Menu,
            multi_line_paste_warning: MultiLinePasteWarning::Auto,
            paste_images: PasteImages::PassThroughCtrlV,
            osc52: Osc52::Write,
            zoom_scope: ZoomScope::Global,
            close_on_exit: CloseOnExit::Never,
            refresh_environment: true,
            conpty_implementation: ConptyImplementation::Auto,
            bell: Bell::default(),
            background_image: BackgroundImage::default(),
            crt: false,
            minimap: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct UiSettings {
    pub font_family: String,
    pub font_size: f64,
    pub font_weight: u32,
    pub scale: f64,
    pub backdrop: Backdrop,
}
impl Default for UiSettings {
    fn default() -> Self {
        Self { font_family: "Segoe UI".into(), font_size: 13.0, font_weight: 400, scale: 1.0, backdrop: Backdrop::Off }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ShellsSettings {
    pub default_profile: String,
}
impl Default for ShellsSettings {
    fn default() -> Self {
        Self { default_profile: "auto".into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct StartupSettings {
    pub mode: StartupMode,
    pub workspace_id: Option<String>,
    /// §16.3 [P1]: start only the active restored tab immediately.
    pub lazy_restore: bool,
}
impl Default for StartupSettings {
    fn default() -> Self {
        Self { mode: StartupMode::RestoreLastSession, workspace_id: None, lazy_restore: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ProcessesSettings {
    pub close_confirm: CloseConfirm,
    pub kill_console_tree_on_close: bool,
}
impl Default for ProcessesSettings {
    fn default() -> Self {
        Self { close_confirm: CloseConfirm::WhenRunning, kill_console_tree_on_close: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CommandFinished {
    pub enabled: bool,
    pub min_duration_sec: u32,
    pub toast: bool,
}
impl Default for CommandFinished {
    fn default() -> Self {
        Self { enabled: true, min_duration_sec: 10, toast: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NotificationsSettings {
    pub command_finished: CommandFinished,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SshSettings {
    pub connection_reuse: ConnectionReuse,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DropSettings {
    pub default_action: DropAction,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LinksSettings {
    pub editor_command: String,
}
impl Default for LinksSettings {
    fn default() -> Self {
        Self { editor_command: "code --goto \"{file}:{line}\"".into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LoggingSettings {
    pub format: LogFormat,
    pub directory: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingSettings {
    pub capture_input: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct QuakeSettings {
    pub hotkey: String,
    pub dropdown: bool,
    pub height_percent: u32,
    pub hide_on_blur: bool,
}
impl Default for QuakeSettings {
    fn default() -> Self {
        Self { hotkey: "Win+Backquote".into(), dropdown: false, height_percent: 50, hide_on_blur: false }
    }
}

/// The full §14.2 schema. Partial or old files load: every level has `#[serde(default)]`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub schema_version: u32,
    pub terminal: TerminalSettings,
    pub theme: ThemeRef,
    pub ui: UiSettings,
    pub shells: ShellsSettings,
    pub startup: StartupSettings,
    pub processes: ProcessesSettings,
    pub notifications: NotificationsSettings,
    pub ssh: SshSettings,
    pub drop: DropSettings,
    pub links: LinksSettings,
    pub logging: LoggingSettings,
    pub recording: RecordingSettings,
    pub quake: QuakeSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            terminal: Default::default(),
            theme: Default::default(),
            ui: Default::default(),
            shells: Default::default(),
            startup: Default::default(),
            processes: Default::default(),
            notifications: Default::default(),
            ssh: Default::default(),
            drop: Default::default(),
            links: Default::default(),
            logging: Default::default(),
            recording: Default::default(),
            quake: Default::default(),
        }
    }
}

fn text(s: &mut String, default: &str) {
    *s = s.trim().to_string();
    if s.is_empty() {
        *s = default.to_string();
    }
}

fn clamp_f(v: f64, lo: f64, hi: f64, default: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { default }
}

impl Settings {
    /// Deserialize leniently (bad leaves fall back to defaults), then [`Settings::sanitize`].
    pub fn from_value(v: &Value) -> Settings {
        let mut s: Settings = lenient(v);
        s.sanitize();
        s
    }

    /// The ONE place that clamps and validates (§14.1). Idempotent.
    pub fn sanitize(&mut self) {
        let d = Settings::default();
        self.schema_version = SCHEMA_VERSION;

        let t = &mut self.terminal;
        text(&mut t.font_family, &d.terminal.font_family);
        t.font_size = clamp_f(t.font_size, 8.0, 32.0, 14.0);
        t.font_weight = t.font_weight.clamp(100, 900);
        t.line_height = clamp_f(t.line_height, 0.8, 2.0, 1.0);
        t.letter_spacing = clamp_f(t.letter_spacing, -5.0, 20.0, 0.0); // range is ours; the spec gives none
        t.scrollback = t.scrollback.min(200_000);
        t.typed_input_color = normalize_hex(&t.typed_input_color).unwrap_or_else(|| d.terminal.typed_input_color.clone());
        t.background_image.path = t.background_image.path.trim().to_string();
        t.background_image.opacity = clamp_f(t.background_image.opacity, 0.0, 1.0, 0.52);

        self.theme.sanitize();

        let u = &mut self.ui;
        text(&mut u.font_family, &d.ui.font_family);
        u.font_size = clamp_f(u.font_size, 10.0, 22.0, 13.0);
        u.font_weight = u.font_weight.clamp(300, 700);
        u.scale = (clamp_f(u.scale, 0.75, 2.0, 1.0) * 20.0).round() / 20.0; // snap to 0.05

        text(&mut self.shells.default_profile, "auto");
        self.startup.workspace_id = self.startup.workspace_id.take().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let cf = &mut self.notifications.command_finished;
        cf.min_duration_sec = cf.min_duration_sec.min(86_400);
        text(&mut self.links.editor_command, &d.links.editor_command);
        self.logging.directory = self.logging.directory.trim().to_string();

        let q = &mut self.quake;
        q.hotkey = q.hotkey.parse::<Chord>().map_or_else(|_| d.quake.hotkey.clone(), |c| c.to_string());
        q.height_percent = q.height_percent.clamp(10, 100);
    }
}

/// Deep-merge `patch` (RFC 7386: patches are deltas, `null` resets a key to its default) onto the
/// current settings, then deserialize + sanitize. Errs only when the patch is not a JSON object.
pub fn apply_patch(cur: &Settings, patch: Value) -> Result<Settings, String> {
    if !patch.is_object() {
        return Err("settings patch must be a JSON object".into());
    }
    let mut doc = to_json(cur);
    merge_patch(&mut doc, &patch);
    Ok(Settings::from_value(&doc))
}

// --------------------------------------------------------------------------------- store

struct State {
    cur: Settings,
    status: LoadStatus,
}

/// Thread-safe settings holder. Share it as `Arc<SettingsStore>`.
pub struct SettingsStore {
    path: PathBuf,
    state: Mutex<State>,
    writer: Arc<DebouncedWriter>,
    save_error: Arc<Mutex<Option<String>>>,
}

/// Read `settings.json`: missing -> defaults; corrupt -> defaults in memory, file untouched
/// (§14.1); legacy WPF file -> backup as `*.legacy-bak.json`, converted and rewritten as v3.
fn read_settings(path: &Path) -> (Settings, LoadStatus) {
    let corrupt = |s: Settings, error: String| (s, LoadStatus::Corrupt { error });
    match ut_fs::read_json::<Value>(path) {
        ReadJson::Missing => (Settings::default(), LoadStatus::Missing),
        ReadJson::Corrupt { error } => corrupt(Settings::default(), error),
        ReadJson::Ok(v) if !v.is_object() => corrupt(Settings::default(), "settings.json must contain a JSON object".into()),
        ReadJson::Ok(v) if crate::migrate::is_legacy_settings(&v) => {
            let s = crate::migrate::settings_from_legacy(&v);
            match ut_fs::backup_copy(path, "legacy") {
                Err(e) => corrupt(s, format!("cannot back up legacy settings: {e}")),
                Ok(bak) => match ut_fs::write_json_atomic(path, &s) {
                    Ok(()) => (s, LoadStatus::Migrated { backup: bak.display().to_string() }),
                    Err(e) => corrupt(s, format!("cannot write migrated settings: {e}")),
                },
            }
        }
        ReadJson::Ok(v) => (Settings::from_value(&v), LoadStatus::Loaded),
    }
}

impl SettingsStore {
    /// Load with a private writer thread; write errors are kept for [`Self::take_save_error`].
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let save_error = Arc::new(Mutex::new(None));
        let sink = save_error.clone();
        let writer = DebouncedWriter::new(move |p, e| *sink.lock() = Some(format!("{}: {e}", p.display())));
        Self::build(path.into(), Arc::new(writer), save_error)
    }

    /// Load with a writer shared with the other persisted files (one flush on exit covers all).
    pub fn load_with_writer(path: impl Into<PathBuf>, writer: Arc<DebouncedWriter>) -> Self {
        Self::build(path.into(), writer, Arc::default())
    }

    fn build(path: PathBuf, writer: Arc<DebouncedWriter>, save_error: Arc<Mutex<Option<String>>>) -> Self {
        let (cur, status) = read_settings(&path);
        Self { path, state: Mutex::new(State { cur, status }), writer, save_error }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn status(&self) -> LoadStatus {
        self.state.lock().status.clone()
    }

    pub fn get(&self) -> Settings {
        self.state.lock().cur.clone()
    }

    /// Apply a delta. Returns the new settings and the delta that was actually applied after
    /// sanitising (`{}` when nothing changed). Schedules the debounced save, except while the
    /// file on disk is corrupt (it is only replaced by [`Self::reset_with_backup`]).
    pub fn patch(&self, patch: Value) -> Result<(Settings, Value), String> {
        let mut st = self.state.lock();
        let new = apply_patch(&st.cur, patch)?;
        let Some(delta) = diff(&to_json(&st.cur), &to_json(&new)) else { return Ok((new, json!({}))) };
        st.cur = new.clone();
        if !matches!(st.status, LoadStatus::Corrupt { .. }) {
            self.writer.queue_json(&self.path, SAVE_DELAY, &st.cur);
        }
        Ok((new, delta))
    }

    /// "Reset (backup created)": move the existing file aside (`*.corrupt-<stamp>.json`), write defaults.
    pub fn reset_with_backup(&self) -> Result<Option<PathBuf>, String> {
        self.writer.flush_all();
        let mut st = self.state.lock();
        let backup = if self.path.exists() {
            Some(ut_fs::quarantine(&self.path).map_err(|e| format!("cannot back up settings: {e}"))?)
        } else {
            None
        };
        let d = Settings::default();
        ut_fs::write_json_atomic(&self.path, &d).map_err(|e| format!("cannot write settings: {e}"))?;
        *st = State { cur: d, status: LoadStatus::Loaded };
        Ok(backup)
    }

    /// Hot-reload after an external edit. Invalid JSON keeps the last valid settings and returns
    /// `Err(message)` (non-blocking banner); otherwise returns the settings and the delta against
    /// what was in memory (`{}` for the echo of our own save).
    pub fn reload(&self) -> Result<(Settings, Value), String> {
        let (new, status) = read_settings(&self.path);
        let mut st = self.state.lock();
        match status {
            LoadStatus::Corrupt { error } => {
                st.status = LoadStatus::Corrupt { error: error.clone() };
                Err(error)
            }
            LoadStatus::Missing => {
                st.status = LoadStatus::Missing; // deleted: keep running on what we have, next save recreates it
                Ok((st.cur.clone(), json!({})))
            }
            status => {
                let delta = diff(&to_json(&st.cur), &to_json(&new)).unwrap_or_else(|| json!({}));
                *st = State { cur: new.clone(), status };
                Ok((new, delta))
            }
        }
    }

    /// Write any pending save now (exit, logoff, hide-to-tray).
    pub fn flush(&self) {
        self.writer.flush_all();
    }

    /// The last background save error, if any (cleared by reading).
    pub fn take_save_error(&self) -> Option<String> {
        self.save_error.lock().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ut core settings {n} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn defaults_match_spec_14_2() {
        let v = to_json(&Settings::default());
        assert_eq!(v["schemaVersion"], 3);
        assert_eq!(v["terminal"]["fontFamily"], "'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace");
        assert_eq!(v["terminal"]["fontSize"].as_f64(), Some(14.0));
        assert_eq!(v["terminal"]["fontWeight"], 400);
        assert_eq!(v["terminal"]["cursorStyle"], "bar");
        assert_eq!(v["terminal"]["scrollback"], 10000);
        assert_eq!(v["terminal"]["renderer"], "webgl");
        assert_eq!(v["terminal"]["typedInputColor"], "#ffffff");
        assert_eq!(v["terminal"]["pasteImages"], "passThroughCtrlV");
        assert_eq!(v["terminal"]["osc52"], "write");
        assert_eq!(v["terminal"]["rightClick"], "menu");
        assert_eq!(v["terminal"]["overridePsReadLineColors"], true);
        assert_eq!(v["terminal"]["bell"], json!({"visual": true, "audible": false, "flashTaskbar": true}));
        assert_eq!(v["terminal"]["backgroundImage"]["opacity"].as_f64(), Some(0.52));
        assert_eq!(v["theme"], json!({"preset": "Default", "overrides": {}}));
        assert_eq!(v["ui"]["fontFamily"], "Segoe UI");
        assert_eq!(v["ui"]["backdrop"], "none");
        assert_eq!(v["shells"], json!({"defaultProfile": "auto"}));
        assert_eq!(v["startup"]["mode"], "restoreLastSession");
        assert_eq!(v["startup"]["workspaceId"], Value::Null);
        assert_eq!(v["processes"], json!({"closeConfirm": "whenRunning", "killConsoleTreeOnClose": true}));
        assert_eq!(v["notifications"]["commandFinished"], json!({"enabled": true, "minDurationSec": 10, "toast": true}));
        assert_eq!(v["ssh"], json!({"connectionReuse": "auto"}));
        assert_eq!(v["drop"], json!({"defaultAction": "copy"}));
        assert_eq!(v["links"]["editorCommand"], "code --goto \"{file}:{line}\"");
        assert_eq!(v["logging"], json!({"format": "plain", "directory": ""}));
        assert_eq!(v["recording"], json!({"captureInput": false}));
        assert_eq!(v["quake"]["hotkey"], "Win+Backquote");
        assert_eq!(v["quake"]["heightPercent"], 50);
        // defaults are a fixed point of sanitize
        let mut s = Settings::default();
        s.sanitize();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn partial_and_junk_files_load() {
        let s = Settings::from_value(&json!({"terminal": {"fontSize": 20, "cursorStyle": "zigzag", "scrollback": "lots",
            "bell": {"audible": true}}, "ui": {"scale": {"x": 1}}, "extra": 1}));
        assert_eq!(s.terminal.font_size, 20.0);
        assert_eq!(s.terminal.cursor_style, CursorStyle::Bar, "unknown enum -> default");
        assert_eq!(s.terminal.scrollback, 10_000, "wrong type -> default");
        assert!(s.terminal.bell.audible && s.terminal.bell.visual, "deep partial keeps sibling defaults");
        assert_eq!(s.ui.scale, 1.0);
        assert_eq!(Settings::from_value(&json!([1, 2])), Settings::default());
    }

    #[test]
    fn sanitize_clamps_everything() {
        let s = Settings::from_value(&json!({
            "terminal": {"fontSize": 99, "fontWeight": 5, "scrollback": 9_999_999, "lineHeight": 9.0,
                "typedInputColor": "#ABC", "backgroundImage": {"opacity": 7}, "fontFamily": "  "},
            "ui": {"fontSize": 3, "fontWeight": 1000, "scale": 1.23},
            "quake": {"hotkey": "Ctrl+Shift+nonsense", "heightPercent": 500},
            "theme": {"preset": " ", "overrides": {"background": "#FFF", "ansi": {"blue": "#00E676", "bogus": "#fff", "red": "red"},
                "ui": {"accent": "nope"}, "unknownKey": "#fff"}},
            "notifications": {"commandFinished": {"minDurationSec": -4}},
            "startup": {"workspaceId": "  "}, "shells": {"defaultProfile": ""}
        }));
        assert_eq!(s.terminal.font_size, 32.0);
        assert_eq!(s.terminal.font_weight, 100);
        assert_eq!(s.terminal.scrollback, 200_000);
        assert_eq!(s.terminal.line_height, 2.0);
        assert_eq!(s.terminal.typed_input_color, "#aabbcc");
        assert_eq!(s.terminal.background_image.opacity, 1.0);
        assert!(s.terminal.font_family.contains("Cascadia"));
        assert_eq!((s.ui.font_size, s.ui.font_weight), (10.0, 700));
        assert_eq!(s.ui.scale, 1.25);
        assert_eq!(s.quake.hotkey, "Win+Backquote");
        assert_eq!(s.quake.height_percent, 100);
        assert_eq!(s.theme.preset, "Default");
        assert_eq!(s.theme.overrides.terminal["background"], "#ffffff");
        assert_eq!(s.theme.overrides.ansi.len(), 1);
        assert_eq!(s.theme.overrides.ansi["blue"], "#00e676");
        assert!(s.theme.overrides.ui.is_empty() && !s.theme.overrides.terminal.contains_key("unknownKey"));
        assert_eq!(s.notifications.command_finished.min_duration_sec, 10, "negative -> default");
        assert_eq!(s.startup.workspace_id, None);
        assert_eq!(s.shells.default_profile, "auto");
        let h = Settings::from_value(&json!({"quake": {"hotkey": "ctrl+shift+f12"}}));
        assert_eq!(h.quake.hotkey, "Ctrl+Shift+F12");
        for (raw, want) in [(0.7, 0.75), (2.4, 2.0), (1.02, 1.0), (1.03, 1.05), (0.75, 0.75)] {
            assert_eq!(Settings::from_value(&json!({"ui": {"scale": raw}})).ui.scale, want, "{raw}");
        }
    }

    #[test]
    fn patch_is_a_delta_with_applied_delta_back() {
        let s = Settings::default();
        let p = apply_patch(&s, json!({"terminal": {"fontSize": 18}, "theme": {"overrides": {"ansi": {"blue": "#00E676"}}}})).unwrap();
        assert_eq!(p.terminal.font_size, 18.0);
        assert_eq!(p.terminal.font_weight, 400, "untouched fields survive a delta");
        assert_eq!(p.theme.overrides.ansi["blue"], "#00e676");
        let p2 = apply_patch(&p, json!({"theme": {"overrides": {"ansi": {"blue": null}}}, "startup": {"workspaceId": "w1"}})).unwrap();
        assert!(p2.theme.overrides.ansi.is_empty());
        assert_eq!(p2.startup.workspace_id.as_deref(), Some("w1"));
        let p3 = apply_patch(&p2, json!({"startup": {"workspaceId": null}})).unwrap();
        assert_eq!(p3.startup.workspace_id, None);
        assert!(apply_patch(&s, json!(5)).is_err());
        assert!(apply_patch(&s, json!([1])).is_err());
        // sanitised on every patch
        assert_eq!(apply_patch(&s, json!({"terminal": {"fontSize": 1000}})).unwrap().terminal.font_size, 32.0);
        assert_eq!(apply_patch(&s, json!({"terminal": {"cursorStyle": "nope"}})).unwrap().terminal.cursor_style, CursorStyle::Bar);
    }

    #[test]
    fn store_patch_debounce_flush_and_reload() {
        let d = tmp("store");
        let f = d.join("sub dir").join("settings.json");
        let store = SettingsStore::load(&f);
        assert_eq!(store.status(), LoadStatus::Missing);
        let (new, delta) = store.patch(json!({"terminal": {"fontSize": 99}})).unwrap();
        assert_eq!(new.terminal.font_size, 32.0);
        assert_eq!(delta, json!({"terminal": {"fontSize": 32.0}}), "the delta is what was really applied");
        assert_eq!(store.patch(json!({"terminal": {"fontSize": 32}})).unwrap().1, json!({}), "no-op patch");
        for _ in 0..200 {
            if f.exists() {
                break; // generous poll: CI machines can be busy
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert_eq!(v["terminal"]["fontSize"].as_f64(), Some(32.0));
        assert_eq!(v["schemaVersion"], 3);

        store.patch(json!({"ui": {"scale": 1.5}})).unwrap();
        store.flush();
        assert_eq!(SettingsStore::load(&f).get().ui.scale, 1.5);
        assert_eq!(SettingsStore::load(&f).status(), LoadStatus::Loaded);

        // external edit
        std::fs::write(&f, r#"{"schemaVersion":3,"terminal":{"fontSize":20},"ui":{"scale":1.5}}"#).unwrap();
        let (s, delta) = store.reload().unwrap();
        assert_eq!(s.terminal.font_size, 20.0);
        assert_eq!(delta, json!({"terminal": {"fontSize": 20.0}}));
        assert_eq!(store.reload().unwrap().1, json!({}));
        // external edit leaves invalid JSON: keep last valid, report, and never save over it
        std::fs::write(&f, "{ broken").unwrap();
        assert!(store.reload().is_err());
        assert_eq!(store.get().terminal.font_size, 20.0);
        store.patch(json!({"terminal": {"fontSize": 22}})).unwrap();
        store.flush();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{ broken");
    }

    #[test]
    fn corrupt_file_is_never_overwritten_until_reset() {
        let d = tmp("corrupt");
        let f = d.join("settings.json");
        std::fs::write(&f, "{ \"terminal\": ").unwrap();
        let store = SettingsStore::load(&f);
        assert!(matches!(store.status(), LoadStatus::Corrupt { .. }));
        assert_eq!(store.get(), Settings::default());
        store.patch(json!({"terminal": {"fontSize": 20}})).unwrap();
        assert_eq!(store.get().terminal.font_size, 20.0, "runs on defaults in memory");
        store.flush();
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{ \"terminal\": ", "file untouched");
        let bak = store.reset_with_backup().unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(bak).unwrap(), "{ \"terminal\": ");
        assert_eq!(store.status(), LoadStatus::Loaded);
        assert_eq!(store.get(), Settings::default());
        assert_eq!(SettingsStore::load(&f).get(), Settings::default());
        store.patch(json!({"terminal": {"fontSize": 21}})).unwrap();
        store.flush();
        assert_eq!(SettingsStore::load(&f).get().terminal.font_size, 21.0, "saving works again after reset");
    }

    #[test]
    fn legacy_file_is_backed_up_and_converted_on_load() {
        let d = tmp("legacy");
        let f = d.join("settings.json");
        std::fs::write(&f, r##"{"FontSize": 16, "ColorAccent": "#00FF88", "ColorInput": "#EEEEEE", "UiSharpness": "Crisp"}"##).unwrap();
        let store = SettingsStore::load(&f);
        assert!(matches!(store.status(), LoadStatus::Migrated { .. }), "{:?}", store.status());
        let s = store.get();
        assert_eq!(s.terminal.font_size, 16.0);
        assert_eq!(s.theme.preset, "Custom");
        assert_eq!(s.theme.overrides.terminal["accent"], "#00ff88");
        assert_eq!(s.terminal.typed_input_color, "#eeeeee");
        assert!(d.join("settings.legacy-bak.json").exists());
        let v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert_eq!(v["schemaVersion"], 3);
        assert!(v.get("FontSize").is_none() && v.get("UiSharpness").is_none());
        assert_eq!(SettingsStore::load(&f).status(), LoadStatus::Loaded);
    }
}
