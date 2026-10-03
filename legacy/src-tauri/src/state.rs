//! Shared application state: every persisted store lives here, behind one debounced writer.

use crate::pane::Pane;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use ut_core::{keybindings, windowstate, Keybindings, LoadStatus, Preset, Rect, Settings, SettingsStore, WindowState};
use ut_data::{SeedEntry, SessionStore, WorkspaceStore};
use ut_fs::DebouncedWriter;
use ut_shell::{GitCache, ShellCache};

pub struct Banner {
    pub kind: &'static str,
    pub text: String,
}

pub struct AppState {
    pub panes: RwLock<HashMap<String, Arc<Pane>>>,
    pub writer: Arc<DebouncedWriter>,
    pub settings: SettingsStore,
    pub keys: Mutex<Keybindings>,
    pub sessions: SessionStore,
    pub workspaces: WorkspaceStore,
    pub shells: ShellCache,
    pub git: GitCache,
    pub user_themes: RwLock<Vec<Preset>>,
    /// Messages produced while loading files (corrupt / migrated); delivered once the UI is ready.
    pub startup: Mutex<Vec<Banner>>,
    pub elevated: bool,
    pub winstate_path: PathBuf,
    pub keys_path: PathBuf,
    /// Last bounds of the window while it was in the Normal state (§16.3: never read them from a
    /// hidden/minimised window).
    pub normal_bounds: Mutex<Option<Rect>>,
    pub maximized: Mutex<bool>,
    /// Set once the window starts closing: blocks any queued autosave (§16.3).
    pub closing: std::sync::atomic::AtomicBool,
    /// Startup arguments (`--cwd`, `--session`, `--workspace`, `-- cmd…`).
    pub args: Mutex<StartArgs>,
    pub ssh_history: Mutex<Vec<String>>,
    pub drops: Mutex<HashMap<String, Arc<crate::drops::DropJob>>>,
}

#[derive(Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartArgs {
    pub cwd: Option<String>,
    pub session: Option<String>,
    pub workspace: Option<String>,
    pub command: Vec<String>,
}

impl StartArgs {
    /// `--cwd <dir> --session <name|id> --workspace <name> -- <command…>` (§16.2).
    pub fn parse(args: &[String]) -> Self {
        let mut a = StartArgs::default();
        let mut it = args.iter().skip(1);
        while let Some(x) = it.next() {
            match x.as_str() {
                "--cwd" => a.cwd = it.next().cloned(),
                "--session" => a.session = it.next().cloned(),
                "--workspace" => a.workspace = it.next().cloned(),
                "--" => {
                    a.command = it.by_ref().cloned().collect();
                }
                _ => {}
            }
        }
        a
    }
}

impl AppState {
    pub fn new() -> Self {
        let dir = ut_fs::app_data_dir();
        let writer = Arc::new(DebouncedWriter::new(|p, e| tracing::error!("write {} failed: {e}", p.display())));
        let mut startup = Vec::new();

        let settings = SettingsStore::load_with_writer(dir.join("settings.json"), writer.clone());
        match settings.status() {
            LoadStatus::Migrated { backup } => startup.push(Banner {
                kind: "info",
                text: format!("Settings were migrated from the previous version. A backup was saved to {backup}."),
            }),
            LoadStatus::Corrupt { .. } | LoadStatus::Loaded | LoadStatus::Missing => {}
        }

        let kl = keybindings::load(&dir.join("keybindings.json"));
        match &kl.status {
            LoadStatus::Corrupt { error } => startup.push(Banner {
                kind: "warning",
                text: format!("keybindings.json could not be read ({error}); using the default keybindings."),
            }),
            LoadStatus::Migrated { backup } => {
                startup.push(Banner { kind: "info", text: format!("Keybindings were migrated. Backup: {backup}") })
            }
            _ => {}
        }
        for bad in &kl.invalid {
            startup.push(Banner { kind: "warning", text: format!("Invalid keybinding ignored: {bad:?}") });
        }

        let sessions = SessionStore::open(dir.join("sessions.json"), writer.clone());
        let workspaces = WorkspaceStore::open(dir.join("workspaces.json"), writer.clone());
        for b in [sessions.banner(), workspaces.banner()].into_iter().flatten() {
            startup.push(Banner { kind: "warning", text: b });
        }

        let shells = ShellCache::new();
        // §8.2: seed once, on first run only — never when the user deleted everything or the file was corrupt.
        if sessions.first_run() {
            let entries: Vec<SeedEntry> = shells
                .get(false)
                .into_iter()
                .map(|p| SeedEntry { name: p.name, shell_path: p.path, arguments: p.arguments, color: p.color })
                .collect();
            sessions.seed(&entries);
        }

        let winstate_path = dir.join("windowstate.json");
        let user_themes = ut_core::load_user_themes(&dir.join("themes"));
        let ssh_history: Vec<String> = match ut_fs::read_json::<Vec<String>>(&dir.join("ssh-history.json")) {
            ut_fs::ReadJson::Ok(v) => v,
            _ => vec![],
        };

        Self {
            panes: RwLock::default(),
            writer,
            settings,
            keys: Mutex::new(kl.keybindings),
            sessions,
            workspaces,
            shells,
            git: GitCache::default(),
            user_themes: RwLock::new(user_themes),
            startup: Mutex::new(startup),
            elevated: ut_pty::elevate::is_elevated(),
            winstate_path,
            keys_path: dir.join("keybindings.json"),
            normal_bounds: Mutex::new(None),
            maximized: Mutex::new(false),
            closing: Default::default(),
            args: Mutex::new(StartArgs::default()),
            ssh_history: Mutex::new(ssh_history),
            drops: Mutex::default(),
        }
    }

    pub fn pane(&self, id: &str) -> Option<Arc<Pane>> {
        self.panes.read().get(id).cloned()
    }

    pub fn cfg(&self) -> Settings {
        self.settings.get()
    }

    pub fn osc52(&self) -> ut_vt_scan::Osc52 {
        match self.cfg().terminal.osc52 {
            ut_core::settings::Osc52::Off => ut_vt_scan::Osc52::Off,
            ut_core::settings::Osc52::ReadWrite => ut_vt_scan::Osc52::ReadWrite,
            ut_core::settings::Osc52::Write => ut_vt_scan::Osc52::Write,
        }
    }

    /// `terminal.conptyImplementation`: a bundled `conpty.dll` next to the exe is used unless "system".
    pub fn conpty_dll(&self) -> Option<PathBuf> {
        use ut_core::settings::ConptyImplementation;
        if self.cfg().terminal.conpty_implementation == ConptyImplementation::System {
            return None;
        }
        let dll = std::env::current_exe().ok()?.parent()?.join("conpty.dll");
        dll.is_file().then_some(dll)
    }

    pub fn kill_console_tree(&self) -> bool {
        self.cfg().processes.kill_console_tree_on_close
    }

    pub fn log_raw(&self) -> bool {
        self.cfg().logging.format == ut_core::settings::LogFormat::Raw
    }

    pub fn log_dir(&self) -> PathBuf {
        let d = self.cfg().logging.directory;
        if d.trim().is_empty() { ut_fs::app_data_dir().join("logs") } else { PathBuf::from(d) }
    }

    pub fn recordings_dir(&self) -> PathBuf {
        ut_fs::app_data_dir().join("recordings")
    }

    pub fn record_input(&self) -> bool {
        self.cfg().recording.capture_input
    }

    pub fn load_window_state(&self) -> (WindowState, LoadStatus) {
        windowstate::load(&self.winstate_path)
    }

    pub fn queue_window_state(&self, ws: &WindowState) {
        self.writer.queue_json(&self.winstate_path, Duration::from_millis(300), ws);
    }

    /// Everything must reach disk (exit, logoff, hide to tray).
    pub fn flush_all(&self) {
        self.settings.flush();
        self.sessions.flush();
        self.workspaces.flush();
        self.writer.flush_all();
    }

    pub fn remember_ssh(&self, input: &str) {
        let mut h = self.ssh_history.lock();
        h.retain(|x| x != input);
        h.insert(0, input.to_string());
        h.truncate(20);
        self.writer.queue_json(ut_fs::app_data_dir().join("ssh-history.json"), Duration::from_millis(500), &*h);
    }
}
