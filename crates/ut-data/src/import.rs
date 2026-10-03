//! Import / export (§8.5), legacy WPF formats (Appendix C.2/C.3, §16.5) and Windows Terminal import (§8.6).

use crate::env::process_env;
use crate::model::{expand_percent, Folder, Session, Snippet};
use crate::store::{At, SessionStore, SessionsFile, State};
use crate::{new_id, DataError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ImportMode {
    Merge,
    Replace,
}

/// Numbers actually applied, not numbers found (§8.6).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// Sessions added.
    pub added: usize,
    /// Sessions skipped as duplicates.
    pub skipped: usize,
    /// Sessions discarded by a Replace import.
    pub replaced: usize,
    /// Snippets added (Merge) or loaded (Replace).
    pub snippets_added: usize,
}

// ------------------------------------------------------------------ parsing (v3 + legacy)

#[derive(Clone)]
pub(crate) struct Parsed {
    pub file: SessionsFile,
    pub legacy: bool,
    /// The input carried a snippets list (Replace keeps the current snippets otherwise).
    pub has_snippets: bool,
}

/// First key of `names` present in `o`, ignoring ASCII case (legacy files are PascalCase).
pub(crate) fn field<'a>(o: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|n| o.get(*n).or_else(|| o.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v)))
}

pub(crate) fn str_field(o: &Map<String, Value>, names: &[&str]) -> Option<String> {
    field(o, names)?.as_str().map(String::from)
}

fn int_field(o: &Map<String, Value>, names: &[&str]) -> Option<i32> {
    field(o, names)?.as_i64().and_then(|n| i32::try_from(n).ok())
}

/// Convert a legacy list; a missing/null list is empty, a malformed one is an error (never silently drop data).
pub(crate) fn convert_list<T>(v: Option<&Value>, what: &str, f: impl Fn(&Map<String, Value>) -> T) -> std::result::Result<Vec<T>, String> {
    match v {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(a)) => a.iter().map(|x| x.as_object().map(&f).ok_or_else(|| format!("a {what} entry is not an object"))).collect(),
        Some(_) => Err(format!("{what}s is not a list")),
    }
}

fn nonblank(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.trim().is_empty())
}

fn legacy_session(o: &Map<String, Value>) -> Session {
    let mut s = Session::default();
    let set = |dst: &mut String, names: &[&str]| {
        if let Some(x) = str_field(o, names) {
            *dst = x;
        }
    };
    set(&mut s.name, &["Name"]);
    set(&mut s.description, &["Description"]);
    set(&mut s.shell_path, &["ShellPath"]);
    set(&mut s.arguments, &["Arguments"]);
    set(&mut s.working_directory, &["WorkingDirectory"]);
    set(&mut s.starting_command, &["StartingCommand"]);
    set(&mut s.theme_background, &["ThemeBackground"]);
    set(&mut s.environment, &["EnvironmentVariables", "Environment"]);
    // IconGlyph was never displayed (C.2): ignored.
    if let Some(id) = nonblank(str_field(o, &["Id"])) {
        s.id = id;
    }
    if let Some(c) = nonblank(str_field(o, &["ColorTag"])) {
        s.color_tag = c;
    }
    s.folder_id = nonblank(str_field(o, &["FolderId"]));
    s.sort_order = int_field(o, &["SortOrder"]).unwrap_or(0);
    s.font_size = int_field(o, &["ThemeFontSize", "FontSize"]).unwrap_or(0);
    s
}

fn legacy_folder(o: &Map<String, Value>) -> Folder {
    let mut f = Folder::default();
    if let Some(id) = nonblank(str_field(o, &["Id"])) {
        f.id = id;
    }
    if let Some(n) = str_field(o, &["Name"]) {
        f.name = n;
    }
    f.sort_order = int_field(o, &["SortOrder"]).unwrap_or(0);
    f
}

fn legacy_snippet(o: &Map<String, Value>) -> Snippet {
    let mut s = Snippet::default();
    if let Some(id) = nonblank(str_field(o, &["Id"])) {
        s.id = id;
    }
    s.name = str_field(o, &["Name"]).unwrap_or_default();
    s.command = str_field(o, &["Command"]).unwrap_or_default();
    s.sort_order = int_field(o, &["SortOrder"]).unwrap_or(0);
    if let Some(b) = field(o, &["AppendEnter"]).and_then(Value::as_bool) {
        s.append_enter = b;
    }
    s
}

/// A legacy `snippets.json` (C.3) next to `sessions.json`; `None` when absent or not a list.
pub(crate) fn legacy_snippets(path: &Path) -> Option<Vec<Snippet>> {
    match ut_fs::read_json::<Value>(path) {
        ut_fs::ReadJson::Ok(Value::Array(a)) => Some(a.iter().filter_map(Value::as_object).map(legacy_snippet).collect()),
        _ => None,
    }
}

/// Understand a sessions file: v3 (`schemaVersion`), legacy v2 (`{Version, Folders, Sessions}`) or legacy v1 (bare array).
pub(crate) fn parse_root(v: Value) -> std::result::Result<Parsed, String> {
    match v {
        Value::Array(a) => {
            let sessions = convert_list(Some(&Value::Array(a)), "session", legacy_session)?;
            Ok(Parsed { file: SessionsFile { sessions, ..Default::default() }, legacy: true, has_snippets: false })
        }
        Value::Object(o) if o.contains_key("schemaVersion") => {
            let has_snippets = o.contains_key("snippets");
            let file = serde_json::from_value(Value::Object(o)).map_err(|e| e.to_string())?;
            Ok(Parsed { file, legacy: false, has_snippets })
        }
        Value::Object(o) if field(&o, &["Version", "Folders", "Sessions"]).is_some() => {
            let snippets = field(&o, &["Snippets"]);
            Ok(Parsed {
                file: SessionsFile {
                    folders: convert_list(field(&o, &["Folders"]), "folder", legacy_folder)?,
                    sessions: convert_list(field(&o, &["Sessions"]), "session", legacy_session)?,
                    snippets: convert_list(snippets, "snippet", legacy_snippet)?,
                    ..Default::default()
                },
                legacy: true,
                has_snippets: snippets.is_some(),
            })
        }
        _ => Err("not a sessions file".into()),
    }
}

// ------------------------------------------------------------------ export / import

/// The v3 root object as pretty JSON (§8.5; default file name `useless-terminal-sessions.json`).
pub fn export_json(store: &SessionStore) -> String {
    // Plain structs with string keys: serialization cannot fail.
    serde_json::to_string_pretty(&store.snapshot()).unwrap_or_default()
}

/// Import v3, legacy v2 or legacy v1 text. `Merge` regenerates colliding ids, de-duplicates by
/// name + command (sessions and snippets) and reuses same-named folders; `Replace` swaps folders and
/// sessions (and snippets, when the input has them).
pub fn import_json(store: &SessionStore, text: &str, mode: ImportMode) -> Result<ImportReport> {
    let bad = |e: String| DataError::Parse(format!("Not a valid sessions file: {e}"));
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| bad(e.to_string()))?;
    let parsed = parse_root(v).map_err(bad)?;
    Ok(store.edit(|st| match mode {
        ImportMode::Merge => {
            let r = merge(st, parsed);
            (r, r.added + r.snippets_added > 0)
        }
        ImportMode::Replace => (replace(st, parsed), true),
    }))
}

fn same_command(a: &Session, b: &Session) -> bool {
    let key = |s: &Session| format!("{}\0{}\0{}", s.name, s.shell_path.trim(), s.arguments.trim()).to_lowercase();
    key(a) == key(b)
}

fn merge(st: &mut State, p: Parsed) -> ImportReport {
    let mut r = ImportReport::default();
    let mut folders = p.file.folders;
    folders.sort_by_key(|f| f.sort_order);
    let mut folder_map = std::collections::HashMap::new();
    for f in folders {
        let existing = st.folders.iter().find(|x| x.name.eq_ignore_ascii_case(&f.name)).map(|x| x.id.clone());
        let actual = existing.unwrap_or_else(|| {
            let mut nf = Folder { sort_order: st.folders.len() as i32, ..f.clone() };
            if nf.id.is_empty() || st.has_folder(&nf.id) {
                nf.id = new_id();
            }
            st.folders.push(nf.clone());
            nf.id
        });
        folder_map.insert(f.id, actual);
    }
    let mut sessions = p.file.sessions;
    sessions.sort_by_key(|s| s.sort_order); // stable: keeps the relative order inside each container
    for mut s in sessions {
        if st.sessions.iter().any(|x| same_command(x, &s)) {
            r.skipped += 1;
            continue;
        }
        s.folder_id = s.folder_id.as_ref().and_then(|f| folder_map.get(f).cloned());
        if s.id.is_empty() || st.sessions.iter().any(|x| x.id == s.id) {
            s.id = new_id();
        }
        let (id, folder) = (s.id.clone(), s.folder_id.clone());
        st.sessions.push(s);
        st.place(&[id], folder.as_deref(), At::End);
        r.added += 1;
    }
    r.snippets_added = st.merge_snippets(p.file.snippets);
    r
}

fn replace(st: &mut State, p: Parsed) -> ImportReport {
    let replaced = st.sessions.len();
    st.folders = p.file.folders;
    st.sessions = p.file.sessions;
    let mut snippets_added = 0;
    if p.has_snippets {
        snippets_added = p.file.snippets.len();
        st.snippets = p.file.snippets;
    }
    st.normalize();
    ImportReport { added: st.sessions.len(), skipped: 0, replaced, snippets_added }
}

/// Add already-built sessions (Windows Terminal, SSH config, ...) at the root, skipping any whose name
/// already exists (exact match). Reports the number ACTUALLY added (§8.6, §8.7).
pub fn add_imported(store: &SessionStore, items: Vec<Session>) -> ImportReport {
    store.edit(|st| {
        let mut r = ImportReport::default();
        for mut s in items {
            if st.sessions.iter().any(|x| x.name == s.name) {
                r.skipped += 1;
                continue;
            }
            if s.id.is_empty() || st.sessions.iter().any(|x| x.id == s.id) {
                s.id = new_id();
            }
            s.folder_id = None;
            let id = s.id.clone();
            st.sessions.push(s);
            st.place(&[id], None, At::End);
            r.added += 1;
        }
        (r, r.added > 0)
    })
}

// ------------------------------------------------------------------ Windows Terminal (§8.6)

const WT_COLOR: &str = "#00e5ff";

/// Remove `//` and `/* */` comments (string-aware) and trailing commas so the result parses as JSON.
pub fn strip_jsonc(src: &str) -> String {
    let src = src.trim_start_matches('\u{feff}');
    let mut out = String::with_capacity(src.len());
    let (mut chars, mut in_str) = (src.chars().peekable(), false);
    while let Some(c) = chars.next() {
        match c {
            _ if in_str => {
                out.push(c);
                if c == '\\' {
                    out.extend(chars.next());
                } else if c == '"' {
                    in_str = false;
                }
            }
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                while chars.next_if(|&n| n != '\n').is_some() {}
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    strip_trailing_commas(&out)
}

fn strip_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let (mut out, mut in_str, mut i) = (String::with_capacity(s.len()), false, 0);
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                i += 1;
                out.push(chars[i]);
            } else if c == '"' {
                in_str = false;
            }
        } else if c == ',' && chars[i + 1..].iter().find(|c| !c.is_whitespace()).is_some_and(|c| matches!(c, '}' | ']')) {
            // trailing comma: drop it
        } else {
            in_str = c == '"';
            out.push(c);
        }
        i += 1;
    }
    out
}

/// The three locations of Windows Terminal's `settings.json` under `%LOCALAPPDATA%`, in lookup order.
pub fn wt_settings_candidates(local_app_data: &Path) -> [PathBuf; 3] {
    let pkg = |name: &str| local_app_data.join("Packages").join(name).join("LocalState").join("settings.json");
    [
        pkg("Microsoft.WindowsTerminal_8wekyb3d8bbwe"),
        pkg("Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe"),
        local_app_data.join("Microsoft").join("Windows Terminal").join("settings.json"),
    ]
}

pub fn find_wt_settings_in(local_app_data: &Path) -> Option<PathBuf> {
    wt_settings_candidates(local_app_data).into_iter().find(|p| p.is_file())
}

/// First existing `settings.json` (stable, preview, unpackaged) under `%LOCALAPPDATA%`.
pub fn find_wt_settings() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| Some(ut_fs::home_dir().join("AppData").join("Local")))?;
    find_wt_settings_in(&local)
}

/// What the shell detector found; used for WT dynamic profiles (§8.6 [P1]).
#[derive(Clone, Debug, Default)]
pub struct Resolver {
    /// `Windows.Terminal.PowershellCore` -> this executable (falls back to `pwsh.exe`).
    pub pwsh_path: Option<String>,
    /// `Windows.Terminal.Wsl` -> `<wsl_exe> -d <name>` (falls back to `wsl.exe`).
    pub wsl_exe: Option<String>,
}

fn hex_color(s: &str) -> Option<String> {
    let h = s.trim().strip_prefix('#')?.to_lowercase();
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match h.len() {
        6 => Some(format!("#{h}")),
        3 => Some(format!("#{}", h.chars().flat_map(|c| [c, c]).collect::<String>())),
        _ => None,
    }
}

fn wt_session(p: &Value, r: &Resolver, env: &[(String, String)]) -> Option<Session> {
    let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let name = text("name")?;
    // Only a literal `true` hides; anything else (even a string) must not abort the import.
    if p.get("hidden") == Some(&Value::Bool(true)) {
        return None;
    }
    let (shell_path, arguments) = match text("commandline") {
        Some(c) => (expand_percent(c, env), String::new()),
        None => match text("source")? {
            "Windows.Terminal.Wsl" => {
                let distro = if name.contains(' ') { format!("\"{name}\"") } else { name.to_string() };
                (r.wsl_exe.clone().unwrap_or_else(|| "wsl.exe".into()), format!("-d {distro}"))
            }
            "Windows.Terminal.PowershellCore" => (r.pwsh_path.clone().unwrap_or_else(|| "pwsh.exe".into()), String::new()),
            _ => return None,
        },
    };
    // `icon` may be an emoji or an ms-appx:// URI; only real file paths are usable as an override.
    let icon = text("icon").map(|i| expand_percent(i, env)).filter(|i| {
        let l = i.to_lowercase();
        !l.contains("://") && [".ico", ".png", ".exe", ".jpg", ".jpeg"].iter().any(|e| l.ends_with(e))
    });
    Some(Session {
        name: format!("[WT] {name}"),
        shell_path,
        arguments,
        working_directory: text("startingDirectory").map(|d| expand_percent(d, env)).unwrap_or_default(),
        color_tag: text("tabColor").and_then(hex_color).unwrap_or_else(|| WT_COLOR.into()),
        icon_override: icon.unwrap_or_default(),
        ..Default::default()
    })
}

/// Convert a Windows Terminal `settings.json` text (JSONC) to sessions. `profiles.list[]` or an array
/// `profiles`; `hidden: true` skipped; `commandline` goes whole into `shellPath` with blank arguments;
/// duplicates by exact name keep the first. `%VAR%` in the command line, directory and icon is expanded.
pub fn wt_profiles_to_sessions(json: &str, resolver: &Resolver) -> Result<Vec<Session>> {
    let v: Value = serde_json::from_str(&strip_jsonc(json))
        .map_err(|e| DataError::Parse(format!("Windows Terminal settings.json is not valid JSON: {e}")))?;
    let profiles = match v.get("profiles") {
        Some(Value::Array(a)) => a.as_slice(),
        Some(o) => o.get("list").and_then(Value::as_array).map_or(&[][..], Vec::as_slice),
        None => &[],
    };
    let env = process_env();
    let mut seen = HashSet::new();
    Ok(profiles.iter().filter_map(|p| wt_session(p, resolver, &env)).filter(|s| seen.insert(s.name.clone())).collect())
}

/// Find, parse and import the user's Windows Terminal profiles (§8.6).
pub fn wt_import(store: &SessionStore, resolver: &Resolver) -> Result<ImportReport> {
    let path = find_wt_settings().ok_or_else(|| DataError::Parse("Windows Terminal settings.json was not found.".into()))?;
    let text = std::fs::read_to_string(&path).map_err(|e| DataError::Parse(format!("Cannot read {}: {e}", path.display())))?;
    Ok(add_imported(store, wt_profiles_to_sessions(&text, resolver)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{tmpdir, writer};
    use serde_json::json;

    fn open(d: &Path) -> SessionStore {
        SessionStore::open(d.join("sessions.json"), writer())
    }

    fn sess(name: &str, cmd: &str) -> Session {
        Session { name: name.into(), shell_path: cmd.into(), ..Default::default() }
    }

    fn names(s: &SessionStore) -> Vec<String> {
        s.list().into_iter().map(|x| x.name).collect()
    }

    #[test]
    fn export_import_roundtrip() {
        let (da, db) = (tmpdir("exp-a"), tmpdir("exp-b"));
        let a = open(&da);
        let f = a.add_folder("Work");
        a.add_session(Session { folder_id: Some(f.id), environment: "A=1".into(), theme_preset: "Dracula".into(), ..sess("one", "pwsh.exe") }).unwrap();
        a.add_session(sess("two", "cmd.exe")).unwrap();
        a.add_snippet(Snippet { name: "s".into(), command: "ls".into(), append_enter: false, ..Default::default() }).unwrap();
        let text = export_json(&a);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["schemaVersion"], 3);
        assert_eq!(v["sessions"][0]["shellPath"], "pwsh.exe");
        let b = open(&db);
        let r = import_json(&b, &text, ImportMode::Replace).unwrap();
        assert_eq!(r, ImportReport { added: 2, skipped: 0, replaced: 0, snippets_added: 1 });
        assert_eq!(serde_json::to_value(a.snapshot()).unwrap(), serde_json::to_value(b.snapshot()).unwrap());
        // importing the same file again in Merge mode adds nothing
        let r = import_json(&b, &text, ImportMode::Merge).unwrap();
        assert_eq!(r, ImportReport { added: 0, skipped: 2, replaced: 0, snippets_added: 0 });
        assert_eq!(b.folders().len(), 1);
    }

    #[test]
    fn merge_dedupes_by_name_and_command_and_regenerates_ids() {
        let d = tmpdir("merge");
        let s = open(&d);
        let w = s.add_folder("Work");
        s.add_session(Session { id: "id1".into(), ..sess("A", "x.exe") }).unwrap();
        s.add_session(Session { id: "id2".into(), ..sess("B", "y.exe") }).unwrap();
        let text = json!({"schemaVersion": 3,
            "folders": [{"id": "w2", "name": "work", "sortOrder": 0}, {"id": w.id, "name": "Other", "sortOrder": 1}],
            "sessions": [
                {"id": "id1", "name": "A", "shellPath": "x.exe"},                              // duplicate
                {"id": "id1", "name": "C", "shellPath": "x.exe", "folderId": "w2"},           // id collision, folder reused
                {"id": "n3", "name": "D", "shellPath": "z.exe", "folderId": w.id},            // goes to the new "Other"
                {"id": "", "name": "E", "shellPath": "e.exe", "folderId": "unknown"},
                {"id": "id2", "name": "B", "shellPath": "other.exe"}],                        // same name, other command
            "snippets": [{"id": "x", "name": "n", "command": "c"}]})
        .to_string();
        let r = import_json(&s, &text, ImportMode::Merge).unwrap();
        assert_eq!(r, ImportReport { added: 4, skipped: 1, replaced: 0, snippets_added: 1 });
        let all = s.list();
        let ids: std::collections::HashSet<_> = all.iter().map(|x| x.id.clone()).collect();
        assert_eq!(all.len(), 6);
        assert_eq!(ids.len(), 6);
        assert!(ids.iter().all(|i| !i.is_empty()));
        let find = |n: &str| all.iter().find(|x| x.name == n && x.shell_path != "y.exe").cloned().unwrap();
        assert_ne!(find("C").id, "id1");
        assert_eq!(find("C").folder_id, Some(w.id.clone())); // matched the existing folder by name
        let other = s.folders().into_iter().find(|f| f.name == "Other").unwrap();
        assert_ne!(other.id, w.id); // colliding folder id regenerated
        assert_eq!(find("D").folder_id, Some(other.id));
        assert_eq!(find("E").folder_id, None);
        assert_eq!(s.folders().len(), 2);
        assert_eq!(s.snippets().len(), 1);
        let t = s.tree();
        assert_eq!(t.folders[0].sessions.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["C"]);
    }

    #[test]
    fn imports_legacy_v2_and_v1() {
        let d = tmpdir("imp-legacy");
        let s = open(&d);
        s.add_snippet(Snippet { name: "keep".into(), command: "me".into(), ..Default::default() }).unwrap();
        let v2 = json!({"Version": 2, "Folders": [{"Id": "f", "Name": "F", "SortOrder": 0}],
            "Sessions": [{"Id": "1", "Name": "One", "ShellPath": "a.exe", "FolderId": "f", "ThemeFontSize": 12, "EnvironmentVariables": "K=V"},
                         {"Id": "2", "Name": "Two", "ShellPath": "b.exe"}]})
        .to_string();
        assert_eq!(import_json(&s, &v2, ImportMode::Merge).unwrap().added, 2);
        let one = s.get("1").unwrap();
        assert_eq!((one.font_size, one.environment.as_str(), one.folder_id.as_deref()), (12, "K=V", Some("f")));
        // v1 bare array, Replace: old sessions are discarded, snippets survive (the input has none)
        let v1 = r#"[{"Id":"9","Name":"Nine","ShellPath":"n.exe"}]"#;
        let r = import_json(&s, v1, ImportMode::Replace).unwrap();
        assert_eq!((r.added, r.replaced), (1, 2));
        assert_eq!(names(&s), ["Nine"]);
        assert!(s.folders().is_empty());
        assert_eq!(s.snippets().len(), 1);
        // a v3 file that carries an (empty) snippets list does replace them
        import_json(&s, r#"{"schemaVersion":3,"sessions":[],"snippets":[]}"#, ImportMode::Replace).unwrap();
        assert!(s.snippets().is_empty() && s.list().is_empty());
    }

    #[test]
    fn import_rejects_garbage_without_touching_the_store() {
        let d = tmpdir("imp-bad");
        let s = open(&d);
        s.add_session(sess("keep", "k.exe")).unwrap();
        let v = s.version();
        for bad in ["not json", "{}", "123", "null", r#"{"schemaVersion":3,"sessions":5}"#, r#"{"Sessions":[1]}"#, r#"{"Sessions":{}}"#] {
            let e = import_json(&s, bad, ImportMode::Replace).unwrap_err();
            assert!(matches!(e, DataError::Parse(_)), "{bad}");
        }
        assert_eq!((s.version(), names(&s)), (v, vec!["keep".to_string()]));
        // BOM is tolerated
        assert!(import_json(&s, "\u{feff}[]", ImportMode::Merge).is_ok());
    }

    #[test]
    fn add_imported_reports_what_was_actually_added() {
        let d = tmpdir("add-imported");
        let s = open(&d);
        s.add_session(sess("[WT] Foo", "foo.exe")).unwrap();
        let items = vec![
            sess("[WT] Foo", "other.exe"),
            Session { id: "same".into(), ..sess("[WT] Bar", "bar.exe") },
            Session { id: "same".into(), folder_id: Some("ghost".into()), ..sess("[WT] Baz", "baz.exe") },
            sess("[WT] Bar", "again.exe"),
        ];
        let r = add_imported(&s, items.clone());
        assert_eq!((r.added, r.skipped), (2, 2));
        assert_eq!(names(&s), ["[WT] Foo", "[WT] Bar", "[WT] Baz"]);
        let all = s.list();
        assert_ne!(all[1].id, all[2].id);
        assert!(all.iter().all(|x| x.folder_id.is_none()));
        let v = s.version();
        assert_eq!(add_imported(&s, items).added, 0);
        assert_eq!(s.version(), v);
    }

    fn parsed(s: &str) -> Value {
        serde_json::from_str(&strip_jsonc(s)).unwrap_or_else(|e| panic!("{e}: {}", strip_jsonc(s)))
    }

    #[test]
    fn strip_jsonc_is_string_aware() {
        let src = "{\"u\": \"http://x//y\", /* c */ \"v\": \"/* keep */\", // line\n \"w\": \"a\\\"//b\", \"t\": \",}\", \"arr\": [1, 2, ], \"o\": {\"k\": 1,},}";
        assert_eq!(parsed(src), json!({"u": "http://x//y", "v": "/* keep */", "w": "a\"//b", "t": ",}", "arr": [1, 2], "o": {"k": 1}}));
        assert_eq!(parsed("\u{feff}{}"), json!({}));
        assert_eq!(parsed("{\"a\":1} // end"), json!({"a": 1}));
        assert_eq!(parsed("[1, // x\n ]"), json!([1]));
        assert_eq!(parsed("{\"a\":1} /* unterminated"), json!({"a": 1}));
        assert_eq!(parsed("{\"n\": \"Ünïcode ✓ \\\\\", /**/ \"m\": 1}"), json!({"n": "Ünïcode ✓ \\", "m": 1}));
        assert_eq!(parsed("{/*/ not closed yet */ \"a\": 1}"), json!({"a": 1}));
        assert_eq!(strip_jsonc("{\"a\": 1}"), "{\"a\": 1}");
    }

    const WT: &str = r##"{
    // comment with "quotes" and a trailing slash /
    "$schema": "https://aka.ms/terminal-profiles-schema", // after-value comment
    "defaultProfile": "{61c54bbd-c2c6-5271-96e7-009a87ff44bf}",
    "profiles": {
        "defaults": { "font": { "face": "Cascadia Code" }, },
        "list": [
            { /* block */ "guid": "{1}", "name": "Windows PowerShell",
              "commandline": "%UT_DATA_TEST_VAR%\\WindowsPowerShell\\powershell.exe",
              "startingDirectory": "%UT_DATA_TEST_VAR%\\work", "hidden": false, },
            { "name": "Command Prompt", "commandline": "cmd.exe", "tabColor": "#F80", "icon": "C:\\icons\\cmd.png" },
            { "name": "Slash // inside", "commandline": "ssh://host /* not a comment */", "hidden": "maybe" },
            { "name": "Hidden one", "commandline": "cmd.exe", "hidden": true },
            { "name": "Ubuntu-22.04", "source": "Windows.Terminal.Wsl" },
            { "name": "Debian Pro", "source": "Windows.Terminal.Wsl", "hidden": false },
            { "name": "PowerShell", "source": "Windows.Terminal.PowershellCore", "icon": "ms-appx:///ProfileIcons/pwsh.png" },
            { "name": "Azure Cloud Shell", "source": "Windows.Terminal.Azure" },
            { "name": "Command Prompt", "commandline": "dup.exe" },
            { "commandline": "noname.exe" },
            "not an object",
        ],
    },
    "schemes": [],
}"##;

    #[test]
    fn windows_terminal_fixture() {
        std::env::set_var("UT_DATA_TEST_VAR", r"C:\Users\x y\OneDrive - BEPSA");
        let r = Resolver { pwsh_path: Some(r"C:\Program Files\PowerShell\7\pwsh.exe".into()), wsl_exe: Some(r"C:\Windows\System32\wsl.exe".into()) };
        let v = wt_profiles_to_sessions(WT, &r).unwrap();
        let n: Vec<&str> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(n, ["[WT] Windows PowerShell", "[WT] Command Prompt", "[WT] Slash // inside", "[WT] Ubuntu-22.04", "[WT] Debian Pro", "[WT] PowerShell"]);
        // %VAR% expanded in the command line and the starting directory; arguments stay blank
        assert_eq!(v[0].shell_path, r"C:\Users\x y\OneDrive - BEPSA\WindowsPowerShell\powershell.exe");
        assert_eq!((v[0].arguments.as_str(), v[0].working_directory.as_str(), v[0].color_tag.as_str()), ("", r"C:\Users\x y\OneDrive - BEPSA\work", "#00e5ff"));
        // tabColor and icon
        assert_eq!((v[1].shell_path.as_str(), v[1].color_tag.as_str(), v[1].icon_override.as_str()), ("cmd.exe", "#ff8800", r"C:\icons\cmd.png"));
        // `//` and `/* */` inside strings survive; a non-boolean `hidden` does not abort or hide
        assert_eq!(v[2].shell_path, "ssh://host /* not a comment */");
        assert_eq!(v[2].full_command(), "ssh://host /* not a comment */");
        // dynamic profiles
        assert_eq!((v[3].shell_path.as_str(), v[3].arguments.as_str()), (r"C:\Windows\System32\wsl.exe", "-d Ubuntu-22.04"));
        assert_eq!(v[3].full_command(), r#""C:\Windows\System32\wsl.exe" -d Ubuntu-22.04"#);
        assert_eq!(v[4].arguments, r#"-d "Debian Pro""#);
        assert_eq!((v[5].shell_path.as_str(), v[5].icon_override.as_str()), (r"C:\Program Files\PowerShell\7\pwsh.exe", ""));
        assert!(v.iter().all(|s| s.id.len() == 32 && s.folder_id.is_none()));
        // without detected executables the dynamic profiles fall back to PATH lookups
        let v = wt_profiles_to_sessions(WT, &Resolver::default()).unwrap();
        assert_eq!((v[3].shell_path.as_str(), v[5].shell_path.as_str()), ("wsl.exe", "pwsh.exe"));
    }

    #[test]
    fn windows_terminal_profiles_array_and_errors() {
        let r = Resolver::default();
        let v = wt_profiles_to_sessions(r#"{"profiles": [{"name": "X", "commandline": "x.exe -a"}, {"name": "Y"}]}"#, &r).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].name.as_str(), v[0].shell_path.as_str(), v[0].arguments.as_str(), v[0].color_tag.as_str()), ("[WT] X", "x.exe -a", "", "#00e5ff"));
        assert!(wt_profiles_to_sessions("{}", &r).unwrap().is_empty());
        assert!(wt_profiles_to_sessions(r#"{"profiles": 5}"#, &r).unwrap().is_empty());
        assert!(matches!(wt_profiles_to_sessions("{ nope", &r), Err(DataError::Parse(_))));
        let v = wt_profiles_to_sessions(r##"{"profiles":{"list":[{"name":"C","commandline":"c.exe","tabColor":"nonsense"},{"name":"D","commandline":"d.exe","tabColor":"#ABCDEF","icon":"🐧"}]}}"##, &r).unwrap();
        assert_eq!((v[0].color_tag.as_str(), v[1].color_tag.as_str(), v[1].icon_override.as_str()), ("#00e5ff", "#abcdef", ""));
    }

    #[test]
    fn wt_settings_lookup_order_and_end_to_end() {
        let local = tmpdir("wt-find");
        assert_eq!(find_wt_settings_in(&local), None);
        let c = wt_settings_candidates(&local);
        assert!(c[0].to_string_lossy().contains("Microsoft.WindowsTerminal_8wekyb3d8bbwe"));
        assert!(c[1].to_string_lossy().contains("Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe"));
        assert!(c[2].ends_with(r"Microsoft\Windows Terminal\settings.json"));
        for (i, p) in c.iter().enumerate().rev() {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, format!(r#"{{"profiles":[{{"name":"P{i}","commandline":"p{i}.exe"}}]}}"#)).unwrap();
            assert_eq!(find_wt_settings_in(&local).as_ref(), Some(p), "later candidates must not win");
        }
        let found = find_wt_settings_in(&local).unwrap();
        let sessions = wt_profiles_to_sessions(&std::fs::read_to_string(found).unwrap(), &Resolver::default()).unwrap();
        let store = open(&tmpdir("wt-store"));
        assert_eq!(add_imported(&store, sessions.clone()).added, 1);
        assert_eq!(add_imported(&store, sessions).added, 0);
        assert_eq!(names(&store), ["[WT] P0"]);
    }
}
