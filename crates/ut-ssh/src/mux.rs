//! Connection reuse / ControlMaster, capability-gated (§10.5, §24 #22). Win32-OpenSSH does not support
//! multiplexing ("getsockname failed"); Git's MSYS ssh does.

use crate::target::parse_argv;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// stderr of `ssh -O check` says the build cannot multiplex?
pub fn classify_mux_stderr(stderr: &str) -> bool {
    let l = stderr.to_lowercase();
    !["not supported", "unsupported", "bad configuration option", "getsockname"].iter().any(|k| l.contains(k))
}

/// `ControlPath=<p>` as ONE argv element. ssh splits option values on whitespace, so a path with spaces
/// must carry its own quotes: `ControlPath="C:/a b/%C"`.
pub(crate) fn control_path_arg(p: &Path) -> String {
    let p = p.to_string_lossy().replace('\\', "/");
    if p.contains(char::is_whitespace) { format!("ControlPath=\"{p}\"") } else { format!("ControlPath={p}") }
}

/// Quote one argument for a Windows command line (CRT rules: wrap in quotes, `"` -> `\"`).
pub(crate) fn win_quote(a: &str) -> String {
    if a.is_empty() || a.contains(|c: char| c.is_whitespace() || c == '"') {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_string()
    }
}

/// Probe once per ssh executable (cached): run `ssh -o ControlMaster=auto -o ControlPath=<tmp> -O check
/// localhost` and classify stderr. Blocking (a few ms); call from a worker thread.
pub fn probe_multiplexing(ssh_exe: &Path) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let cached = cache.lock().ok().and_then(|m| m.get(ssh_exe).copied());
    if let Some(v) = cached {
        return v;
    }
    let sock = std::env::temp_dir().join(format!("ut-ssh-probe-{}.sock", std::process::id()));
    let out = crate::run(
        Command::new(ssh_exe).args(["-o", "ControlMaster=auto", "-o"]).arg(control_path_arg(&sock)).args(["-O", "check", "localhost"]),
        None,
        Duration::from_secs(10),
        None,
    );
    let supported = matches!(&out, Ok(o) if !o.timed_out && classify_mux_stderr(&o.stderr));
    if let Ok(mut m) = cache.lock() {
        m.insert(ssh_exe.to_path_buf(), supported);
    }
    supported
}

/// `%LOCALAPPDATA%\UselessTerminal\ssh-mux`, created on first use and restricted to the current user
/// (best effort: `icacls /inheritance:r /grant:r "<USER>:(OI)(CI)F"`).
pub fn mux_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = ut_fs::local_data_dir().join("ssh-mux");
        if std::fs::create_dir_all(&d).is_ok() {
            if let Some(user) = std::env::var_os("USERNAME") {
                let grant = format!("{}:(OI)(CI)F", user.to_string_lossy());
                let _ = crate::run(
                    Command::new("icacls").arg(&d).args(["/inheritance:r", "/grant:r"]).arg(grant),
                    None,
                    Duration::from_secs(10),
                    None,
                );
            }
        }
        d
    })
    .clone()
}

/// Add `-o ControlMaster=auto -o ControlPersist=yes -o ControlPath=<mux_dir>/%C` to an ssh command line,
/// only when it is an ssh command with a destination and no ControlPath of its own, and
/// `mode` is `"always"`, or `"auto"` (anything but `"never"`/`"always"`) and the build multiplexes.
pub fn inject_connection_reuse(cmdline: &str, mode: &str, ssh_exe: &Path) -> String {
    inject_with(cmdline, mode, || probe_multiplexing(ssh_exe), mux_dir)
}

fn inject_with(cmdline: &str, mode: &str, supported: impl FnOnce() -> bool, dir: impl FnOnce() -> PathBuf) -> String {
    let unchanged = || cmdline.to_string();
    if mode == "never" {
        return unchanged();
    }
    match parse_argv(cmdline) {
        Some(t) if t.control_path.is_none() => {}
        _ => return unchanged(),
    }
    if mode != "always" && !supported() {
        return unchanged();
    }
    let end = first_token_end(cmdline);
    let cp = win_quote(&control_path_arg(&dir().join("%C")));
    format!("{} -o ControlMaster=auto -o ControlPersist=yes -o {cp}{}", &cmdline[..end], &cmdline[end..])
}

/// Byte offset just past argv[0] (same quoting rule as the tokenizer).
fn first_token_end(s: &str) -> usize {
    let mut quoted = false;
    let mut started = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    return i;
                }
            }
            _ => started = true,
        }
    }
    s.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify() {
        assert!(!classify_mux_stderr("getsockname failed: Not a socket\r\nRead from remote host localhost: Unknown error"));
        assert!(!classify_mux_stderr("command-line: line 0: Bad configuration option: controlmaster"));
        assert!(!classify_mux_stderr("ControlMaster is not supported on this platform"));
        assert!(!classify_mux_stderr("Unsupported option \"ControlPath\""));
        assert!(classify_mux_stderr("Control socket connect(C:/x.sock): No such file or directory"));
        assert!(classify_mux_stderr(""));
    }

    #[test]
    fn probe_matches_the_installed_builds_and_is_cached() {
        for (exe, want) in [
            (r"C:\Windows\System32\OpenSSH\ssh.exe", false),
            (r"C:\Program Files\Git\usr\bin\ssh.exe", true),
        ] {
            let exe = Path::new(exe);
            if exe.is_file() {
                assert_eq!(probe_multiplexing(exe), want, "{}", exe.display());
                assert_eq!(probe_multiplexing(exe), want);
            }
        }
        assert!(!probe_multiplexing(Path::new("Z:/definitely/not/ssh.exe")));
    }

    #[test]
    fn quoting_helpers() {
        assert_eq!(control_path_arg(Path::new(r"C:\Users\me\mux\%C")), "ControlPath=C:/Users/me/mux/%C");
        assert_eq!(control_path_arg(Path::new(r"C:\Users\a b\mux\%C")), "ControlPath=\"C:/Users/a b/mux/%C\"");
        assert_eq!(win_quote("plain"), "plain");
        assert_eq!(win_quote("ControlPath=\"C:/a b/%C\""), "\"ControlPath=\\\"C:/a b/%C\\\"\"");
        assert_eq!(first_token_end(r#""C:\Program Files\ssh.exe" host"#), r#""C:\Program Files\ssh.exe""#.len());
        assert_eq!(first_token_end("  ssh host"), 5);
        assert_eq!(first_token_end("ssh"), 3);
    }

    #[test]
    fn injects_only_into_ssh_without_controlpath_when_supported() {
        let dir = || PathBuf::from(r"C:\Users\me\AppData\Local\UselessTerminal\ssh-mux");
        let yes = || true;
        let no = || false;
        let cmd = r#""C:\Program Files\Git\usr\bin\ssh.exe" -p 22 bob@h"#;
        let out = inject_with(cmd, "auto", yes, dir);
        assert_eq!(
            out,
            r#""C:\Program Files\Git\usr\bin\ssh.exe" -o ControlMaster=auto -o ControlPersist=yes -o ControlPath=C:/Users/me/AppData/Local/UselessTerminal/ssh-mux/%C -p 22 bob@h"#
        );
        // the injected command still parses, and the target now carries the ControlPath for helpers
        let t = parse_argv(&out).unwrap();
        assert_eq!((t.host.as_str(), t.port), ("h", Some(22)));
        assert_eq!(t.control_path.as_deref(), Some("C:/Users/me/AppData/Local/UselessTerminal/ssh-mux/%C"));
        // not supported
        assert_eq!(inject_with(cmd, "auto", no, dir), cmd);
        // "always" ignores the probe
        assert_ne!(inject_with(cmd, "always", no, dir), cmd);
        // "never"
        assert_eq!(inject_with(cmd, "never", yes, dir), cmd);
        // own ControlPath
        let own = "ssh -o ControlPath=/x/%C host";
        assert_eq!(inject_with(own, "always", yes, dir), own);
        // not ssh / no destination
        assert_eq!(inject_with("pwsh -NoLogo", "always", yes, dir), "pwsh -NoLogo");
        assert_eq!(inject_with("ssh -V", "always", yes, dir), "ssh -V");
        // bare `ssh` token at the start
        assert!(inject_with("ssh host", "always", yes, dir).starts_with("ssh -o ControlMaster=auto"));
    }

    #[test]
    fn spaced_mux_dir_is_quoted_for_ssh() {
        let dir = || PathBuf::from(r"C:\Users\a b\mux");
        let out = inject_with("ssh host", "always", || true, dir);
        assert!(out.contains(r#"-o "ControlPath=\"C:/Users/a b/mux/%C\"""#), "{out}");
    }
}
