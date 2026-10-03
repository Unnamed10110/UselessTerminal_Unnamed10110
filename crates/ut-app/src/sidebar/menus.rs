//! Context menus of the sidebar (§8.2) and the actions they trigger. Menus are rebuilt on every frame they are open,
//! so labels (Expand/Collapse, Move up/down, "Open 3 sessions") never go stale.

use super::Sidebar;
use crate::kit::{menu_item, popup_menu};
use crate::panels::PanelCtx;
use crate::theme::parse_color;
use egui::{Context, Id, Pos2, Sense, Ui, Vec2};
use std::cell::RefCell;

/// The six swatches of the session editor (§8.3), also offered for bulk colour changes.
pub const SWATCHES: [&str; 6] = ["#00ff44", "#ff003c", "#ffff00", "#00e5ff", "#ff00ff", "#ff8800"];

pub enum MenuKind {
    /// The "⋯" button of the header.
    Header,
    /// Empty tree space.
    Background,
    Folder(String),
    Session(String),
    /// A snippet, or the empty snippet list.
    Snippet(Option<String>),
    /// A detected shell profile (by id).
    Shell(String),
}

pub struct Menu {
    pub pos: Pos2,
    pub kind: MenuKind,
}

/// Everything a menu entry, a hover button or a key can ask for.
pub enum Act {
    Open { ids: Vec<String>, admin: bool },
    NewSession(Option<String>),
    Edit(String),
    Duplicate(String),
    CopyCommand(String),
    Delete(Vec<String>),
    SetColor(Vec<String>, String),
    NewFolder,
    RenameFolder(String),
    RenameSession(String),
    MoveFolder { id: String, up: bool },
    SetOpen(String, bool),
    DeleteFolder(String),
    Export,
    Import,
    ImportWt,
    ImportSsh,
    QuickConnect,
    RefreshShells,
    OpenShell(String),
    SaveShell(String),
    NewSnippet,
    EditSnippet(String),
    RunSnippet(String),
    DeleteSnippet(String),
}

impl Sidebar {
    pub(super) fn open_menu(&mut self, pos: Pos2, kind: MenuKind) {
        self.menu = Some(Menu { pos, kind });
    }

    pub(super) fn show_menu(&mut self, ctx: &Context, x: &mut PanelCtx) {
        let Some(m) = self.menu.take() else { return };
        let chosen: RefCell<Option<Act>> = RefCell::new(None);
        let th = &x.theme.ui;
        // One entry: records the action and closes the menu when clicked.
        let item = |ui: &mut Ui, label: &str, hint: &str, enabled: bool, act: Act| -> bool {
            if menu_item(ui, label, hint, enabled, false) {
                *chosen.borrow_mut() = Some(act);
                true
            } else {
                false
            }
        };
        let id = Id::new("sb-menu");
        let keep = match &m.kind {
            MenuKind::Header => popup_menu(ctx, id, m.pos, |ui| {
                let mut done = false;
                done |= item(ui, "Export sessions…", "", true, Act::Export);
                done |= item(ui, "Import sessions…", "", true, Act::Import);
                done |= item(ui, "New folder", "", true, Act::NewFolder);
                ui.separator();
                done |= item(ui, "Import from Windows Terminal", "", true, Act::ImportWt);
                done |= item(ui, "Import from SSH Config…", "", true, Act::ImportSsh);
                ui.separator();
                done |= item(ui, "Quick SSH connect…", "", true, Act::QuickConnect);
                done |= item(ui, "Refresh shells", "", true, Act::RefreshShells);
                done
            }),
            MenuKind::Background => popup_menu(ctx, id, m.pos, |ui| {
                let mut done = false;
                done |= item(ui, "New session", "", true, Act::NewSession(None));
                done |= item(ui, "New folder", "", true, Act::NewFolder);
                ui.separator();
                done |= item(ui, "Import from Windows Terminal", "", true, Act::ImportWt);
                done |= item(ui, "Import from SSH Config…", "", true, Act::ImportSsh);
                ui.separator();
                done |= item(ui, "Export sessions…", "", true, Act::Export);
                done |= item(ui, "Import sessions…", "", true, Act::Import);
                done
            }),
            MenuKind::Folder(fid) => {
                let order = x.core.sessions.folders();
                let i = order.iter().position(|f| &f.id == fid);
                let open = self.st.is_open(fid);
                let fid = fid.clone();
                popup_menu(ctx, id, m.pos, |ui| {
                    let mut done = false;
                    done |= item(ui, "New session in folder", "", true, Act::NewSession(Some(fid.clone())));
                    done |= item(ui, "Rename", "F2", true, Act::RenameFolder(fid.clone()));
                    done |= item(ui, "Move up", "", i.is_some_and(|i| i > 0), Act::MoveFolder { id: fid.clone(), up: true });
                    done |= item(ui, "Move down", "", i.is_some_and(|i| i + 1 < order.len()), Act::MoveFolder { id: fid.clone(), up: false });
                    ui.separator();
                    done |= item(ui, "Expand", "", !open, Act::SetOpen(fid.clone(), true));
                    done |= item(ui, "Collapse", "", open, Act::SetOpen(fid.clone(), false));
                    ui.separator();
                    // The consequence is explained where it is decided (the confirmation), not in a tooltip.
                    done |= item(ui, "Delete folder", "Del", true, Act::DeleteFolder(fid.clone()));
                    done
                })
            }
            MenuKind::Session(sid) => {
                // A right-click inside the selection acts on all of it.
                let mut ids = self.sel_sessions();
                if !ids.contains(sid) {
                    ids = vec![sid.clone()];
                }
                let many = ids.len() > 1;
                let open_label = if many { format!("Open {} sessions", ids.len()) } else { "Open".to_string() };
                let del_label = if many { format!("Delete {} sessions", ids.len()) } else { "Delete".to_string() };
                let sid = sid.clone();
                let bulk = ids.clone();
                popup_menu(ctx, id, m.pos, |ui| {
                    let mut done = false;
                    done |= item(ui, &open_label, "Enter", true, Act::Open { ids: ids.clone(), admin: false });
                    done |= item(ui, "Run as administrator", "Ctrl+Enter", true, Act::Open { ids: ids.clone(), admin: true });
                    done |= item(ui, "Edit", "", !many, Act::Edit(sid.clone()));
                    done |= item(ui, "Duplicate", "", !many, Act::Duplicate(sid.clone()));
                    done |= item(ui, "Copy command line", "", !many, Act::CopyCommand(sid.clone()));
                    ui.separator();
                    // Bulk colour change (§8.2 [P1]): one row of swatches that applies to the whole selection.
                    ui.horizontal(|ui| {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Colour").small().color(th.muted));
                        for c in SWATCHES {
                            let (r, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                            if resp.hovered() {
                                ui.painter().circle_stroke(r.center(), 9.0, egui::Stroke::new(1.5, th.foreground));
                            }
                            ui.painter().circle_filled(r.center(), 6.5, parse_color(c));
                            if resp.on_hover_text(c).clicked() {
                                *chosen.borrow_mut() = Some(Act::SetColor(bulk.clone(), c.to_string()));
                                done = true;
                            }
                        }
                    });
                    ui.separator();
                    done |= item(ui, &del_label, "Del", true, Act::Delete(ids.clone()));
                    done
                })
            }
            MenuKind::Snippet(None) => popup_menu(ctx, id, m.pos, |ui| item(ui, "New snippet", "", true, Act::NewSnippet)),
            MenuKind::Snippet(Some(sid)) => {
                let sid = sid.clone();
                popup_menu(ctx, id, m.pos, |ui| {
                    let mut done = false;
                    done |= item(ui, "Run", "Enter", true, Act::RunSnippet(sid.clone()));
                    done |= item(ui, "Edit", "F2", true, Act::EditSnippet(sid.clone()));
                    ui.separator();
                    done |= item(ui, "New snippet", "", true, Act::NewSnippet);
                    done |= item(ui, "Delete", "Del", true, Act::DeleteSnippet(sid.clone()));
                    done
                })
            }
            MenuKind::Shell(pid) => {
                let pid = pid.clone();
                popup_menu(ctx, id, m.pos, |ui| {
                    let mut done = false;
                    done |= item(ui, "Open in a new tab", "", true, Act::OpenShell(pid.clone()));
                    done |= item(ui, "Save as session", "", true, Act::SaveShell(pid.clone()));
                    ui.separator();
                    done |= item(ui, "Refresh shells", "", true, Act::RefreshShells);
                    done
                })
            }
        };
        let act = chosen.into_inner();
        if keep && act.is_none() {
            self.menu = Some(m);
        }
        if let Some(a) = act {
            self.run(a, x);
        }
    }
}
