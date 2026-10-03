//! Git branch resolution without spawning `git.exe` (§7.6).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Walks up from `cwd` to the first `.git`. A directory is the gitdir; a file containing
/// `gitdir: <path>` (worktrees, submodules; relative paths resolve against the file's folder)
/// points at it. `None` when there is no repository or the gitfile is malformed.
pub fn find_git_dir(cwd: &Path) -> Option<PathBuf> {
    if !cwd.is_absolute() {
        return None; // never resolve against the process's own cwd
    }
    for dir in cwd.ancestors() {
        let dot = dir.join(".git");
        match std::fs::metadata(&dot) {
            Ok(m) if m.is_dir() => return Some(dot),
            Ok(m) if m.is_file() => return gitdir_from_file(&dot),
            _ => {}
        }
    }
    None
}

fn gitdir_from_file(file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    let target = text.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
    let p = Path::new(target);
    Some(normalize(&if p.is_absolute() { p.to_path_buf() } else { file.parent()?.join(p) }))
}

/// Lexical `.`/`..` removal, so the path handed to a file watcher is clean.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// `ref: refs/heads/<name>` → name; a bare 40-hex (64 for SHA-256 repos) object id (detached
/// HEAD) → first 8 characters; anything else → `None`.
pub fn parse_head(text: &str) -> Option<String> {
    let t = text.trim();
    if let Some(r) = t.strip_prefix("ref:") {
        return r.trim().strip_prefix("refs/heads/").filter(|n| !n.is_empty()).map(str::to_string);
    }
    (matches!(t.len(), 40 | 64) && t.bytes().all(|b| b.is_ascii_hexdigit())).then(|| t[..8].to_string())
}

/// Remembers, per cwd, which `<gitdir>/HEAD` file serves it, so repeated lookups cost one
/// file read instead of a directory walk. The branch itself is never cached (it is
/// re-read every call, so `git checkout` is seen on the next refresh).
#[derive(Default)]
pub struct GitCache {
    heads: Mutex<HashMap<PathBuf, PathBuf>>,
}

impl GitCache {
    /// The HEAD file for `cwd` (for the app to watch). `cwd` must be an absolute Windows path
    /// (not a remote or POSIX-mapped one). Not-a-repo results are not cached: `git init` shows up at once.
    pub fn head_file_for(&self, cwd: impl AsRef<Path>) -> Option<PathBuf> {
        let cwd = cwd.as_ref();
        let mut heads = self.heads.lock();
        if let Some(h) = heads.get(cwd) {
            if h.is_file() {
                return Some(h.clone());
            }
            heads.remove(cwd);
        }
        let head = find_git_dir(cwd)?.join("HEAD");
        if !head.is_file() {
            return None;
        }
        if heads.len() >= 256 {
            heads.clear();
        }
        heads.insert(cwd.to_path_buf(), head.clone());
        Some(head)
    }

    /// Current branch name (or short hash when detached) for `cwd`.
    pub fn branch_for(&self, cwd: impl AsRef<Path>) -> Option<String> {
        parse_head(&std::fs::read_to_string(self.head_file_for(cwd)?).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Fresh temp dir whose name contains spaces (§23.29).
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("ut shell git {tag} {}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    #[test]
    fn head_parsing() {
        assert_eq!(parse_head("ref: refs/heads/main\n").as_deref(), Some("main"));
        assert_eq!(parse_head("ref: refs/heads/feature/x y\r\n").as_deref(), Some("feature/x y"));
        assert_eq!(parse_head("ref:refs/heads/dev").as_deref(), Some("dev"));
        assert_eq!(parse_head(&format!("{SHA}\n")).as_deref(), Some("01234567"));
        assert_eq!(parse_head(&"A".repeat(64)).as_deref(), Some("AAAAAAAA"));
        assert_eq!(parse_head("ref: refs/tags/v1"), None);
        assert_eq!(parse_head("ref: refs/heads/"), None);
        assert_eq!(parse_head(&SHA[..39]), None);
        assert_eq!(parse_head(&SHA.replace('0', "g")), None);
        assert_eq!(parse_head(""), None);
    }

    #[test]
    fn plain_repo_from_subdirectory() {
        let t = Tmp::new("plain");
        let repo = t.0.join("my repo");
        write(&repo.join(".git/HEAD"), "ref: refs/heads/main\n");
        fs::create_dir_all(repo.join("a b/c")).unwrap();
        let c = GitCache::default();
        assert_eq!(c.branch_for(&repo).as_deref(), Some("main"));
        assert_eq!(c.branch_for(repo.join("a b/c")).as_deref(), Some("main"));
        assert_eq!(c.head_file_for(repo.join("a b")), Some(repo.join(".git").join("HEAD")));
    }

    #[test]
    fn not_a_repo_relative_and_empty() {
        let t = Tmp::new("none");
        let c = GitCache::default();
        // nothing under the temp dir is a repo; stop before reaching any real ancestor repo
        assert_eq!(find_git_dir(&t.0).filter(|g| g.starts_with(&t.0)), None);
        assert_eq!(c.branch_for(""), None);
        assert_eq!(c.branch_for("relative/dir"), None);
        write(&t.0.join("repo/.git/HEAD"), "ref: refs/heads/x\n");
        assert_eq!(c.branch_for(t.0.join("repo")).as_deref(), Some("x")); // appears without restart
    }

    #[test]
    fn detached_head() {
        let t = Tmp::new("detached");
        write(&t.0.join(".git/HEAD"), &format!("{SHA}\n"));
        assert_eq!(GitCache::default().branch_for(&t.0).as_deref(), Some("01234567"));
    }

    #[test]
    fn garbage_head_is_blank() {
        let t = Tmp::new("garbage");
        write(&t.0.join(".git/HEAD"), "whatever\n");
        assert_eq!(GitCache::default().branch_for(&t.0), None);
    }

    #[test]
    fn worktree_gitfile_relative() {
        let t = Tmp::new("wt");
        let main = t.0.join("main repo");
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&main.join(".git/worktrees/wt1/HEAD"), "ref: refs/heads/topic\n");
        let wt = t.0.join("work tree");
        write(&wt.join(".git"), "gitdir: ../main repo/.git/worktrees/wt1\n");
        fs::create_dir_all(wt.join("src")).unwrap();
        let c = GitCache::default();
        assert_eq!(c.branch_for(wt.join("src")).as_deref(), Some("topic"));
        assert_eq!(c.branch_for(&main).as_deref(), Some("main"));
        // clean path for the watcher: no `..`
        let head = c.head_file_for(&wt).unwrap();
        assert_eq!(head, main.join(".git").join("worktrees").join("wt1").join("HEAD"));
    }

    #[test]
    fn worktree_gitfile_absolute() {
        let t = Tmp::new("wtabs");
        let gd = t.0.join("gd dir");
        write(&gd.join("HEAD"), "ref: refs/heads/abs\n");
        let wt = t.0.join("w");
        write(&wt.join(".git"), &format!("gitdir: {}\n", gd.display()));
        assert_eq!(GitCache::default().branch_for(&wt).as_deref(), Some("abs"));
    }

    #[test]
    fn submodule_gitfile() {
        let t = Tmp::new("sub");
        let sup = t.0.join("super");
        write(&sup.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&sup.join(".git/modules/lib/HEAD"), &format!("{SHA}\n"));
        let sub = sup.join("vendor/lib dir");
        write(&sub.join(".git"), "gitdir: ../../.git/modules/lib\n");
        let c = GitCache::default();
        assert_eq!(c.branch_for(&sub).as_deref(), Some("01234567"));
        assert_eq!(c.branch_for(sup.join("vendor")).as_deref(), Some("main"));
    }

    #[test]
    fn broken_gitfile_is_none_not_parent_repo() {
        let t = Tmp::new("broken");
        write(&t.0.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&t.0.join("sub/.git"), "not a gitfile\n");
        assert_eq!(GitCache::default().branch_for(t.0.join("sub")), None);
        write(&t.0.join("sub2/.git"), "gitdir: ../missing\n");
        assert_eq!(GitCache::default().branch_for(t.0.join("sub2")), None);
    }

    #[test]
    fn rereads_head_on_every_call_and_survives_deletion() {
        let t = Tmp::new("reread");
        let head = t.0.join(".git/HEAD");
        write(&head, "ref: refs/heads/one\n");
        let c = GitCache::default();
        assert_eq!(c.branch_for(&t.0).as_deref(), Some("one"));
        write(&head, "ref: refs/heads/two\n"); // `git checkout two`
        assert_eq!(c.branch_for(&t.0).as_deref(), Some("two"));
        fs::remove_dir_all(t.0.join(".git")).unwrap();
        assert_eq!(c.branch_for(&t.0), None);
        write(&head, "ref: refs/heads/three\n");
        assert_eq!(c.branch_for(&t.0).as_deref(), Some("three"));
    }
}
