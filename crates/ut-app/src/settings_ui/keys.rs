//! Keybindings editor (§15.3): searchable list of actions, click a chord and press the new one, conflict
//! detection with a choice of how to resolve it, reset per action / all, the "Shell-safe" keymap. Changes go through
//! the `Keybindings` model (validation, normalisation, `keybindings.json`) and `Cmd::ReloadKeymap` tells the app to
//! rebuild the router's shortcut table. The global Quake hotkey is a setting (`quake.hotkey`), edited here too.

use super::capture::{Outcome, Recorder, Scope};
use super::form::armed_button;
use crate::kit::ToastKind;
use crate::panels::{Cmd, PanelCtx};
use crate::theme::Theme;
use egui::{CollapsingHeader, Id, Margin, RichText, Stroke, Ui};
use std::collections::BTreeMap;
use ut_core::{keybindings, Chord, Keybindings};

// ------------------------------------------------------------------------------------------ catalogue

const TABLE: &[(&str, &str, &str)] = &[
    ("newTab", "New tab", "Tabs"),
    ("closePane", "Close pane / tab", "Tabs"),
    ("nextTab", "Next tab", "Tabs"),
    ("prevTab", "Previous tab", "Tabs"),
    ("duplicateTab", "Duplicate tab", "Tabs"),
    ("splitRight", "Split pane right", "Panes"),
    ("splitDown", "Split pane down", "Panes"),
    ("movePaneFocus", "Move focus between panes (any arrow key)", "Panes"),
    ("copy", "Copy selection", "Clipboard"),
    ("paste", "Paste", "Clipboard"),
    ("search", "Find in terminal", "Terminal"),
    ("exportBuffer", "Save output to file…", "Terminal"),
    ("prevCommand", "Jump to previous command", "Terminal"),
    ("nextCommand", "Jump to next command", "Terminal"),
    ("scrollPageUp", "Scroll up one page", "Terminal"),
    ("scrollPageDown", "Scroll down one page", "Terminal"),
    ("zoomIn", "Zoom in", "Terminal"),
    ("zoomOut", "Zoom out", "Terminal"),
    ("zoomReset", "Reset zoom", "Terminal"),
    ("togglePanel", "Toggle sessions panel", "Window"),
    ("toggleBrowser", "Toggle browser panel", "Window"),
    ("settings", "Open settings", "Window"),
    ("commandPalette", "Command palette", "Window"),
    ("quickConnect", "Quick SSH connect", "Window"),
    ("newSession", "New saved session", "Window"),
    ("quake", "Show / hide the window (global hotkey)", "Window"),
];

const GROUPS: [&str; 8] = ["Tabs", "Panes", "Clipboard", "Terminal", "Window", "Go to tab 1–9", "Go to tab (numpad)", "Other"];

/// Display label and group of an action id (`selectTab3`, `selectTabNumpad0` are generated).
pub fn describe(id: &str) -> (String, &'static str) {
    if let Some(n) = id.strip_prefix("selectTabNumpad").and_then(|n| n.parse::<u32>().ok()) {
        return (format!("Go to tab {} (numpad {n})", if n == 0 { 10 } else { n }), GROUPS[6]);
    }
    if let Some(n) = id.strip_prefix("selectTab").and_then(|n| n.parse::<u32>().ok()) {
        return (format!("Go to tab {n}"), GROUPS[5]);
    }
    match TABLE.iter().find(|(a, _, _)| *a == id) {
        Some((_, label, group)) => (label.to_string(), group),
        None => (id.to_string(), GROUPS[7]),
    }
}

/// Actions grouped for display, in table order (tab numbers numerically).
pub fn grouped(ids: impl IntoIterator<Item = String>) -> Vec<(&'static str, Vec<String>)> {
    let mut out: Vec<(&'static str, Vec<String>)> = GROUPS.iter().map(|g| (*g, vec![])).collect();
    for id in ids {
        let g = describe(&id).1;
        if let Some(slot) = out.iter_mut().find(|(n, _)| *n == g) {
            slot.1.push(id);
        }
    }
    let pos = |id: &String| {
        TABLE.iter().position(|(a, _, _)| a == id).unwrap_or_else(|| id.chars().filter(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))
    };
    for (_, v) in &mut out {
        v.sort_by_key(pos);
    }
    out.retain(|(_, v)| !v.is_empty());
    out
}

// ---------------------------------------------------------------------------------------- pure rules

/// Actions (other than `action`) that already use a chord overlapping `chord` (`Arrow` overlaps every arrow key).
pub fn others_using(eff: &BTreeMap<String, Vec<String>>, action: &str, chord: &Chord) -> Vec<String> {
    eff.iter().filter(|(a, cs)| a.as_str() != action && cs.iter().any(|c| c.parse::<Chord>().is_ok_and(|c| c.overlaps(chord)))).map(|(a, _)| a.clone()).collect()
}

/// The chord list after recording `chord` over slot `idx` (or appending when `None`), without duplicates.
pub fn new_chords(current: &[String], idx: Option<usize>, chord: &str) -> Vec<String> {
    let mut v = current.to_vec();
    match idx {
        Some(i) if i < v.len() => v[i] = chord.to_string(),
        _ => v.push(chord.to_string()),
    }
    let mut seen = Vec::new();
    v.retain(|c| {
        let fresh = !seen.contains(c);
        seen.push(c.clone());
        fresh
    });
    v
}

/// `other`'s chords once `chord` has been taken from it.
pub fn take_over(other: &[String], chord: &Chord) -> Vec<String> {
    other.iter().filter(|c| !c.parse::<Chord>().is_ok_and(|c| c.overlaps(chord))).cloned().collect()
}

/// Does an action row match the search box (label, id or any chord, case-insensitive)?
pub fn matches_query(query: &str, label: &str, id: &str, chords: &[String]) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || label.to_lowercase().contains(&q) || id.to_lowercase().contains(&q) || chords.iter().any(|c| c.to_lowercase().contains(&q) || chord_label(c).to_lowercase().contains(&q))
}

/// `Ctrl+Comma` shown with the current layout's key names (`Ctrl+,`; the Quake key is `Ñ` on es-ES, §7.8).
pub fn chord_label(chord: &str) -> String {
    let mut t: Vec<&str> = chord.split('+').collect();
    let Some(key) = t.pop() else { return String::new() };
    join_label(&t, &crate::sys::key_label(key))
}

/// `["Ctrl", "Shift"]` + the layout's key label -> `Ctrl+Shift+Key`; a lone character is upper-cased and a literal
/// `+` key is spelled out so it can't be mistaken for the separator.
fn join_label(mods: &[&str], key: &str) -> String {
    let key = if key == "+" { "Plus".to_string() } else if key.chars().count() == 1 { key.to_uppercase() } else { key.to_string() };
    mods.iter().map(|s| format!("{s}+")).collect::<String>() + &key
}

/// An arrow key recorded for `movePaneFocus` means "any arrow" (§15.2).
fn adapt(action: &str, mut c: Chord) -> Chord {
    if action == "movePaneFocus" && matches!(c.key.as_str(), "Up" | "Down" | "Left" | "Right") {
        c.key = "Arrow".into();
    }
    c
}

// ---------------------------------------------------------------------------------------------- UI

enum Op {
    Begin(String, Option<usize>),
    Stop,
    Dismiss,
    Set(String, Vec<String>),
    Unbind(String),
    Reset(String),
    ResetAll,
    ShellSafe,
    Takeover { action: String, idx: Option<usize>, chord: String, others: Vec<String> },
}

struct Pending {
    action: String,
    idx: Option<usize>,
    chord: String,
    others: Vec<String>,
}

#[derive(Default)]
pub struct KeysUi {
    search: String,
    conflicts_only: bool,
    rec: Recorder,
    target: Option<(String, Option<usize>)>,
    pending: Option<Pending>,
}

fn chip(ui: &mut Ui, th: &Theme, text: &str, conflict: bool) -> egui::Response {
    let stroke = if conflict { Stroke::new(1.5, th.ui.warning) } else { Stroke::new(1.0, th.ui.card_border) };
    ui.add(egui::Button::new(RichText::new(text).monospace().color(if conflict { th.ui.warning } else { th.ui.foreground })).stroke(stroke).corner_radius(4))
}

impl KeysUi {
    pub fn is_capturing(&self) -> bool {
        self.rec.active
    }

    /// Drop any half-finished recording (the window closed or the section changed).
    pub fn reset_transient(&mut self) {
        self.rec.stop();
        self.target = None;
        self.pending = None;
    }

    pub fn ui(&mut self, ui: &mut Ui, x: &mut PanelCtx, quake: &str) {
        let th = x.theme;
        let core = x.core;
        let mut eff = core.keys.lock().effective();
        eff.insert("quake".into(), vec![quake.to_string()]);
        let defaults = keybindings::defaults();
        let mut ops: Vec<Op> = vec![];

        // ---- recording: poll the keyboard, then act on the verdict
        let scope = if self.target.as_ref().is_some_and(|(a, _)| a == "quake") { Scope::Global } else { Scope::App };
        if let Some(out) = self.rec.step(ui, scope) {
            let (action, idx) = self.target.take().unwrap_or_default();
            match out {
                Outcome::Chord(c) => {
                    let c = adapt(&action, c);
                    let chord = c.to_string();
                    if action == "quake" {
                        if let Err(e) = crate::winhooks::parse_hotkey(&chord) {
                            self.target = Some((action, idx));
                            self.rec.active = true;
                            self.rec.notice = Some(e);
                        } else {
                            ops.push(Op::Set(action, vec![chord]));
                        }
                    } else {
                        let others = others_using(&eff, &action, &c);
                        if others.is_empty() {
                            let cur = eff.get(&action).cloned().unwrap_or_default();
                            ops.push(Op::Set(action, new_chords(&cur, idx, &chord)));
                        } else {
                            self.pending = Some(Pending { action, idx, chord, others });
                        }
                    }
                }
                _ => {} // Cancel
            }
        }

        // ---- header
        let n_conflicts: usize = eff.iter().filter(|(a, cs)| cs.iter().any(|c| c.parse::<Chord>().is_ok_and(|c| !others_using(&eff, a, &c).is_empty()))).count();
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search actions or shortcuts…").desired_width(240.0));
            ui.checkbox(&mut self.conflicts_only, "Conflicts only");
            if n_conflicts > 0 {
                ui.label(RichText::new(format!("⚠ {n_conflicts} actions share a shortcut")).color(th.ui.warning));
            }
        });
        ui.horizontal_wrapped(|ui| {
            if armed_button(ui, th, Id::new("keys-reset-all"), "Reset all to defaults", "Click again to reset everything") {
                ops.push(Op::ResetAll);
            }
            if ui.button("Shell-safe keymap").on_hover_text("Windows Terminal style: New tab Ctrl+Shift+T, Close pane Ctrl+Shift+W, Sessions panel Ctrl+Shift+E.\nLeaves Ctrl+T / Ctrl+W / Ctrl+B to the shell (readline, tmux).").clicked() {
                ops.push(Op::ShellSafe);
            }
            if ui.button("Open keybindings.json").clicked() {
                if !core.keys_path.exists() {
                    let _ = core.keys.lock().save(&core.keys_path); // nothing saved yet: give the editor a file
                }
                let _ = crate::sys::shell_open(&core.keys_path.to_string_lossy());
            }
        });
        ui.label(RichText::new("Click a shortcut and press the new keys (Esc cancels). Right-click a shortcut to remove it. Changes apply immediately.").small().color(th.ui.muted));
        ui.add_space(6.0);

        // ---- the list
        let searching = !self.search.trim().is_empty() || self.conflicts_only;
        for (group, ids) in grouped(eff.keys().cloned()) {
            let rows: Vec<&String> = ids
                .iter()
                .filter(|id| {
                    let chords = eff.get(*id).map(Vec::as_slice).unwrap_or(&[]);
                    let in_conflict = chords.iter().any(|c| c.parse::<Chord>().is_ok_and(|c| !others_using(&eff, id, &c).is_empty()));
                    matches_query(&self.search, &describe(id).0, id, chords) && (!self.conflicts_only || in_conflict)
                })
                .collect();
            if rows.is_empty() {
                continue;
            }
            let numbered = group.starts_with("Go to tab");
            let mut h = CollapsingHeader::new(RichText::new(group).strong()).id_salt(("keys-group", group)).default_open(!numbered);
            if searching {
                h = h.open(Some(true));
            }
            h.show(ui, |ui| {
                egui::Grid::new(("keys-grid", group)).num_columns(3).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
                    for id in rows {
                        self.row(ui, th, id, &eff, &defaults, &mut ops);
                    }
                });
            });
        }

        self.apply(x, &eff, ops);
    }

    #[allow(clippy::too_many_arguments)]
    fn row(&self, ui: &mut Ui, th: &Theme, id: &str, eff: &BTreeMap<String, Vec<String>>, defaults: &BTreeMap<String, Vec<String>>, ops: &mut Vec<Op>) {
        let chords = eff.get(id).cloned().unwrap_or_default();
        let modified = defaults.get(id).is_some_and(|d| *d != chords);
        let recording_here = self.rec.active && self.target.as_ref().is_some_and(|(a, _)| a == id);
        let (label, _) = describe(id);

        ui.scope(|ui| {
            ui.set_min_width(260.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(if modified { "●" } else { " " }).small().color(th.ui.accent)).on_hover_text("Changed from the default");
                ui.label(label).on_hover_text(id);
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.set_min_width(250.0);
            if recording_here {
                let held = self.rec.held();
                let text = if held.is_empty() { "Press the new shortcut…".to_string() } else { format!("{held}…") };
                egui::Frame::new().stroke(Stroke::new(1.5, th.ui.accent)).corner_radius(4).inner_margin(Margin::symmetric(8, 3)).show(ui, |ui| {
                    ui.label(RichText::new(text).color(th.ui.accent));
                });
                if ui.button("Cancel").clicked() {
                    ops.push(Op::Stop);
                }
                return;
            }
            for (i, c) in chords.iter().enumerate() {
                let conflict = c.parse::<Chord>().is_ok_and(|p| !others_using(eff, id, &p).is_empty());
                let mut r = chip(ui, th, &chord_label(c), conflict).on_hover_text("Click, then press the new shortcut");
                if conflict {
                    let who: Vec<String> = c.parse::<Chord>().map(|p| others_using(eff, id, &p)).unwrap_or_default().iter().map(|a| describe(a).0).collect();
                    r = r.on_hover_text(format!("Also used by: {}", who.join(", ")));
                }
                if r.clicked() {
                    ops.push(Op::Begin(id.to_string(), Some(i)));
                }
                r.context_menu(|ui| {
                    if ui.button("Change…").clicked() {
                        ops.push(Op::Begin(id.to_string(), Some(i)));
                        ui.close();
                    }
                    if ui.button("Remove this shortcut").clicked() {
                        let mut rest = chords.clone();
                        rest.remove(i);
                        ops.push(Op::Set(id.to_string(), rest));
                        ui.close();
                    }
                });
            }
            if chords.is_empty() {
                ui.label(RichText::new("unbound").italics().color(th.ui.muted));
            }
            let add_label = if chords.is_empty() { "Set…" } else { "+" };
            if ui.small_button(add_label).on_hover_text("Add a shortcut for this action").clicked() {
                ops.push(Op::Begin(id.to_string(), None));
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(modified, egui::Button::new("Reset").small()).on_hover_text("Back to the default shortcut").clicked() {
                ops.push(Op::Reset(id.to_string()));
            }
            if id != "quake" && ui.add_enabled(!chords.is_empty(), egui::Button::new("Unbind").small()).on_hover_text("The keys pass through to the shell").clicked() {
                ops.push(Op::Unbind(id.to_string()));
            }
        });
        ui.end_row();

        if recording_here {
            if let Some(n) = &self.rec.notice {
                ui.label("");
                ui.scope(|ui| {
                    ui.set_max_width(360.0); // wrap: a long message must not widen the grid column
                    ui.label(RichText::new(n).small().color(th.ui.error));
                });
                ui.label("");
                ui.end_row();
            }
        }
        if let Some(p) = self.pending.as_ref().filter(|p| p.action == id) {
            ui.label("");
            ui.vertical(|ui| {
                let who: Vec<String> = p.others.iter().map(|a| describe(a).0).collect();
                ui.label(RichText::new(format!("{} is already used by: {}", chord_label(&p.chord), who.join(", "))).color(th.ui.warning));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Take it over").on_hover_text("Unbind it from the other action(s) and assign it here").clicked() {
                        ops.push(Op::Takeover { action: p.action.clone(), idx: p.idx, chord: p.chord.clone(), others: p.others.clone() });
                    }
                    if ui.button("Assign anyway").on_hover_text("Both actions keep it (the first match wins)").clicked() {
                        let cur = eff.get(id).cloned().unwrap_or_default();
                        ops.push(Op::Set(p.action.clone(), new_chords(&cur, p.idx, &p.chord)));
                    }
                    if ui.button("Cancel").clicked() {
                        ops.push(Op::Dismiss);
                    }
                });
            });
            ui.label("");
            ui.end_row();
        }
    }

    fn apply(&mut self, x: &mut PanelCtx, eff: &BTreeMap<String, Vec<String>>, ops: Vec<Op>) {
        let core = x.core;
        let mut changed = false;
        let mut errors: Vec<String> = vec![];
        {
            let mut kb = core.keys.lock();
            let mut set = |kb: &mut Keybindings, action: &str, chords: &[String]| -> bool {
                if action == "quake" {
                    // The global hotkey is a setting, not a keybindings.json entry.
                    let v = chords.first().map_or(serde_json::Value::Null, |c| serde_json::json!(c));
                    let _ = core.settings.patch(serde_json::json!({ "quake": { "hotkey": v } }));
                    return false;
                }
                match kb.set_binding(action, chords) {
                    Ok(_) => true,
                    Err(e) => {
                        errors.push(e);
                        false
                    }
                }
            };
            for op in ops {
                match op {
                    Op::Begin(a, i) => {
                        self.pending = None;
                        self.target = Some((a, i));
                        self.rec.start();
                    }
                    Op::Stop => {
                        self.rec.stop();
                        self.target = None;
                    }
                    Op::Dismiss => self.pending = None,
                    Op::Set(a, chords) => {
                        changed |= set(&mut kb, &a, &chords);
                        self.pending = None;
                    }
                    Op::Unbind(a) => changed |= set(&mut kb, &a, &[]),
                    Op::Reset(a) => {
                        if a == "quake" {
                            let _ = core.settings.patch(serde_json::json!({ "quake": { "hotkey": null } }));
                        } else {
                            kb.reset(Some(&a));
                            changed = true;
                        }
                    }
                    Op::ResetAll => {
                        kb.reset(None);
                        let _ = core.settings.patch(serde_json::json!({ "quake": { "hotkey": null } }));
                        changed = true;
                    }
                    Op::ShellSafe => {
                        kb.apply_shell_safe();
                        changed = true;
                    }
                    Op::Takeover { action, idx, chord, others } => {
                        if let Ok(parsed) = chord.parse::<Chord>() {
                            for o in &others {
                                let rest = take_over(eff.get(o).map(Vec::as_slice).unwrap_or(&[]), &parsed);
                                changed |= set(&mut kb, o, &rest);
                            }
                        }
                        let cur = eff.get(&action).cloned().unwrap_or_default();
                        changed |= set(&mut kb, &action, &new_chords(&cur, idx, &chord));
                        self.pending = None;
                    }
                }
            }
            if changed {
                if let Err(e) = kb.save(&core.keys_path) {
                    errors.push(format!("Could not save keybindings.json: {e}"));
                }
            }
        }
        if changed {
            x.cmds.push(Cmd::ReloadKeymap);
        }
        for e in errors {
            x.toasts.push(e, ToastKind::Error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff_of(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs.iter().map(|(a, cs)| (a.to_string(), cs.iter().map(|c| c.to_string()).collect())).collect()
    }

    #[test]
    fn every_default_action_has_a_label_and_a_group() {
        for id in keybindings::defaults().keys() {
            let (label, group) = describe(id);
            assert_ne!(group, "Other", "{id} needs an entry in the editor's table");
            assert!(!label.is_empty() && label != *id || id.starts_with("selectTab"), "{id}");
        }
        assert_eq!(describe("selectTab3").0, "Go to tab 3");
        assert_eq!(describe("selectTabNumpad0").0, "Go to tab 10 (numpad 0)");
        assert_eq!(describe("selectTabNumpad4").0, "Go to tab 4 (numpad 4)");
        assert_eq!(describe("future").1, "Other");
    }

    #[test]
    fn grouping_covers_everything_in_a_stable_order() {
        let g = grouped(keybindings::defaults().into_keys());
        let total: usize = g.iter().map(|(_, v)| v.len()).sum();
        assert_eq!(total, keybindings::defaults().len());
        assert_eq!(g[0].0, "Tabs");
        assert_eq!(g[0].1[0], "newTab");
        let nums = &g.iter().find(|(n, _)| *n == "Go to tab 1–9").unwrap().1;
        assert_eq!(nums.first().map(String::as_str), Some("selectTab1"));
        assert_eq!(nums.last().map(String::as_str), Some("selectTab9"));
        let np = &g.iter().find(|(n, _)| *n == "Go to tab (numpad)").unwrap().1;
        assert_eq!((np.first().map(String::as_str), np.len()), (Some("selectTabNumpad0"), 10));
    }

    #[test]
    fn conflicts_see_other_actions_and_arrows() {
        let eff = keybindings::Keybindings::default().effective();
        let ctrl_t: Chord = "Ctrl+T".parse().unwrap();
        assert_eq!(others_using(&eff, "search", &ctrl_t), ["newTab"]);
        assert!(others_using(&eff, "newTab", &ctrl_t).is_empty(), "an action never conflicts with itself");
        let up: Chord = "Ctrl+Shift+Up".parse().unwrap();
        assert_eq!(others_using(&eff, "search", &up), ["movePaneFocus"], "Arrow overlaps every arrow key");
        assert!(others_using(&eff, "search", &"Ctrl+Alt+F9".parse().unwrap()).is_empty());
    }

    #[test]
    fn recording_replaces_adds_and_dedups() {
        let cur = vec!["Ctrl+Shift+C".to_string(), "Ctrl+Insert".to_string()];
        assert_eq!(new_chords(&cur, Some(1), "Ctrl+Alt+C"), ["Ctrl+Shift+C", "Ctrl+Alt+C"]);
        assert_eq!(new_chords(&cur, None, "Ctrl+Alt+C"), ["Ctrl+Shift+C", "Ctrl+Insert", "Ctrl+Alt+C"]);
        assert_eq!(new_chords(&cur, Some(0), "Ctrl+Insert"), ["Ctrl+Insert"], "recording an existing chord does not duplicate it");
        assert_eq!(new_chords(&[], None, "F5"), ["F5"]);
        assert_eq!(new_chords(&cur, Some(9), "F5").len(), 3, "a stale index appends");
    }

    #[test]
    fn taking_a_chord_over_leaves_the_rest() {
        let c: Chord = "Ctrl+Shift+Left".parse().unwrap();
        assert_eq!(take_over(&["Ctrl+Shift+Arrow".into(), "F9".into()], &c), ["F9"]);
        assert!(take_over(&["Ctrl+T".into()], &"Ctrl+T".parse().unwrap()).is_empty());
        assert_eq!(take_over(&["Ctrl+T".into()], &"Ctrl+W".parse().unwrap()), ["Ctrl+T"]);
    }

    #[test]
    fn editing_flow_writes_through_the_model() {
        // record Ctrl+T for `search` (conflicts with newTab), take it over, and check the model's verdict
        let mut kb = Keybindings::default();
        let eff = kb.effective();
        let c: Chord = "Ctrl+T".parse().unwrap();
        let others = others_using(&eff, "search", &c);
        assert_eq!(others, ["newTab"]);
        for o in &others {
            kb.set_binding(o, &take_over(&eff[o], &c)).unwrap();
        }
        kb.set_binding("search", &new_chords(&eff["search"], Some(0), "Ctrl+T")).unwrap();
        let e = kb.effective();
        assert_eq!(e["search"], ["Ctrl+T"]);
        assert!(e["newTab"].is_empty());
        assert!(kb.conflicts().is_empty());
        assert_eq!(kb.to_json()["bindings"]["newTab"], serde_json::Value::Null, "unbound is written as null");
        // movePaneFocus keeps "any arrow"
        assert_eq!(adapt("movePaneFocus", "Ctrl+Shift+Down".parse().unwrap()).to_string(), "Ctrl+Shift+Arrow");
        assert_eq!(adapt("search", "Ctrl+Shift+Down".parse().unwrap()).to_string(), "Ctrl+Shift+Down");
        // Win chords are refused by the model for ordinary actions, accepted for the global one
        assert!(kb.set_binding("search", &["Win+F".into()]).is_err());
        assert!(kb.set_binding("quake", &["Win+F1".into()]).is_ok());
    }

    #[test]
    fn chord_labels_follow_the_layout_without_ambiguity() {
        assert_eq!(join_label(&["Win"], "ñ"), "Win+Ñ");
        assert_eq!(join_label(&["Ctrl", "Shift"], "+"), "Ctrl+Shift+Plus");
        assert_eq!(join_label(&["Ctrl"], "PageUp"), "Ctrl+PageUp");
        assert_eq!(join_label(&[], "F5"), "F5");
        assert_eq!(chord_label("Ctrl+T"), "Ctrl+T");
    }

    #[test]
    fn search_matches_label_id_and_chord() {
        let chords = vec!["Ctrl+Shift+P".to_string()];
        assert!(matches_query("palette", "Command palette", "commandPalette", &chords));
        assert!(matches_query("COMMANDPAL", "Command palette", "commandPalette", &chords));
        assert!(matches_query("ctrl+shift+p", "Command palette", "commandPalette", &chords));
        assert!(matches_query("", "x", "x", &[]));
        assert!(!matches_query("zzz", "Command palette", "commandPalette", &chords));
    }
}
