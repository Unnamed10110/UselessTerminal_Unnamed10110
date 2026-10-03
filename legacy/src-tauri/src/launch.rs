//! Resolve a `pty_spawn` request (command | profile | session) to a command line, cwd, env and
//! post-spawn writes: shell-integration plan (§6.5) + environment layering (§4.3).

use crate::pane::{Delivery, Resolved, SpawnReq};
use crate::state::AppState;
use std::path::PathBuf;
use ut_data::{default_env, fresh_parent_env, layer_env, parse_env, process_env, Session};
use ut_shell::{IntegrationMode, IntegrationRequest, PostSpawn, ShellKind};

pub fn resolve(state: &AppState, req: &SpawnReq) -> Result<Resolved, String> {
    let settings = state.cfg();
    let profiles = state.shells.get(false);

    // 1. What to run.
    let session: Option<Session> = match &req.launch.session_id {
        Some(id) => Some(state.sessions.get(id).ok_or_else(|| format!("Unknown session {id}"))?),
        None => None,
    };
    let command_line = if let Some(s) = &session {
        s.full_command()
    } else if let Some(c) = req.launch.command.as_ref().filter(|c| !c.trim().is_empty()) {
        c.clone()
    } else if let Some(p) = req.launch.profile_id.as_ref().and_then(|id| profiles.iter().find(|p| &p.id == id)) {
        p.command.clone()
    } else {
        ut_shell::resolve_default(&settings.shells.default_profile, &profiles)
            .or_else(|| profiles.iter().find(|p| p.is_default))
            .map(|p| p.command.clone())
            .unwrap_or_else(|| "cmd.exe".into())
    };

    // 2. Environment (parent → integration → session) and working directory.
    let base = if settings.terminal.refresh_environment { fresh_parent_env() } else { process_env() };
    let mut session_env = session.as_ref().map(|s| s.resolved_env(&base)).unwrap_or_default();
    session_env.extend(req.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    let cwd = req
        .cwd
        .clone()
        .filter(|c| !c.trim().is_empty())
        .or_else(|| session.as_ref().map(|s| s.resolved_cwd(&base)))
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(ut_fs::home_dir);

    // 3. Shell integration + starting command.
    let starting = req
        .starting_command
        .clone()
        .or_else(|| session.as_ref().map(|s| s.starting_command.clone()))
        .filter(|c| !c.trim().is_empty());
    let off = req.integration.as_deref() == Some("off")
        || (req.integration.is_none() && session.as_ref().is_some_and(|s| s.integration == ut_data::Integration::Off));
    let plan = ut_shell::plan(&IntegrationRequest {
        command_line,
        mode: if off { IntegrationMode::Off } else { IntegrationMode::Auto },
        starting_command: starting,
        typed_input_color: settings.terminal.typed_input_color.clone(),
        override_ps_readline: settings.terminal.override_ps_read_line_colors,
        session_env: session_env.clone(),
        temp_dir: Some(ut_fs::local_data_dir().join("tmp")),
    });

    // 4. SSH connection reuse (§10.5), only for ssh command lines.
    let command_line = match ut_ssh::locate::find_ssh() {
        Some(ssh) if plan.kind == ShellKind::Ssh => {
            let mode = match settings.ssh.connection_reuse {
                ut_core::settings::ConnectionReuse::Auto => "auto",
                ut_core::settings::ConnectionReuse::Always => "always",
                ut_core::settings::ConnectionReuse::Never => "never",
            };
            ut_ssh::mux::inject_connection_reuse(&plan.command_line, mode, &ssh)
        }
        _ => plan.command_line.clone(),
    };

    let env = layer_env(&default_env(env!("CARGO_PKG_VERSION")), &base, &plan.env, &session_env);
    let deliveries = plan
        .post_spawn
        .iter()
        .map(|PostSpawn::WriteAfterFirstOutputQuiet { quiet_ms, cap_ms, bytes }| Delivery::AfterFirstOutputQuiet {
            quiet_ms: *quiet_ms,
            cap_ms: *cap_ms,
            bytes: bytes.clone(),
        })
        .collect();

    Ok(Resolved {
        command_line,
        cwd,
        env,
        deliveries,
        shell_kind: plan.kind,
        notes: plan.notes,
        injected: plan.injected,
        local_input_color: plan.local_input_color,
        session_name: session.map(|s| s.name),
    })
}

/// Parse a session environment text (used by validation commands).
#[allow(dead_code)]
pub fn env_pairs(text: &str) -> Vec<(String, String)> {
    parse_env(text)
}
