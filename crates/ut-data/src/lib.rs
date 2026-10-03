//! Persistent user data: sessions/folders/snippets (§8), workspaces (§16.4), legacy WPF migration
//! (§16.5, Appendix C), Windows Terminal import (§8.6) and the child-process environment layering (§4.3).
//!
//! Everything that crosses the IPC boundary is `serde` camelCase. All functions are synchronous;
//! stores use interior mutability and persist through a shared `ut_fs::DebouncedWriter`.

pub mod env;
pub mod import;
pub mod model;
pub mod store;
pub mod workspaces;

#[cfg(windows)]
pub use env::fresh_parent_env;
pub use env::{default_env, layer_env, process_env};
pub use import::{
    add_imported, export_json, find_wt_settings, find_wt_settings_in, import_json, strip_jsonc, wt_import,
    wt_profiles_to_sessions, ImportMode, ImportReport, Resolver,
};
pub use model::{
    env_get, expand_env_values, expand_percent, expand_vars, parse_env, validate_env, EnvProblem, Folder, Integration,
    Session, Snippet, DEFAULT_COLOR,
};
pub use store::{Half, SeedEntry, SessionStore, SessionsFile, Target, Tree, TreeFolder};
pub use workspaces::{Workspace, WorkspaceStore, WorkspaceTab};

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    /// Rejected user input (message is UI-presentable).
    #[error("{0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(&'static str),
    /// A file or JSON text that could not be understood.
    #[error("{0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, DataError>;

/// A fresh 32-hex GUID without dashes (§8.1).
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;
    use std::sync::Arc;
    use ut_fs::DebouncedWriter;

    /// Fresh temp dir whose path contains spaces (§23.29).
    pub fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ut data test {name} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn writer() -> Arc<DebouncedWriter> {
        Arc::new(DebouncedWriter::new(|p, e| panic!("{p:?}: {e}")))
    }

    /// Files in `dir` whose name starts with `prefix`.
    pub fn files_with_prefix(dir: &std::path::Path, prefix: &str) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .map(|e| e.path())
            .collect()
    }
}
