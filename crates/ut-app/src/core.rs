//! Everything that is not UI state: persisted stores behind one debounced writer, shell detection, git,
//! theme derivation and launch resolution.

use parking_lot::{Mutex, RwLock};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use ut_core::{effective_with, keybindings, windowstate, EffectiveTheme, Keybindings, LoadStatus, Preset, Settings, SettingsStore, WindowState};
use ut_data::{default_env, fresh_parent_env, layer_env, process_env, SeedEntry, Session, SessionStore, WorkspaceStore};
use ut_fs::DebouncedWriter;
use ut_shell::{GitCache, IntegrationMode, IntegrationRequest, PostSpawn, ShellCache, ShellKind, ShellProfile};
use ut_term::{Delivery, SessionConfig};

pub struct Banner {
    pub kind: BannerKind,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Info,
    Warning,
    Error,
}

/// What a launch request resolves to before the PTY exists.
pub struct Resolved {
    pub command_line: String,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub deliveries: Vec<Delivery>,
    pub kind: ShellKind,
    pub notes: Vec<String>,
    pub injected: bool,
    pub local_input_color: bool,
    pub session_name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LaunchReq {
    /// Raw command line (profile command, quick connect, …).
    pub command: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub starting_command: Option<String>,
    /// `Some(true)` forces integration off.
    pub integration_off: Option<bool>,
}

pub struct Core {
    pub writer: Arc<DebouncedWriter>,
    pub settings: SettingsStore,
    pub keys: Mutex<Keybindings>,
    pub sessions: SessionStore,
    pub workspaces: WorkspaceStore,
    pub shells: ShellCache,
    pub git: GitCache,
    pub user_themes: RwLock<Vec<Preset>>,
    pub startup: Mutex<Vec<Banner>>,
    pub elevated: bool,
    pub os_build: u32,
    pub computer_name: String,
    pub winstate_path: PathBuf,
    pub keys_path: PathBuf,
    pub ssh_history: Mutex<Vec<String>>,
}

impl Core {
    pub fn new() -> Arc<Core> {
        let dir = ut_fs::app_data_dir();
        let writer = Arc::new(DebouncedWriter::new(|p, e| tracing::error!("write {} failed: {e}", p.display())));
        let mut startup = Vec::new();
        let mut note = |kind, text: String| startup.push(Banner { kind, text });

        let settings = SettingsStore::load_with_writer(dir.join("settings.json"), writer.clone());
        if let LoadStatus::Migrated { backup } = settings.status() {
            note(BannerKind::Info, format!("Settings were migrated from the previous version. Backup: {backup}"));
        }
        if let LoadStatus::Corrupt { error } = settings.status() {
            note(BannerKind::Error, format!("settings.json could not be read ({error}). Defaults are in use and the file was not changed."));
        }
        let kl = keybindings::load(&dir.join("keybindings.json"));
        match &kl.status {
            LoadStatus::Corrupt { error } => note(BannerKind::Warning, format!("keybindings.json could not be read ({error}); using the default keybindings.")),
            LoadStatus::Migrated { backup } => note(BannerKind::Info, format!("Keybindings were migrated. Backup: {backup}")),
            _ => {}
        }
        for bad in &kl.invalid {
            note(BannerKind::Warning, format!("Invalid keybinding ignored: {bad:?}"));
        }
        let sessions = SessionStore::open(dir.join("sessions.json"), writer.clone());
        let workspaces = WorkspaceStore::open(dir.join("workspaces.json"), writer.clone());
        for b in [sessions.banner(), workspaces.banner()].into_iter().flatten() {
            note(BannerKind::Warning, b);
        }
        let shells = ShellCache::new();
        // §8.2: seed once, on first run only.
        if sessions.first_run() {
            let entries: Vec<SeedEntry> = shells.get(false).into_iter().map(|p| SeedEntry { name: p.name, shell_path: p.path, arguments: p.arguments, color: p.color }).collect();
            sessions.seed(&entries);
        }
        let ssh_history = match ut_fs::read_json::<Vec<String>>(&dir.join("ssh-history.json")) {
            ut_fs::ReadJson::Ok(v) => v,
            _ => vec![],
        };
        Arc::new(Core {
            writer,
            settings,
            keys: Mutex::new(kl.keybindings),
            sessions,
            workspaces,
            shells,
            git: GitCache::default(),
            user_themes: RwLock::new(ut_core::load_user_themes(&dir.join("themes"))),
            startup: Mutex::new(startup),
            elevated: ut_pty::elevate::is_elevated(),
            os_build: os_build(),
            computer_name: std::env::var("COMPUTERNAME").unwrap_or_default(),
            winstate_path: dir.join("windowstate.json"),
            keys_path: dir.join("keybindings.json"),
            ssh_history: Mutex::new(ssh_history),
        })
    }

    pub fn cfg(&self) -> Settings {
        self.settings.get()
    }

    pub fn theme(&self) -> EffectiveTheme {
        effective_with(&self.cfg().theme, &self.user_themes.read())
    }

    pub fn preset_names(&self) -> Vec<String> {
        let mut v: Vec<String> = ut_core::preset_names().into_iter().map(String::from).collect();
        v.extend(self.user_themes.read().iter().map(|p| p.name.clone()));
        v
    }

    pub fn load_window_state(&self) -> (WindowState, LoadStatus) {
        windowstate::load(&self.winstate_path)
    }

    pub fn queue_window_state(&self, ws: &WindowState) {
        self.writer.queue_json(&self.winstate_path, Duration::from_millis(300), ws);
    }

    pub fn flush_all(&self) {
        self.settings.flush();
        self.sessions.flush();
        self.workspaces.flush();
        self.writer.flush_all();
    }

    pub fn conpty_dll(&self) -> Option<PathBuf> {
        use ut_core::settings::ConptyImplementation;
        if self.cfg().terminal.conpty_implementation == ConptyImplementation::System {
            return None;
        }
        let dll = std::env::current_exe().ok()?.parent()?.join("conpty.dll");
        dll.is_file().then_some(dll)
    }

    pub fn log_dir(&self) -> PathBuf {
        let d = self.cfg().logging.directory;
        if d.trim().is_empty() { ut_fs::app_data_dir().join("logs") } else { PathBuf::from(d) }
    }

    pub fn recordings_dir(&self) -> PathBuf {
        ut_fs::app_data_dir().join("recordings")
    }

    pub fn remember_ssh(&self, input: &str) {
        let mut h = self.ssh_history.lock();
        h.retain(|x| x != input);
        h.insert(0, input.to_string());
        h.truncate(20);
        self.writer.queue_json(ut_fs::app_data_dir().join("ssh-history.json"), Duration::from_millis(500), &*h);
    }

    pub fn default_profile(&self) -> Option<ShellProfile> {
        let profiles = self.shells.get(false);
        let want = self.cfg().shells.default_profile;
        ut_shell::resolve_default(&want, &profiles).cloned().or_else(|| profiles.iter().find(|p| p.is_default).cloned()).or_else(|| profiles.first().cloned())
    }

    /// Resolve a launch request to a command line, cwd, env and post-spawn writes: shell-integration plan (§6.5),
    /// environment layering (§4.3), SSH connection reuse (§10.5).
    pub fn resolve_launch(&self, req: &LaunchReq) -> Result<Resolved, String> {
        let settings = self.cfg();
        let session: Option<Session> = match &req.session_id {
            Some(id) => Some(self.sessions.get(id).ok_or_else(|| format!("Unknown session {id}"))?),
            None => None,
        };
        let command_line = if let Some(s) = &session {
            s.full_command()
        } else if let Some(c) = req.command.as_ref().filter(|c| !c.trim().is_empty()) {
            c.clone()
        } else {
            self.default_profile().map(|p| p.command).unwrap_or_else(|| "cmd.exe".into())
        };
        let base = if settings.terminal.refresh_environment { fresh_parent_env() } else { process_env() };
        let mut session_env = session.as_ref().map(|s| s.resolved_env(&base)).unwrap_or_default();
        session_env.extend(req.env.iter().cloned());
        let cwd = req
            .cwd
            .clone()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| session.as_ref().map(|s| s.resolved_cwd(&base)))
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(ut_fs::home_dir);
        let starting = req.starting_command.clone().or_else(|| session.as_ref().map(|s| s.starting_command.clone())).filter(|c| !c.trim().is_empty());
        let off = req.integration_off == Some(true) || (req.integration_off.is_none() && session.as_ref().is_some_and(|s| s.integration == ut_data::Integration::Off));
        let plan = ut_shell::plan(&IntegrationRequest {
            command_line,
            mode: if off { IntegrationMode::Off } else { IntegrationMode::Auto },
            starting_command: starting,
            typed_input_color: settings.terminal.typed_input_color.clone(),
            override_ps_readline: settings.terminal.override_ps_read_line_colors,
            session_env: session_env.clone(),
            temp_dir: Some(ut_fs::local_data_dir().join("tmp")),
        });
        let command_line = match ut_ssh::locate::find_ssh() {
            Some(ssh) if plan.kind == ShellKind::Ssh => {
                use ut_core::settings::ConnectionReuse::*;
                let mode = match settings.ssh.connection_reuse {
                    Auto => "auto",
                    Always => "always",
                    Never => "never",
                };
                ut_ssh::mux::inject_connection_reuse(&plan.command_line, mode, &ssh)
            }
            _ => plan.command_line.clone(),
        };
        let env = layer_env(&default_env(env!("CARGO_PKG_VERSION")), &base, &plan.env, &session_env);
        let deliveries = plan
            .post_spawn
            .iter()
            .map(|PostSpawn::WriteAfterFirstOutputQuiet { quiet_ms, cap_ms, bytes }| Delivery::AfterFirstOutputQuiet { quiet_ms: *quiet_ms, cap_ms: *cap_ms, bytes: bytes.clone() })
            .collect();
        Ok(Resolved {
            command_line,
            cwd,
            env,
            deliveries,
            kind: plan.kind,
            notes: plan.notes,
            injected: plan.injected,
            local_input_color: plan.local_input_color,
            session_name: session.map(|s| s.name),
        })
    }

    /// Terminal session config for a resolved launch.
    pub fn session_config(&self, r: &Resolved, cols: u16, rows: u16, palette: ut_term::Palette, repaint: Arc<dyn Fn() + Send + Sync>) -> SessionConfig {
        let t = self.cfg().terminal;
        SessionConfig {
            command_line: r.command_line.clone(),
            cwd: Some(r.cwd.clone()),
            env: r.env.clone(),
            cols,
            rows,
            scrollback: t.scrollback as usize,
            conpty_dll: self.conpty_dll(),
            osc52: match t.osc52 {
                ut_core::settings::Osc52::Off => ut_vt_scan::Osc52::Off,
                ut_core::settings::Osc52::ReadWrite => ut_vt_scan::Osc52::ReadWrite,
                ut_core::settings::Osc52::Write => ut_vt_scan::Osc52::Write,
            },
            computer_name: self.computer_name.clone(),
            deliveries: r.deliveries.clone(),
            repaint,
            palette,
            cursor: match t.cursor_style {
                ut_core::settings::CursorStyle::Bar => ut_term::CursorSetting::Bar,
                ut_core::settings::CursorStyle::Block => ut_term::CursorSetting::Block,
                ut_core::settings::CursorStyle::Underline => ut_term::CursorSetting::Underline,
            },
        }
    }
}

/// The real OS build number (shown in diagnostics; ConPTY behaviour differs per build).
pub fn os_build() -> u32 {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let mut buf = [0u16; 32];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"), w!("CurrentBuildNumber"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut len))
    };
    if r.is_err() {
        return 0;
    }
    String::from_utf16_lossy(&buf).trim_end_matches('\0').trim().parse().unwrap_or(0)
}
