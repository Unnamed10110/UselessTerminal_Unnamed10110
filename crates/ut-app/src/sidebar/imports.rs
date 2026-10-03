//! Import / export flows (§8.5-§8.7): sessions JSON (merge or replace), Windows Terminal profiles and the improved
//! SSH config import (preview with a checklist; each host becomes `ssh <alias>` so ssh applies its own config).

use super::editor::dialog_button;
use super::{Modal, Sidebar};
use crate::kit::{Button, ButtonKind, Dialog, ToastKind};
use crate::panels::PanelCtx;
use egui::{Align, Context, Id, Key, Layout, Modifiers, RichText};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use ut_data::{ImportMode, ImportReport, Resolver, Session};
use ut_ssh::config::{host_description, SshHost};

const SSH_COLOR: &str = "#6be5ff";

/// One host of the SSH config preview.
pub struct HostRow {
    pub host: SshHost,
    /// The session name it would get (`[SSH] alias`).
    pub name: String,
    pub description: String,
    /// A session with this name already exists: shown, but not importable again.
    pub exists: bool,
    pub checked: bool,
}

pub struct SshImport {
    ssh: PathBuf,
    rows: Vec<HostRow>,
}

/// Rows for the preview: later duplicates of an alias (the same host in two included files) are dropped, hosts whose
/// session already exists are marked and unchecked, everything else starts checked.
pub fn plan_ssh(hosts: Vec<SshHost>, existing: &HashSet<String>) -> Vec<HostRow> {
    let mut seen = HashSet::new();
    hosts
        .into_iter()
        .filter(|h| seen.insert(h.alias.clone()))
        .map(|host| {
            let name = format!("[SSH] {}", host.alias);
            let exists = existing.contains(&name);
            HostRow { description: host_description(&host), name, exists, checked: !exists, host }
        })
        .collect()
}

/// Sessions for the checked, not-yet-existing rows (§8.7: name `[SSH] {alias}`, shell path = ssh, arguments = the alias,
/// HostName/User/Port in the description only).
pub fn sessions_for(rows: &[HostRow], ssh: &Path) -> Vec<Session> {
    rows.iter()
        .filter(|r| r.checked && !r.exists)
        .map(|r| {
            let alias = &r.host.alias;
            Session {
                name: r.name.clone(),
                description: r.description.clone(),
                shell_path: ssh.to_string_lossy().into_owned(),
                arguments: if alias.contains(char::is_whitespace) { format!("\"{alias}\"") } else { alias.clone() },
                color_tag: SSH_COLOR.into(),
                ..Default::default()
            }
        })
        .collect()
}

/// Numbers actually applied (§8.6), not numbers found.
pub fn import_message(r: &ImportReport, mode: ImportMode) -> String {
    let mut s = format!("Imported {} session(s)", r.added);
    if r.skipped > 0 {
        s += &format!(", {} skipped (already present)", r.skipped);
    }
    if mode == ImportMode::Replace && r.replaced > 0 {
        s += &format!("; {} previous session(s) replaced", r.replaced);
    }
    if r.snippets_added > 0 {
        s += &format!(", {} snippet(s)", r.snippets_added);
    }
    s + "."
}

pub fn wt_message(r: &ImportReport) -> String {
    match (r.added, r.skipped) {
        (0, 0) => "No Windows Terminal profiles were found.".into(),
        (0, n) => format!("All {n} Windows Terminal profile(s) were already imported."),
        (a, 0) => format!("Imported {a} Windows Terminal profile(s)."),
        (a, n) => format!("Imported {a} Windows Terminal profile(s); {n} already present."),
    }
}

pub fn ssh_message(added: usize, skipped: usize) -> String {
    if skipped > 0 {
        format!("Imported {added} SSH host(s) from ~/.ssh/config ({skipped} already present).")
    } else {
        format!("Imported {added} SSH host(s) from ~/.ssh/config.")
    }
}

impl Sidebar {
    pub(super) fn export_sessions(&mut self, x: &mut PanelCtx) {
        let text = ut_data::export_json(&x.core.sessions);
        let Some(path) = rfd::FileDialog::new().add_filter("Sessions", &["json"]).set_file_name("useless-terminal-sessions.json").save_file() else { return };
        match std::fs::write(&path, text) {
            Ok(()) => x.toasts.push(format!("Exported sessions to {}", path.display()), ToastKind::Success),
            Err(e) => x.toasts.push(format!("Could not write {}: {e}", path.display()), ToastKind::Error),
        }
    }

    /// Pick a file, then ask Merge or Replace (§8.5).
    pub(super) fn import_sessions(&mut self, x: &mut PanelCtx) {
        let Some(path) = rfd::FileDialog::new().add_filter("Sessions", &["json"]).add_filter("All files", &["*"]).pick_file() else { return };
        let text = match std::fs::read(&path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => return x.toasts.push(format!("Could not read {}: {e}", path.display()), ToastKind::Error),
        };
        let d = Dialog::new(
            "Import sessions",
            "Merge adds the imported sessions to your current ones (duplicates are skipped). Replace discards all current sessions and folders first.",
            vec![Button::new("Cancel", ButtonKind::Normal), Button::new("Replace", ButtonKind::Danger), Button::new("Merge", ButtonKind::Primary)],
        );
        self.modal = Some(Modal::Import(d, text));
    }

    pub(super) fn do_import(&mut self, x: &mut PanelCtx, text: &str, mode: ImportMode) {
        match ut_data::import_json(&x.core.sessions, text, mode) {
            Ok(r) => x.toasts.push(import_message(&r, mode), if r.added > 0 { ToastKind::Success } else { ToastKind::Info }),
            Err(e) => x.toasts.push(e.to_string(), ToastKind::Error),
        }
    }

    pub(super) fn import_wt(&mut self, x: &mut PanelCtx) {
        if ut_data::find_wt_settings().is_none() {
            return x.toasts.push("Windows Terminal settings.json was not found.", ToastKind::Warning);
        }
        let profiles = if self.shells.loaded() { self.shells.list.clone() } else { x.core.shells.get(false) };
        let resolver = Resolver { pwsh_path: profiles.iter().find(|p| p.id == "pwsh").map(|p| p.path.clone()), wsl_exe: profiles.iter().find(|p| p.id == "wsl").map(|p| p.path.clone()) };
        match ut_data::wt_import(&x.core.sessions, &resolver) {
            Ok(r) => x.toasts.push(wt_message(&r), if r.added > 0 { ToastKind::Success } else { ToastKind::Info }),
            Err(e) => x.toasts.push(e.to_string(), ToastKind::Error),
        }
    }

    /// Read `~/.ssh/config` (following `Include`) and open the preview.
    pub(super) fn import_ssh(&mut self, x: &mut PanelCtx) {
        let Some(ssh) = ut_ssh::locate::find_ssh() else {
            return x.toasts.push("ssh.exe was not found.", ToastKind::Error);
        };
        let hosts = match ut_ssh::config::read_ssh_config(None) {
            Ok(h) => h,
            Err(e) => return x.toasts.push(format!("Could not read ~/.ssh/config: {e}"), ToastKind::Error),
        };
        if hosts.is_empty() {
            return x.toasts.push("No SSH hosts found in ~/.ssh/config.", ToastKind::Info);
        }
        let existing: HashSet<String> = x.core.sessions.list().into_iter().map(|s| s.name).collect();
        self.ssh = Some(SshImport { ssh, rows: plan_ssh(hosts, &existing) });
    }

    pub(super) fn show_ssh_import(&mut self, ctx: &Context, x: &mut PanelCtx) {
        let Some(mut d) = self.ssh.take() else { return };
        let th = &x.theme.ui;
        let (mut import, mut cancel) = (false, false);
        let n = d.rows.iter().filter(|r| r.checked && !r.exists).count();
        let modal = egui::Modal::new(Id::new("sb-ssh-import")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 40.0).clamp(320.0, 460.0));
            ui.spacing_mut().scroll = egui::style::ScrollStyle::floating();
            ui.label(RichText::new("Import SSH hosts").strong().size(15.0));
            ui.add_space(4.0);
            ui.label(RichText::new(format!("{} host(s) in ~/.ssh/config. Each becomes a session that runs `ssh <alias>`, so ssh applies your whole configuration (ProxyJump, keys, options).", d.rows.len())).small().color(th.muted));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.small_button("Select all").clicked() {
                    d.rows.iter_mut().filter(|r| !r.exists).for_each(|r| r.checked = true);
                }
                if ui.small_button("Select none").clicked() {
                    d.rows.iter_mut().for_each(|r| r.checked = false);
                }
            });
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 260.0).clamp(120.0, 300.0)).auto_shrink([false, true]).show(ui, |ui| {
                for r in &mut d.rows {
                    ui.add_enabled_ui(!r.exists, |ui| {
                        ui.checkbox(&mut r.checked, &r.name);
                    });
                    let sub = match (r.exists, r.description.is_empty()) {
                        (true, _) => "already imported".to_string(),
                        (false, false) => r.description.clone(),
                        (false, true) => String::new(),
                    };
                    if !sub.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            ui.label(RichText::new(sub).small().color(th.muted));
                        });
                    }
                }
            });
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled_ui(n > 0, |ui| {
                    if dialog_button(ui, th, &format!("Import {n}"), true) {
                        import = true;
                    }
                });
                if dialog_button(ui, th, "Cancel", false) {
                    cancel = true;
                }
            });
        });
        if cancel || (modal.is_top_modal && !modal.any_popup_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))) {
            self.focus_tree = true;
            return;
        }
        if import {
            let r = ut_data::add_imported(&x.core.sessions, sessions_for(&d.rows, &d.ssh));
            x.toasts.push(ssh_message(r.added, r.skipped), if r.added > 0 { ToastKind::Success } else { ToastKind::Info });
            self.focus_tree = true;
            return;
        }
        self.ssh = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(alias: &str, hostname: Option<&str>, user: Option<&str>, port: Option<u16>) -> SshHost {
        SshHost { alias: alias.into(), hostname: hostname.map(Into::into), user: user.map(Into::into), port, ..Default::default() }
    }

    #[test]
    fn plan_marks_existing_and_drops_duplicate_aliases() {
        let hosts = vec![host("web", Some("10.0.0.1"), Some("bob"), Some(2222)), host("db", None, None, None), host("web", Some("other"), None, None), host("old", None, None, None)];
        let existing: HashSet<String> = ["[SSH] old".to_string(), "unrelated".to_string()].into();
        let rows = plan_ssh(hosts, &existing);
        let view: Vec<(&str, bool, bool, &str)> = rows.iter().map(|r| (r.name.as_str(), r.exists, r.checked, r.description.as_str())).collect();
        assert_eq!(view, [("[SSH] web", false, true, "bob@10.0.0.1:2222"), ("[SSH] db", false, true, ""), ("[SSH] old", true, false, "")]);
    }

    #[test]
    fn sessions_run_ssh_with_the_alias_and_only_for_checked_new_hosts() {
        let mut rows = plan_ssh(vec![host("web", None, None, None), host("my box", None, None, None), host("off", None, None, None), host("old", None, None, None)], &["[SSH] old".to_string()].into());
        rows[2].checked = false;
        rows[3].checked = true; // an existing one can never be imported again, even if forced on
        let ssh = Path::new(r"C:\Windows\System32\OpenSSH\ssh.exe");
        let s = sessions_for(&rows, ssh);
        assert_eq!(s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["[SSH] web", "[SSH] my box"]);
        assert_eq!((s[0].shell_path.as_str(), s[0].arguments.as_str(), s[0].color_tag.as_str()), (r"C:\Windows\System32\OpenSSH\ssh.exe", "web", "#6be5ff"));
        assert_eq!(s[1].arguments, "\"my box\"", "an alias with a space stays one argument");
        assert!(s[0].full_command().ends_with(" web"));
    }

    #[test]
    fn messages_report_what_was_applied() {
        let r = |added, skipped, replaced, snippets_added| ImportReport { added, skipped, replaced, snippets_added };
        assert_eq!(import_message(&r(3, 0, 0, 0), ImportMode::Merge), "Imported 3 session(s).");
        assert_eq!(import_message(&r(2, 1, 0, 4), ImportMode::Merge), "Imported 2 session(s), 1 skipped (already present), 4 snippet(s).");
        assert_eq!(import_message(&r(5, 0, 7, 0), ImportMode::Replace), "Imported 5 session(s); 7 previous session(s) replaced.");
        assert_eq!(wt_message(&r(0, 0, 0, 0)), "No Windows Terminal profiles were found.");
        assert_eq!(wt_message(&r(0, 3, 0, 0)), "All 3 Windows Terminal profile(s) were already imported.");
        assert_eq!(wt_message(&r(2, 1, 0, 0)), "Imported 2 Windows Terminal profile(s); 1 already present.");
        assert_eq!(ssh_message(4, 0), "Imported 4 SSH host(s) from ~/.ssh/config.");
        assert_eq!(ssh_message(1, 2), "Imported 1 SSH host(s) from ~/.ssh/config (2 already present).");
    }
}
