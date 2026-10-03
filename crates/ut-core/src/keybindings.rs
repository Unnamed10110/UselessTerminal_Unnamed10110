//! Keybindings model (§15): chord parsing/normalisation, defaults, user file IO, conflicts,
//! legacy WPF import (Appendix C.5). Matching `KeyboardEvent`s happens in the web UI.

use crate::LoadStatus;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use ut_fs::{read_json, ReadJson};

// ------------------------------------------------------------------------------- chords

/// A parsed chord. Canonical text is `Ctrl+Shift+Alt+Win+<Key>` (§15.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    pub key: String,
}

const NAMED: [&str; 27] = [
    "Tab", "Enter", "Esc", "Space", "Backspace", "Delete", "Insert", "Home", "End", "PageUp", "PageDown", "Up", "Down",
    "Left", "Right", "Comma", "Period", "Minus", "Equal", "Backquote", "Slash", "Backslash", "BracketLeft",
    "BracketRight", "Semicolon", "Quote", "Arrow",
];

/// Legacy WPF `Key` names and common synonyms (lowercase) -> canonical key (§15.1).
const ALIASES: [(&str, &str); 28] = [
    ("escape", "Esc"), ("return", "Enter"), ("back", "Backspace"), ("del", "Delete"), ("ins", "Insert"),
    ("prior", "PageUp"), ("pgup", "PageUp"), ("next", "PageDown"), ("pgdn", "PageDown"), ("spacebar", "Space"),
    ("oemcomma", "Comma"), ("oemperiod", "Period"), ("oemminus", "Minus"), ("oemplus", "Equal"),
    ("oem3", "Backquote"), ("oemtilde", "Backquote"), ("oem2", "Slash"), ("oemquestion", "Slash"),
    ("oem5", "Backslash"), ("oempipe", "Backslash"), ("oem4", "BracketLeft"), ("oemopenbrackets", "BracketLeft"),
    ("oem6", "BracketRight"), ("oemclosebrackets", "BracketRight"), ("oem1", "Semicolon"),
    ("oemsemicolon", "Semicolon"), ("oem7", "Quote"), ("oemquotes", "Quote"),
];

fn canon_key(tok: &str) -> Option<String> {
    let l = tok.to_ascii_lowercase();
    let one_digit = |s: &str| s.len() == 1 && s.as_bytes()[0].is_ascii_digit();
    if tok.len() == 1 && tok.as_bytes()[0].is_ascii_alphanumeric() {
        return Some(l.to_ascii_uppercase());
    }
    if let Some(d) = l.strip_prefix('d').filter(|d| one_digit(d)) {
        return Some(d.to_string()); // WPF `D1`
    }
    if let Some(n) = l.strip_prefix("numpad").filter(|n| one_digit(n)) {
        return Some(format!("Numpad{n}"));
    }
    if let Some(n) = l.strip_prefix('f').filter(|n| n.bytes().all(|b| b.is_ascii_digit())).and_then(|n| n.parse::<u8>().ok()) {
        return (1..=24).contains(&n).then(|| format!("F{n}"));
    }
    NAMED
        .iter()
        .find(|k| k.eq_ignore_ascii_case(tok))
        .map(|k| k.to_string())
        .or_else(|| ALIASES.iter().find(|(a, _)| *a == l).map(|(_, k)| k.to_string()))
}

impl FromStr for Chord {
    type Err = String;
    /// Accepts canonical names and legacy WPF names, any case, any modifier order.
    fn from_str(s: &str) -> Result<Chord, String> {
        let toks: Vec<&str> = s.split('+').map(str::trim).collect();
        let (key, mods) = toks.split_last().ok_or("empty chord")?;
        let mut c = Chord { ctrl: false, shift: false, alt: false, win: false, key: String::new() };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" => c.alt = true,
                "win" | "windows" | "meta" | "super" => c.win = true,
                _ => return Err(format!("unknown modifier '{m}' in '{s}'")),
            }
        }
        c.key = canon_key(key).ok_or_else(|| format!("unknown key '{key}' in '{s}'"))?;
        Ok(c)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [(self.ctrl, "Ctrl"), (self.shift, "Shift"), (self.alt, "Alt"), (self.win, "Win")] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&self.key)
    }
}

fn is_arrow(k: &str) -> bool {
    matches!(k, "Up" | "Down" | "Left" | "Right" | "Arrow")
}

impl Chord {
    /// Same modifiers and the same key, where `Arrow` stands for any arrow key.
    pub fn overlaps(&self, o: &Chord) -> bool {
        (self.ctrl, self.shift, self.alt, self.win) == (o.ctrl, o.shift, o.alt, o.win)
            && (self.key == o.key || (is_arrow(&self.key) && is_arrow(&o.key) && (self.key == "Arrow" || o.key == "Arrow")))
    }
}

/// Parse and re-format a chord in canonical form (`"oemcomma"`-style legacy names included).
pub fn normalize_chord(s: &str) -> Result<String, String> {
    s.parse::<Chord>().map(|c| c.to_string())
}

// ------------------------------------------------------------------------------ defaults

const BASE: [(&str, &[&str]); 25] = [
    ("newTab", &["Ctrl+T"]),
    ("closePane", &["Ctrl+W"]),
    ("togglePanel", &["Ctrl+B"]),
    ("toggleBrowser", &["Ctrl+Shift+B"]),
    ("settings", &["Ctrl+Comma"]),
    ("nextTab", &["Ctrl+Tab"]),
    ("prevTab", &["Ctrl+Shift+Tab"]),
    ("newSession", &["Ctrl+Shift+N"]),
    ("duplicateTab", &["Ctrl+Shift+D"]),
    ("commandPalette", &["Ctrl+Shift+P"]),
    ("quickConnect", &["Ctrl+Shift+O"]),
    ("movePaneFocus", &["Ctrl+Shift+Arrow"]),
    ("prevCommand", &["Ctrl+Alt+Up"]),
    ("nextCommand", &["Ctrl+Alt+Down"]),
    ("search", &["Ctrl+Shift+F"]),
    ("exportBuffer", &["Ctrl+Shift+S"]),
    ("copy", &["Ctrl+Shift+C", "Ctrl+Insert"]), // plus Ctrl+C when there is a selection (UI rule)
    ("paste", &["Ctrl+V", "Ctrl+Shift+V", "Shift+Insert"]),
    // Canonical modifier order is Ctrl+Shift+Alt, so §15.2's "Alt+Shift+Equal" reads Shift+Alt+Equal.
    ("splitRight", &["Shift+Alt+Equal"]),
    ("splitDown", &["Shift+Alt+Minus"]),
    ("zoomIn", &["Ctrl+Equal"]),
    ("zoomOut", &["Ctrl+Minus"]),
    ("zoomReset", &["Ctrl+0"]),
    ("scrollPageUp", &["Shift+PageUp"]),
    ("scrollPageDown", &["Shift+PageDown"]),
];

/// The built-in "Shell-safe (Windows Terminal style)" keymap [P1]: only these three differ.
pub const SHELL_SAFE: [(&str, &str); 3] =
    [("newTab", "Ctrl+Shift+T"), ("closePane", "Ctrl+Shift+W"), ("togglePanel", "Ctrl+Shift+E")];

/// Every action with its default chords (§15.2), canonical text.
pub fn defaults() -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> =
        BASE.iter().map(|(a, c)| (a.to_string(), c.iter().map(|s| s.to_string()).collect())).collect();
    for n in 1..=9 {
        m.insert(format!("selectTab{n}"), vec![format!("Ctrl+{n}")]);
    }
    for n in 0..=9 {
        m.insert(format!("selectTabNumpad{n}"), vec![format!("Ctrl+Alt+Numpad{n}")]);
    }
    m.insert("quake".into(), vec!["Win+Backquote".into()]); // global; settings.quake.hotkey is what gets registered
    m
}

// -------------------------------------------------------------------------------- model

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct InvalidBinding {
    pub action: String,
    pub value: String,
    pub error: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Conflict {
    pub chord: String,
    pub actions: Vec<String>,
}

/// The user's overrides on top of [`defaults`]: `Some(chords)` = rebound, `None` = unbound,
/// absent = default. Unknown actions are kept (forward compatibility).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keybindings {
    overrides: BTreeMap<String, Option<Vec<String>>>,
}

pub struct KeyLoad {
    pub keybindings: Keybindings,
    pub status: LoadStatus,
    /// Chords that could not be parsed (reported, never fatal; the action keeps its default).
    pub invalid: Vec<InvalidBinding>,
}

/// `Win+…` chords only make sense as global hotkeys (§15.1).
fn parse_for(action: &str, s: &str) -> Result<String, String> {
    let c: Chord = s.parse()?;
    if c.win && action != "quake" {
        return Err(format!("'{s}': Win chords are only valid for global hotkeys"));
    }
    Ok(c.to_string())
}

impl Keybindings {
    /// Parse the file shape `{schemaVersion, bindings}` or the legacy `{Bindings}` (C.5).
    pub fn from_json(v: &Value) -> (Keybindings, Vec<InvalidBinding>) {
        let mut kb = Keybindings::default();
        let mut invalid = vec![];
        let Some(map) = v.get("bindings").or_else(|| v.get("Bindings")).and_then(Value::as_object) else {
            return (kb, invalid);
        };
        for (action, val) in map {
            let items: Vec<&Value> = match val {
                Value::Null => {
                    kb.overrides.insert(action.clone(), None);
                    continue;
                }
                Value::Array(a) => a.iter().collect(),
                other => vec![other],
            };
            let mut chords: Vec<String> = vec![];
            let mut bad = false;
            for it in &items {
                match it.as_str().ok_or_else(|| "not a string".to_string()).and_then(|s| parse_for(action, s)) {
                    Ok(c) if !chords.contains(&c) => chords.push(c),
                    Ok(_) => {}
                    Err(error) => {
                        bad = true;
                        invalid.push(InvalidBinding { action: action.clone(), value: it.to_string(), error });
                    }
                }
            }
            if !chords.is_empty() {
                kb.overrides.insert(action.clone(), Some(chords));
            } else if !bad {
                kb.overrides.insert(action.clone(), None); // `[]` = unbound
            }
        }
        (kb, invalid)
    }

    /// The file contents: a single chord is written as a string, several as an array, unbound as `null`.
    pub fn to_json(&self) -> Value {
        let b: Map<String, Value> = self
            .overrides
            .iter()
            .map(|(a, c)| {
                let v = match c {
                    None => Value::Null,
                    Some(v) if v.len() == 1 => json!(v[0]),
                    Some(v) => json!(v),
                };
                (a.clone(), v)
            })
            .collect();
        json!({ "schemaVersion": 3, "bindings": b })
    }

    /// action -> chords for every known action (defaults overlaid with the user's choices).
    pub fn effective(&self) -> BTreeMap<String, Vec<String>> {
        let mut m = defaults();
        for (a, c) in &self.overrides {
            if let Some(slot) = m.get_mut(a) {
                *slot = c.clone().unwrap_or_default();
            }
        }
        m
    }

    /// Chords claimed by more than one action (an `Arrow` chord overlaps every arrow key).
    pub fn conflicts(&self) -> Vec<Conflict> {
        let eff = self.effective();
        let all: Vec<(&str, Chord)> = eff
            .iter()
            .flat_map(|(a, cs)| cs.iter().filter_map(move |c| c.parse().ok().map(|c| (a.as_str(), c))))
            .collect();
        let (mut out, mut seen) = (vec![], BTreeSet::new());
        for (_, c) in &all {
            let actions: BTreeSet<&str> = all.iter().filter(|(_, o)| o.overlaps(c)).map(|(a, _)| *a).collect();
            if actions.len() > 1 && seen.insert(actions.clone()) {
                out.push(Conflict { chord: c.to_string(), actions: actions.into_iter().map(String::from).collect() });
            }
        }
        out
    }

    /// Rebind `action` (known actions only). Returns the new file contents.
    pub fn set_binding(&mut self, action: &str, chords: &[String]) -> Result<Value, String> {
        let def = defaults();
        let default = def.get(action).ok_or_else(|| format!("unknown action '{action}'"))?;
        let mut norm: Vec<String> = vec![];
        for c in chords {
            let c = parse_for(action, c)?;
            if !norm.contains(&c) {
                norm.push(c);
            }
        }
        if norm.is_empty() {
            return self.unbind(action);
        }
        if &norm == default {
            self.overrides.remove(action);
        } else {
            self.overrides.insert(action.to_string(), Some(norm));
        }
        Ok(self.to_json())
    }

    /// Unbind `action`: the key passes through to the shell (§15.1).
    pub fn unbind(&mut self, action: &str) -> Result<Value, String> {
        if !defaults().contains_key(action) {
            return Err(format!("unknown action '{action}'"));
        }
        self.overrides.insert(action.to_string(), None);
        Ok(self.to_json())
    }

    /// Back to the default for one action, or for every known action (`None`; unknown ones stay).
    pub fn reset(&mut self, action: Option<&str>) -> Value {
        match action {
            Some(a) => {
                self.overrides.remove(a);
            }
            None => {
                let def = defaults();
                self.overrides.retain(|a, _| !def.contains_key(a));
            }
        }
        self.to_json()
    }

    /// Apply the "Shell-safe (Windows Terminal style)" keymap on top of the current overrides.
    pub fn apply_shell_safe(&mut self) -> Value {
        for (a, c) in SHELL_SAFE {
            self.overrides.insert(a.to_string(), Some(vec![c.to_string()]));
        }
        self.to_json()
    }

    /// Atomic write. A corrupt existing file is quarantined first so it is never silently lost.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if matches!(read_json::<Value>(path), ReadJson::Corrupt { .. }) {
            ut_fs::quarantine(path)?;
        }
        ut_fs::write_json_atomic(path, &self.to_json())
    }
}

/// Load `keybindings.json`. Missing -> defaults. Corrupt -> defaults, file untouched.
/// A legacy WPF file (`{Bindings}`, no `schemaVersion`) is backed up as `*.legacy-bak.json`,
/// converted (key names translated, §15.1) and rewritten in v3 format.
pub fn load(path: &Path) -> KeyLoad {
    let done = |keybindings, status, invalid| KeyLoad { keybindings, status, invalid };
    let v = match read_json::<Value>(path) {
        ReadJson::Missing => return done(Keybindings::default(), LoadStatus::Missing, vec![]),
        ReadJson::Corrupt { error } => return done(Keybindings::default(), LoadStatus::Corrupt { error }, vec![]),
        ReadJson::Ok(v) => v,
    };
    let (kb, invalid) = Keybindings::from_json(&v);
    if v.get("schemaVersion").is_some() || v.get("Bindings").is_none() {
        return done(kb, LoadStatus::Loaded, invalid);
    }
    let status = match ut_fs::backup_copy(path, "legacy").map_err(|e| format!("cannot back up legacy keybindings: {e}")) {
        Ok(bak) => match kb.save(path) {
            Ok(()) => LoadStatus::Migrated { backup: bak.display().to_string() },
            Err(e) => LoadStatus::Corrupt { error: format!("cannot write migrated keybindings: {e}") },
        },
        Err(error) => LoadStatus::Corrupt { error },
    };
    done(kb, status, invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ut core kb {n} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn defaults_are_canonical_and_complete() {
        let d = defaults();
        for (a, cs) in &d {
            for c in cs {
                assert_eq!(&normalize_chord(c).unwrap(), c, "{a}");
            }
        }
        assert_eq!(d["selectTab9"], ["Ctrl+9"]);
        assert_eq!(d["selectTabNumpad0"], ["Ctrl+Alt+Numpad0"]);
        assert_eq!(d["copy"], ["Ctrl+Shift+C", "Ctrl+Insert"]);
        assert_eq!(d["paste"].len(), 3);
        assert_eq!(d["quake"], ["Win+Backquote"]);
        assert_eq!(d.len(), 25 + 9 + 10 + 1);
        assert!(Keybindings::default().conflicts().is_empty(), "{:?}", Keybindings::default().conflicts());
    }

    #[test]
    fn chord_parse_format_and_legacy_names() {
        assert_eq!(normalize_chord("shift+ctrl+alt+t").unwrap(), "Ctrl+Shift+Alt+T");
        assert_eq!(normalize_chord("Alt+Shift+Equal").unwrap(), "Shift+Alt+Equal");
        for (legacy, want) in [
            ("Ctrl+D1", "Ctrl+1"), ("Ctrl+OemComma", "Ctrl+Comma"), ("Ctrl+OemMinus", "Ctrl+Minus"),
            ("Ctrl+OemPlus", "Ctrl+Equal"), ("Ctrl+Oem3", "Ctrl+Backquote"), ("Ctrl+NumPad1", "Ctrl+Numpad1"),
            ("Ctrl+Shift+Arrow", "Ctrl+Shift+Arrow"), ("Ctrl+Alt+Up", "Ctrl+Alt+Up"), ("Shift+Prior", "Shift+PageUp"),
            ("Ctrl+Next", "Ctrl+PageDown"), ("Ctrl+Return", "Ctrl+Enter"), ("f12", "F12"), ("Win+Oem3", "Win+Backquote"),
            ("Ctrl+Escape", "Ctrl+Esc"), ("Ctrl+OemOpenBrackets", "Ctrl+BracketLeft"), ("ctrl+oem7", "Ctrl+Quote"),
        ] {
            assert_eq!(normalize_chord(legacy).unwrap(), want, "{legacy}");
        }
        for bad in ["", "Ctrl+", "Ctrl++", "Hyper+A", "Ctrl+F25", "Ctrl+F0", "Ctrl+Numpad10", "Ctrl+é", "Ctrl+Shift"] {
            assert!(normalize_chord(bad).is_err(), "{bad:?}");
        }
        assert_eq!(normalize_chord("F24").unwrap(), "F24");
    }

    #[test]
    fn effective_null_unbinds_and_unknown_kept() {
        let v = json!({"schemaVersion": 3, "bindings": {
            "newTab": "Ctrl+Shift+T", "closePane": null, "copy": ["Ctrl+Shift+C"], "future": "Ctrl+Alt+Q",
            "paste": ["Ctrl+Bogus", "Ctrl+V"], "search": "Nope+F", "settings": "Win+X", "zoomIn": 7 }});
        let (kb, invalid) = Keybindings::from_json(&v);
        let e = kb.effective();
        assert_eq!(e["newTab"], ["Ctrl+Shift+T"]);
        assert!(e["closePane"].is_empty());
        assert_eq!(e["copy"], ["Ctrl+Shift+C"]);
        assert_eq!(e["paste"], ["Ctrl+V"]);
        assert_eq!(e["search"], ["Ctrl+Shift+F"], "all-invalid keeps the default");
        assert_eq!(e["settings"], ["Ctrl+Comma"], "Win chord rejected for non-global actions");
        assert_eq!(e["zoomIn"], ["Ctrl+Equal"]);
        assert!(!e.contains_key("future"));
        assert_eq!(invalid.len(), 4, "{invalid:?}");
        assert_eq!(kb.to_json()["bindings"]["future"], "Ctrl+Alt+Q", "unknown actions survive a save");
        assert_eq!(kb.to_json()["bindings"]["closePane"], Value::Null);
    }

    #[test]
    fn conflicts_including_arrow() {
        let mut kb = Keybindings::default();
        kb.set_binding("newSession", &["Ctrl+T".into()]).unwrap();
        let c = kb.conflicts();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].actions, ["newSession", "newTab"]);
        kb.reset(None);
        kb.set_binding("search", &["Ctrl+Shift+Up".into()]).unwrap(); // movePaneFocus is Ctrl+Shift+Arrow
        let c = kb.conflicts();
        assert!(c.iter().any(|c| c.actions == ["movePaneFocus", "search"]), "{c:?}");
    }

    #[test]
    fn set_unbind_reset_and_shell_safe() {
        let mut kb = Keybindings::default();
        let j = kb.set_binding("newTab", &["ctrl+shift+t".into()]).unwrap();
        assert_eq!(j["bindings"]["newTab"], "Ctrl+Shift+T");
        let j = kb.set_binding("newTab", &["Ctrl+T".into()]).unwrap();
        assert_eq!(j["bindings"], json!({}), "back to default drops the override");
        assert!(kb.set_binding("nope", &["Ctrl+T".into()]).is_err());
        assert!(kb.set_binding("newTab", &["Ctrl+Wat".into()]).is_err());
        assert!(kb.set_binding("newTab", &["Win+T".into()]).is_err());
        assert!(kb.set_binding("quake", &["Win+F1".into()]).is_ok());
        assert_eq!(kb.unbind("copy").unwrap()["bindings"]["copy"], Value::Null);
        assert!(kb.effective()["copy"].is_empty());
        kb.reset(Some("copy"));
        assert_eq!(kb.effective()["copy"].len(), 2);
        kb.apply_shell_safe();
        let e = kb.effective();
        assert_eq!((e["newTab"][0].as_str(), e["closePane"][0].as_str(), e["togglePanel"][0].as_str()),
            ("Ctrl+Shift+T", "Ctrl+Shift+W", "Ctrl+Shift+E"));
        assert_eq!(e["paste"], defaults()["paste"], "the rest is unchanged");
        assert!(kb.conflicts().is_empty());
    }

    #[test]
    fn file_roundtrip_corrupt_and_legacy() {
        let d = tmp("io");
        let f = d.join("keybindings.json");
        assert_eq!(load(&f).status, LoadStatus::Missing);
        let mut kb = Keybindings::default();
        kb.set_binding("copy", &["Ctrl+Shift+C".into()]).unwrap();
        kb.unbind("closePane").unwrap();
        kb.save(&f).unwrap();
        let l = load(&f);
        assert_eq!(l.status, LoadStatus::Loaded);
        assert_eq!(l.keybindings, kb);

        std::fs::write(&f, "{ nope").unwrap();
        let l = load(&f);
        assert!(matches!(l.status, LoadStatus::Corrupt { .. }));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{ nope", "load leaves a corrupt file alone");
        kb.save(&f).unwrap(); // quarantines the corrupt one first
        assert!(std::fs::read_dir(&d).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("corrupt-")));

        // legacy C.5
        let g = d.join("legacy").join("keybindings.json");
        std::fs::create_dir_all(g.parent().unwrap()).unwrap();
        std::fs::write(&g, r#"{"Bindings":{"newTab":"Ctrl+OemPlus","settings":"Ctrl+OemComma","selectTab1":"Ctrl+D1","movePaneFocus":"Ctrl+Shift+Arrow","prevCommand":"Ctrl+Alt+Up"}}"#).unwrap();
        let l = load(&g);
        assert!(matches!(l.status, LoadStatus::Migrated { .. }), "{:?}", l.status);
        let e = l.keybindings.effective();
        assert_eq!(e["newTab"], ["Ctrl+Equal"]);
        assert_eq!(e["settings"], ["Ctrl+Comma"]);
        assert!(g.with_file_name("keybindings.legacy-bak.json").exists());
        let v: Value = serde_json::from_slice(&std::fs::read(&g).unwrap()).unwrap();
        assert_eq!(v["schemaVersion"], 3);
        assert_eq!(v["bindings"]["newTab"], "Ctrl+Equal");
        assert_eq!(load(&g).status, LoadStatus::Loaded, "second start sees a v3 file");
    }
}
