//! Locating ssh / scp / sftp (§10.1).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment lookup, injectable for tests.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

fn process_env(k: &str) -> Option<OsString> {
    std::env::var_os(k)
}

/// `%SystemRoot%\System32\OpenSSH\ssh.exe`, then `%ProgramFiles%\Git\usr\bin\ssh.exe`, then `ssh` on `PATH`.
pub fn find_ssh() -> Option<PathBuf> {
    find_ssh_in(&process_env)
}

pub fn find_ssh_in(env: Env) -> Option<PathBuf> {
    let win32 = env("SystemRoot").map(|r| PathBuf::from(r).join("System32").join("OpenSSH").join("ssh.exe"));
    let git = env("ProgramFiles").map(|r| PathBuf::from(r).join("Git").join("usr").join("bin").join("ssh.exe"));
    [win32, git].into_iter().flatten().find(|p| p.is_file()).or_else(|| which("ssh", env))
}

/// `scp` next to `ssh`, then on `PATH`.
pub fn find_scp(ssh: &Path) -> Option<PathBuf> {
    find_sibling(ssh, "scp", &process_env)
}

/// `sftp` next to `ssh`, then on `PATH`.
pub fn find_sftp(ssh: &Path) -> Option<PathBuf> {
    find_sibling(ssh, "sftp", &process_env)
}

pub fn find_sibling(ssh: &Path, name: &str, env: Env) -> Option<PathBuf> {
    ssh.parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(format!("{name}.exe")))
        .filter(|p| p.is_file())
        .or_else(|| which(name, env))
}

fn which(name: &str, env: Env) -> Option<PathBuf> {
    let path = env("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|d| [d.join(format!("{name}.exe")), d.join(name)])
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"").unwrap();
    }

    fn root(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ut ssh locate {n} {}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn order_is_win32_then_git_then_path() {
        let r = root("order");
        let (sys, pf, bin) = (r.join("Windows"), r.join("Program Files"), r.join("bin"));
        let win32 = sys.join("System32/OpenSSH/ssh.exe");
        let git = pf.join("Git/usr/bin/ssh.exe");
        let onpath = bin.join("ssh.exe");
        let env = |k: &str| -> Option<OsString> {
            match k {
                "SystemRoot" => Some(sys.clone().into()),
                "ProgramFiles" => Some(pf.clone().into()),
                "PATH" => Some(bin.clone().into()),
                _ => None,
            }
        };
        assert_eq!(find_ssh_in(&env), None);
        touch(&onpath);
        assert_eq!(find_ssh_in(&env), Some(onpath));
        touch(&git);
        assert_eq!(find_ssh_in(&env), Some(git));
        touch(&win32);
        assert_eq!(find_ssh_in(&env), Some(win32));
    }

    #[test]
    fn scp_next_to_ssh_first_then_path() {
        let r = root("scp");
        let ssh = r.join("a/ssh.exe");
        let bin = r.join("bin");
        let env = |k: &str| (k == "PATH").then(|| bin.clone().into_os_string());
        assert_eq!(find_sibling(&ssh, "scp", &env), None);
        touch(&bin.join("scp.exe"));
        assert_eq!(find_sibling(&ssh, "scp", &env), Some(bin.join("scp.exe")));
        touch(&r.join("a/scp.exe"));
        assert_eq!(find_sibling(&ssh, "scp", &env), Some(r.join("a/scp.exe")));
        assert_eq!(find_sibling(&ssh, "sftp", &env), None);
    }
}
