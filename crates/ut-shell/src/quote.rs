//! Quoting a local path for each shell kind (§11.6) and Windows → POSIX path mapping.

use crate::kind::ShellKind;

/// Quotes one local path for pasting into a shell of `kind`.
///
/// PowerShell `'…'` (quotes doubled, curly quotes too: PowerShell treats `‘’‚‛` as quotes),
/// prefixed with `& ` when it is the command itself; cmd and Unknown `"…"`; Git Bash/MSYS
/// `'…'` after `C:\x` → `/c/x`; WSL `'…'` after `C:\x` → `/mnt/c/x`; SSH the path as-is,
/// single-quoted (a local path: the user's choice).
pub fn quote_path(kind: ShellKind, path: &str, first_token: bool) -> String {
    match kind {
        ShellKind::PowerShell => {
            let mut q = String::from(if first_token { "& '" } else { "'" });
            for c in path.chars() {
                q.push(c);
                if "'‘’‚‛".contains(c) {
                    q.push(c);
                }
            }
            q.push('\'');
            q
        }
        ShellKind::Cmd | ShellKind::Unknown => format!("\"{path}\""),
        ShellKind::Posix => single_quote(&to_posix_path(path)),
        ShellKind::Wsl => single_quote(&to_wsl_path(path)),
        ShellKind::Ssh => single_quote(path),
    }
}

/// Space-joined arguments (never `& `-prefixed).
pub fn quote_paths<S: AsRef<str>>(kind: ShellKind, paths: &[S]) -> String {
    paths.iter().map(|p| quote_path(kind, p.as_ref(), false)).collect::<Vec<_>>().join(" ")
}

/// POSIX single quotes; an embedded `'` becomes `'\''`.
fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `C:\x y\z` → `/c/x y/z`; UNC `\\srv\sh\x` → `//srv/sh/x`; anything else unchanged.
pub fn to_posix_path(p: &str) -> String {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    if let Some((d, rest)) = split_drive(p) {
        format!("/{d}{rest}")
    } else if p.starts_with(r"\\") {
        p.replace('\\', "/")
    } else {
        p.to_string()
    }
}

/// `C:\x` → `/mnt/c/x`; `\\wsl.localhost\<distro>\home\u` (or `\\wsl$\…`) → `/home/u`
/// (assumes the pane runs that distro); other UNC paths like [`to_posix_path`].
pub fn to_wsl_path(p: &str) -> String {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    if let Some((d, rest)) = split_drive(p) {
        return format!("/mnt/{d}{rest}");
    }
    let low = p.to_ascii_lowercase();
    for prefix in [r"\\wsl.localhost\", r"\\wsl$\"] {
        if low.starts_with(prefix) {
            let rest = &p[prefix.len()..]; // `<distro>\path`
            return rest.find('\\').map_or("/".to_string(), |i| rest[i..].replace('\\', "/"));
        }
    }
    to_posix_path(p)
}

/// `C:\a\b` → (`c`, `/a/b`); `C:` → (`c`, ``).
fn split_drive(p: &str) -> Option<(char, String)> {
    let mut it = p.chars();
    let d = it.next().filter(char::is_ascii_alphabetic)?;
    if it.next() != Some(':') {
        return None;
    }
    let rest = p[2..].replace('\\', "/");
    let rest = if rest.is_empty() || rest.starts_with('/') { rest } else { format!("/{rest}") };
    Some((d.to_ascii_lowercase(), rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShellKind::*;

    const P: &str = r"C:\Users\sbritos\OneDrive - BEPSA DEL PARAGUAY SAECA\my file.txt";

    #[test]
    fn powershell() {
        assert_eq!(quote_path(PowerShell, P, false), format!("'{P}'"));
        assert_eq!(quote_path(PowerShell, P, true), format!("& '{P}'"));
        assert_eq!(quote_path(PowerShell, r"C:\it's here\a.txt", false), r"'C:\it''s here\a.txt'");
        assert_eq!(quote_path(PowerShell, "C:\\it\u{2019}s\\a", false), "'C:\\it\u{2019}\u{2019}s\\a'");
        assert_eq!(quote_path(PowerShell, r"C:\$x `y` $(z)", false), r"'C:\$x `y` $(z)'");
    }

    #[test]
    fn cmd_and_unknown() {
        assert_eq!(quote_path(Cmd, P, false), format!("\"{P}\""));
        assert_eq!(quote_path(Cmd, P, true), format!("\"{P}\""));
        assert_eq!(quote_path(Unknown, r"C:\a b", false), "\"C:\\a b\"");
    }

    #[test]
    fn posix_git_bash() {
        assert_eq!(quote_path(Posix, r"C:\Users\a b\c.txt", false), "'/c/Users/a b/c.txt'");
        assert_eq!(quote_path(Posix, r"d:\x", false), "'/d/x'");
        assert_eq!(quote_path(Posix, r"C:\it's\a", false), r"'/c/it'\''s/a'");
        assert_eq!(quote_path(Posix, "/home/u/a b", false), "'/home/u/a b'");
        assert_eq!(quote_path(Posix, r"\\srv\share\a b", false), "'//srv/share/a b'");
        assert_eq!(quote_path(Posix, r"\\?\C:\x y", false), "'/c/x y'");
        assert_eq!(quote_path(Posix, r"C:\", false), "'/c/'");
        assert_eq!(quote_path(Posix, "C:", false), "'/c'");
    }

    #[test]
    fn wsl() {
        assert_eq!(quote_path(Wsl, r"C:\Users\a b\c.txt", false), "'/mnt/c/Users/a b/c.txt'");
        assert_eq!(quote_path(Wsl, r"C:\it's", false), r"'/mnt/c/it'\''s'");
        assert_eq!(quote_path(Wsl, r"\\wsl.localhost\Ubuntu\home\u\a b", false), "'/home/u/a b'");
        assert_eq!(quote_path(Wsl, r"\\WSL$\Ubuntu\tmp", false), "'/tmp'");
        assert_eq!(quote_path(Wsl, r"\\wsl.localhost\Ubuntu", false), "'/'");
        assert_eq!(quote_path(Wsl, "/home/u", false), "'/home/u'");
    }

    #[test]
    fn ssh_as_is() {
        assert_eq!(quote_path(Ssh, P, false), format!("'{P}'"));
        assert_eq!(quote_path(Ssh, "a'b", false), r"'a'\''b'");
    }

    #[test]
    fn many() {
        assert_eq!(quote_paths(PowerShell, &[r"C:\a b", r"C:\c"]), r"'C:\a b' 'C:\c'");
        assert_eq!(quote_paths(Posix, &[String::from(r"C:\a b"), String::from(r"C:\c")]), "'/c/a b' '/c/c'");
        assert_eq!(quote_paths::<&str>(Cmd, &[]), "");
    }
}
