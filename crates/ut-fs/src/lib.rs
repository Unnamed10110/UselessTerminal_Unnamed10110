//! Filesystem helpers shared by every crate: data directories, atomic writes (§16.2),
//! corrupt-file quarantine (§8.4), a debounced writer, and timestamp formatting.

use parking_lot::{Condvar, Mutex};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- directories

/// `%APPDATA%\UselessTerminal` (config: settings, sessions, ...). `UT_APPDATA` overrides it (tests).
pub fn app_data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("UT_APPDATA") {
        return PathBuf::from(p);
    }
    base_dir("APPDATA")
}

/// `%LOCALAPPDATA%\UselessTerminal` (caches, WebView2 profiles). `UT_LOCALAPPDATA` overrides it (tests).
pub fn local_data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("UT_LOCALAPPDATA") {
        return PathBuf::from(p);
    }
    base_dir("LOCALAPPDATA")
}

fn base_dir(var: &str) -> PathBuf {
    let root = std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData")))
        .unwrap_or_else(std::env::temp_dir);
    root.join("UselessTerminal")
}

/// `%USERPROFILE%` (falls back to the temp dir).
pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

// ------------------------------------------------------------------- time

/// ISO-8601 local time with offset, e.g. `2026-10-01T07:30:00+02:00`.
pub fn now_iso() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// `yyyyMMdd-HHmmss` in local time (log / recording file names).
pub fn now_stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Replace characters that are illegal in Windows file names; empty → `fallback`.
pub fn sanitize_file_name(title: &str, fallback: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let s = s.trim().trim_matches('.').trim();
    let s: String = s.chars().take(80).collect();
    if s.is_empty() { fallback.to_string() } else { s }
}

// ------------------------------------------------------------ atomic write

/// Atomically replace `path` with `bytes`: write `<path>.tmp` in the same directory, flush it,
/// then `ReplaceFileW` (target exists) or `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`.
/// A crash leaves either the old file or the new one, never a partial file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    let r = replace(&tmp, path);
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

#[cfg(windows)]
fn replace(tmp: &Path, target: &Path) -> io::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        REPLACE_FILE_FLAGS,
    };
    let (t, p) = (HSTRING::from(tmp.as_os_str()), HSTRING::from(target.as_os_str()));
    unsafe {
        if target.exists() {
            ReplaceFileW(&p, &t, None, REPLACE_FILE_FLAGS(0), None, None)
        } else {
            MoveFileExW(&t, &p, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
        }
    }
    .map_err(|e| io::Error::from_raw_os_error((e.code().0 & 0xFFFF) as i32))
}

#[cfg(not(windows))]
fn replace(tmp: &Path, target: &Path) -> io::Result<()> {
    std::fs::rename(tmp, target)
}

/// Serialize pretty JSON and write atomically.
pub fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let s = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_atomic(path, &s)
}

// ----------------------------------------------------------------- reading

#[derive(Debug)]
pub enum ReadJson<T> {
    /// File does not exist (first run).
    Missing,
    Ok(T),
    /// File exists but failed to read/parse. It has NOT been touched.
    Corrupt { error: String },
}

/// Read and parse a JSON file. Never modifies the file.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> ReadJson<T> {
    match std::fs::read(path) {
        Ok(b) => {
            let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b);
            match serde_json::from_slice(b) {
                Ok(v) => ReadJson::Ok(v),
                Err(e) => ReadJson::Corrupt { error: e.to_string() },
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => ReadJson::Missing,
        Err(e) => ReadJson::Corrupt { error: e.to_string() },
    }
}

/// Rename `foo.json` → `foo.corrupt-<stamp>.json` (§8.4) and return the new path.
pub fn quarantine(path: &Path) -> io::Result<PathBuf> {
    backup_as(path, "corrupt", true)
}

/// Copy `foo.json` → `foo.<tag>-bak.json` (e.g. `legacy`), keeping the original. Returns the copy's path.
pub fn backup_copy(path: &Path, tag: &str) -> io::Result<PathBuf> {
    backup_as(path, &format!("{tag}-bak"), false)
}

fn backup_as(path: &Path, tag: &str, rename: bool) -> io::Result<PathBuf> {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let stamp = if rename { format!("-{}", now_stamp()) } else { String::new() };
    let mut dest = path.with_file_name(format!("{stem}.{tag}{stamp}{ext}"));
    let mut n = 1;
    while dest.exists() {
        dest = path.with_file_name(format!("{stem}.{tag}{stamp}-{n}{ext}"));
        n += 1;
    }
    if rename { std::fs::rename(path, &dest)? } else { std::fs::copy(path, &dest).map(|_| ())? }
    Ok(dest)
}

// --------------------------------------------------------- debounced writer

type ErrorHook = Arc<dyn Fn(&Path, &io::Error) + Send + Sync>;

struct Pending {
    bytes: Vec<u8>,
    due: Instant,
}

struct Shared {
    pending: Mutex<(HashMap<PathBuf, Pending>, bool)>, // (jobs, shutdown)
    cv: Condvar,
}

/// Debounced atomic writer (§16.2): `queue()` replaces any pending write for the same path and
/// writes it after `delay` of quiet. `flush_all()` writes everything synchronously (call on exit,
/// WM_ENDSESSION, hide-to-tray). Errors are reported through `on_error`, never swallowed.
pub struct DebouncedWriter {
    shared: Arc<Shared>,
    on_error: ErrorHook,
}

impl DebouncedWriter {
    pub fn new(on_error: impl Fn(&Path, &io::Error) + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared { pending: Mutex::new((HashMap::new(), false)), cv: Condvar::new() });
        let on_error: ErrorHook = Arc::new(on_error);
        let (s, h) = (shared.clone(), on_error.clone());
        std::thread::Builder::new()
            .name("ut-fs-writer".into())
            .spawn(move || Self::run(s, h))
            .expect("spawn writer thread");
        Self { shared, on_error }
    }

    fn run(shared: Arc<Shared>, on_error: ErrorHook) {
        let mut g = shared.pending.lock();
        loop {
            if g.1 && g.0.is_empty() {
                return;
            }
            let now = Instant::now();
            let due: Vec<PathBuf> =
                g.0.iter().filter(|(_, p)| p.due <= now || g.1).map(|(k, _)| k.clone()).collect();
            if due.is_empty() {
                match g.0.values().map(|p| p.due).min() {
                    Some(t) => {
                        shared.cv.wait_until(&mut g, t);
                    }
                    None => shared.cv.wait(&mut g),
                }
                continue;
            }
            let jobs: Vec<(PathBuf, Pending)> = due.into_iter().filter_map(|k| g.0.remove_entry(&k)).collect();
            drop(g);
            for (path, p) in jobs {
                if let Err(e) = write_atomic(&path, &p.bytes) {
                    on_error(&path, &e);
                }
            }
            g = shared.pending.lock();
        }
    }

    pub fn queue(&self, path: impl Into<PathBuf>, delay: Duration, bytes: Vec<u8>) {
        let mut g = self.shared.pending.lock();
        g.0.insert(path.into(), Pending { bytes, due: Instant::now() + delay });
        self.shared.cv.notify_all();
    }

    pub fn queue_json<T: serde::Serialize>(&self, path: impl Into<PathBuf>, delay: Duration, value: &T) {
        let path = path.into();
        match serde_json::to_vec_pretty(value) {
            Ok(b) => self.queue(path, delay, b),
            Err(e) => (self.on_error)(&path, &io::Error::other(e)),
        }
    }

    /// Write every pending file now, on the calling thread.
    pub fn flush_all(&self) {
        let jobs: Vec<(PathBuf, Pending)> = self.shared.pending.lock().0.drain().collect();
        for (path, p) in jobs {
            if let Err(e) = write_atomic(&path, &p.bytes) {
                (self.on_error)(&path, &e);
            }
        }
    }
}

impl Drop for DebouncedWriter {
    fn drop(&mut self) {
        self.flush_all();
        let mut g = self.shared.pending.lock();
        g.1 = true;
        self.shared.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ut-fs-test-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_tmp() {
        let d = tmpdir("atomic");
        let f = d.join("a b").join("x.json"); // spaces in path (§23.29)
        write_atomic(&f, b"one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        assert!(!f.with_file_name("x.json.tmp").exists());
    }

    #[test]
    fn corrupt_and_missing() {
        let d = tmpdir("corrupt");
        let f = d.join("s.json");
        assert!(matches!(read_json::<serde_json::Value>(&f), ReadJson::Missing));
        std::fs::write(&f, b"{ nope").unwrap();
        assert!(matches!(read_json::<serde_json::Value>(&f), ReadJson::Corrupt { .. }));
        assert!(f.exists(), "read_json must not touch the file");
        let q = quarantine(&f).unwrap();
        assert!(!f.exists() && q.exists());
        assert!(q.file_name().unwrap().to_string_lossy().starts_with("s.corrupt-"));
    }

    #[test]
    fn debounce_coalesces_and_flushes() {
        let d = tmpdir("debounce");
        let f = d.join("d.json");
        let w = DebouncedWriter::new(|p, e| panic!("{p:?}: {e}"));
        w.queue(&f, Duration::from_millis(30), b"1".to_vec());
        w.queue(&f, Duration::from_millis(30), b"2".to_vec());
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(std::fs::read(&f).unwrap(), b"2");
        w.queue(&f, Duration::from_secs(60), b"3".to_vec());
        w.flush_all();
        assert_eq!(std::fs::read(&f).unwrap(), b"3");
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_file_name("a:b/c", "x"), "a_b_c");
        assert_eq!(sanitize_file_name("  ", "session"), "session");
    }
}
