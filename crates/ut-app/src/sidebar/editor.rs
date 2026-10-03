//! Session edit dialog (§8.3): every field, inline validation, colour tag swatches + custom colour, environment editor
//! with per-line problems. The store validates again on save; its message is shown inline.

use super::menus::SWATCHES;
use crate::theme::{self, parse_color, Theme};
use egui::text::{CCursor, CCursorRange};
use egui::{Align, Color32, Context, Id, Key, Layout, Modifiers, RichText, Sense, Stroke, TextEdit, Ui, Vec2};
use std::path::Path;
use ut_data::{Integration, Session, SessionStore};
use ut_shell::ShellProfile;

pub enum Outcome {
    Open,
    Saved(Session),
    Cancelled,
}

#[derive(Default, Debug, PartialEq)]
pub struct Errors {
    pub name: String,
    pub path: String,
    pub bg: String,
    pub save: String,
}

impl Errors {
    fn any(&self) -> bool {
        !(self.name.is_empty() && self.path.is_empty() && self.bg.is_empty())
    }
}

/// Field checks that block saving (§8.3): name and path are required, the background must be a colour.
pub fn check(s: &Session) -> Errors {
    let mut e = Errors::default();
    if s.name.trim().is_empty() {
        e.name = "Name is required.".into();
    }
    if s.shell_path.trim().is_empty() {
        e.path = "Shell path is required.".into();
    }
    let bg = s.theme_background.trim();
    if !bg.is_empty() && !is_hex_color(bg) {
        e.bg = "Use a colour like #1e1e2e.".into();
    }
    e
}

pub fn is_hex_color(s: &str) -> bool {
    s.trim().strip_prefix('#').is_some_and(|h| matches!(h.len(), 3 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit()))
}

pub fn hex_of(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn rgb_of(c: &str) -> [u8; 3] {
    let c: Color32 = parse_color(c);
    [c.r(), c.g(), c.b()]
}

/// The detected shell whose path and arguments equal these (the "Preset shell" re-syncs on every edit, §8.3 [P1]).
pub fn profile_for<'a>(shells: &'a [ShellProfile], path: &str, args: &str) -> Option<&'a ShellProfile> {
    shells.iter().find(|p| p.path.eq_ignore_ascii_case(path.trim()) && p.arguments.trim() == args.trim())
}

/// A blank session on the default shell, optionally inside a folder.
pub fn new_session(shell: Option<&ShellProfile>, folder: Option<String>) -> Session {
    Session { shell_path: shell.map(|p| p.path.clone()).unwrap_or_default(), arguments: shell.map(|p| p.arguments.clone()).unwrap_or_default(), folder_id: folder, ..Default::default() }
}

#[derive(Default)]
struct Warnings {
    key: (String, String, String),
    path: Option<String>,
    cwd: Option<String>,
}

pub struct SessionEditor {
    pub is_new: bool,
    s: Session,
    name_touched: bool,
    presets: Vec<String>,
    bg_rgb: [u8; 3],
    errors: Errors,
    warn: Warnings,
    /// Frames left in which the name field grabs focus and selects its text (the text state only exists after the
    /// first frame).
    focus_name: u8,
    submit: bool,
}

impl SessionEditor {
    pub fn new(session: Session, is_new: bool, presets: Vec<String>) -> Self {
        let bg_rgb = rgb_of(if session.theme_background.is_empty() { "#000000" } else { &session.theme_background });
        Self { is_new, s: session, name_touched: !is_new, presets, bg_rgb, errors: Errors::default(), warn: Warnings::default(), focus_name: 2, submit: false }
    }

    /// Non-blocking hints (§8.3 [P1]): a path that does not resolve or a missing working directory. Recomputed only when
    /// one of the three fields changed.
    fn refresh_warnings(&mut self) {
        let key = (self.s.shell_path.clone(), self.s.arguments.clone(), self.s.working_directory.clone());
        if key == self.warn.key {
            return;
        }
        let path = (!self.s.shell_path.trim().is_empty() && ut_shell::resolve_exe(&self.s.full_command()).is_none())
            .then(|| "This path does not resolve to an existing program; the session may fail to start.".to_string());
        let wd = self.s.working_directory.trim();
        let cwd = (!wd.is_empty() && !Path::new(&ut_data::expand_vars(wd, &ut_data::process_env())).is_dir()).then(|| "This working directory does not exist.".to_string());
        self.warn = Warnings { key, path, cwd };
    }

    fn apply_profile(&mut self, p: &ShellProfile) {
        self.s.shell_path = p.path.clone();
        self.s.arguments = p.arguments.clone();
        self.s.color_tag = p.color.clone();
        if !self.name_touched {
            self.s.name = p.name.clone();
        }
        self.errors.path.clear();
    }

    /// Normalise the text fields and hand the session to the store; its error text is shown inline.
    fn try_save(&mut self, store: &SessionStore) -> Option<Session> {
        let mut s = self.s.clone();
        for f in [&mut s.name, &mut s.shell_path, &mut s.arguments, &mut s.working_directory, &mut s.starting_command, &mut s.theme_background] {
            *f = f.trim().to_string();
        }
        self.errors = check(&s);
        if self.errors.any() {
            return None;
        }
        let r = if self.is_new { store.add_session(s) } else { store.update_session(s) };
        match r {
            Ok(saved) => Some(saved),
            Err(e) => {
                self.errors.save = e.to_string();
                None
            }
        }
    }

    pub fn show(&mut self, ctx: &Context, theme: &Theme, store: &SessionStore, shells: &[ShellProfile]) -> Outcome {
        self.refresh_warnings();
        let th = &theme.ui;
        let screen = ctx.content_rect();
        let width = (screen.width() - 40.0).clamp(320.0, 580.0);
        let max_h = (screen.height() - 170.0).max(160.0);
        let mut cancel = false;
        let modal = egui::Modal::new(Id::new("sb-session-editor")).show(ctx, |ui| {
            ui.set_width(width);
            // Floating bars overlay the fields instead of reserving a column next to them.
            ui.spacing_mut().scroll = egui::style::ScrollStyle::floating();
            ui.label(RichText::new(if self.is_new { "New Session" } else { "Edit Session" }).strong().size(15.0));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| self.form(ui, th, shells));
            ui.add_space(6.0);
            if !self.errors.save.is_empty() {
                ui.label(RichText::new(&self.errors.save).color(th.error));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if dialog_button(ui, th, "Save", true) {
                    self.submit = true;
                }
                if dialog_button(ui, th, "Cancel", false) {
                    cancel = true;
                }
            });
            if ui.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Enter)) {
                self.submit = true;
            }
        });
        // Esc closes (unless a combo/colour popup is open and takes it first); a click on the backdrop must not throw
        // the edits away.
        if cancel || (modal.is_top_modal && !modal.any_popup_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))) {
            return Outcome::Cancelled;
        }
        if std::mem::take(&mut self.submit) {
            if let Some(saved) = self.try_save(store) {
                return Outcome::Saved(saved);
            }
        }
        Outcome::Open
    }

    fn form(&mut self, ui: &mut Ui, th: &theme::Ui, shells: &[ShellProfile]) {
        ui.spacing_mut().item_spacing.y = 4.0;
        let w = ui.available_width();
        let enter = |ui: &Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

        // 1. name
        field_label(ui, th, "Name *");
        let r = ui.add(TextEdit::singleline(&mut self.s.name).desired_width(w));
        if self.focus_name > 0 {
            r.request_focus();
            if let Some(mut st) = TextEdit::load_state(ui.ctx(), r.id) {
                st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(self.s.name.chars().count()))));
                st.store(ui.ctx(), r.id);
            }
            self.focus_name -= 1;
        }
        if r.changed() {
            self.name_touched = true;
            self.errors.name.clear();
        }
        self.submit |= enter(ui, &r);
        error_label(ui, th, &self.errors.name);

        // 2. description
        field_label(ui, th, "Description");
        ui.add(TextEdit::multiline(&mut self.s.description).desired_rows(2).desired_width(w));

        // 3. preset shell
        field_label(ui, th, "Preset shell");
        let cur = profile_for(shells, &self.s.shell_path, &self.s.arguments).map(|p| p.id.clone());
        let mut pick: Option<ShellProfile> = None;
        egui::ComboBox::from_id_salt("sb-preset")
            .width(ui.available_width())
            .selected_text(cur.as_ref().and_then(|id| shells.iter().find(|p| &p.id == id)).map_or("— Custom —", |p| p.name.as_str()))
            .show_ui(ui, |ui| {
                // "Custom" is the state of any path that is not a detected shell: choosing it changes nothing.
                let _ = ui.selectable_label(cur.is_none(), "— Custom —");
                for p in shells {
                    if ui.selectable_label(cur.as_deref() == Some(p.id.as_str()), &p.name).clicked() {
                        pick = Some(p.clone());
                    }
                }
            });
        if let Some(p) = pick {
            self.apply_profile(&p);
        }

        // 4. shell path
        field_label(ui, th, "Shell path *");
        ui.horizontal(|ui| {
            let w = ui.available_width() - 84.0;
            let r = ui.add(TextEdit::singleline(&mut self.s.shell_path).desired_width(w).hint_text(r"e.g. C:\Program Files\PowerShell\7\pwsh.exe"));
            if r.changed() {
                self.errors.path.clear();
            }
            self.submit |= enter(ui, &r);
            if ui.button("Browse…").clicked() {
                let mut d = rfd::FileDialog::new().set_title("Select shell").add_filter("Executables", &["exe"]).add_filter("All files", &["*"]);
                if let Some(dir) = Path::new(self.s.shell_path.trim().trim_matches('"')).parent().filter(|p| p.is_dir()) {
                    d = d.set_directory(dir);
                }
                if let Some(p) = d.pick_file() {
                    self.s.shell_path = p.to_string_lossy().into_owned();
                    self.errors.path.clear();
                }
            }
        });
        error_label(ui, th, &self.errors.path);
        warn_label(ui, th, self.warn.path.as_deref());

        // 5. arguments
        field_label(ui, th, "Arguments");
        ui.add(TextEdit::singleline(&mut self.s.arguments).desired_width(w));

        // 6. working directory
        field_label(ui, th, "Working directory");
        ui.horizontal(|ui| {
            let w = ui.available_width() - 84.0;
            let r = ui.add(TextEdit::singleline(&mut self.s.working_directory).desired_width(w).hint_text("blank = your home folder"));
            self.submit |= enter(ui, &r);
            if ui.button("Browse…").clicked() {
                let mut d = rfd::FileDialog::new().set_title("Select working directory");
                if Path::new(self.s.working_directory.trim()).is_dir() {
                    d = d.set_directory(self.s.working_directory.trim());
                }
                if let Some(p) = d.pick_folder() {
                    self.s.working_directory = p.to_string_lossy().into_owned();
                }
            }
        });
        warn_label(ui, th, self.warn.cwd.as_deref());

        // 7. starting command
        field_label(ui, th, "Starting command");
        let r = ui.add(TextEdit::singleline(&mut self.s.starting_command).desired_width(w));
        self.submit |= enter(ui, &r);
        ui.label(RichText::new("Runs after shell starts (e.g. ssh user@host, cd /project)").small().color(th.muted));

        // 8. theme override
        ui.add_space(6.0);
        ui.label(RichText::new("THEME OVERRIDE").small().strong().color(th.accent));
        ui.columns(3, |cols| {
            let ui = &mut cols[0];
            field_label(ui, th, "Background colour");
            ui.horizontal(|ui| {
                // The picker button is `interact_size` wide: a wider row would stretch every column of `columns`.
                let field_w = ui.available_width() - ui.spacing().interact_size.x - ui.spacing().item_spacing.x;
                let r = ui.add(TextEdit::singleline(&mut self.s.theme_background).desired_width(field_w).hint_text("none"));
                if r.changed() {
                    self.errors.bg.clear();
                    if is_hex_color(&self.s.theme_background) {
                        self.bg_rgb = rgb_of(self.s.theme_background.trim());
                    }
                }
                if ui.color_edit_button_srgb(&mut self.bg_rgb).changed() {
                    self.s.theme_background = hex_of(self.bg_rgb);
                    self.errors.bg.clear();
                }
            });
            error_label(ui, th, &self.errors.bg);

            let ui = &mut cols[1];
            field_label(ui, th, "Font size");
            ui.add(egui::DragValue::new(&mut self.s.font_size).range(0..=32).speed(0.2));
            ui.label(RichText::new("0 = global (otherwise 8–32)").small().color(th.muted));

            let ui = &mut cols[2];
            field_label(ui, th, "Theme preset");
            let shown = if self.s.theme_preset.is_empty() { "— None —".to_string() } else { self.s.theme_preset.clone() };
            egui::ComboBox::from_id_salt("sb-theme-preset").width(ui.available_width()).selected_text(shown).show_ui(ui, |ui| {
                if ui.selectable_label(self.s.theme_preset.is_empty(), "— None —").clicked() {
                    self.s.theme_preset.clear();
                }
                // A preset that no longer exists stays selectable so opening and saving never drops it silently.
                let mut names = self.presets.clone();
                if !self.s.theme_preset.is_empty() && !names.contains(&self.s.theme_preset) {
                    names.push(self.s.theme_preset.clone());
                }
                for n in names {
                    if ui.selectable_label(self.s.theme_preset == n, &n).clicked() {
                        self.s.theme_preset = n;
                    }
                }
            });
        });

        // 9. colour tag
        ui.add_space(6.0);
        field_label(ui, th, "Colour tag");
        ui.horizontal(|ui| {
            for c in SWATCHES {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                let on = self.s.color_tag.eq_ignore_ascii_case(c);
                ui.painter().circle_filled(r.center(), if resp.hovered() { 10.5 } else { 9.5 }, parse_color(c));
                if on {
                    ui.painter().circle_stroke(r.center(), 11.5, Stroke::new(2.0, th.foreground));
                }
                if resp.on_hover_text(c).clicked() {
                    self.s.color_tag = c.to_string();
                }
            }
            // A colour that is not one of the swatches (custom or imported) is kept as it is (§24 #17).
            let custom = !SWATCHES.iter().any(|c| self.s.color_tag.eq_ignore_ascii_case(c));
            let mut rgb = rgb_of(&self.s.color_tag);
            let r = ui.color_edit_button_srgb(&mut rgb).on_hover_text("Custom colour");
            if custom {
                ui.painter().rect_stroke(r.rect.expand(2.0), 4.0, Stroke::new(2.0, th.foreground), egui::StrokeKind::Outside);
            }
            if r.changed() {
                self.s.color_tag = hex_of(rgb);
            }
            ui.label(RichText::new(&self.s.color_tag).monospace().small().color(th.muted));
        });

        // 10. environment
        ui.add_space(6.0);
        field_label(ui, th, "Environment variables");
        ui.add(TextEdit::multiline(&mut self.s.environment).font(egui::TextStyle::Monospace).desired_rows(4).desired_width(w).hint_text("KEY=VALUE, one per line"));
        for p in ut_data::validate_env(&self.s.environment) {
            ui.label(RichText::new(format!("Line {}: {}", p.line, p.message)).small().color(th.warning));
        }

        // 11. shell integration (+ the default launch mode)
        ui.add_space(6.0);
        field_label(ui, th, "Shell integration");
        egui::ComboBox::from_id_salt("sb-integration").selected_text(if self.s.integration == Integration::Off { "Off" } else { "Auto" }).show_ui(ui, |ui| {
            ui.selectable_value(&mut self.s.integration, Integration::Auto, "Auto");
            ui.selectable_value(&mut self.s.integration, Integration::Off, "Off");
        });
        ui.checkbox(&mut self.s.run_as_admin, "Run as administrator by default");
    }
}

pub(super) fn field_label(ui: &mut Ui, th: &theme::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(th.muted));
}

pub(super) fn error_label(ui: &mut Ui, th: &theme::Ui, text: &str) {
    if !text.is_empty() {
        ui.label(RichText::new(text).small().color(th.error));
    }
}

fn warn_label(ui: &mut Ui, th: &theme::Ui, text: Option<&str>) {
    if let Some(t) = text {
        ui.label(RichText::new(t).small().color(th.warning));
    }
}

/// Same look as the buttons of `kit::Dialog`.
pub(super) fn dialog_button(ui: &mut Ui, th: &theme::Ui, label: &str, primary: bool) -> bool {
    let mut b = egui::Button::new(RichText::new(label).color(if primary { th.tab_selected_fg } else { th.foreground })).min_size(Vec2::new(76.0, 26.0));
    if primary {
        b = b.fill(th.accent);
    }
    ui.add(b).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ut_shell::ShellKind;

    fn prof(id: &str, path: &str, args: &str) -> ShellProfile {
        ShellProfile { id: id.into(), name: id.into(), path: path.into(), arguments: args.into(), color: "#00e5ff".into(), kind: ShellKind::PowerShell, is_default: false, command: String::new() }
    }

    #[test]
    fn required_fields_and_background() {
        let ok = Session { name: "n".into(), shell_path: "x.exe".into(), ..Default::default() };
        assert_eq!(check(&ok), Errors::default());
        let bad = Session { name: "  ".into(), shell_path: "".into(), theme_background: "blue".into(), ..Default::default() };
        let e = check(&bad);
        assert_eq!((e.name.as_str(), e.path.as_str(), e.bg.as_str()), ("Name is required.", "Shell path is required.", "Use a colour like #1e1e2e."));
        assert!(e.any());
        for good in ["#fff", "#1e1e2e", " #1E1E2EFF ", ""] {
            assert!(check(&Session { theme_background: good.into(), ..ok.clone() }).bg.is_empty(), "{good:?}");
        }
        for bad in ["1e1e2e", "#12", "#12345", "#gggggg", "rgb(1,2,3)"] {
            assert!(!is_hex_color(bad), "{bad:?}");
        }
    }

    #[test]
    fn preset_follows_path_and_arguments() {
        let shells = [prof("wsl", r"C:\Windows\System32\wsl.exe", ""), prof("wsl:Ubuntu", r"C:\Windows\System32\wsl.exe", "-d \"Ubuntu\"")];
        assert_eq!(profile_for(&shells, r"c:\windows\system32\WSL.EXE ", "  ").map(|p| p.id.as_str()), Some("wsl"));
        assert_eq!(profile_for(&shells, r"C:\Windows\System32\wsl.exe", "-d \"Ubuntu\" ").map(|p| p.id.as_str()), Some("wsl:Ubuntu"));
        assert!(profile_for(&shells, r"C:\Windows\System32\wsl.exe", "-d Other").is_none());
        assert!(profile_for(&[], "x", "").is_none());
    }

    #[test]
    fn new_sessions_start_on_the_default_shell_in_the_chosen_folder() {
        let p = prof("pwsh", r"C:\pwsh.exe", "-NoLogo");
        let s = new_session(Some(&p), Some("f1".into()));
        assert_eq!((s.shell_path.as_str(), s.arguments.as_str(), s.folder_id.as_deref(), s.name.as_str()), (r"C:\pwsh.exe", "-NoLogo", Some("f1"), "New Session"));
        assert_eq!(s.color_tag, ut_data::DEFAULT_COLOR);
        assert_eq!(new_session(None, None).shell_path, "");
    }

    #[test]
    fn colour_helpers_round_trip_and_keep_unknown_formats_usable() {
        assert_eq!(hex_of([0, 229, 255]), "#00e5ff");
        assert_eq!(rgb_of("#00e5ff"), [0, 229, 255]);
        assert_eq!(rgb_of("#0ef"), [0, 238, 255]);
        assert_eq!(rgb_of("garbage"), [0, 0, 0]);
    }

    #[test]
    fn saving_trims_adds_and_updates_through_the_store() {
        let d = std::env::temp_dir().join(format!("ut sidebar editor {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let w = std::sync::Arc::new(ut_fs::DebouncedWriter::new(|p, e| panic!("{p:?}: {e}")));
        let store = SessionStore::open(d.join("sessions.json"), w);
        let mut ed = SessionEditor::new(new_session(None, None), true, vec![]);
        assert!(ed.try_save(&store).is_none(), "blank path is rejected");
        assert!(ed.errors.path.contains("required"));
        ed.s.name = "  Box ".into();
        ed.s.shell_path = " ssh.exe ".into();
        ed.s.font_size = 99;
        ed.s.color_tag = "#123abc".into();
        let saved = ed.try_save(&store).expect("saved");
        assert_eq!((saved.name.as_str(), saved.shell_path.as_str(), saved.font_size, saved.color_tag.as_str()), ("Box", "ssh.exe", 0, "#123abc"));
        // editing the same session updates it in place
        let mut ed = SessionEditor::new(saved.clone(), false, vec![]);
        ed.s.name = "Box2".into();
        ed.try_save(&store).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.get(&saved.id).unwrap().name, "Box2");
        // a session deleted meanwhile surfaces the store's error instead of panicking
        store.delete_session(&saved.id);
        assert!(ed.try_save(&store).is_none());
        assert!(!ed.errors.save.is_empty());
    }
}
