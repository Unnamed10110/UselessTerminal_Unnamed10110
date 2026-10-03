//! Shell-kind detection (§6.5) and command-line splitting (§9.2).

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ShellKind {
    PowerShell,
    Ssh,
    Wsl,
    Cmd,
    Posix,
    Unknown,
}

/// §6.5 rules, in order, on the lowercased command line. The only additions to the literal
/// text are first-token checks (`wsl -d x`, `cmd /k x`, `sh -i`) at the rule they belong to,
/// which would otherwise fall through to `Unknown`.
pub fn detect_kind(cmdline: &str) -> ShellKind {
    let l = cmdline.trim().to_lowercase();
    let stem = file_stem_lower(&split_exe_and_args(cmdline).0);
    if l.contains("pwsh") || l.contains("powershell") {
        ShellKind::PowerShell
    } else if stem == "ssh" {
        ShellKind::Ssh
    } else if l.contains("wsl.exe") || l.contains("\\wsl") || l == "wsl" || stem == "wsl" {
        ShellKind::Wsl
    } else if l.ends_with("cmd.exe")
        || l.ends_with("cmd.exe\"")
        || l.ends_with("\\cmd")
        || l == "cmd"
        || l.contains("\\cmd.exe")
        || stem == "cmd"
    {
        ShellKind::Cmd
    } else if l.contains("bash")
        || l.contains("zsh")
        || l.contains("\\sh.exe")
        || l.ends_with("/sh")
        || stem == "sh"
    {
        ShellKind::Posix
    } else {
        ShellKind::Unknown
    }
}

/// File name of `exe` without its extension, lowercased (`C:\x\SSH.EXE` → `ssh`).
pub fn file_stem_lower(exe: &str) -> String {
    let name = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    let name = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    name.to_lowercase()
}

/// Splits a command line into `(executable, arguments)` without touching the text of either
/// (quotes around the executable are dropped, the arguments are returned verbatim, trimmed).
///
/// * `"C:\a b\x.exe" -v` → quoted: the executable is the text between the quotes.
/// * Unquoted, first word looks like a path: words are added until the prefix is an existing
///   file (§9.2 step 3: naive parsing "would stop at the first space, e.g. C:\Program"). When
///   nothing exists on disk, the first prefix ending in `.exe/.bat/.cmd` wins, but never
///   across an option-looking word (`-x`, `/c`), so `C:\tools\ssh -o X=y.exe` stays `ssh`.
/// * Anything else: the first word.
pub fn split_exe_and_args(cmdline: &str) -> (String, String) {
    let s = cmdline.trim();
    if let Some(rest) = s.strip_prefix('"') {
        return match rest.find('"') {
            Some(i) => (rest[..i].to_string(), rest[i + 1..].trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
    }
    let mut ends = Vec::new(); // byte offset of the end of each word
    let mut in_word = false;
    for (i, c) in s.char_indices() {
        match (c.is_whitespace(), in_word) {
            (true, true) => {
                ends.push(i);
                in_word = false;
            }
            (false, _) => in_word = true,
            _ => {}
        }
    }
    if in_word {
        ends.push(s.len());
    }
    let Some(&first_end) = ends.first() else {
        return (String::new(), String::new());
    };
    let mut exe_end = first_end;
    if s[..first_end].contains(['\\', '/']) {
        let on_disk = ends.iter().take(32).copied().find(|&e| {
            is_file(Path::new(&s[..e])) || is_file(Path::new(&format!("{}.exe", &s[..e])))
        });
        exe_end = on_disk.or_else(|| exe_by_extension(s, &ends)).unwrap_or(first_end);
    }
    (s[..exe_end].to_string(), s[exe_end..].trim().to_string())
}

fn exe_by_extension(s: &str, ends: &[usize]) -> Option<usize> {
    for (k, &e) in ends.iter().enumerate() {
        let l = s[..e].to_ascii_lowercase();
        if [".exe", ".bat", ".cmd"].iter().any(|x| l.ends_with(x)) {
            return Some(e);
        }
        let next = s[e..*ends.get(k + 1)?].trim_start();
        if next.starts_with('/') || (next.starts_with('-') && next.len() > 1) {
            return None;
        }
    }
    None
}

/// Exists and is not a directory. `symlink_metadata` so Store app-execution aliases
/// (`WindowsApps\pwsh.exe`, reparse points that cannot be followed) count as files.
pub(crate) fn is_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShellKind::*;

    #[test]
    fn kind_table() {
        let t = [
            (r"pwsh", PowerShell),
            (r"PWSH.EXE -NoLogo", PowerShell),
            (r#""C:\Program Files\PowerShell\7\pwsh.exe""#, PowerShell),
            (r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", PowerShell),
            (r"powershell -NoProfile", PowerShell),
            (r"ssh user@host", Ssh),
            (r"ssh.exe -p 2222 u@h", Ssh),
            (r#""C:\Program Files\Git\usr\bin\ssh.exe" -J jump u@h"#, Ssh),
            (r"C:\Program Files\OpenSSH\ssh.exe u@h", Ssh),
            (r"C:\Windows\System32\OpenSSH\ssh.exe u@h", Ssh),
            (r"wsl", Wsl),
            (r"wsl.exe", Wsl),
            (r"C:\Windows\System32\wsl.exe -d Ubuntu", Wsl),
            (r#"wsl.exe -d "Ubuntu-22.04""#, Wsl),
            (r"wsl -d Ubuntu", Wsl),
            (r"cmd", Cmd),
            (r"cmd.exe", Cmd),
            (r"CMD.EXE", Cmd),
            (r"C:\Windows\System32\cmd.exe", Cmd),
            (r#""C:\Windows\System32\cmd.exe""#, Cmd),
            (r"C:\Windows\System32\cmd.exe /k echo hi", Cmd),
            (r"C:\Windows\System32\cmd", Cmd),
            (r"cmd /k echo hi", Cmd),
            (r"bash", Posix),
            (r#""C:\Program Files\Git\bin\bash.exe" --login -i"#, Posix),
            (r"C:\msys64\usr\bin\zsh.exe", Posix),
            (r"C:\Git\usr\bin\sh.exe", Posix),
            (r"/usr/bin/sh", Posix),
            (r"sh -i", Posix),
            (r"nu.exe", Unknown),
            (r"python.exe", Unknown),
            (r"", Unknown),
        ];
        for (cmd, want) in t {
            assert_eq!(detect_kind(cmd), want, "{cmd}");
        }
    }

    #[test]
    fn kind_rule_order() {
        // 1 beats everything, 2 beats 3..5, 3 beats 4/5, 4 beats 5 (spec order)
        assert_eq!(detect_kind("ssh host pwsh"), PowerShell);
        assert_eq!(detect_kind("ssh bash-box"), Ssh);
        assert_eq!(detect_kind(r"wsl.exe -d bash-distro"), Wsl);
        assert_eq!(detect_kind(r"C:\Windows\System32\cmd.exe /c bash"), Cmd);
    }

    #[test]
    fn serde_names() {
        assert_eq!(serde_json::to_string(&PowerShell).unwrap(), "\"powerShell\"");
        assert_eq!(serde_json::to_string(&Posix).unwrap(), "\"posix\"");
        assert_eq!(serde_json::from_str::<ShellKind>("\"wsl\"").unwrap(), Wsl);
    }

    #[test]
    fn stems() {
        assert_eq!(file_stem_lower(r"C:\x y\SSH.EXE"), "ssh");
        assert_eq!(file_stem_lower("/usr/bin/zsh"), "zsh");
        assert_eq!(file_stem_lower("cmd"), "cmd");
        assert_eq!(file_stem_lower(".hidden"), ".hidden");
    }

    fn sp(c: &str) -> (String, String) {
        split_exe_and_args(c)
    }

    #[test]
    fn split_quoted_and_bare() {
        assert_eq!(sp(r#""C:\Program Files\Git\bin\bash.exe" --login -i"#), (r"C:\Program Files\Git\bin\bash.exe".into(), "--login -i".into()));
        assert_eq!(sp(r#"  "C:\a b\x.exe"  "#), (r"C:\a b\x.exe".into(), "".into()));
        assert_eq!(sp(r#""C:\a b\x.exe"-v"#), (r"C:\a b\x.exe".into(), "-v".into()));
        assert_eq!(sp(r#""unterminated C:\x"#), ("unterminated C:\\x".into(), "".into()));
        assert_eq!(sp("pwsh -NoLogo"), ("pwsh".into(), "-NoLogo".into()));
        assert_eq!(sp("cmd /c foo.exe"), ("cmd".into(), "/c foo.exe".into()));
        assert_eq!(sp("wsl.exe -d \"Ubuntu\""), ("wsl.exe".into(), "-d \"Ubuntu\"".into()));
        assert_eq!(sp(""), ("".into(), "".into()));
        assert_eq!(sp("   "), ("".into(), "".into()));
    }

    #[test]
    fn split_unquoted_spaced_paths_without_files() {
        assert_eq!(sp(r"C:\Program Files\PowerShell\7\pwsh.exe -NoLogo"), (r"C:\Program Files\PowerShell\7\pwsh.exe".into(), "-NoLogo".into()));
        assert_eq!(sp(r"C:\Program Files\Git\bin\bash.exe --login -i"), (r"C:\Program Files\Git\bin\bash.exe".into(), "--login -i".into()));
        assert_eq!(sp(r"C:\Program Files (x86)\Git\bin\bash.exe"), (r"C:\Program Files (x86)\Git\bin\bash.exe".into(), "".into()));
        assert_eq!(
            sp(r"C:\Users\x\OneDrive - BEPSA DEL PARAGUAY SAECA\tools\sh.CMD /c y"),
            (r"C:\Users\x\OneDrive - BEPSA DEL PARAGUAY SAECA\tools\sh.CMD".into(), "/c y".into())
        );
        // no extension and nothing on disk: first word; options never get swallowed
        assert_eq!(sp(r"C:\tools\ssh -o ProxyCommand=x.exe h"), (r"C:\tools\ssh".into(), "-o ProxyCommand=x.exe h".into()));
        assert_eq!(sp(r"C:\Program Files\x\y.exe /c a.exe"), (r"C:\Program Files\x\y.exe".into(), "/c a.exe".into()));
    }

    #[test]
    fn split_unquoted_spaced_paths_on_disk() {
        let base = std::env::temp_dir().join(format!("ut shell kind {}", std::process::id()));
        let dir = base.join("My Tools - X").join("bin dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tool"), b"").unwrap(); // no extension
        std::fs::write(dir.join("other.dat"), b"").unwrap();
        let b = base.display();

        let c = format!(r"{b}\My Tools - X\bin dir\tool --flag value");
        assert_eq!(sp(&c), (format!(r"{b}\My Tools - X\bin dir\tool"), "--flag value".into()));
        // `.exe` is tried too: file is `tool` but spec §9.2 prefix may omit it elsewhere
        std::fs::write(dir.join("t2.exe"), b"").unwrap();
        let c = format!(r"{b}\My Tools - X\bin dir\t2 -x");
        assert_eq!(sp(&c), (format!(r"{b}\My Tools - X\bin dir\t2"), "-x".into()));
        // detect_kind uses it too
        std::fs::write(dir.join("ssh.exe"), b"").unwrap();
        assert_eq!(detect_kind(&format!(r"{b}\My Tools - X\bin dir\ssh.exe u@h")), ShellKind::Ssh);
        let _ = std::fs::remove_dir_all(&base);
    }
}
