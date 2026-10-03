//! File drops (§11.2-11.5): destination resolution, conflict detection, rename/replace, local copy and
//! remote upload. High-level API: [`plan_drop`] then [`execute_drop`]. Every remote command string and
//! argument vector is built by a pure function (quoting, scripts, scp/sftp argv, error text).
//!
//! Remote helper commands run `ssh … -- <target> sh -s` with the script on stdin: no local command-line
//! quoting can mangle remote paths, and the script is quoted for POSIX `sh` only.

use crate::locate::{find_scp, find_sftp, find_ssh};
use crate::target::SshTarget;
use crate::{run, CancelToken, Progress};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const RM_TIMEOUT: Duration = Duration::from_secs(120);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(2 * 3600);
const CHUNK: usize = 1 << 20;

// ------------------------------------------------------------------- types

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Destination {
    Local(PathBuf),
    /// `path` is the already-resolved remote directory (see `target::remote_dest`).
    Remote { target: SshTarget, path: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropItem {
    /// Name in the destination folder.
    pub name: String,
    pub source: PathBuf,
    pub is_dir: bool,
    /// Bytes (recursive for folders).
    pub size: u64,
    /// The destination already has an item of that name (or an earlier item of this drop has it).
    pub conflict: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropPlan {
    pub dest: Destination,
    pub items: Vec<DropItem>,
    /// Names of the conflicting items, for the "File already exists" dialog.
    pub conflicts: Vec<String>,
    /// Dropped paths that could not be read: `(name, error)`. They are reported as failures.
    pub invalid: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resolution {
    Skip,
    Rename,
    Replace,
    Cancel,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropResult {
    /// Final names in the destination (a renamed item shows its new name).
    pub copied: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub skipped: Vec<String>,
    pub cancelled: bool,
}

// -------------------------------------------------------- local destination

/// §11.2: the cwd if it is a directory; else its Unix-style form (`/mnt/x/…`, `/x/…`, `x:/…`) converted
/// to a Windows path if that exists; else `%USERPROFILE%`.
pub fn resolve_local_dest(cwd: Option<&str>) -> PathBuf {
    if let Some(c) = cwd.map(str::trim).filter(|c| !c.is_empty()) {
        if Path::new(c).is_dir() {
            return PathBuf::from(c);
        }
        if let Some(w) = unix_to_windows(c).map(PathBuf::from).filter(|w| w.is_dir()) {
            return w;
        }
    }
    ut_fs::home_dir()
}

pub fn unix_to_windows(p: &str) -> Option<String> {
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return p.contains('/').then(|| p.replace('/', "\\")); // x:/… -> x:\…
    }
    let rest = p.strip_prefix("/mnt/").or_else(|| p.strip_prefix('/'))?;
    let (letter, tail) = rest.split_once('/').unwrap_or((rest, ""));
    (letter.len() == 1 && letter.as_bytes()[0].is_ascii_alphabetic())
        .then(|| format!("{}:\\{}", letter.to_ascii_uppercase(), tail.replace('/', "\\")))
}

// ------------------------------------------------------ pure remote builders

/// POSIX single quotes: `'` -> `'"'"'`.
pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Quote a remote path for `sh`, expanding a leading `~` on the REMOTE side (§24 #23):
/// `~` -> `"$HOME"`, `~/rest` -> `"$HOME"/'rest'`; everything else is single-quoted.
pub fn quote_remote(path: &str) -> String {
    match path {
        "~" => "\"$HOME\"".to_string(),
        _ => match path.strip_prefix("~/") {
            Some("") => "\"$HOME\"/".to_string(),
            Some(rest) => format!("\"$HOME\"/{}", sq(rest)),
            None => sq(path),
        },
    }
}

/// `dir` + `/` + `name` without doubling slashes; an empty dir means the home directory.
pub fn remote_join(dir: &str, name: &str) -> String {
    let d = dir.trim();
    let d = if d.is_empty() { "~" } else { d };
    format!("{}/{name}", d.trim_end_matches('/'))
}

/// One round trip for all items (§11.4 [P1]): a `UT_EXISTS` / `UT_MISSING` line per path, in order.
pub fn conflict_script(paths: &[String]) -> String {
    paths.iter().map(|p| format!("test -e {} && echo UT_EXISTS || echo UT_MISSING\n", quote_remote(p))).collect()
}

/// `None` (indeterminate => no conflict) unless there is exactly one marker per item.
pub fn parse_conflict_output(out: &str, n: usize) -> Option<Vec<bool>> {
    let marks: Vec<bool> = out
        .lines()
        .filter_map(|l| match l.trim() {
            "UT_EXISTS" => Some(true),
            "UT_MISSING" => Some(false),
            _ => None,
        })
        .collect();
    (marks.len() == n).then_some(marks)
}

fn split_name(name: &str, is_dir: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// `"{stem} ({n}){ext}"` for n = 1..=999, then a GUID suffix (§11.4). Folders have no extension.
pub fn name_candidates(name: &str, is_dir: bool) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = split_name(name, is_dir);
    (1..=999)
        .map(|n| n.to_string())
        .chain(std::iter::once(uuid::Uuid::new_v4().simple().to_string()))
        .map(move |s| format!("{stem} ({s}){ext}"))
}

/// Remote rename probe (`test -e` per candidate, looped remotely in one round trip). Prints
/// `UT_FREE <n>`: the first free n (1000 means "use the GUID candidate").
pub fn rename_probe_script(dir: &str, name: &str, is_dir: bool) -> String {
    let (stem, ext) = split_name(name, is_dir);
    format!(
        "d={}\ni=1\nwhile [ -e \"$d\"/{}\"$i\"{} ]; do i=$((i+1)); [ \"$i\" -gt 999 ] && break; done\necho UT_FREE $i\n",
        quote_remote(dir),
        sq(&format!("{stem} (")),
        sq(&format!("){ext}")),
    )
}

/// The candidate name for the index printed by [`rename_probe_script`].
pub fn parse_free_name(out: &str, name: &str, is_dir: bool) -> Option<String> {
    let n: usize = out.lines().find_map(|l| l.trim().strip_prefix("UT_FREE "))?.trim().parse().ok()?;
    name_candidates(name, is_dir).nth(n.clamp(1, 1000) - 1)
}

/// `rm -rf -- <q>`; refuses `""`, `.`, `..`, `/`, `~`, `~/` and anything ending in `/.` or `/..` (§11.4).
pub fn rm_script(path: &str) -> Result<String, String> {
    let p = path.trim();
    let t = p.trim_end_matches('/');
    if matches!(t, "" | "." | ".." | "~") || t.ends_with("/..") || t.ends_with("/.") {
        return Err(format!("Refusing to delete '{p}'."));
    }
    Ok(format!("rm -rf -- {}\n", quote_remote(p)))
}

/// argv for a helper `ssh` that runs the script fed on stdin.
pub fn ssh_helper_args(target: &SshTarget) -> Vec<String> {
    let mut a: Vec<String> = ["-T", "-o", "BatchMode=yes"].map(String::from).into();
    a.extend(target.conn_args());
    a.extend(["--".into(), target.target_spec(), "sh".into(), "-s".into()]);
    a
}

/// `user@host` for scp/sftp: IPv6 hosts are bracketed.
fn scp_host(t: &SshTarget) -> String {
    let h = if t.host.contains(':') { format!("[{}]", t.host) } else { t.host.clone() };
    match &t.user {
        Some(u) => format!("{u}@{h}"),
        None => h,
    }
}

/// `scp [-r] -o BatchMode=yes [-o ControlPath] [-P] [-i] [-F] [-J] [-o…] -- <local> <target>:<remote>` (§11.5).
/// `remote_path` is passed as-is (`~` is expanded by the remote side); prefer sftp for unusual names.
pub fn scp_upload_args(target: &SshTarget, is_dir: bool, local: &str, remote_path: &str) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    if is_dir {
        a.push("-r".into());
    }
    a.extend(["-o".into(), "BatchMode=yes".into()]);
    a.extend(target.scp_args());
    a.extend(["--".into(), local.into(), format!("{}:{remote_path}", scp_host(target))]);
    a
}

/// argv for `sftp -b -` (commands on stdin).
pub fn sftp_args(target: &SshTarget) -> Vec<String> {
    let mut a: Vec<String> = ["-b", "-", "-o", "BatchMode=yes"].map(String::from).into();
    a.extend(target.scp_args());
    a.extend(["--".into(), scp_host(target)]);
    a
}

fn sftp_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The batch for one item. sftp starts in the home directory, so `~` / `~/x` become relative paths.
pub fn sftp_script(local: &str, is_dir: bool, remote_path: &str) -> String {
    let remote = if remote_path == "~" { "." } else { remote_path.strip_prefix("~/").unwrap_or(remote_path) };
    format!("put {}{} {}\n", if is_dir { "-r " } else { "" }, sftp_quote(local), sftp_quote(remote))
}

/// §11.5 error text. `output` is the tool's stderr (or stdout when stderr is empty).
pub fn format_transfer_error(tool: &str, code: Option<i32>, output: &str) -> String {
    const KNOWN: [&str; 5] =
        ["Permission denied", "Host key verification failed", "Connection refused", "Connection timed out", "Could not resolve"];
    let lines: Vec<&str> = output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let Some(&first) = KNOWN.iter().find_map(|k| lines.iter().find(|l| l.contains(k))).or(lines.first()) else {
        let c = code.map_or("an error".to_string(), |c| c.to_string());
        return format!("{tool} exited {c}. For password logins, open SSH as its own tab so the drop can reuse that connection.");
    };
    if first.to_lowercase().contains("password") {
        format!("{first} Open SSH as its own tab (Quick Connect or a saved session) so drops reuse the login.")
    } else {
        first.to_string()
    }
}

// ---------------------------------------------------------------- local I/O

fn io_msg(e: &io::Error) -> String {
    let s = e.to_string();
    s.rsplit_once(" (os error").map_or(s.clone(), |(m, _)| m.to_string())
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn is_inside(child: &Path, parent: &Path) -> bool {
    match (fs::canonicalize(child), fs::canonicalize(parent)) {
        (Ok(c), Ok(p)) => c.starts_with(p),
        _ => child.starts_with(parent),
    }
}

fn dir_size(p: &Path) -> u64 {
    fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

fn cancelled_err() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Cancelled.")
}

/// Copy with overwrite, in 1 MiB chunks so progress and cancel work; keeps mtime and the read-only flag.
/// A partially written file is removed on failure or cancel.
fn copy_file(src: &Path, dst: &Path, cancel: &CancelToken, on_bytes: &mut dyn FnMut(u64)) -> io::Result<()> {
    let mut r = fs::File::open(src)?;
    let meta = r.metadata()?;
    let mut w = fs::File::create(dst)?;
    let body = (|| {
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(cancelled_err());
            }
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
            on_bytes(n as u64);
        }
        w.flush()?;
        if let Ok(m) = meta.modified() {
            let _ = w.set_modified(m);
        }
        Ok(())
    })();
    drop(w);
    match body {
        Ok(()) => {
            let _ = fs::set_permissions(dst, meta.permissions());
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(dst);
            Err(e)
        }
    }
}

fn copy_dir(src: &Path, dst: &Path, cancel: &CancelToken, on_bytes: &mut dyn FnMut(u64)) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let is_link = e.file_type()?.is_symlink();
        let Ok(md) = fs::metadata(&from) else { continue }; // dangling link
        if md.is_dir() {
            // ponytail: directory links/junctions are not followed (a link to an ancestor would recurse forever)
            if !is_link {
                copy_dir(&from, &to, cancel, on_bytes)?;
            }
        } else {
            copy_file(&from, &to, cancel, on_bytes)?;
        }
    }
    Ok(())
}

#[allow(clippy::permissions_set_readonly_false)] // Windows attribute, not a Unix mode
fn clear_readonly(p: &Path) {
    if let Ok(m) = fs::metadata(p) {
        let mut perm = m.permissions();
        if perm.readonly() {
            perm.set_readonly(false);
            let _ = fs::set_permissions(p, perm);
        }
    }
}

fn clear_readonly_tree(p: &Path) {
    clear_readonly(p);
    for e in fs::read_dir(p).into_iter().flatten().flatten() {
        match e.file_type() {
            Ok(t) if t.is_dir() => clear_readonly_tree(&e.path()),
            Ok(t) if !t.is_symlink() => clear_readonly(&e.path()),
            _ => {}
        }
    }
}

/// Local "Replace" (§11.4): reset attributes, then delete (folders recursively). Refuses when the source
/// lives inside the item being deleted.
fn replace_local(dest: &Path, source: &Path) -> Result<(), String> {
    if is_inside(source, dest) {
        return Err("the source is inside the item it would replace".into());
    }
    let md = fs::symlink_metadata(dest).map_err(|e| io_msg(&e))?;
    let r = if md.file_type().is_symlink() {
        clear_readonly(dest);
        fs::remove_file(dest).or_else(|_| fs::remove_dir(dest))
    } else if md.is_dir() {
        clear_readonly_tree(dest);
        fs::remove_dir_all(dest)
    } else {
        clear_readonly(dest);
        fs::remove_file(dest)
    };
    r.map_err(|e| io_msg(&e))
}

// ---------------------------------------------------------------- remote I/O

pub(crate) struct Tools {
    pub ssh: PathBuf,
    pub scp: Option<PathBuf>,
    pub sftp: Option<PathBuf>,
}

impl Tools {
    fn find() -> Option<Self> {
        let ssh = find_ssh()?;
        Some(Self { scp: find_scp(&ssh), sftp: find_sftp(&ssh), ssh })
    }
}

fn ssh_script(
    tools: &Tools,
    target: &SshTarget,
    script: &str,
    timeout: Duration,
    cancel: Option<&CancelToken>,
) -> io::Result<crate::Out> {
    run(Command::new(&tools.ssh).args(ssh_helper_args(target)), Some(script.as_bytes()), timeout, cancel)
}

/// One `ssh` round trip (20 s). Anything indeterminate counts as "no conflict".
fn remote_conflicts(tools: &Tools, target: &SshTarget, dir: &str, names: &[&str]) -> Vec<bool> {
    let paths: Vec<String> = names.iter().map(|n| remote_join(dir, n)).collect();
    ssh_script(tools, target, &conflict_script(&paths), CHECK_TIMEOUT, None)
        .ok()
        .and_then(|o| parse_conflict_output(&o.stdout, names.len()))
        .unwrap_or_else(|| vec![false; names.len()])
}

fn remote_free_name(tools: &Tools, target: &SshTarget, dir: &str, item: &DropItem, cancel: &CancelToken) -> Result<String, String> {
    let o = ssh_script(tools, target, &rename_probe_script(dir, &item.name, item.is_dir), CHECK_TIMEOUT, Some(cancel))
        .map_err(|e| format!("Could not start ssh: {}", io_msg(&e)))?;
    parse_free_name(&o.stdout, &item.name, item.is_dir)
        .ok_or_else(|| format_transfer_error("ssh", o.code, &o.stderr))
}

fn remote_rm(tools: &Tools, target: &SshTarget, path: &str, cancel: &CancelToken) -> Result<(), String> {
    let script = rm_script(path)?;
    let o = ssh_script(tools, target, &script, RM_TIMEOUT, Some(cancel)).map_err(|e| format!("Could not start ssh: {}", io_msg(&e)))?;
    if o.timed_out {
        return Err("Timed out deleting the existing item.".into());
    }
    if o.code == Some(0) { Ok(()) } else { Err(format_transfer_error("ssh", o.code, &o.stderr)) }
}

/// Upload one item with sftp (preferred) or scp. The tool runs in the source's parent folder and gets
/// `./name`, which avoids scp treating `C:\…` as `host:path`.
fn upload(tools: &Tools, target: &SshTarget, item: &DropItem, remote_path: &str, cancel: &CancelToken) -> Result<(), String> {
    let local = format!("./{}", item.name);
    let (exe, tool, args, stdin) = if let Some(sftp) = &tools.sftp {
        (sftp, "sftp", sftp_args(target), Some(sftp_script(&local, item.is_dir, remote_path)))
    } else if let Some(scp) = &tools.scp {
        (scp, "scp", scp_upload_args(target, item.is_dir, &local, remote_path), None)
    } else {
        return Err("Neither sftp nor scp was found next to ssh.".into());
    };
    let mut cmd = Command::new(exe);
    cmd.args(args);
    if let Some(parent) = item.source.parent() {
        cmd.current_dir(parent);
    }
    let o = run(&mut cmd, stdin.as_deref().map(str::as_bytes), UPLOAD_TIMEOUT, Some(cancel))
        .map_err(|e| format!("Could not start {tool}: {}", io_msg(&e)))?;
    if o.cancelled {
        return Err("Cancelled.".into());
    }
    if o.timed_out {
        return Err(format!("{tool} timed out after 2 hours."));
    }
    if o.code == Some(0) {
        return Ok(());
    }
    let text = if o.stderr.trim().is_empty() { &o.stdout } else { &o.stderr };
    Err(format_transfer_error(tool, o.code, text))
}

// ------------------------------------------------------------- plan / execute

fn build_item(p: &Path) -> Result<DropItem, String> {
    let source = std::path::absolute(p).map_err(|e| io_msg(&e))?;
    let md = fs::metadata(&source).map_err(|e| io_msg(&e))?;
    let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| "The path has no file name.".to_string())?;
    let is_dir = md.is_dir();
    Ok(DropItem { name, size: if is_dir { dir_size(&source) } else { md.len() }, source, is_dir, conflict: false })
}

/// Stat the dropped paths and detect name conflicts at the destination (local: the path exists and is not
/// the source itself; remote: one `ssh` round trip). Blocking: up to 20 s for remote destinations.
pub fn plan_drop(paths: &[PathBuf], dest: Destination) -> DropPlan {
    plan_with(paths, dest, Tools::find().as_ref())
}

pub(crate) fn plan_with(paths: &[PathBuf], dest: Destination, tools: Option<&Tools>) -> DropPlan {
    let (mut items, mut invalid) = (Vec::new(), Vec::new());
    for p in paths {
        match build_item(p) {
            Ok(i) => items.push(i),
            Err(e) => invalid.push((p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()), e)),
        }
    }
    let exists: Vec<bool> = match &dest {
        Destination::Local(dir) => items
            .iter()
            .map(|i| {
                let d = dir.join(&i.name);
                d.symlink_metadata().is_ok() && !same_path(&d, &i.source)
            })
            .collect(),
        Destination::Remote { target, path } => match tools {
            Some(t) if !items.is_empty() => {
                let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
                remote_conflicts(t, target, path, &names)
            }
            _ => vec![false; items.len()],
        },
    };
    let mut seen = HashSet::new();
    for (it, e) in items.iter_mut().zip(exists) {
        // two dropped items with the same name collide with each other as well
        it.conflict = e | !seen.insert(it.name.to_lowercase());
    }
    let conflicts = items.iter().filter(|i| i.conflict).map(|i| i.name.clone()).collect();
    DropPlan { dest, items, conflicts, invalid }
}

/// Run the plan. `resolution` applies to every conflicting item (`Cancel` aborts everything). Items run
/// sequentially; `progress` is called at each item start and per copied chunk (local copies).
pub fn execute_drop(plan: &DropPlan, resolution: Resolution, cancel: &CancelToken, progress: &dyn Fn(Progress)) -> DropResult {
    execute_with(plan, resolution, cancel, progress, Tools::find().as_ref())
}

pub(crate) fn execute_with(
    plan: &DropPlan,
    resolution: Resolution,
    cancel: &CancelToken,
    progress: &dyn Fn(Progress),
    tools: Option<&Tools>,
) -> DropResult {
    let mut res = DropResult { failed: plan.invalid.clone(), ..Default::default() };
    if resolution == Resolution::Cancel {
        res.cancelled = true;
        return res;
    }
    let total = plan.items.len();
    let bytes_total: u64 = plan.items.iter().map(|i| i.size).sum();
    let mut done = 0u64;
    for (idx, item) in plan.items.iter().enumerate() {
        if cancel.is_cancelled() {
            res.cancelled = true;
            break;
        }
        let report = |bytes_done: u64| progress(Progress { index: idx + 1, total, name: item.name.clone(), bytes_done, bytes_total });
        report(done);
        let mut in_item = 0u64;
        let r = match &plan.dest {
            Destination::Local(dir) => local_one(dir, item, resolution, cancel, &mut |n| {
                in_item += n;
                report(done + in_item);
            }),
            Destination::Remote { target, path } => match tools {
                Some(t) => remote_one(t, target, path, item, resolution, cancel),
                None => Err("ssh was not found.".to_string()),
            },
        };
        done += item.size;
        match r {
            Ok(Some(name)) => res.copied.push(name),
            Ok(None) => res.skipped.push(item.name.clone()),
            Err(_) if cancel.is_cancelled() => {
                res.cancelled = true;
                break;
            }
            Err(e) => res.failed.push((item.name.clone(), e)),
        }
    }
    res
}

fn local_one(
    dir: &Path,
    item: &DropItem,
    resolution: Resolution,
    cancel: &CancelToken,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Option<String>, String> {
    let mut name = item.name.clone();
    if item.conflict {
        match resolution {
            Resolution::Skip | Resolution::Cancel => return Ok(None),
            Resolution::Rename => {
                if let Some(free) = name_candidates(&item.name, item.is_dir).find(|c| dir.join(c).symlink_metadata().is_err()) {
                    name = free;
                }
            }
            Resolution::Replace => replace_local(&dir.join(&name), &item.source).map_err(|e| format!("Could not replace the existing item: {e}"))?,
        }
    }
    let dest = dir.join(&name);
    if dest.symlink_metadata().is_ok() && same_path(&dest, &item.source) {
        return Err("It is already in the destination folder.".into());
    }
    if item.is_dir && is_inside(dir, &item.source) {
        return Err("Cannot copy a folder into itself.".into());
    }
    let r = if item.is_dir { copy_dir(&item.source, &dest, cancel, on_bytes) } else { copy_file(&item.source, &dest, cancel, on_bytes) };
    r.map_err(|e| io_msg(&e))?;
    Ok(Some(name))
}

fn remote_one(
    tools: &Tools,
    target: &SshTarget,
    dir: &str,
    item: &DropItem,
    resolution: Resolution,
    cancel: &CancelToken,
) -> Result<Option<String>, String> {
    let mut name = item.name.clone();
    if item.conflict {
        match resolution {
            Resolution::Skip | Resolution::Cancel => return Ok(None),
            Resolution::Rename => name = remote_free_name(tools, target, dir, item, cancel)?,
            Resolution::Replace => remote_rm(tools, target, &remote_join(dir, &name), cancel)?,
        }
    }
    upload(tools, target, item, &remote_join(dir, &name), cancel)?;
    Ok(Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ---- pure builders

    #[test]
    fn single_quotes() {
        assert_eq!(sq("a b"), "'a b'");
        assert_eq!(sq("it's"), r#"'it'"'"'s'"#);
        assert_eq!(sq(""), "''");
    }

    #[test]
    fn tilde_is_expanded_remotely() {
        assert_eq!(quote_remote("~"), r#""$HOME""#);
        assert_eq!(quote_remote("~/"), r#""$HOME"/"#);
        assert_eq!(quote_remote("~/rest"), r#""$HOME"/'rest'"#);
        assert_eq!(quote_remote("~/my dir/it's"), r#""$HOME"/'my dir/it'"'"'s'"#);
        assert_eq!(quote_remote("/var/www"), "'/var/www'");
        assert_eq!(quote_remote("~bob/x"), "'~bob/x'"); // only ~ and ~/ expand
        assert_eq!(quote_remote("a~b"), "'a~b'");
        assert_eq!(quote_remote("$HOME/x"), "'$HOME/x'"); // never expands anything else
    }

    #[test]
    fn remote_join_cases() {
        assert_eq!(remote_join("~", "a b"), "~/a b");
        assert_eq!(remote_join("~/", "x"), "~/x");
        assert_eq!(remote_join("/", "x"), "/x");
        assert_eq!(remote_join("/srv/app/", "x"), "/srv/app/x");
        assert_eq!(remote_join("", "x"), "~/x");
        assert_eq!(remote_join("  /srv  ", "x"), "/srv/x");
    }

    #[test]
    fn conflict_script_and_parsing() {
        let s = conflict_script(&["~/a b".into(), "/srv/it's".into()]);
        assert_eq!(
            s,
            "test -e \"$HOME\"/'a b' && echo UT_EXISTS || echo UT_MISSING\ntest -e '/srv/it'\"'\"'s' && echo UT_EXISTS || echo UT_MISSING\n"
        );
        assert_eq!(parse_conflict_output("Welcome!\r\nUT_EXISTS\r\nmotd\r\nUT_MISSING\r\n", 2), Some(vec![true, false]));
        assert_eq!(parse_conflict_output("UT_EXISTS\n", 2), None); // indeterminate
        assert_eq!(parse_conflict_output("", 1), None);
        assert_eq!(parse_conflict_output("Permission denied\n", 1), None);
    }

    #[test]
    fn name_candidate_scheme() {
        let c: Vec<String> = name_candidates("report.final.txt", false).collect();
        assert_eq!(c.len(), 1000);
        assert_eq!(c[0], "report.final (1).txt");
        assert_eq!(c[998], "report.final (999).txt");
        assert!(c[999].starts_with("report.final (") && c[999].ends_with(").txt") && c[999].len() > 30);
        assert_eq!(name_candidates("noext", false).next().unwrap(), "noext (1)");
        assert_eq!(name_candidates(".bashrc", false).next().unwrap(), ".bashrc (1)");
        assert_eq!(name_candidates("v1.2", true).next().unwrap(), "v1.2 (1)"); // folders: no extension
    }

    #[test]
    fn rename_probe_script_text_and_index() {
        let s = rename_probe_script("~/up dir", "a b.txt", false);
        assert_eq!(
            s,
            "d=\"$HOME\"/'up dir'\ni=1\nwhile [ -e \"$d\"/'a b ('\"$i\"').txt' ]; do i=$((i+1)); [ \"$i\" -gt 999 ] && break; done\necho UT_FREE $i\n"
        );
        assert_eq!(parse_free_name("junk\nUT_FREE 3\n", "a.txt", false).as_deref(), Some("a (3).txt"));
        let guid = parse_free_name("UT_FREE 1000", "a.txt", false).unwrap();
        assert!(guid.starts_with("a (") && guid.len() > 20);
        assert_eq!(parse_free_name("nothing", "a.txt", false), None);
    }

    #[test]
    fn rm_guard() {
        for bad in ["", "  ", ".", "..", "/", "//", "~", "~/", "~//", "/srv/..", "~/.", "x/."] {
            assert!(rm_script(bad).is_err(), "{bad:?} must be refused");
        }
        assert_eq!(rm_script("~/old dir").unwrap(), "rm -rf -- \"$HOME\"/'old dir'\n");
        assert_eq!(rm_script("/srv/x/").unwrap(), "rm -rf -- '/srv/x/'\n");
        assert_eq!(rm_script("~/.config").unwrap(), "rm -rf -- \"$HOME\"/'.config'\n");
    }

    fn target() -> SshTarget {
        crate::target::parse_argv("ssh -p 2222 -i key -F cfg -J jump -o ControlPath=/m/%C -o X=1 bob@host").unwrap()
    }

    #[test]
    fn helper_and_transfer_argv() {
        let t = target();
        assert_eq!(
            ssh_helper_args(&t),
            ["-T", "-o", "BatchMode=yes", "-o", "ControlPath=/m/%C", "-p", "2222", "-i", "key", "-F", "cfg", "-J", "jump", "-o", "X=1", "--", "bob@host", "sh", "-s"]
        );
        assert_eq!(
            scp_upload_args(&t, true, "./dir", "~/dest/dir"),
            ["-r", "-o", "BatchMode=yes", "-o", "ControlPath=/m/%C", "-P", "2222", "-i", "key", "-F", "cfg", "-J", "jump", "-o", "X=1", "--", "./dir", "bob@host:~/dest/dir"]
        );
        assert_eq!(scp_upload_args(&SshTarget::from_host("h"), false, "./f", "/x/f"), ["-o", "BatchMode=yes", "--", "./f", "h:/x/f"]);
        assert_eq!(
            sftp_args(&t),
            ["-b", "-", "-o", "BatchMode=yes", "-o", "ControlPath=/m/%C", "-P", "2222", "-i", "key", "-F", "cfg", "-J", "jump", "-o", "X=1", "--", "bob@host"]
        );
        // IPv6 targets are bracketed for scp/sftp, plain for ssh
        let v6 = SshTarget::from_host("::1");
        assert_eq!(scp_upload_args(&v6, false, "./f", "/x").last().unwrap(), "[::1]:/x");
        assert_eq!(sftp_args(&v6).last().unwrap(), "[::1]");
        assert_eq!(ssh_helper_args(&v6)[3], "--");
        assert_eq!(ssh_helper_args(&v6)[4], "::1");
    }

    #[test]
    fn sftp_batch_text() {
        assert_eq!(sftp_script("./a b.txt", false, "~/up/a b.txt"), "put \"./a b.txt\" \"up/a b.txt\"\n");
        assert_eq!(sftp_script("./d", true, "~/d"), "put -r \"./d\" \"d\"\n");
        assert_eq!(sftp_script("./f", false, "~"), "put \"./f\" \".\"\n");
        assert_eq!(sftp_script("./f", false, "/srv/f"), "put \"./f\" \"/srv/f\"\n");
        assert_eq!(sftp_script("./q\"x", false, "/a\\b"), "put \"./q\\\"x\" \"/a\\\\b\"\n");
    }

    #[test]
    fn error_formatting() {
        let hint = " Open SSH as its own tab (Quick Connect or a saved session) so drops reuse the login.";
        assert_eq!(
            format_transfer_error("scp", Some(255), "Warning: Permanently added 'h' to known hosts.\nuser@h: Permission denied (publickey,password).\n"),
            format!("user@h: Permission denied (publickey,password).{hint}")
        );
        assert_eq!(
            format_transfer_error("scp", Some(255), "ssh: connect to host h port 22: Connection refused\r\nlost connection"),
            "ssh: connect to host h port 22: Connection refused"
        );
        assert_eq!(format_transfer_error("scp", Some(255), "Host key verification failed."), "Host key verification failed.");
        assert_eq!(
            format_transfer_error("scp", Some(255), "ssh: Could not resolve hostname nope: No such host is known."),
            "ssh: Could not resolve hostname nope: No such host is known."
        );
        assert_eq!(format_transfer_error("scp", Some(255), "Connection timed out during banner exchange"), "Connection timed out during banner exchange");
        assert_eq!(
            format_transfer_error("scp", Some(1), "  \n"),
            "scp exited 1. For password logins, open SSH as its own tab so the drop can reuse that connection."
        );
        assert!(format_transfer_error("sftp", None, "").starts_with("sftp exited an error."));
        assert_eq!(format_transfer_error("scp", Some(1), "scp: /x: No space left on device\nmore"), "scp: /x: No space left on device");
        assert_eq!(format_transfer_error("scp", Some(1), "bob@h's password: "), format!("bob@h's password:{hint}"));
    }

    #[test]
    fn unix_style_paths() {
        assert_eq!(unix_to_windows("/mnt/c/Users/me").as_deref(), Some(r"C:\Users\me"));
        assert_eq!(unix_to_windows("/c/Users/me/").as_deref(), Some(r"C:\Users\me\"));
        assert_eq!(unix_to_windows("/d").as_deref(), Some(r"D:\"));
        assert_eq!(unix_to_windows("/mnt/e/").as_deref(), Some(r"E:\"));
        assert_eq!(unix_to_windows("c:/Users/me").as_deref(), Some(r"c:\Users\me"));
        assert_eq!(unix_to_windows("/home/me"), None);
        assert_eq!(unix_to_windows("/mnt/data/x"), None);
        assert_eq!(unix_to_windows(r"C:\already"), None);
        assert_eq!(unix_to_windows("relative/x"), None);
    }

    // ---- local filesystem behaviour (paths with spaces, §23.29)

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ut ssh transfer {n} {}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    fn no_progress(_: Progress) {}

    fn run_local(plan: &DropPlan, r: Resolution) -> DropResult {
        execute_with(plan, r, &CancelToken::new(), &no_progress, None)
    }

    #[test]
    fn resolve_local_dest_rules() {
        let d = tmp("dest");
        let s = d.to_string_lossy().into_owned();
        assert_eq!(resolve_local_dest(Some(&s)), d);
        assert_eq!(resolve_local_dest(Some(&format!("  {s}  "))), d);
        // `x:/…` form of an existing dir
        let slashed = s.replace('\\', "/");
        assert_eq!(resolve_local_dest(Some(&slashed)), d); // exists as-is on Windows
        // `/c/…` and `/mnt/c/…` forms
        let drive = s.chars().next().unwrap().to_ascii_lowercase();
        let rest = s[2..].replace('\\', "/"); // "/Users/…/ut ssh transfer dest N"
        let win = PathBuf::from(format!("{}:{}", drive.to_ascii_uppercase(), s[2..].replace('/', "\\")));
        assert_eq!(resolve_local_dest(Some(&format!("/{drive}{rest}"))), win);
        assert_eq!(resolve_local_dest(Some(&format!("/mnt/{drive}{rest}"))), win);
        // missing dirs fall back to the home directory
        assert_eq!(resolve_local_dest(Some("/definitely/not/here")), ut_fs::home_dir());
        assert_eq!(resolve_local_dest(Some("   ")), ut_fs::home_dir());
        assert_eq!(resolve_local_dest(None), ut_fs::home_dir());
    }

    #[test]
    fn copies_files_and_folders_with_spaces() {
        let root = tmp("copy");
        let (src, dst) = (root.join("source dir"), root.join("dest dir"));
        fs::create_dir_all(&dst).unwrap();
        write(&src.join("a file.txt"), "hello");
        write(&src.join("tree/sub dir/deep.txt"), "deep");
        write(&src.join("tree/top.txt"), "top");
        let plan = plan_with(&[src.join("a file.txt"), src.join("tree")], Destination::Local(dst.clone()), None);
        assert!(plan.conflicts.is_empty() && plan.invalid.is_empty());
        assert_eq!(plan.items[1].size, 7);
        let seen = RefCell::new(Vec::new());
        let r = execute_with(&plan, Resolution::Rename, &CancelToken::new(), &|p| seen.borrow_mut().push(p), None);
        assert_eq!(r.copied, ["a file.txt", "tree"]);
        assert!(r.failed.is_empty() && !r.cancelled);
        assert_eq!(read(&dst.join("a file.txt")), "hello");
        assert_eq!(read(&dst.join("tree/sub dir/deep.txt")), "deep");
        let seen = seen.into_inner();
        assert_eq!(seen.first().unwrap().index, 1);
        let last = seen.last().unwrap();
        assert_eq!((last.index, last.total, last.bytes_total), (2, 2, 12));
        assert_eq!(last.bytes_done, 12);
    }

    #[test]
    fn conflict_detection_and_resolutions() {
        let root = tmp("conflict");
        let (src, dst) = (root.join("src"), root.join("dst dir"));
        write(&src.join("x.txt"), "NEW");
        write(&src.join("folder/inner.txt"), "NEWINNER");
        write(&dst.join("x.txt"), "OLD");
        write(&dst.join("x (1).txt"), "OLD1");
        write(&dst.join("folder/old.txt"), "OLDINNER");
        let paths = [src.join("x.txt"), src.join("folder")];
        let plan = plan_with(&paths, Destination::Local(dst.clone()), None);
        assert_eq!(plan.conflicts, ["x.txt", "folder"]);

        // Skip
        let r = run_local(&plan, Resolution::Skip);
        assert_eq!((r.copied.len(), r.skipped.clone()), (0, vec!["x.txt".to_string(), "folder".to_string()]));
        assert_eq!(read(&dst.join("x.txt")), "OLD");

        // Cancel
        let r = run_local(&plan, Resolution::Cancel);
        assert!(r.cancelled && r.copied.is_empty());

        // Rename: "x (1).txt" is taken, so "x (2).txt"; folder -> "folder (1)"
        let r = run_local(&plan, Resolution::Rename);
        assert_eq!(r.copied, ["x (2).txt", "folder (1)"]);
        assert_eq!(read(&dst.join("x (2).txt")), "NEW");
        assert_eq!(read(&dst.join("x.txt")), "OLD");
        assert_eq!(read(&dst.join("folder (1)/inner.txt")), "NEWINNER");

        // Replace: file overwritten, folder replaced wholesale (old.txt gone)
        let r = run_local(&plan, Resolution::Replace);
        assert_eq!(r.copied, ["x.txt", "folder"]);
        assert_eq!(read(&dst.join("x.txt")), "NEW");
        assert!(!dst.join("folder/old.txt").exists());
        assert_eq!(read(&dst.join("folder/inner.txt")), "NEWINNER");
    }

    #[test]
    fn replace_removes_read_only_items() {
        let root = tmp("ro");
        let (src, dst) = (root.join("src"), root.join("dst"));
        write(&src.join("f.txt"), "new");
        write(&src.join("d/n.txt"), "n");
        write(&dst.join("f.txt"), "old");
        write(&dst.join("d/ro.txt"), "ro");
        for p in [dst.join("f.txt"), dst.join("d/ro.txt")] {
            let mut perm = fs::metadata(&p).unwrap().permissions();
            perm.set_readonly(true);
            fs::set_permissions(&p, perm).unwrap();
        }
        let plan = plan_with(&[src.join("f.txt"), src.join("d")], Destination::Local(dst.clone()), None);
        assert_eq!(plan.conflicts.len(), 2);
        let r = run_local(&plan, Resolution::Replace);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(read(&dst.join("f.txt")), "new");
        assert!(!dst.join("d/ro.txt").exists());
    }

    #[test]
    fn same_path_is_not_a_conflict_and_is_never_clobbered() {
        let root = tmp("same");
        write(&root.join("keep.txt"), "precious");
        let plan = plan_with(&[root.join("keep.txt")], Destination::Local(root.clone()), None);
        assert!(plan.conflicts.is_empty());
        let r = run_local(&plan, Resolution::Rename);
        assert_eq!(r.failed.len(), 1);
        assert_eq!(read(&root.join("keep.txt")), "precious");
    }

    #[test]
    fn folder_into_itself_is_refused() {
        let root = tmp("self");
        write(&root.join("tree/a.txt"), "a");
        fs::create_dir_all(root.join("tree/sub")).unwrap();
        for dest in [root.join("tree"), root.join("tree/sub")] {
            let plan = plan_with(&[root.join("tree")], Destination::Local(dest), None);
            let r = run_local(&plan, Resolution::Rename);
            assert!(r.copied.is_empty(), "copied into itself");
            assert!(r.failed[0].1.contains("into itself") || r.failed[0].1.contains("already"), "{:?}", r.failed);
        }
        assert!(!root.join("tree/sub/tree").exists());
    }

    #[test]
    fn replace_never_deletes_the_source() {
        let root = tmp("inside");
        // dropping tree/x/x onto tree: dest "tree/x" contains the source
        write(&root.join("tree/x/x/data.txt"), "data");
        let plan = plan_with(&[root.join("tree/x/x")], Destination::Local(root.join("tree")), None);
        assert_eq!(plan.conflicts, ["x"]);
        let r = run_local(&plan, Resolution::Replace);
        assert_eq!(r.failed.len(), 1, "{r:?}");
        assert_eq!(read(&root.join("tree/x/x/data.txt")), "data");
    }

    #[test]
    fn duplicate_names_within_one_drop_conflict() {
        let root = tmp("dup");
        write(&root.join("a/same.txt"), "A");
        write(&root.join("b/same.txt"), "B");
        let dst = root.join("dst");
        fs::create_dir_all(&dst).unwrap();
        let plan = plan_with(&[root.join("a/same.txt"), root.join("b/same.txt")], Destination::Local(dst.clone()), None);
        assert_eq!((plan.items[0].conflict, plan.items[1].conflict), (false, true));
        let r = run_local(&plan, Resolution::Rename);
        assert_eq!(r.copied, ["same.txt", "same (1).txt"]);
        assert_eq!((read(&dst.join("same.txt")), read(&dst.join("same (1).txt"))), ("A".into(), "B".into()));
    }

    #[test]
    fn invalid_paths_are_reported_not_fatal() {
        let root = tmp("invalid");
        write(&root.join("ok.txt"), "ok");
        let dst = root.join("dst");
        fs::create_dir_all(&dst).unwrap();
        let plan = plan_with(&[root.join("missing.txt"), root.join("ok.txt")], Destination::Local(dst.clone()), None);
        assert_eq!(plan.invalid.len(), 1);
        let r = run_local(&plan, Resolution::Rename);
        assert_eq!(r.copied, ["ok.txt"]);
        assert_eq!(r.failed[0].0, "missing.txt");
    }

    #[test]
    fn cancel_stops_and_removes_partial_file() {
        let root = tmp("cancel");
        let dst = root.join("dst");
        fs::create_dir_all(&dst).unwrap();
        write(&root.join("big.bin"), &"x".repeat(3 << 20));
        write(&root.join("later.txt"), "later");
        let plan = plan_with(&[root.join("big.bin"), root.join("later.txt")], Destination::Local(dst.clone()), None);
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let r = execute_with(&plan, Resolution::Rename, &cancel, &move |p| {
            if p.bytes_done > 0 {
                c2.cancel(); // after the first 1 MiB chunk
            }
        }, None);
        assert!(r.cancelled);
        assert!(r.copied.is_empty() && r.failed.is_empty(), "{r:?}");
        assert!(!dst.join("big.bin").exists(), "partial file must be removed");
        assert!(!dst.join("later.txt").exists());
    }

    #[test]
    fn plan_and_result_serialize_camel_case() {
        let plan = DropPlan { dest: Destination::Remote { target: SshTarget::from_host("h"), path: "~".into() }, items: vec![], conflicts: vec![], invalid: vec![] };
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v["dest"]["remote"]["path"], "~");
        assert_eq!(v["dest"]["remote"]["target"]["host"], "h");
        let back: DropPlan = serde_json::from_value(v).unwrap();
        assert_eq!(back, plan);
        assert_eq!(serde_json::to_value(Resolution::Replace).unwrap(), "replace");
        let r = DropResult { failed: vec![("a".into(), "b".into())], ..Default::default() };
        assert_eq!(serde_json::to_value(&r).unwrap()["failed"][0][1], "b");
    }

    // ---- remote flow against fake ssh / sftp / scp (.cmd scripts)

    fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, format!("@echo off\r\n{body}\r\n")).unwrap();
        p
    }

    #[test]
    fn remote_conflicts_one_round_trip_with_fake_ssh() {
        let root = tmp("fakessh");
        let ssh = fake(&root, "ssh.cmd", "findstr \"^\" >nul\r\necho Welcome banner\r\necho UT_EXISTS\r\necho UT_MISSING");
        let tools = Tools { ssh, scp: None, sftp: None };
        let t = SshTarget::from_host("fakehost");
        assert_eq!(remote_conflicts(&tools, &t, "~", &["a", "b"]), [true, false]);
        // wrong number of markers -> indeterminate -> no conflict
        assert_eq!(remote_conflicts(&tools, &t, "~", &["a", "b", "c"]), [false, false, false]);
        // ssh that fails to run -> no conflict
        let missing = Tools { ssh: root.join("nope.cmd"), scp: None, sftp: None };
        assert_eq!(remote_conflicts(&missing, &t, "~", &["a"]), [false]);
    }

    #[test]
    fn remote_upload_prefers_sftp_and_replays_the_batch() {
        let root = tmp("fakesftp");
        let tools_dir = root.join("tools");
        fs::create_dir_all(&tools_dir).unwrap();
        // ssh answers every marker query with EXISTS; sftp records its stdin; scp must not be used
        let ssh = fake(&tools_dir, "ssh.cmd", "findstr \"^\" >nul\r\necho UT_EXISTS");
        let sftp = fake(&tools_dir, "sftp.cmd", &format!("findstr \"^\" > \"{}\"\r\nexit /b 0", tools_dir.join("sftp-in.txt").display()));
        let scp = fake(&tools_dir, "scp.cmd", "echo scp must not run 1>&2\r\nexit /b 9");
        let tools = Tools { ssh, scp: Some(scp), sftp: Some(sftp) };
        let src = root.join("my files");
        write(&src.join("one two.txt"), "1");
        let dest = Destination::Remote { target: SshTarget::from_host("fakehost"), path: "~/up dir".into() };
        let plan = plan_with(&[src.join("one two.txt")], dest, Some(&tools));
        assert_eq!(plan.conflicts, ["one two.txt"]); // fake ssh says it exists
        let r = execute_with(&plan, Resolution::Replace, &CancelToken::new(), &no_progress, Some(&tools));
        assert_eq!(r.copied, ["one two.txt"], "{r:?}");
        let batch = read(&tools_dir.join("sftp-in.txt"));
        assert_eq!(batch.trim(), "put \"./one two.txt\" \"up dir/one two.txt\"");
    }

    #[test]
    fn remote_failures_use_the_spec_error_text() {
        let root = tmp("fakefail");
        let ssh = fake(&root, "ssh.cmd", "findstr \"^\" >nul\r\necho UT_MISSING");
        let sftp = fake(&root, "sftp.cmd", "findstr \"^\" >nul\r\necho bob@h: Permission denied (publickey,password). 1>&2\r\nexit /b 255");
        let tools = Tools { ssh, scp: None, sftp: Some(sftp) };
        let src = root.join("src");
        write(&src.join("f.txt"), "1");
        let dest = Destination::Remote { target: SshTarget::from_host("h"), path: "~".into() };
        let plan = plan_with(&[src.join("f.txt")], dest, Some(&tools));
        assert!(plan.conflicts.is_empty());
        let r = execute_with(&plan, Resolution::Rename, &CancelToken::new(), &no_progress, Some(&tools));
        assert!(r.copied.is_empty());
        assert_eq!(r.failed[0].0, "f.txt");
        assert!(r.failed[0].1.starts_with("bob@h: Permission denied (publickey,password)."), "{}", r.failed[0].1);
        assert!(r.failed[0].1.ends_with("so drops reuse the login."));

        // empty output
        let scp = fake(&root, "scp2.cmd", "exit /b 7");
        let tools = Tools { ssh: tools.ssh, scp: Some(scp), sftp: None };
        let r = execute_with(&plan, Resolution::Rename, &CancelToken::new(), &no_progress, Some(&tools));
        assert_eq!(r.failed[0].1, "scp exited 7. For password logins, open SSH as its own tab so the drop can reuse that connection.");

        // no tools at all
        let r = execute_with(&plan, Resolution::Rename, &CancelToken::new(), &no_progress, None);
        assert_eq!(r.failed[0].1, "ssh was not found.");
    }

    #[test]
    fn remote_rename_and_replace_go_through_ssh_scripts() {
        let root = tmp("fakerename");
        // ssh records the script it received and answers UT_FREE 4 / succeeds
        let log = root.join("ssh-in.txt");
        let ssh = fake(&root, "ssh.cmd", &format!("findstr \"^\" >> \"{}\"\r\necho UT_EXISTS\r\necho UT_FREE 4", log.display()));
        let sftp = fake(&root, "sftp.cmd", &format!("findstr \"^\" > \"{}\"", root.join("sftp-in.txt").display()));
        let tools = Tools { ssh, scp: None, sftp: Some(sftp) };
        let src = root.join("src");
        write(&src.join("a.txt"), "1");
        let dest = Destination::Remote { target: SshTarget::from_host("h"), path: "~/d".into() };
        let plan = plan_with(&[src.join("a.txt")], dest.clone(), Some(&tools));
        assert_eq!(plan.conflicts, ["a.txt"]);
        let r = execute_with(&plan, Resolution::Rename, &CancelToken::new(), &no_progress, Some(&tools));
        assert_eq!(r.copied, ["a (4).txt"]);
        assert!(read(&root.join("sftp-in.txt")).contains("\"d/a (4).txt\""));
        assert!(read(&log).contains("while [ -e \"$d\"/'a ('\"$i\"').txt' ]"));

        let _ = fs::remove_file(&log);
        let r = execute_with(&plan, Resolution::Replace, &CancelToken::new(), &no_progress, Some(&tools));
        assert_eq!(r.copied, ["a.txt"], "{r:?}");
        assert!(read(&log).contains("rm -rf -- \"$HOME\"/'d/a.txt'"));
    }

    // ---- real POSIX shell semantics of the generated scripts (Git for Windows' bash), skipped if absent

    fn git_bash() -> Option<PathBuf> {
        ["ProgramFiles", "ProgramW6432"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|p| PathBuf::from(p).join("Git").join("usr").join("bin").join("bash.exe"))
            .find(|p| p.is_file())
    }

    fn posix(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    fn sh(bash: &Path, home: &Path, script: &str) -> String {
        let mut c = Command::new(bash);
        c.arg("-s").env("HOME", posix(home));
        run(&mut c, Some(script.as_bytes()), Duration::from_secs(60), None).unwrap().stdout
    }

    #[test]
    fn generated_scripts_behave_in_a_real_posix_shell() {
        let Some(bash) = git_bash() else { return };
        let home = tmp("posix home");
        let existing = home.join("up dir").join("it's here.txt");
        write(&existing, "x");
        write(&home.join("up dir/it's here (1).txt"), "x");
        // `~/rest` must expand against $HOME (§24 #23), including names with spaces and quotes
        let out = sh(
            &bash,
            &home,
            &conflict_script(&["~/up dir/it's here.txt".into(), "~/up dir/nope".into(), "~/literal tilde".into()]),
        );
        assert_eq!(parse_conflict_output(&out, 3), Some(vec![true, false, false]));
        // a literal directory named "~" must NOT be what we test
        write(&home.join("~/literal tilde"), "x");
        let out = sh(&bash, &home, &conflict_script(&["~/literal tilde".into()]));
        assert_eq!(parse_conflict_output(&out, 1), Some(vec![false]));
        // rename probe skips the taken "(1)" candidate
        let out = sh(&bash, &home, &rename_probe_script("~/up dir", "it's here.txt", false));
        assert_eq!(parse_free_name(&out, "it's here.txt", false).as_deref(), Some("it's here (2).txt"));
        // rm removes exactly the target, relative to $HOME
        let out = sh(&bash, &home, &rm_script("~/up dir/it's here.txt").unwrap());
        assert!(out.trim().is_empty());
        assert!(!existing.exists());
        assert!(home.join("up dir/it's here (1).txt").exists());
        assert!(home.join("~/literal tilde").exists());
    }
}
