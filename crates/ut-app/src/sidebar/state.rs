//! Sidebar UI state that survives restarts: which folders are collapsed and whether the bottom sections are open (§8.2
//! "expansion state is persisted per folder"). It lives in its own small file next to the stores because none of the
//! spec's data files hold UI state; a missing or unreadable file simply means "everything expanded".

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use ut_fs::{DebouncedWriter, ReadJson};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct UiState {
    /// Folders the user collapsed. New folders start expanded, so only the exceptions are stored.
    pub collapsed: BTreeSet<String>,
    pub snippets_open: bool,
    pub shells_open: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self { collapsed: BTreeSet::new(), snippets_open: true, shells_open: false }
    }
}

impl UiState {
    pub fn path() -> PathBuf {
        ut_fs::app_data_dir().join("sidebar.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        match ut_fs::read_json::<UiState>(path) {
            ReadJson::Ok(s) => s,
            _ => Self::default(),
        }
    }

    /// Debounced atomic save through the shared writer (flushed on exit with the other files).
    pub fn save(&self, writer: &DebouncedWriter) {
        writer.queue_json(Self::path(), Duration::from_millis(300), self);
    }

    pub fn is_open(&self, folder: &str) -> bool {
        !self.collapsed.contains(folder)
    }

    /// `true` when something changed (the caller saves).
    pub fn set_open(&mut self, folder: &str, open: bool) -> bool {
        if open {
            self.collapsed.remove(folder)
        } else {
            self.collapsed.insert(folder.to_string())
        }
    }

    /// Drop collapsed ids whose folder no longer exists, so deleted folders do not pile up in the file.
    pub fn forget_missing(&mut self, exists: impl Fn(&str) -> bool) -> bool {
        let before = self.collapsed.len();
        self.collapsed.retain(|id| exists(id));
        self.collapsed.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_default_to_open_and_toggle() {
        let mut s = UiState::default();
        assert!(s.is_open("a"));
        assert!(s.set_open("a", false));
        assert!(!s.set_open("a", false), "no change, no save");
        assert!(!s.is_open("a"));
        assert!(s.set_open("a", true));
        assert!(s.is_open("a"));
        assert!(!s.set_open("never-collapsed", true));
    }

    #[test]
    fn round_trips_and_tolerates_junk() {
        let d = std::env::temp_dir().join(format!("ut sidebar state {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("sidebar.json");
        assert_eq!(UiState::load_from(&f), UiState::default(), "missing file");
        let mut s = UiState::default();
        s.set_open("x", false);
        s.shells_open = true;
        s.snippets_open = false;
        ut_fs::write_json_atomic(&f, &s).unwrap();
        assert_eq!(UiState::load_from(&f), s);
        std::fs::write(&f, "{ nope").unwrap();
        assert_eq!(UiState::load_from(&f), UiState::default(), "corrupt file");
        std::fs::write(&f, r#"{"collapsed":["q"],"unknown":1}"#).unwrap();
        let t = UiState::load_from(&f);
        assert!(!t.is_open("q") && t.snippets_open, "missing fields take defaults");
    }

    #[test]
    fn forgets_deleted_folders() {
        let mut s = UiState::default();
        s.set_open("a", false);
        s.set_open("b", false);
        assert!(s.forget_missing(|id| id == "a"));
        assert!(!s.is_open("a") && s.is_open("b"));
        assert!(!s.forget_missing(|id| id == "a"));
    }
}
