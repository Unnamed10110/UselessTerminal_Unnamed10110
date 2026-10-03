//! Workspaces (§16.4): named sets of tabs, stored in `workspaces.json`.

use crate::import::{convert_list, field, str_field};
use crate::store::{load_file, SAVE_DELAY, SCHEMA_VERSION};
use crate::{new_id, DataError, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use ut_fs::DebouncedWriter;

/// One tab of a workspace. `session_id` is stored when the tab came from a session (§24 #33).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceTab {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub title: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// [P1] pane layout tree, opaque to this crate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub tabs: Vec<WorkspaceTab>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self { id: new_id(), name: "My Workspace".into(), tabs: vec![] }
    }
}

/// v3 root of `workspaces.json`. (Legacy C.4 files are a bare PascalCase array.)
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WorkspacesFile {
    schema_version: u32,
    workspaces: Vec<Workspace>,
}

impl Default for WorkspacesFile {
    fn default() -> Self {
        Self { schema_version: SCHEMA_VERSION, workspaces: vec![] }
    }
}

fn parse(v: Value) -> std::result::Result<(Vec<Workspace>, bool), String> {
    match v {
        Value::Array(a) => {
            let blank = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
            let ws = convert_list(Some(&Value::Array(a)), "workspace", |o| {
                let tabs = convert_list(field(o, &["Tabs"]), "tab", |t| WorkspaceTab {
                    session_id: blank(str_field(t, &["SessionId"])),
                    title: str_field(t, &["Title"]).unwrap_or_default(),
                    command: str_field(t, &["Command"]).unwrap_or_default(),
                    cwd: blank(str_field(t, &["WorkingDirectory"])),
                    starting_command: blank(str_field(t, &["StartingCommand"])),
                    ..Default::default()
                });
                let id = blank(str_field(o, &["Id"])).unwrap_or_else(new_id);
                tabs.map(|tabs| Workspace { id, name: str_field(o, &["Name"]).unwrap_or_default(), tabs })
            })?;
            Ok((ws.into_iter().collect::<std::result::Result<_, _>>()?, true))
        }
        Value::Object(o) if o.contains_key("schemaVersion") => {
            let f: WorkspacesFile = serde_json::from_value(Value::Object(o)).map_err(|e| e.to_string())?;
            Ok((f.workspaces, false))
        }
        _ => Err("not a workspaces file".into()),
    }
}

#[derive(Default)]
struct State {
    items: Vec<Workspace>,
    banner: Option<String>,
}

/// Thread-safe workspace list with debounced (300 ms) atomic saves; a corrupt file is quarantined like `sessions.json`.
pub struct WorkspaceStore {
    path: PathBuf,
    writer: Arc<DebouncedWriter>,
    save_enabled: bool,
    version: AtomicU64,
    state: Mutex<State>,
}

impl WorkspaceStore {
    /// `%APPDATA%\UselessTerminal\workspaces.json`.
    pub fn default_path() -> PathBuf {
        ut_fs::app_data_dir().join("workspaces.json")
    }

    pub fn open(path: impl Into<PathBuf>, writer: Arc<DebouncedWriter>) -> Self {
        let path = path.into();
        let l = load_file(&path, "Workspaces", parse);
        let mut st = State { items: l.data.unwrap_or_default(), banner: l.banner };
        let mut seen = std::collections::HashSet::new();
        for w in &mut st.items {
            if w.id.is_empty() || !seen.insert(w.id.clone()) {
                w.id = new_id();
                seen.insert(w.id.clone());
            }
        }
        if l.migrated {
            let file = WorkspacesFile { workspaces: st.items.clone(), ..Default::default() };
            if let Err(e) = ut_fs::write_json_atomic(&path, &file) {
                st.banner.get_or_insert(format!("Could not save workspaces: {e}"));
            }
        }
        Self { path, writer, save_enabled: l.save_enabled, version: AtomicU64::new(0), state: Mutex::new(st) }
    }

    pub fn banner(&self) -> Option<String> {
        self.state.lock().banner.clone()
    }

    pub fn clear_banner(&self) {
        self.state.lock().banner = None;
    }

    /// Incremented by every change (the app emits `workspaces:changed` from it).
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn flush(&self) {
        self.writer.flush_all();
    }

    fn edit<R>(&self, f: impl FnOnce(&mut Vec<Workspace>) -> (R, bool)) -> R {
        let mut st = self.state.lock();
        let (r, changed) = f(&mut st.items);
        if changed {
            self.version.fetch_add(1, Ordering::Release);
            if self.save_enabled {
                let file = WorkspacesFile { workspaces: st.items.clone(), ..Default::default() };
                self.writer.queue_json(&self.path, SAVE_DELAY, &file);
            }
        }
        r
    }

    pub fn list(&self) -> Vec<Workspace> {
        self.state.lock().items.clone()
    }

    pub fn get(&self, id: &str) -> Option<Workspace> {
        self.state.lock().items.iter().find(|w| w.id == id).cloned()
    }

    /// Add a workspace (name required; a blank or colliding id is replaced). Returns the stored copy.
    pub fn add(&self, mut w: Workspace) -> Result<Workspace> {
        w.name = w.name.trim().to_string();
        if w.name.is_empty() {
            return Err(DataError::Invalid("Name is required.".into()));
        }
        Ok(self.edit(|items| {
            if w.id.is_empty() || items.iter().any(|x| x.id == w.id) {
                w.id = new_id();
            }
            items.push(w.clone());
            (w, true)
        }))
    }

    pub fn rename(&self, id: &str, name: &str) -> bool {
        let name = name.trim();
        self.edit(|items| match items.iter_mut().find(|w| w.id == id) {
            Some(w) if !name.is_empty() && w.name != name => {
                w.name = name.into();
                (true, true)
            }
            _ => (false, false),
        })
    }

    pub fn delete(&self, id: &str) -> bool {
        self.edit(|items| {
            let n = items.len();
            items.retain(|w| w.id != id);
            let removed = items.len() != n;
            (removed, removed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{files_with_prefix, tmpdir, writer};
    use serde_json::json;

    fn open(d: &std::path::Path) -> WorkspaceStore {
        WorkspaceStore::open(d.join("workspaces.json"), writer())
    }

    fn tab(title: &str) -> WorkspaceTab {
        WorkspaceTab { title: title.into(), command: "pwsh.exe".into(), ..Default::default() }
    }

    #[test]
    fn crud_and_roundtrip_keep_session_ids() {
        let d = tmpdir("ws-crud");
        let s = open(&d);
        assert!(s.banner().is_none() && s.list().is_empty());
        assert!(s.add(Workspace { name: "  ".into(), ..Default::default() }).is_err());
        let tabs = vec![
            WorkspaceTab {
                session_id: Some("abc".into()),
                cwd: Some(r"C:\Users\x\OneDrive - BEPSA DEL PARAGUAY SAECA".into()),
                starting_command: Some("ls".into()),
                color: Some("#00e5ff".into()),
                layout: Some(json!({"split": "h", "panes": [1, 2]})),
                ..tab("one")
            },
            tab("two"),
        ];
        let w = s.add(Workspace { id: String::new(), name: " Dev ".into(), tabs: tabs.clone() }).unwrap();
        assert_eq!((w.id.len(), w.name.as_str()), (32, "Dev"));
        let dup = s.add(Workspace { id: w.id.clone(), name: "Other".into(), tabs: vec![] }).unwrap();
        assert_ne!(dup.id, w.id);
        assert_eq!(s.version(), 2);
        assert!(s.rename(&dup.id, "Renamed") && !s.rename(&dup.id, "Renamed") && !s.rename(&dup.id, " ") && !s.rename("x", "y"));
        assert!(s.delete(&dup.id) && !s.delete(&dup.id));
        s.flush();

        let v: Value = serde_json::from_slice(&std::fs::read(d.join("workspaces.json")).unwrap()).unwrap();
        assert_eq!(v["schemaVersion"], 3);
        assert_eq!(v["workspaces"][0]["tabs"][0]["sessionId"], "abc");
        assert_eq!(v["workspaces"][0]["tabs"][0]["startingCommand"], "ls");
        assert!(v["workspaces"][0]["tabs"][1].get("sessionId").is_none(), "absent optionals are omitted");
        let s2 = open(&d);
        assert_eq!(s2.list().len(), 1);
        assert_eq!(s2.get(&w.id).unwrap().tabs, tabs);
        assert!(s2.get("nope").is_none());
    }

    #[test]
    fn corrupt_file_is_quarantined() {
        for (tag, text) in [("ws-corrupt1", "[ nope"), ("ws-corrupt2", r#"{"workspaces": []}"#), ("ws-corrupt3", r#"[{"Tabs": 5}]"#)] {
            let d = tmpdir(tag);
            std::fs::write(d.join("workspaces.json"), text).unwrap();
            let s = open(&d);
            assert_eq!(s.banner().as_deref(), Some("Workspaces file was unreadable and was backed up; starting empty."), "{tag}");
            assert!(s.list().is_empty());
            assert_eq!(files_with_prefix(&d, "workspaces.corrupt-").len(), 1);
            assert!(!d.join("workspaces.json").exists());
            s.add(Workspace { name: "n".into(), ..Default::default() }).unwrap();
            s.flush();
            assert!(d.join("workspaces.json").exists());
        }
    }

    #[test]
    fn legacy_c4_is_converted_with_backup() {
        let d = tmpdir("ws-legacy");
        let legacy = r#"[{"Id":"w1","Name":"Dev","Tabs":[
            {"SessionId":null,"Title":"t","Command":"pwsh.exe","WorkingDirectory":"C:\\x y","StartingCommand":""},
            {"SessionId":"abc","Title":"u","Command":"cmd.exe"}]},
            {"Id":"","Name":"Empty","Tabs":null}]"#;
        std::fs::write(d.join("workspaces.json"), legacy).unwrap();
        let s = open(&d);
        assert!(s.banner().is_none());
        assert_eq!(std::fs::read_to_string(&files_with_prefix(&d, "workspaces.legacy-bak")[0]).unwrap(), legacy);
        let v: Value = serde_json::from_slice(&std::fs::read(d.join("workspaces.json")).unwrap()).unwrap();
        assert_eq!(v["schemaVersion"], 3);
        let l = s.list();
        assert_eq!((l.len(), l[0].id.as_str(), l[0].name.as_str(), l[0].tabs.len()), (2, "w1", "Dev", 2));
        assert_eq!(l[0].tabs[0], WorkspaceTab { cwd: Some(r"C:\x y".into()), ..tab("t") });
        assert_eq!(l[0].tabs[1].session_id.as_deref(), Some("abc"));
        assert_eq!((l[1].id.len(), l[1].tabs.len()), (32, 0));
    }
}
