//! `sessions.json` store (§8): sessions, root folders and snippets, the §8.2 drag-and-drop rules,
//! load outcomes (first run / corrupt / legacy, §8.4, §16.5) and debounced atomic persistence.

use crate::import::{legacy_snippets, parse_root, Parsed};
use crate::model::{Folder, Session, Snippet, DEFAULT_COLOR};
use crate::{new_id, DataError, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use ut_fs::{DebouncedWriter, ReadJson};

pub const SCHEMA_VERSION: u32 = 3;
pub(crate) const SAVE_DELAY: Duration = Duration::from_millis(300);

/// The v3 root object of `sessions.json` (also the export format, §8.5).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionsFile {
    pub schema_version: u32,
    pub folders: Vec<Folder>,
    pub sessions: Vec<Session>,
    pub snippets: Vec<Snippet>,
}

impl Default for SessionsFile {
    fn default() -> Self {
        Self { schema_version: SCHEMA_VERSION, folders: vec![], sessions: vec![], snippets: vec![] }
    }
}

/// One session created by the first-run seeding (one per detected shell).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SeedEntry {
    pub name: String,
    pub shell_path: String,
    pub arguments: String,
    pub color: String,
}

/// Where a drag-and-drop landed (§8.2). JSON: `{"kind":"folder","id":"…"}` / `{"kind":"root"}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum Target {
    /// A folder's header row.
    Folder(String),
    /// A session card.
    Session(String),
    /// Empty tree space.
    Root,
    /// A folder card outside its header row (its children area). Like `Folder` for dragged sessions
    /// (dropped inside the folder); for a dragged folder it is "anything else" (moved to the end).
    FolderEdge(String),
}

/// Top/bottom half of the drop target (measured on the header row for folders, §8.2).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Half {
    Top,
    Bottom,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TreeFolder {
    #[serde(flatten)]
    pub folder: Folder,
    pub sessions: Vec<Session>,
}

/// Root folders by `sortOrder` with their sessions, then the root sessions (§8.2).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Tree {
    pub folders: Vec<TreeFolder>,
    pub sessions: Vec<Session>,
}

// ---------------------------------------------------------------- loading

/// Result of reading a data file; shared with `WorkspaceStore`.
pub(crate) struct Loaded<T> {
    pub data: Option<T>,
    pub first_run: bool,
    pub banner: Option<String>,
    /// False when the on-disk file could not be preserved: never overwrite it then.
    pub save_enabled: bool,
    /// Converted from a legacy format (already backed up): write the v3 file right away.
    pub migrated: bool,
}

/// Missing -> first run. Legacy -> `<name>.legacy-bak.json` + convert. Unreadable -> quarantine
/// (`<name>.corrupt-<ts>.json`) + banner; the caller must start empty and never re-seed (§8.4).
pub(crate) fn load_file<T>(path: &Path, what: &str, parse: impl FnOnce(Value) -> std::result::Result<(T, bool), String>) -> Loaded<T> {
    let mut l = Loaded { data: None, first_run: false, banner: None, save_enabled: true, migrated: false };
    match ut_fs::read_json::<Value>(path) {
        ReadJson::Missing => l.first_run = true,
        ReadJson::Ok(v) => match parse(v) {
            Ok((data, legacy)) => {
                l.data = Some(data);
                if legacy {
                    match ut_fs::backup_copy(path, "legacy") {
                        Ok(_) => l.migrated = true,
                        Err(e) => {
                            l.save_enabled = false;
                            l.banner = Some(format!("{what} file uses an old format and could not be backed up ({e}); changes will not be saved."));
                        }
                    }
                }
            }
            Err(_) => quarantine(path, what, &mut l),
        },
        ReadJson::Corrupt { .. } => quarantine(path, what, &mut l),
    }
    l
}

fn quarantine<T>(path: &Path, what: &str, l: &mut Loaded<T>) {
    l.banner = Some(match ut_fs::quarantine(path) {
        Ok(_) => format!("{what} file was unreadable and was backed up; starting empty."),
        Err(e) => {
            l.save_enabled = false;
            format!("{what} file was unreadable and could not be backed up ({e}); starting empty, changes will not be saved.")
        }
    });
}

// ------------------------------------------------------------------ state

#[derive(Clone, Copy)]
pub(crate) enum At<'a> {
    End,
    Before(&'a str),
    After(&'a str),
}

#[derive(Default)]
pub(crate) struct State {
    pub folders: Vec<Folder>,
    pub sessions: Vec<Session>,
    pub snippets: Vec<Snippet>,
    pub first_run: bool,
    pub banner: Option<String>,
}

impl State {
    pub fn file(&self) -> SessionsFile {
        SessionsFile {
            schema_version: SCHEMA_VERSION,
            folders: self.folders.clone(),
            sessions: self.sessions.clone(),
            snippets: self.snippets.clone(),
        }
    }

    pub fn has_folder(&self, id: &str) -> bool {
        self.folders.iter().any(|f| f.id == id)
    }

    /// Indices of one container's sessions in `sortOrder` (stable, so ties keep file order).
    fn order(&self, folder: Option<&str>) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.sessions.len()).filter(|&i| self.sessions[i].folder_id.as_deref() == folder).collect();
        v.sort_by_key(|&i| self.sessions[i].sort_order);
        v
    }

    fn sorted_folder_ids(&self) -> Vec<String> {
        let mut f: Vec<&Folder> = self.folders.iter().collect();
        f.sort_by_key(|f| f.sort_order);
        f.into_iter().map(|f| f.id.clone()).collect()
    }

    /// Every session index in sidebar order: folders by `sortOrder` with their sessions, then root.
    fn tree_order(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.sorted_folder_ids().iter().flat_map(|f| self.order(Some(f))).collect();
        v.extend(self.order(None));
        v
    }

    fn renumber(&mut self, folder: Option<&str>) {
        for (n, i) in self.order(folder).into_iter().enumerate() {
            self.sessions[i].sort_order = n as i32;
        }
    }

    fn set_folder_order(&mut self, ids: &[String]) {
        for (n, id) in ids.iter().enumerate() {
            if let Some(f) = self.folders.iter_mut().find(|f| &f.id == id) {
                f.sort_order = n as i32;
            }
        }
    }

    /// Move `ids` (in the given order) into `folder` at `at`, then renumber every affected container (§8.2).
    pub fn place(&mut self, ids: &[String], folder: Option<&str>, at: At) {
        let mut sources: Vec<Option<String>> = Vec::new();
        for s in self.sessions.iter().filter(|s| ids.contains(&s.id)) {
            if !sources.contains(&s.folder_id) {
                sources.push(s.folder_id.clone());
            }
        }
        let moving: Vec<usize> = ids.iter().filter_map(|id| self.sessions.iter().position(|s| &s.id == id)).collect();
        let mut ordered: Vec<usize> = self.order(folder).into_iter().filter(|i| !moving.contains(i)).collect();
        let at_index = |t: &str, off: usize| ordered.iter().position(|&i| self.sessions[i].id == t).map(|p| p + off);
        let pos = match at {
            At::End => None,
            At::Before(t) => at_index(t, 0),
            At::After(t) => at_index(t, 1),
        }
        .unwrap_or(ordered.len());
        ordered.splice(pos..pos, moving);
        for (n, &i) in ordered.iter().enumerate() {
            self.sessions[i].folder_id = folder.map(String::from);
            self.sessions[i].sort_order = n as i32;
        }
        for src in sources.iter().filter(|s| s.as_deref() != folder) {
            self.renumber(src.as_deref());
        }
    }

    /// The §8.2 drop table. Pure data; the caller decides whether anything changed.
    fn drop_items(&mut self, sessions: &[String], folder: Option<&str>, target: &Target, half: Half) {
        if let Some(fid) = folder {
            // Dragged folder: before/after another folder, "anything else" -> end.
            if !self.has_folder(fid) {
                return;
            }
            let mut ids = self.sorted_folder_ids();
            ids.retain(|i| i != fid);
            let pos = match target {
                Target::Folder(t) if t == fid => return,
                Target::Folder(t) => match ids.iter().position(|i| i == t) {
                    Some(p) => p + usize::from(half == Half::Bottom),
                    None => return,
                },
                _ => ids.len(),
            };
            ids.insert(pos, fid.to_string());
            self.set_folder_order(&ids);
            return;
        }
        // Dragged sessions keep their visual order.
        let dragged: Vec<String> = self.tree_order().into_iter().map(|i| &self.sessions[i].id).filter(|id| sessions.contains(id)).cloned().collect();
        if dragged.is_empty() {
            return;
        }
        match target {
            Target::Folder(f) | Target::FolderEdge(f) if self.has_folder(f) => self.place(&dragged, Some(f), At::End),
            Target::Session(s) if !dragged.contains(s) => {
                let Some(t) = self.sessions.iter().find(|x| &x.id == s) else { return };
                let tf = t.folder_id.clone();
                let at = if half == Half::Top { At::Before(s) } else { At::After(s) };
                self.place(&dragged, tf.as_deref(), at);
            }
            Target::Root => self.place(&dragged, None, At::End),
            _ => {} // dropped on a dragged item or an unknown target
        }
    }

    /// Unique non-empty ids, no dangling `folderId`, `sortOrder` 0..n-1 in every container.
    pub fn normalize(&mut self) {
        fn fix(ids: &mut HashSet<String>, id: &mut String) {
            if id.is_empty() || !ids.insert(id.clone()) {
                *id = new_id();
                ids.insert(id.clone());
            }
        }
        let (mut f, mut s, mut n) = (HashSet::new(), HashSet::new(), HashSet::new());
        self.folders.iter_mut().for_each(|x| fix(&mut f, &mut x.id));
        self.sessions.iter_mut().for_each(|x| fix(&mut s, &mut x.id));
        self.snippets.iter_mut().for_each(|x| fix(&mut n, &mut x.id));
        for i in 0..self.sessions.len() {
            if self.sessions[i].folder_id.as_deref().is_some_and(|id| !f.contains(id)) {
                self.sessions[i].folder_id = None;
            }
        }
        let ids = self.sorted_folder_ids();
        self.set_folder_order(&ids);
        self.renumber(None);
        for id in &ids {
            self.renumber(Some(id));
        }
        self.snippets.sort_by_key(|x| x.sort_order);
        self.snippets.iter_mut().enumerate().for_each(|(n, x)| x.sort_order = n as i32);
    }

    /// Add snippets that are not already present (same name + command), returning how many were added.
    pub fn merge_snippets(&mut self, items: Vec<Snippet>) -> usize {
        let mut added = 0;
        for mut s in items {
            if self.snippets.iter().any(|x| x.name.eq_ignore_ascii_case(&s.name) && x.command == s.command) {
                continue;
            }
            if s.id.is_empty() || self.snippets.iter().any(|x| x.id == s.id) {
                s.id = new_id();
            }
            s.sort_order = self.snippets.len() as i32;
            self.snippets.push(s);
            added += 1;
        }
        added
    }

    fn tree(&self) -> Tree {
        let members = |f: Option<&str>| self.order(f).into_iter().map(|i| self.sessions[i].clone()).collect::<Vec<_>>();
        let folders = self
            .sorted_folder_ids()
            .into_iter()
            .filter_map(|id| {
                let folder = self.folders.iter().find(|f| f.id == id)?.clone();
                Some(TreeFolder { sessions: members(Some(&id)), folder })
            })
            .collect();
        Tree { folders, sessions: members(None) }
    }
}

// ------------------------------------------------------------------ store

/// Sessions, folders and snippets. Thread-safe; every mutation bumps `version()` and queues a
/// debounced (300 ms) atomic save through the shared writer.
pub struct SessionStore {
    path: PathBuf,
    writer: Arc<DebouncedWriter>,
    save_enabled: bool,
    version: AtomicU64,
    state: Mutex<State>,
}

impl SessionStore {
    /// `%APPDATA%\UselessTerminal\sessions.json`.
    pub fn default_path() -> PathBuf {
        ut_fs::app_data_dir().join("sessions.json")
    }

    /// Load (or migrate, or quarantine) `path`. Check `first_run()` and `banner()` afterwards.
    /// A legacy `snippets.json` next to it is merged once and then moved to `snippets.legacy-bak.json`.
    pub fn open(path: impl Into<PathBuf>, writer: Arc<DebouncedWriter>) -> Self {
        let path = path.into();
        let l = load_file(&path, "Sessions", |v| {
            parse_root(v).map(|p| {
                let legacy = p.legacy;
                (p, legacy)
            })
        });
        let mut st = State { first_run: l.first_run, banner: l.banner, ..Default::default() };
        if let Some(Parsed { file, .. }) = l.data {
            (st.folders, st.sessions, st.snippets) = (file.folders, file.sessions, file.snippets);
        }
        st.normalize();

        // A legacy snippets.json (C.3) is merged once, then moved to snippets.legacy-bak.json. On a first run it
        // stays until the next launch so a crash before seeding loses nothing (the merge is idempotent).
        let snip_path = path.with_file_name("snippets.json");
        let merged = if l.save_enabled { legacy_snippets(&snip_path).map(|items| st.merge_snippets(items)) } else { None };
        let cleanup = merged.is_some() && !st.first_run;
        let saved = if l.migrated || (cleanup && merged > Some(0)) {
            ut_fs::write_json_atomic(&path, &st.file())
                .map_err(|e| st.banner = Some(format!("Could not save sessions: {e}")))
                .is_ok()
        } else {
            true
        };
        if saved && cleanup && ut_fs::backup_copy(&snip_path, "legacy").is_ok() {
            let _ = std::fs::remove_file(&snip_path);
        }
        Self { path, writer, save_enabled: l.save_enabled, version: AtomicU64::new(0), state: Mutex::new(st) }
    }

    /// True until `seed` has run on a missing file. Never true after a corrupt or legacy load.
    pub fn first_run(&self) -> bool {
        self.state.lock().first_run
    }

    pub fn banner(&self) -> Option<String> {
        self.state.lock().banner.clone()
    }

    pub fn clear_banner(&self) {
        self.state.lock().banner = None;
    }

    /// Incremented by every change (the app emits `sessions:changed` / `snippets:changed` from it).
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Write any pending save now (exit, logoff, hide to tray). Flushes the whole shared writer.
    pub fn flush(&self) {
        self.writer.flush_all();
    }

    /// Run `f` under the lock; when it reports a change, bump the version and queue a save.
    pub(crate) fn edit<R>(&self, f: impl FnOnce(&mut State) -> (R, bool)) -> R {
        let mut st = self.state.lock();
        let (r, changed) = f(&mut st);
        if changed {
            self.version.fetch_add(1, Ordering::Release);
            if self.save_enabled {
                self.writer.queue_json(&self.path, SAVE_DELAY, &st.file());
            }
        }
        r
    }

    /// The v3 root (export, §8.5).
    pub fn snapshot(&self) -> SessionsFile {
        self.state.lock().file()
    }

    // ---- sessions

    /// All sessions in sidebar order.
    pub fn list(&self) -> Vec<Session> {
        let st = self.state.lock();
        st.tree_order().into_iter().map(|i| st.sessions[i].clone()).collect()
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        self.state.lock().sessions.iter().find(|s| s.id == id).cloned()
    }

    pub fn tree(&self) -> Tree {
        self.state.lock().tree()
    }

    /// Flat search (§8.2): trimmed, case-insensitive over name, description and command line
    /// (path + arguments); blank query -> nothing; ordered by `sortOrder`.
    pub fn search(&self, query: &str) -> Vec<Session> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        let st = self.state.lock();
        let mut hits: Vec<Session> = st
            .tree_order()
            .into_iter()
            .map(|i| &st.sessions[i])
            .filter(|s| {
                let cmd = format!("{} {}", s.shell_path, s.arguments);
                [&s.name, &s.description, &cmd].iter().any(|f| f.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
        hits.sort_by_key(|s| s.sort_order);
        hits
    }

    /// Validate and append to its folder (root when `folderId` is unknown). A blank or colliding id is replaced.
    pub fn add_session(&self, mut s: Session) -> Result<Session> {
        s.validate()?;
        s.sanitize();
        Ok(self.edit(|st| {
            if s.id.is_empty() || st.sessions.iter().any(|x| x.id == s.id) {
                s.id = new_id();
            }
            if s.folder_id.as_deref().is_some_and(|f| !st.has_folder(f)) {
                s.folder_id = None;
            }
            let (id, folder, i) = (s.id.clone(), s.folder_id.clone(), st.sessions.len());
            st.sessions.push(s);
            st.place(&[id], folder.as_deref(), At::End);
            (st.sessions[i].clone(), true)
        }))
    }

    /// Replace the session with the same id. Keeps its position unless `folderId` changed.
    pub fn update_session(&self, mut s: Session) -> Result<Session> {
        s.validate()?;
        s.sanitize();
        self.edit(|st| {
            let Some(i) = st.sessions.iter().position(|x| x.id == s.id) else {
                return (Err(DataError::NotFound("Session")), false);
            };
            if s.folder_id.as_deref().is_some_and(|f| !st.has_folder(f)) {
                s.folder_id = None;
            }
            let new_folder = std::mem::replace(&mut s.folder_id, st.sessions[i].folder_id.clone());
            s.sort_order = st.sessions[i].sort_order;
            let moved = new_folder != s.folder_id;
            let id = s.id.clone();
            st.sessions[i] = s;
            if moved {
                st.place(&[id], new_folder.as_deref(), At::End);
            }
            (Ok(st.sessions[i].clone()), true)
        })
    }

    pub fn delete_session(&self, id: &str) -> bool {
        self.edit(|st| match st.sessions.iter().position(|s| s.id == id) {
            Some(i) => {
                let folder = st.sessions.remove(i).folder_id;
                st.renumber(folder.as_deref());
                (true, true)
            }
            None => (false, false),
        })
    }

    /// Copy of every field with a new id, placed right after the original.
    pub fn duplicate_session(&self, id: &str) -> Option<Session> {
        self.edit(|st| {
            let Some(copy) = st.sessions.iter().find(|s| s.id == id).map(Session::duplicate) else {
                return (None, false);
            };
            let (cid, folder, i) = (copy.id.clone(), copy.folder_id.clone(), st.sessions.len());
            st.sessions.push(copy);
            st.place(&[cid], folder.as_deref(), At::After(id));
            (Some(st.sessions[i].clone()), true)
        })
    }

    /// First-run seeding: one root session per entry. Only effective once (while `first_run()`); writes the file
    /// even for an empty list so it never repeats.
    pub fn seed(&self, entries: &[SeedEntry]) -> bool {
        self.edit(|st| {
            if !st.first_run {
                return (false, false);
            }
            st.first_run = false;
            for (n, e) in entries.iter().enumerate() {
                let color = if e.color.trim().is_empty() { DEFAULT_COLOR } else { e.color.as_str() };
                st.sessions.push(Session {
                    name: e.name.clone(),
                    shell_path: e.shell_path.clone(),
                    arguments: e.arguments.clone(),
                    color_tag: color.into(),
                    sort_order: n as i32,
                    ..Default::default()
                });
            }
            (true, true)
        })
    }

    /// Apply the whole §8.2 drop table. `folder` is the dragged folder (then `sessions` is ignored);
    /// otherwise `sessions` are dragged in sidebar order. Returns false (and saves nothing) for a no-op,
    /// such as a drop onto one of the dragged items.
    pub fn move_items(&self, sessions: &[String], folder: Option<&str>, target: &Target, half: Half) -> bool {
        self.edit(|st| {
            let before = (st.folders.clone(), st.sessions.clone());
            st.drop_items(sessions, folder, target, half);
            let changed = before != (st.folders.clone(), st.sessions.clone());
            (changed, changed)
        })
    }

    // ---- folders

    pub fn folders(&self) -> Vec<Folder> {
        let st = self.state.lock();
        st.sorted_folder_ids().iter().filter_map(|id| st.folders.iter().find(|f| &f.id == id).cloned()).collect()
    }

    pub fn add_folder(&self, name: &str) -> Folder {
        let name = if name.trim().is_empty() { "New Folder" } else { name.trim() };
        self.edit(|st| {
            let f = Folder { name: name.into(), sort_order: st.folders.len() as i32, ..Default::default() };
            st.folders.push(f.clone());
            (f, true)
        })
    }

    pub fn rename_folder(&self, id: &str, name: &str) -> bool {
        let name = name.trim();
        self.edit(|st| match st.folders.iter_mut().find(|f| f.id == id) {
            Some(f) if !name.is_empty() && f.name != name => {
                f.name = name.into();
                (true, true)
            }
            _ => (false, false),
        })
    }

    /// Delete a folder; its sessions move to the root list (appended in their order).
    pub fn delete_folder(&self, id: &str) -> bool {
        self.edit(|st| {
            if !st.has_folder(id) {
                return (false, false);
            }
            let ids: Vec<String> = st.order(Some(id)).into_iter().map(|i| st.sessions[i].id.clone()).collect();
            st.place(&ids, None, At::End);
            st.folders.retain(|f| f.id != id);
            let order = st.sorted_folder_ids();
            st.set_folder_order(&order);
            (true, true)
        })
    }

    pub fn move_folder_up(&self, id: &str) -> bool {
        self.shift_folder(id, -1)
    }

    pub fn move_folder_down(&self, id: &str) -> bool {
        self.shift_folder(id, 1)
    }

    fn shift_folder(&self, id: &str, delta: isize) -> bool {
        self.edit(|st| {
            let mut ids = st.sorted_folder_ids();
            let Some(p) = ids.iter().position(|i| i == id) else { return (false, false) };
            let Some(q) = p.checked_add_signed(delta).filter(|&q| q < ids.len()) else { return (false, false) };
            ids.swap(p, q);
            st.set_folder_order(&ids);
            (true, true)
        })
    }

    // ---- snippets

    /// Snippets by `sortOrder`.
    pub fn snippets(&self) -> Vec<Snippet> {
        let mut v = self.state.lock().snippets.clone();
        v.sort_by_key(|s| s.sort_order);
        v
    }

    fn check_snippet(s: &Snippet) -> Result<()> {
        if s.name.trim().is_empty() || s.command.trim().is_empty() {
            return Err(DataError::Invalid("Name and command are required.".into()));
        }
        Ok(())
    }

    pub fn add_snippet(&self, mut s: Snippet) -> Result<Snippet> {
        Self::check_snippet(&s)?;
        Ok(self.edit(|st| {
            if s.id.is_empty() || st.snippets.iter().any(|x| x.id == s.id) {
                s.id = new_id();
            }
            s.sort_order = st.snippets.len() as i32;
            st.snippets.push(s.clone());
            (s, true)
        }))
    }

    /// Replace by id, keeping the snippet's position.
    pub fn update_snippet(&self, mut s: Snippet) -> Result<Snippet> {
        Self::check_snippet(&s)?;
        self.edit(|st| match st.snippets.iter_mut().find(|x| x.id == s.id) {
            Some(x) => {
                s.sort_order = x.sort_order;
                *x = s.clone();
                (Ok(s), true)
            }
            None => (Err(DataError::NotFound("Snippet")), false),
        })
    }

    pub fn delete_snippet(&self, id: &str) -> bool {
        self.edit(|st| {
            let n = st.snippets.len();
            st.snippets.retain(|s| s.id != id);
            st.snippets.sort_by_key(|s| s.sort_order);
            st.snippets.iter_mut().enumerate().for_each(|(i, s)| s.sort_order = i as i32);
            let removed = st.snippets.len() != n;
            (removed, removed)
        })
    }

    /// New order by id; unknown ids are ignored and snippets not listed keep their order after the listed ones.
    pub fn reorder_snippets(&self, ids: &[String]) -> bool {
        self.edit(|st| {
            let mut before = st.snippets.clone();
            before.sort_by_key(|s| s.sort_order);
            st.snippets.sort_by_key(|s| s.sort_order);
            let rank = |s: &Snippet| ids.iter().position(|i| i == &s.id).unwrap_or(usize::MAX);
            st.snippets.sort_by_key(rank); // stable: unlisted keep their relative order
            st.snippets.iter_mut().enumerate().for_each(|(i, s)| s.sort_order = i as i32);
            let changed = before != st.snippets;
            (changed, changed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{files_with_prefix, tmpdir, writer};
    use serde_json::json;

    fn open(dir: &Path) -> SessionStore {
        SessionStore::open(dir.join("sessions.json"), writer())
    }

    fn sess(name: &str) -> Session {
        Session { name: name.into(), shell_path: "x.exe".into(), ..Default::default() }
    }

    fn names(v: &[Session]) -> String {
        v.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// "A[a1 a2] B[b1] r1 r2" (folder ids are their names in the fixture).
    fn layout(s: &SessionStore) -> String {
        let t = s.tree();
        let mut parts: Vec<String> = t.folders.iter().map(|f| format!("{}[{}]", f.folder.id, names(&f.sessions))).collect();
        parts.push(names(&t.sessions));
        parts.join(" ").trim().to_string()
    }

    fn assert_renumbered(s: &SessionStore) {
        let t = s.tree();
        for (n, f) in t.folders.iter().enumerate() {
            assert_eq!(f.folder.sort_order, n as i32);
            for (i, x) in f.sessions.iter().enumerate() {
                assert_eq!(x.sort_order, i as i32, "{}", x.name);
            }
        }
        for (i, x) in t.sessions.iter().enumerate() {
            assert_eq!(x.sort_order, i as i32, "{}", x.name);
        }
    }

    /// A[a1 a2 a3] B[b1 b2] r1 r2 r3, ids == names.
    fn fixture(tag: &str) -> (SessionStore, PathBuf) {
        let d = tmpdir(tag);
        let mk = |id: &str, folder: Option<&str>, n: i32| json!({"id": id, "name": id, "shellPath": "x.exe", "folderId": folder, "sortOrder": n});
        let sessions = [("a1", Some("A"), 0), ("a2", Some("A"), 1), ("a3", Some("A"), 2), ("b1", Some("B"), 0), ("b2", Some("B"), 1), ("r1", None, 0), ("r2", None, 1), ("r3", None, 2)];
        let v = json!({"schemaVersion": 3,
            "folders": [{"id": "A", "name": "A", "sortOrder": 0}, {"id": "B", "name": "B", "sortOrder": 1}],
            "sessions": sessions.iter().map(|(i, f, n)| mk(i, *f, *n)).collect::<Vec<_>>(),
            "snippets": []});
        std::fs::write(d.join("sessions.json"), v.to_string()).unwrap();
        (open(&d), d)
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn mv(s: &SessionStore, drag: &[&str], target: Target, half: Half) -> bool {
        s.move_items(&ids(drag), None, &target, half)
    }

    fn t_session(id: &str) -> Target {
        Target::Session(id.into())
    }

    fn t_folder(id: &str) -> Target {
        Target::Folder(id.into())
    }

    #[test]
    fn first_run_seeds_once_and_never_again() {
        let d = tmpdir("seed");
        let s = open(&d);
        assert!(s.first_run() && s.banner().is_none());
        let e = vec![
            SeedEntry { name: "PowerShell".into(), shell_path: "pwsh.exe".into(), arguments: "-NoLogo".into(), color: "#00e5ff".into() },
            SeedEntry { name: "cmd".into(), shell_path: "cmd.exe".into(), arguments: String::new(), color: String::new() },
        ];
        assert!(s.seed(&e));
        assert!(!s.first_run() && !s.seed(&e));
        let l = s.list();
        assert_eq!((names(&l).as_str(), l[1].color_tag.as_str(), l[0].arguments.as_str()), ("PowerShell cmd", "#00ff44", "-NoLogo"));
        s.flush();

        let s = open(&d);
        assert!(!s.first_run() && !s.seed(&e));
        assert_eq!(s.list().len(), 2);
        // a user who deleted everything must not get the seeds back (§24 #16)
        for x in s.list() {
            assert!(s.delete_session(&x.id));
        }
        s.flush();
        let s = open(&d);
        assert!(!s.first_run() && !s.seed(&e) && s.list().is_empty());
    }

    #[test]
    fn corrupt_file_is_quarantined_never_reseeded_never_overwritten() {
        for (tag, text) in [("corrupt1", "{ nope"), ("corrupt2", r#"{"foo": 1}"#), ("corrupt3", ""), ("corrupt4", r#"{"schemaVersion":3,"sessions":"x"}"#)] {
            let d = tmpdir(tag);
            std::fs::write(d.join("sessions.json"), text).unwrap();
            let s = open(&d);
            assert_eq!(s.banner().as_deref(), Some("Sessions file was unreadable and was backed up; starting empty."));
            assert!(!s.first_run() && !s.seed(&[]) && s.list().is_empty(), "{tag}");
            let q = files_with_prefix(&d, "sessions.corrupt-");
            assert_eq!(q.len(), 1, "{tag}");
            assert_eq!(std::fs::read_to_string(&q[0]).unwrap(), text);
            assert!(!d.join("sessions.json").exists());
            // first change writes a fresh valid file
            s.add_session(sess("n")).unwrap();
            s.flush();
            let v: Value = serde_json::from_slice(&std::fs::read(d.join("sessions.json")).unwrap()).unwrap();
            assert_eq!(v["schemaVersion"], 3);
            s.clear_banner();
            assert!(s.banner().is_none());
        }
    }

    #[test]
    fn legacy_v2_is_converted_with_backup() {
        let d = tmpdir("legacy2");
        let legacy = json!({"Version": 2,
            "Folders": [{"Id": "f1", "Name": "Work", "ParentId": null, "SortOrder": 3}],
            "Sessions": [
                {"Id": "s1", "Name": "Dev", "Description": "d", "ShellPath": "pwsh.exe", "Arguments": "-NoLogo",
                 "WorkingDirectory": "C:\\Users\\x\\OneDrive - BEPSA DEL PARAGUAY SAECA", "StartingCommand": "ls", "ColorTag": "#123456",
                 "IconGlyph": "E756", "FolderId": "f1", "SortOrder": 5, "ThemeBackground": "#101010", "ThemeFontSize": 14,
                 "EnvironmentVariables": "A=1"},
                {"Id": "", "Name": "Loose", "ShellPath": "cmd.exe", "FolderId": "gone", "SortOrder": 1, "ColorTag": ""}
            ]})
        .to_string();
        std::fs::write(d.join("sessions.json"), &legacy).unwrap();
        let s = open(&d);
        assert!(!s.first_run() && s.banner().is_none());
        assert_eq!(std::fs::read_to_string(&files_with_prefix(&d, "sessions.legacy-bak")[0]).unwrap(), legacy);
        // v3 is on disk immediately, no flush needed
        let v: Value = serde_json::from_slice(&std::fs::read(d.join("sessions.json")).unwrap()).unwrap();
        assert_eq!(v["schemaVersion"], 3);
        let dev = s.get("s1").unwrap();
        assert_eq!(
            (dev.name.as_str(), dev.shell_path.as_str(), dev.arguments.as_str(), dev.starting_command.as_str(), dev.color_tag.as_str()),
            ("Dev", "pwsh.exe", "-NoLogo", "ls", "#123456")
        );
        assert_eq!((dev.folder_id.as_deref(), dev.font_size, dev.environment.as_str(), dev.theme_background.as_str()), (Some("f1"), 14, "A=1", "#101010"));
        assert!(dev.working_directory.contains("OneDrive - BEPSA"));
        let loose = s.list().into_iter().find(|x| x.name == "Loose").unwrap();
        assert_eq!(loose.id.len(), 32);
        assert_eq!((loose.folder_id.clone(), loose.color_tag.as_str()), (None, "#00ff44")); // dangling folder -> root
        assert_renumbered(&s);
        assert_eq!(s.folders()[0].name, "Work");
    }

    #[test]
    fn legacy_v1_array_is_converted() {
        let d = tmpdir("legacy1");
        std::fs::write(d.join("sessions.json"), r#"[{"Id":"a","Name":"One","ShellPath":"cmd.exe","SortOrder":1},{"Id":"b","Name":"Two","ShellPath":"pwsh.exe","SortOrder":0}]"#).unwrap();
        let s = open(&d);
        assert_eq!(names(&s.list()), "Two One");
        assert_eq!(files_with_prefix(&d, "sessions.legacy-bak").len(), 1);
        assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(d.join("sessions.json")).unwrap()).unwrap()["schemaVersion"], 3);
    }

    #[test]
    fn legacy_snippets_json_is_merged_once() {
        let d = tmpdir("legacysnip");
        let v3 = json!({"schemaVersion": 3, "folders": [], "sessions": [], "snippets": [{"id": "k", "name": "Keep", "command": "echo k", "appendEnter": false, "sortOrder": 0}]});
        std::fs::write(d.join("sessions.json"), v3.to_string()).unwrap();
        std::fs::write(d.join("snippets.json"), r#"[{"Id":"k","Name":"Old","Command":"ls","SortOrder":0},{"Id":"z","Name":"Keep","Command":"echo k","SortOrder":1}]"#).unwrap();
        let s = open(&d);
        let sn = s.snippets();
        assert_eq!(sn.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Keep", "Old"]); // 2nd "Keep" is a duplicate
        assert_ne!(sn[1].id, "k"); // colliding id regenerated
        assert!(sn[1].append_enter && !sn[0].append_enter);
        assert!(!d.join("snippets.json").exists());
        assert_eq!(files_with_prefix(&d, "snippets.legacy-bak").len(), 1);
        drop(s);
        let s = open(&d);
        assert_eq!(s.snippets().len(), 2);
    }

    #[test]
    fn legacy_snippets_on_first_run_survive_until_next_launch() {
        let d = tmpdir("legacysnip-first");
        std::fs::write(d.join("snippets.json"), r#"[{"Id":"z","Name":"S","Command":"ls","SortOrder":0}]"#).unwrap();
        let s = open(&d);
        assert!(s.first_run());
        assert_eq!(s.snippets().len(), 1);
        assert!(d.join("snippets.json").exists() && !d.join("sessions.json").exists());
        s.seed(&[]);
        s.flush();
        let s = open(&d);
        assert!(!s.first_run());
        assert_eq!(s.snippets().len(), 1);
        assert!(!d.join("snippets.json").exists());
    }

    #[test]
    fn session_crud_and_validation() {
        let d = tmpdir("crud");
        let s = open(&d);
        assert!(s.add_session(Session { shell_path: String::new(), ..sess("n") }).is_err());
        assert!(s.add_session(Session { name: " ".into(), ..sess("n") }).is_err());
        let v0 = s.version();
        let a = s.add_session(Session { font_size: 99, ..sess("a") }).unwrap();
        let b = s.add_session(sess("b")).unwrap();
        assert_eq!((a.sort_order, b.sort_order, a.font_size), (0, 1, 0));
        assert_eq!(s.version(), v0 + 2);
        // a colliding id is regenerated
        let c = s.add_session(Session { id: a.id.clone(), ..sess("c") }).unwrap();
        assert_ne!(c.id, a.id);
        // an unknown folder falls back to root
        let e = s.add_session(Session { folder_id: Some("nope".into()), ..sess("e") }).unwrap();
        assert_eq!(e.folder_id, None);
        // update keeps position
        let u = s.update_session(Session { name: "A2".into(), color_tag: "#abcdef".into(), sort_order: 50, ..a.clone() }).unwrap();
        assert_eq!((u.name.as_str(), u.sort_order, u.color_tag.as_str()), ("A2", 0, "#abcdef"));
        assert!(matches!(s.update_session(sess("ghost")), Err(DataError::NotFound(_))));
        assert!(s.update_session(Session { name: "".into(), ..a.clone() }).is_err());
        assert_eq!(names(&s.list()), "A2 b c e");
        // moving through update changes folder and appends
        let f = s.add_folder("F");
        s.update_session(Session { folder_id: Some(f.id.clone()), ..s.get(&b.id).unwrap() }).unwrap();
        assert_eq!(s.tree().folders[0].sessions[0].name, "b");
        assert_renumbered(&s);
        let before = s.version();
        assert!(s.delete_session(&a.id) && !s.delete_session(&a.id));
        assert_eq!(s.version(), before + 1);
        assert_renumbered(&s);
        assert!(s.get(&a.id).is_none());
    }

    #[test]
    fn duplicate_session_copies_everything_after_the_original() {
        let d = tmpdir("dup");
        let s = open(&d);
        let a = s.add_session(Session { environment: "A=1".into(), theme_preset: "Dracula".into(), font_size: 12, ..sess("a") }).unwrap();
        s.add_session(sess("b")).unwrap();
        let c = s.duplicate_session(&a.id).unwrap();
        assert_ne!(c.id, a.id);
        assert_eq!(c.sort_order, 1);
        assert_eq!(Session { id: a.id.clone(), sort_order: a.sort_order, ..c }, a);
        assert_eq!(names(&s.list()), "a a b");
        assert_renumbered(&s);
        assert!(s.duplicate_session("nope").is_none());
    }

    #[test]
    fn folders_add_rename_move_delete() {
        let (s, _d) = fixture("folders");
        let n = s.add_folder("  ");
        assert_eq!(n.name, "New Folder");
        assert!(s.rename_folder(&n.id, " Docs ") && !s.rename_folder(&n.id, "Docs") && !s.rename_folder(&n.id, " ") && !s.rename_folder("x", "y"));
        let order = || s.folders().iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(order(), ["A", "B", "Docs"]);
        assert!(s.move_folder_up(&n.id) && s.move_folder_up(&n.id) && !s.move_folder_up(&n.id));
        assert_eq!(order(), ["Docs", "A", "B"]);
        assert!(s.move_folder_down(&n.id) && s.move_folder_down(&n.id) && !s.move_folder_down(&n.id));
        assert_eq!(order(), ["A", "B", "Docs"]);
        // deleting a folder moves its sessions to the root list
        assert!(s.delete_folder("B") && !s.delete_folder("B"));
        assert_eq!(layout(&s).replace(&n.id, "Docs"), "A[a1 a2 a3] Docs[] r1 r2 r3 b1 b2");
        assert_renumbered(&s);
    }

    #[test]
    fn drop_sessions_on_folders_and_root() {
        let (s, _d) = fixture("drop-folder");
        assert!(mv(&s, &["r1"], t_folder("B"), Half::Top));
        assert_eq!(layout(&s), "A[a1 a2 a3] B[b1 b2 r1] r2 r3");
        assert!(mv(&s, &["b1"], Target::FolderEdge("A".into()), Half::Bottom));
        assert_eq!(layout(&s), "A[a1 a2 a3 b1] B[b2 r1] r2 r3");
        assert!(mv(&s, &["a1"], Target::Root, Half::Top));
        assert_eq!(layout(&s), "A[a2 a3 b1] B[b2 r1] r2 r3 a1");
        // already in the folder: appended to its end
        assert!(mv(&s, &["a2"], t_folder("A"), Half::Top));
        assert_eq!(layout(&s), "A[a3 b1 a2] B[b2 r1] r2 r3 a1");
        assert_renumbered(&s);
        assert!(!mv(&s, &["a2"], t_folder("nope"), Half::Top));
    }

    #[test]
    fn drop_sessions_before_and_after_a_session() {
        let (s, _d) = fixture("drop-session");
        assert!(mv(&s, &["r3"], t_session("a2"), Half::Top));
        assert_eq!(layout(&s), "A[a1 r3 a2 a3] B[b1 b2] r1 r2");
        assert!(mv(&s, &["r1"], t_session("a2"), Half::Bottom));
        assert_eq!(layout(&s), "A[a1 r3 a2 r1 a3] B[b1 b2] r2");
        assert!(mv(&s, &["a1"], t_session("a3"), Half::Bottom)); // same container
        assert_eq!(layout(&s), "A[r3 a2 r1 a3 a1] B[b1 b2] r2");
        assert!(mv(&s, &["a1"], t_session("r3"), Half::Top));
        assert_eq!(layout(&s), "A[a1 r3 a2 r1 a3] B[b1 b2] r2");
        assert!(mv(&s, &["b2"], t_session("r2"), Half::Top)); // into the root
        assert_eq!(layout(&s), "A[a1 r3 a2 r1 a3] B[b1] b2 r2");
        assert_renumbered(&s);
    }

    #[test]
    fn drop_on_a_dragged_item_is_a_noop() {
        let (s, _d) = fixture("drop-noop");
        let (before, v) = (layout(&s), s.version());
        assert!(!mv(&s, &["a1", "a2"], t_session("a2"), Half::Top));
        assert!(!mv(&s, &["r1"], t_session("r1"), Half::Bottom));
        assert!(!mv(&s, &["r1"], t_session("missing"), Half::Bottom));
        assert!(!mv(&s, &["missing"], Target::Root, Half::Top));
        assert!(!mv(&s, &[], Target::Root, Half::Top));
        assert!(!mv(&s, &["r3"], Target::Root, Half::Top)); // already last at root
        assert_eq!((layout(&s), s.version()), (before, v));
    }

    #[test]
    fn multi_drag_keeps_sidebar_order_across_containers() {
        let (s, _d) = fixture("drop-multi");
        // given out of order; dragged in folder/sortOrder order: a2 (A), b1 (B), r3 (root)
        assert!(mv(&s, &["r3", "b1", "a2"], t_session("r1"), Half::Bottom));
        assert_eq!(layout(&s), "A[a1 a3] B[b2] r1 a2 b1 r3 r2");
        assert_renumbered(&s);
        assert!(mv(&s, &["a2", "r2"], t_folder("B"), Half::Top));
        assert_eq!(layout(&s), "A[a1 a3] B[b2 a2 r2] r1 b1 r3");
        assert_renumbered(&s);
    }

    #[test]
    fn drop_folders() {
        let (s, _d) = fixture("drop-folders");
        let drag = |f: &str, t: Target, h: Half| s.move_items(&[], Some(f), &t, h);
        let order = || s.folders().iter().map(|f| f.id.clone()).collect::<String>();
        assert!(drag("A", t_folder("B"), Half::Bottom));
        assert_eq!(order(), "BA");
        assert!(drag("A", t_folder("B"), Half::Top));
        assert_eq!(order(), "AB");
        assert!(!drag("A", t_folder("A"), Half::Top)); // onto itself
        assert!(!drag("A", t_folder("zzz"), Half::Top));
        assert!(!drag("zzz", t_folder("A"), Half::Top));
        // anything else -> end
        assert!(drag("A", t_session("r1"), Half::Top));
        assert_eq!(order(), "BA");
        assert!(!drag("A", Target::Root, Half::Top)); // already last
        assert!(drag("B", Target::Root, Half::Top));
        assert_eq!(order(), "AB");
        assert!(drag("A", Target::FolderEdge("B".into()), Half::Top));
        assert_eq!(order(), "BA");
        // sessions are untouched, folder sort orders renumbered
        assert_eq!(layout(&s), "B[b1 b2] A[a1 a2 a3] r1 r2 r3");
        assert_renumbered(&s);
    }

    #[test]
    fn target_json_shape() {
        assert_eq!(serde_json::to_value(Target::Root).unwrap(), json!({"kind": "root"}));
        assert_eq!(serde_json::to_value(Target::FolderEdge("x".into())).unwrap(), json!({"kind": "folderEdge", "id": "x"}));
        assert_eq!(serde_json::from_value::<Target>(json!({"kind": "session", "id": "s"})).unwrap(), t_session("s"));
        assert_eq!(serde_json::to_value(Half::Bottom).unwrap(), json!("bottom"));
    }

    #[test]
    fn tree_and_search() {
        let d = tmpdir("search");
        let s = open(&d);
        s.add_session(Session { name: "Build box".into(), shell_path: "ssh.exe".into(), arguments: "dev@10.0.0.5".into(), ..Default::default() }).unwrap();
        let f = s.add_folder("F");
        s.add_session(Session { name: "PowerShell".into(), shell_path: r"C:\Program Files\PowerShell\7\pwsh.exe".into(), description: "Daily DRIVER".into(), folder_id: Some(f.id.clone()), ..Default::default() }).unwrap();
        s.add_session(Session { name: "cmd".into(), shell_path: "cmd.exe".into(), ..Default::default() }).unwrap();
        let t = s.tree();
        assert_eq!((t.folders.len(), names(&t.folders[0].sessions).as_str(), names(&t.sessions).as_str()), (1, "PowerShell", "Build box cmd"));
        assert_eq!(names(&s.list()), "PowerShell Build box cmd");
        assert!(s.search("   ").is_empty() && s.search("").is_empty());
        assert_eq!(names(&s.search("  POWER  ")), "PowerShell");
        assert_eq!(names(&s.search("pwsh")), "PowerShell"); // command
        assert_eq!(names(&s.search("driver")), "PowerShell"); // description
        assert_eq!(names(&s.search("10.0.0")), "Build box"); // arguments
        assert_eq!(names(&s.search("ssh.exe dev@")), "Build box"); // spans path + arguments
        assert_eq!(names(&s.search(".exe")), "PowerShell Build box cmd");
        assert!(s.search("nothing").is_empty());
        let j = serde_json::to_value(&t).unwrap();
        assert_eq!(j["folders"][0]["name"], "F");
        assert_eq!(j["folders"][0]["sortOrder"], 0);
        assert_eq!(j["folders"][0]["sessions"][0]["name"], "PowerShell");
    }

    #[test]
    fn search_orders_by_sort_order() {
        let (s, _d) = fixture("search-order");
        // sortOrder ties across containers keep sidebar order; lower sortOrder first
        assert_eq!(names(&s.search("r")), "r1 r2 r3");
        assert_eq!(names(&s.search("b")), "b1 b2");
        assert_eq!(names(&s.search("1")), "a1 b1 r1");
        assert_eq!(names(&s.search("2")), "a2 b2 r2");
        // literal "ordered by sortOrder": ties keep sidebar order, so containers interleave
        assert_eq!(names(&s.search("e")), "a1 b1 r1 a2 b2 r2 a3 r3");
    }

    #[test]
    fn debounced_save_and_flush_roundtrip() {
        let d = tmpdir("persist");
        let s = open(&d);
        let f = s.add_folder("Work");
        let a = s.add_session(Session { folder_id: Some(f.id.clone()), environment: "A=1".into(), ..sess("a") }).unwrap();
        s.add_snippet(Snippet { name: "n".into(), command: "ls".into(), ..Default::default() }).unwrap();
        assert_eq!(s.version(), 3);
        s.flush();
        let v: Value = serde_json::from_slice(&std::fs::read(d.join("sessions.json")).unwrap()).unwrap();
        assert_eq!((v["schemaVersion"].clone(), v["sessions"][0]["folderId"].clone()), (json!(3), json!(f.id)));
        assert_eq!(v["snippets"][0]["appendEnter"], true);
        let s2 = open(&d);
        assert_eq!(s2.get(&a.id).unwrap(), a);
        assert_eq!(s2.snippets().len(), 1);
        assert!(!d.join("sessions.json.tmp").exists());
    }

    #[test]
    fn debounce_waits_then_writes_by_itself() {
        let d = tmpdir("debounce");
        let s = open(&d);
        s.add_session(sess("a")).unwrap();
        assert!(!d.join("sessions.json").exists(), "saves are debounced");
        std::thread::sleep(Duration::from_millis(900));
        assert!(d.join("sessions.json").exists());
    }

    #[test]
    fn snippets_crud_and_reorder() {
        let d = tmpdir("snippets");
        let s = open(&d);
        assert!(s.add_snippet(Snippet { name: "x".into(), ..Default::default() }).is_err());
        let mk = |n: &str| s.add_snippet(Snippet { name: n.into(), command: format!("echo {n}"), ..Default::default() }).unwrap();
        let (a, b, c) = (mk("a"), mk("b"), mk("c"));
        assert_eq!((a.sort_order, c.sort_order), (0, 2));
        let order = || s.snippets().iter().map(|x| x.name.clone()).collect::<String>();
        let v = s.version();
        assert!(s.reorder_snippets(&ids(&[&c.id, "bogus", &a.id])));
        assert_eq!(order(), "cab");
        assert_eq!(s.version(), v + 1);
        assert!(!s.reorder_snippets(&ids(&[&c.id, &a.id, &b.id])));
        assert_eq!(s.version(), v + 1);
        let u = s.update_snippet(Snippet { name: "A!".into(), append_enter: false, sort_order: 99, ..a.clone() }).unwrap();
        assert_eq!((u.sort_order, u.append_enter), (1, false));
        assert!(matches!(s.update_snippet(Snippet { id: "x".into(), ..a.clone() }), Err(DataError::NotFound(_))));
        assert!(s.delete_snippet(&c.id) && !s.delete_snippet(&c.id));
        assert_eq!(s.snippets().iter().map(|x| x.sort_order).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn load_repairs_ids_orphans_and_ties() {
        let d = tmpdir("normalize");
        let v = json!({"schemaVersion": 3,
            "folders": [{"id": "f", "name": "F", "sortOrder": 0}, {"id": "f", "name": "G", "sortOrder": 0}],
            "sessions": [
                {"id": "s", "name": "one", "shellPath": "x", "folderId": "f", "sortOrder": 0},
                {"id": "s", "name": "two", "shellPath": "x", "folderId": "ghost", "sortOrder": 0},
                {"name": "three", "shellPath": "x", "sortOrder": 0}]});
        std::fs::write(d.join("sessions.json"), v.to_string()).unwrap();
        let s = open(&d);
        let l = s.list();
        assert_eq!(l.len(), 3);
        let all: std::collections::HashSet<_> = l.iter().map(|x| x.id.clone()).collect();
        assert_eq!(all.len(), 3);
        assert_eq!(s.folders().iter().map(|f| f.id.as_str()).collect::<std::collections::HashSet<_>>().len(), 2);
        assert_eq!(l.iter().filter(|x| x.folder_id.is_none()).count(), 2);
        assert_renumbered(&s);
    }
}
