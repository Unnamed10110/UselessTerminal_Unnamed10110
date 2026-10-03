//! The plain pages of the settings window: one function per page, each a list of `Form` rows bound to §14.2 paths.

use super::capture::{Outcome, Recorder, Scope};
use super::fontpick;
use super::form::Form;
use super::keys::chord_label;
use crate::kit::ToastKind;
use crate::panels::PanelCtx;
use egui::{Id, RichText, Ui};
use ut_core::Settings;
use ut_shell::ShellProfile;

/// Background images: png, jpg, jpeg, gif, webp, bmp up to 15 MiB (§5.12). `Ok(())` for an empty path (no image).
pub fn check_image(path: &str) -> Result<(), String> {
    let p = path.trim();
    if p.is_empty() {
        return Ok(());
    }
    let ext_ok = std::path::Path::new(p).extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"));
    if !ext_ok {
        return Err("Unsupported format. Use png, jpg, gif, webp or bmp.".into());
    }
    match std::fs::metadata(p) {
        Err(_) => Err("The file does not exist.".into()),
        Ok(m) if !m.is_file() => Err("That is not a file.".into()),
        Ok(m) if m.len() > 15 * 1024 * 1024 => Err("The image is larger than 15 MiB.".into()),
        Ok(_) => Ok(()),
    }
}

/// How `settings.json` was found at startup, in words (About page).
pub fn load_status_text(s: &ut_core::LoadStatus, exists: bool) -> String {
    use ut_core::LoadStatus::*;
    match s {
        Loaded => "settings.json was loaded at startup.".into(),
        Missing if exists => "settings.json was created by this session (first run).".into(),
        Missing => "First run: defaults are in use; settings.json is written with your first change.".into(),
        Migrated { backup } => format!("Settings were migrated from the WPF version. Backup: {backup}"),
        Corrupt { error } => format!("settings.json could not be read ({error}); defaults are in use and the file was left untouched."),
    }
}

/// A merge patch that resets every top-level section (everything but `schemaVersion`) to its defaults.
pub fn reset_patch(doc: &serde_json::Value) -> serde_json::Value {
    let keys = doc.as_object().map(|o| o.keys().filter(|k| *k != "schemaVersion").cloned().collect::<Vec<_>>()).unwrap_or_default();
    serde_json::Value::Object(keys.into_iter().map(|k| (k, serde_json::Value::Null)).collect())
}

fn note(ui: &mut Ui, f: &Form, text: &str) {
    ui.label(RichText::new(text).small().color(f.th.ui.muted));
}

// ------------------------------------------------------------------------------------------ terminal

pub fn terminal(ui: &mut Ui, x: &mut PanelCtx, f: &mut Form, _cfg: &Settings) {
    let th = f.th;
    Form::group(ui, th, "Font", |ui| {
        Form::grid(ui, "term-font", |ui| {
            let stack = f.s("terminal.fontFamily").to_string();
            Form::row(ui, th, "Font family", "Installed fonts; monospace only by default", |ui| {
                if let Some(name) = fontpick::picker(ui, Id::new("term-font-picker"), &fontpick::primary(&stack), true) {
                    f.patch.set("terminal.fontFamily", fontpick::stack_for(&name));
                }
            });
            f.text(ui, "terminal.fontFamily", "Font stack", "First installed family wins; any CSS-style list works", 380.0, |s| {
                if s.trim().is_empty() { Err("The font stack can't be empty.".into()) } else { Ok(s.trim().to_string()) }
            });
            f.slider(ui, "terminal.fontSize", "Size", "8 – 32 pt. Ctrl+wheel over a terminal zooms.", 8.0..=32.0, 1.0, " pt");
            f.int_slider(ui, "terminal.fontWeight", "Weight", "Bold text uses weight + 250", 300..=700, 50, "");
            f.slider(ui, "terminal.lineHeight", "Line height", "", 0.8..=2.0, 0.05, "×");
            f.slider(ui, "terminal.letterSpacing", "Letter spacing", "", -5.0..=20.0, 0.5, " px");
        });
        if !x.fonts.missing.is_empty() {
            ui.label(RichText::new(format!("Not installed (skipped): {}", x.fonts.missing.join(", "))).small().color(th.ui.warning));
        }
    });
    Form::group(ui, th, "Cursor and scrollback", |ui| {
        Form::grid(ui, "term-cursor", |ui| {
            f.choice(ui, "terminal.cursorStyle", "Cursor style", "", &[("bar", "Bar"), ("block", "Block"), ("underline", "Underline")]);
            f.check(ui, "terminal.cursorBlink", "Blinking cursor", "");
            f.drag_int(ui, "terminal.scrollback", "Scrollback", "0 – 200 000 lines. Applies to new tabs.", 0..=200_000, 100.0, " lines");
        });
    });
    egui::CollapsingHeader::new("Renderer compatibility").id_salt("renderer-compat").show(ui, |ui| {
        note(ui, f, "Kept for settings.json compatibility with the web renderer. The native renderer does not use them.");
        Form::grid(ui, "term-renderer", |ui| {
            f.choice(ui, "terminal.renderer", "Renderer", "", &[("webgl", "WebGL"), ("dom", "DOM")]);
            f.choice(ui, "terminal.rendererRepair", "Redraw repair", "", &[("auto", "Auto"), ("on", "On"), ("off", "Off")]);
        });
    });
}

// ------------------------------------------------------------------------------------------ interface

pub fn interface(ui: &mut Ui, x: &mut PanelCtx, f: &mut Form, cfg: &Settings) {
    let th = f.th;
    Form::group(ui, th, "Interface font and size", |ui| {
        Form::grid(ui, "ui-font", |ui| {
            Form::row(ui, th, "UI font", "Tabs, panels and dialogs", |ui| {
                if let Some(name) = fontpick::picker(ui, Id::new("ui-font-picker"), &cfg.ui.font_family, false) {
                    f.patch.set("ui.fontFamily", name);
                }
            });
            f.text(ui, "ui.fontFamily", "Family name", "", 260.0, |s| if s.trim().is_empty() { Err("The UI font can't be empty.".into()) } else { Ok(s.trim().to_string()) });
            f.slider(ui, "ui.fontSize", "Size", "10 – 22", 10.0..=22.0, 1.0, " px");
            f.int_slider(ui, "ui.fontWeight", "Weight", "", 300..=700, 50, "");
            f.percent_slider(ui, "ui.scale", "Scale", "75 – 200 %. Ctrl+wheel over the tabs, the sessions panel or the status bar.", 75.0..=200.0, 5.0, true);
            f.choice(ui, "ui.backdrop", "Window backdrop", "Mica / acrylic need Windows 11", &[("none", "None"), ("mica", "Mica"), ("acrylic", "Acrylic")]);
        });
    });
    super::theme_ui::ui_tokens(ui, x, f, cfg);
}

// ------------------------------------------------------------------------------------------ background

pub fn background(ui: &mut Ui, _x: &mut PanelCtx, f: &mut Form, cfg: &Settings) {
    let th = f.th;
    Form::group(ui, th, "Shell background image", |ui| {
        Form::grid(ui, "bg-image", |ui| {
            f.text(ui, "terminal.backgroundImage.path", "Image", "png, jpg, gif, webp or bmp up to 15 MiB", 380.0, |s| check_image(s).map(|()| s.trim().to_string()));
            Form::row(ui, th, "", "", |ui| {
                if ui.button("Browse…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "bmp"]).pick_file() {
                        match check_image(&p.to_string_lossy()) {
                            Ok(()) => f.patch.set("terminal.backgroundImage.path", p.to_string_lossy().into_owned()),
                            Err(e) => f.errors.push(e),
                        }
                    }
                }
                if ui.add_enabled(!cfg.terminal.background_image.path.is_empty(), egui::Button::new("Clear")).clicked() {
                    f.patch.set("terminal.backgroundImage.path", "");
                }
                if let Err(e) = check_image(&cfg.terminal.background_image.path) {
                    ui.label(RichText::new(e).small().color(th.ui.error));
                }
            });
            f.percent_slider(ui, "terminal.backgroundImage.opacity", "Image opacity", "0 – 100 %. The theme background is laid over it.", 0.0..=100.0, 1.0, false);
        });
    });
    Form::group(ui, th, "Effects", |ui| {
        Form::grid(ui, "effects", |ui| {
            f.check(ui, "terminal.minimap", "Minimap scrollbar", "An overview strip of the scrollback on the right edge");
            f.check(ui, "terminal.crt", "Retro CRT mode", "Scanlines, vignette and a faint glow");
        });
    });
}

// ------------------------------------------------------------------------------------------- behavior

pub fn behavior(ui: &mut Ui, _x: &mut PanelCtx, f: &mut Form, _cfg: &Settings) {
    let th = f.th;
    Form::group(ui, th, "Copy and paste", |ui| {
        Form::grid(ui, "copy-paste", |ui| {
            f.check(ui, "terminal.copyOnSelect", "Copy on select", "Selecting text copies it immediately");
            f.check(ui, "terminal.clearSelectionOnCopy", "Clear selection after copy", "");
            f.check(ui, "terminal.trimTrailingWhitespaceOnCopy", "Trim trailing spaces", "");
            f.choice(ui, "terminal.multiLinePasteWarning", "Multi-line paste warning", "Auto: only when the program has no bracketed paste", &[("auto", "Auto"), ("always", "Always"), ("never", "Never")]);
            f.choice(ui, "terminal.pasteImages", "Pasting an image", "", &[("passThroughCtrlV", "Send Ctrl+V to the program"), ("inlinePreview", "Show it inline")]);
            f.choice(ui, "terminal.osc52", "Clipboard access by programs (OSC 52)", "", &[("off", "Off"), ("write", "Write only"), ("readwrite", "Read and write")]);
        });
    });
    Form::group(ui, th, "Mouse and zoom", |ui| {
        Form::grid(ui, "mouse", |ui| {
            f.choice(ui, "terminal.rightClick", "Right click", "", &[("menu", "Context menu"), ("paste", "Paste"), ("copyPaste", "Copy / paste")]);
            f.choice(ui, "terminal.zoomScope", "Font zoom applies to", "", &[("global", "All panes"), ("pane", "The pane only")]);
        });
    });
    Form::group(ui, th, "Bell", |ui| {
        Form::grid(ui, "bell", |ui| {
            f.check(ui, "terminal.bell.visual", "Visual bell", "A short flash of the pane");
            f.check(ui, "terminal.bell.audible", "Audible bell", "");
            f.check(ui, "terminal.bell.flashTaskbar", "Flash the taskbar", "When the window is in the background");
        });
    });
    Form::group(ui, th, "Closing", |ui| {
        Form::grid(ui, "closing", |ui| {
            f.choice(ui, "terminal.closeOnExit", "When the shell exits", "", &[("never", "Keep the pane"), ("graceful", "Close on exit code 0"), ("always", "Always close")]);
            f.choice(ui, "processes.closeConfirm", "Confirm before closing", "", &[("whenRunning", "When something is running"), ("always", "Always"), ("never", "Never")]);
            f.check(ui, "processes.killConsoleTreeOnClose", "Kill the process tree on close", "Child processes die with their pane");
        });
    });
    Form::group(ui, th, "Notifications", |ui| {
        Form::grid(ui, "notify", |ui| {
            f.check(ui, "notifications.commandFinished.enabled", "Command finished", "Flash the taskbar when a long command ends in the background");
            f.drag_int(ui, "notifications.commandFinished.minDurationSec", "Only after", "", 0..=86_400, 1.0, " s");
            f.check(ui, "notifications.commandFinished.toast", "Show a toast", "");
        });
    });
    Form::group(ui, th, "Shell integration", |ui| {
        Form::grid(ui, "integration", |ui| {
            f.check(ui, "terminal.overridePsReadLineColors", "Colour PowerShell input", "Typed text uses your typed-input colour (Theme page)");
        });
    });
}

// ----------------------------------------------------------------------------------- shells & startup

pub fn shells(ui: &mut Ui, x: &mut PanelCtx, f: &mut Form, cfg: &Settings, profiles: &mut Vec<ShellProfile>) {
    let th = f.th;
    Form::group(ui, th, "Default shell", |ui| {
        Form::grid(ui, "default-shell", |ui| {
            let mut opts: Vec<(String, String)> = vec![("auto".into(), "Automatic (PowerShell 7 if installed, else Windows PowerShell)".into())];
            opts.extend(profiles.iter().map(|p| (p.id.clone(), format!("{}  —  {}", p.name, p.path))));
            f.combo(ui, "shells.defaultProfile", "New tabs open", "Used by Ctrl+T and the + button", &opts);
            Form::row(ui, th, "", "", |ui| {
                if ui.button("Re-detect shells").clicked() {
                    *profiles = x.core.shells.get(true);
                }
                if !profiles.iter().any(|p| p.id.eq_ignore_ascii_case(&cfg.shells.default_profile)) && !cfg.shells.default_profile.eq_ignore_ascii_case("auto") {
                    ui.label(RichText::new(format!("\"{}\" was not detected; the automatic choice is used.", cfg.shells.default_profile)).small().color(th.ui.warning));
                }
            });
            f.choice(ui, "terminal.conptyImplementation", "ConPTY", "Bundled conpty.dll next to the exe, or the Windows one. New tabs.", &[("auto", "Auto"), ("bundled", "Bundled"), ("system", "System")]);
            f.check(ui, "terminal.refreshEnvironment", "Fresh environment", "Shells start with the current registry environment (new PATH entries without logging off)");
        });
    });
    Form::group(ui, th, "Startup", |ui| {
        Form::grid(ui, "startup", |ui| {
            f.choice(ui, "startup.mode", "On launch", "", &[("restoreLastSession", "Restore last session"), ("workspace", "Open a workspace"), ("defaultTab", "One default tab")]);
            if cfg.startup.mode == ut_core::settings::StartupMode::Workspace {
                let mut opts = vec![(String::new(), "(none)".to_string())];
                opts.extend(x.core.workspaces.list().into_iter().map(|w| (w.id, w.name)));
                let cur = cfg.startup.workspace_id.clone().unwrap_or_default();
                let shown = opts.iter().find(|(id, _)| *id == cur).map(|(_, n)| n.clone()).unwrap_or_else(|| "(missing workspace)".into());
                Form::row(ui, th, "Workspace", "Saved from the command palette", |ui| {
                    egui::ComboBox::from_id_salt("startup-workspace").selected_text(shown).width(260.0).show_ui(ui, |ui| {
                        for (id, name) in &opts {
                            if ui.selectable_label(*id == cur, name).clicked() && *id != cur {
                                if id.is_empty() {
                                    f.patch.reset("startup.workspaceId");
                                } else {
                                    f.patch.set("startup.workspaceId", id.clone());
                                }
                            }
                        }
                    });
                });
            }
            f.check(ui, "startup.lazyRestore", "Lazy restore", "Start only the active tab at launch; the others start in the background");
        });
    });
    Form::group(ui, th, "Opening files from the terminal", |ui| {
        Form::grid(ui, "links", |ui| {
            f.text(ui, "links.editorCommand", "Editor command", "{file} and {line} are replaced; e.g. code --goto \"{file}:{line}\"", 380.0, |s| if s.trim().is_empty() { Err("The editor command can't be empty.".into()) } else { Ok(s.trim().to_string()) });
        });
    });
}

// ---------------------------------------------------------------------------------------- window

pub fn window(ui: &mut Ui, x: &mut PanelCtx, f: &mut Form, cfg: &Settings, rec: &mut Recorder) {
    let th = f.th;
    if let Some(out) = rec.step(ui, Scope::Global) {
        if let Outcome::Chord(c) = out {
            let chord = c.to_string();
            match crate::winhooks::parse_hotkey(&chord) {
                Ok(_) => f.patch.set("quake.hotkey", chord),
                Err(e) => {
                    rec.active = true;
                    rec.notice = Some(e);
                }
            }
        }
    }
    Form::group(ui, th, "Quake mode", |ui| {
        Form::grid(ui, "quake", |ui| {
            Form::row(ui, th, "Global hotkey", "Shows or hides the window from anywhere", |ui| {
                if rec.active {
                    let held = rec.held();
                    ui.label(RichText::new(if held.is_empty() { "Press the new hotkey…".to_string() } else { format!("{held}…") }).color(th.ui.accent));
                    if ui.button("Cancel").clicked() {
                        rec.stop();
                    }
                } else {
                    ui.label(RichText::new(chord_label(&cfg.quake.hotkey)).monospace().strong());
                    if ui.button("Change…").clicked() {
                        rec.start();
                    }
                    if ui.add_enabled(cfg.quake.hotkey != "Win+Backquote", egui::Button::new("Reset")).clicked() {
                        f.patch.reset("quake.hotkey");
                    }
                }
            });
            if let (true, Some(n)) = (rec.active, rec.notice.clone()) {
                Form::row(ui, th, "", "", |ui| {
                    ui.set_max_width(340.0); // wrap: a long message must not widen the grid (and the window)
                    ui.label(RichText::new(n).small().color(th.ui.error));
                });
            }
            f.check(ui, "quake.dropdown", "Drop-down from the top", "Slides down over the monitor under the cursor instead of toggling in place");
            f.drag_int(ui, "quake.heightPercent", "Drop-down height", "10 – 100 % of the work area", 10..=100, 1.0, " %");
            f.check(ui, "quake.hideOnBlur", "Hide when it loses focus", "");
        });
    });
    Form::group(ui, th, "Tray icon", |ui| {
        note(ui, f, "The tray icon is always shown: double-click shows the window; the menu has Show / Hide, New Tab, Settings and Exit. The window is never hidden when it is minimised.");
    });
    let _ = x;
}

// ------------------------------------------------------------------------------------ ssh & drops

pub fn network(ui: &mut Ui, _x: &mut PanelCtx, f: &mut Form, _cfg: &Settings) {
    let th = f.th;
    Form::group(ui, th, "SSH", |ui| {
        Form::grid(ui, "ssh", |ui| {
            f.choice(ui, "ssh.connectionReuse", "Connection reuse", "OpenSSH ControlMaster: a second tab to the same host skips the login. Auto uses it when the installed ssh supports it.", &[("auto", "Auto"), ("always", "Always"), ("never", "Never")]);
        });
    });
    Form::group(ui, th, "Dropping files on a terminal", |ui| {
        Form::grid(ui, "drops", |ui| {
            f.choice(ui, "drop.defaultAction", "Default action", "Hold Shift to do the other one", &[("copy", "Copy to the session"), ("paste", "Paste the path")]);
        });
    });
}

// ----------------------------------------------------------------------------------------- logging

pub fn logging(ui: &mut Ui, x: &mut PanelCtx, f: &mut Form, cfg: &Settings) {
    let th = f.th;
    Form::group(ui, th, "Session logs", |ui| {
        Form::grid(ui, "logging", |ui| {
            f.choice(ui, "logging.format", "Format", "Plain keeps only the text; Raw keeps the escape sequences", &[("plain", "Plain text"), ("raw", "Raw")]);
            f.text(ui, "logging.directory", "Folder", "Empty = the app's logs folder", 380.0, |s| Ok(s.trim().to_string()));
            Form::row(ui, th, "", "", |ui| {
                if ui.button("Browse…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        f.patch.set("logging.directory", p.to_string_lossy().into_owned());
                    }
                }
                if ui.add_enabled(!cfg.logging.directory.is_empty(), egui::Button::new("Use default")).clicked() {
                    f.patch.set("logging.directory", "");
                }
                if ui.button("Open folder").clicked() {
                    let dir = x.core.log_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    if let Err(e) = crate::sys::shell_open(&dir.to_string_lossy()) {
                        x.toasts.push(e, ToastKind::Error);
                    }
                }
            });
        });
    });
    Form::group(ui, th, "Recording (asciicast)", |ui| {
        Form::grid(ui, "recording", |ui| {
            f.check(ui, "recording.captureInput", "Record keystrokes", "Adds the typed input to the recording (passwords included)");
        });
    });
}

// -------------------------------------------------------------------------------------------- about

pub fn about(ui: &mut Ui, x: &mut PanelCtx, _f: &mut Form, _cfg: &Settings) {
    let th = x.theme;
    ui.add_space(8.0);
    ui.label(RichText::new("Useless Terminal").size(22.0).strong().color(th.ui.accent));
    ui.label(format!("Version {}  ·  native Rust build (egui, alacritty_terminal, ConPTY)", env!("CARGO_PKG_VERSION")));
    ui.label(RichText::new("Developer: Unnamed10110").color(th.ui.muted));
    ui.add_space(6.0);
    ui.label(format!("Windows build {}  ·  {}", x.core.os_build, if x.elevated { "running as administrator" } else { "standard user" }));
    Form::group(ui, th, "Files", |ui| {
        let core = x.core;
        let dir = ut_fs::app_data_dir();
        ui.label(RichText::new(dir.display().to_string()).monospace().small());
        ui.label(RichText::new(load_status_text(&core.settings.status(), core.settings.path().exists())).small().color(th.ui.muted));
        ui.horizontal_wrapped(|ui| {
            for (label, path) in [("settings.json", core.settings.path().to_path_buf()), ("keybindings.json", core.keys_path.clone()), ("Data folder", dir.clone()), ("Logs folder", core.log_dir()), ("Themes folder", dir.join("themes"))] {
                if ui.button(format!("Open {label}")).clicked() {
                    let is_file = path.extension().is_some();
                    let _ = std::fs::create_dir_all(if is_file { path.parent().unwrap_or(&dir) } else { &path });
                    if is_file && !path.exists() {
                        // first run: nothing has been saved yet, so write the current state to have something to open
                        if path == core.keys_path {
                            let _ = core.keys.lock().save(&path);
                        } else {
                            let _ = ut_fs::write_json_atomic(&path, &core.cfg());
                        }
                    }
                    if let Err(e) = crate::sys::shell_open(&path.to_string_lossy()) {
                        x.toasts.push(e, ToastKind::Error);
                    }
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_validation_follows_the_spec() {
        assert!(check_image("").is_ok());
        assert!(check_image("   ").is_ok());
        assert!(check_image("C:\\x\\notes.txt").unwrap_err().contains("Unsupported"));
        assert!(check_image("C:\\definitely\\missing\\pic.png").unwrap_err().contains("does not exist"));
        let d = std::env::temp_dir().join(format!("ut app img {}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let ok = d.join("bg.JPG");
        std::fs::write(&ok, b"x").unwrap();
        assert!(check_image(&ok.to_string_lossy()).is_ok(), "extension check is case-insensitive");
        let big = d.join("big.png");
        std::fs::write(&big, vec![0u8; 15 * 1024 * 1024 + 1]).unwrap();
        assert!(check_image(&big.to_string_lossy()).unwrap_err().contains("15 MiB"));
        let dir_named_png = d.join("dir.png");
        std::fs::create_dir_all(&dir_named_png).unwrap();
        assert!(check_image(&dir_named_png.to_string_lossy()).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn status_texts() {
        use ut_core::LoadStatus;
        assert!(load_status_text(&LoadStatus::Missing, false).contains("First run"));
        assert!(load_status_text(&LoadStatus::Missing, true).contains("created"));
        assert!(load_status_text(&LoadStatus::Corrupt { error: "eof".into() }, true).contains("untouched"));
        assert!(load_status_text(&LoadStatus::Migrated { backup: "b.json".into() }, true).contains("b.json"));
    }

    #[test]
    fn reset_patch_restores_every_default_but_keeps_the_schema() {
        let mut s = Settings::default();
        s.terminal.font_size = 20.0;
        s.theme.preset = "Nord".into();
        s.quake.hotkey = "Ctrl+F12".into();
        s.startup.workspace_id = Some("w1".into());
        s.theme.overrides.ui.insert("accent".into(), "#ff0000".into());
        let doc = serde_json::to_value(&s).unwrap();
        let p = reset_patch(&doc);
        assert!(p.get("schemaVersion").is_none());
        assert_eq!(p["theme"], json!(null));
        assert_eq!(ut_core::apply_patch(&s, p).unwrap(), Settings::default());
    }

    #[test]
    fn revert_is_reset_then_snapshot() {
        // the window's Revert: null everything, then re-apply the snapshot taken when it opened
        let mut before = Settings::default();
        before.terminal.font_size = 18.0;
        before.theme.overrides.terminal.insert("accent".into(), "#00ff00".into());
        let mut now = before.clone();
        now.terminal.font_size = 25.0;
        now.theme.overrides.ui.insert("accent".into(), "#ff0000".into());
        now.theme.overrides.terminal.clear();
        now.startup.workspace_id = Some("w".into());
        let reset = ut_core::apply_patch(&now, reset_patch(&serde_json::to_value(&now).unwrap())).unwrap();
        let back = ut_core::apply_patch(&reset, serde_json::to_value(&before).unwrap()).unwrap();
        assert_eq!(back, before, "keys added after opening (overrides, workspace id) are gone again");
    }
}
