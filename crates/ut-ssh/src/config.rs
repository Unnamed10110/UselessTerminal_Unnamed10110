//! `~/.ssh/config` parsing for the "import SSH hosts" feature (§8.7). The app turns the returned hosts
//! into sessions named `[SSH] {alias}`; each runs `ssh <alias>` so ssh applies the whole configuration.

use crate::target::expand_tilde;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshHost {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
    /// File the `Host` line came from (empty when parsed from text).
    pub source_file: String,
}

/// Split a value into arguments honoring `"…"` / `'…'`; an unquoted word starting with `#` is a comment.
fn split_args(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote, mut in_tok) = (Vec::new(), String::new(), None::<char>, false);
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_tok = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_tok {
                    out.push(std::mem::take(&mut cur));
                    in_tok = false;
                }
            }
            (None, '#') if !in_tok => break,
            (None, c) => {
                cur.push(c);
                in_tok = true;
            }
        }
    }
    if in_tok {
        out.push(cur);
    }
    out
}

/// `Key value`, `Key=value`, `Key = value` -> (lowercase key, rest). Blank and `#` lines -> `None`.
fn key_value(line: &str) -> Option<(String, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let (k, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    Some((k.to_ascii_lowercase(), rest))
}

/// A pattern becomes a session only if it has no `*`, `?` or `!` (and cannot be mistaken for an option).
fn usable_alias(p: &str) -> bool {
    !p.is_empty() && !p.starts_with('-') && !p.contains(['*', '?', '!', '"'])
}

#[derive(Default)]
struct Parser {
    hosts: Vec<SshHost>,
    /// Indices of the hosts the current `Host` block applies to (empty inside `Match` / wildcard-only blocks,
    /// so their directives cannot leak into a previous block).
    cur: Vec<usize>,
}

impl Parser {
    /// Feed one line; returns the patterns of an `Include` directive for the caller to expand.
    fn line(&mut self, line: &str, source: &str) -> Option<Vec<String>> {
        let (key, value) = key_value(line)?;
        let args = split_args(value);
        match key.as_str() {
            "host" => {
                self.cur.clear();
                for a in args.iter().filter(|a| usable_alias(a)) {
                    self.hosts.push(SshHost { alias: a.clone(), source_file: source.to_string(), ..Default::default() });
                    self.cur.push(self.hosts.len() - 1);
                }
            }
            "match" => self.cur.clear(),
            "include" => return Some(args),
            "hostname" | "user" | "port" | "identityfile" | "proxyjump" => {
                let v = args.into_iter().next()?;
                for &i in &self.cur {
                    let h = &mut self.hosts[i];
                    // ssh uses the first value it finds for a keyword
                    match key.as_str() {
                        "hostname" => h.hostname.get_or_insert_with(|| v.clone()),
                        "user" => h.user.get_or_insert_with(|| v.clone()),
                        "identityfile" => h.identity_file.get_or_insert_with(|| v.clone()),
                        "proxyjump" => h.proxy_jump.get_or_insert_with(|| v.clone()),
                        _ => {
                            if h.port.is_none() {
                                h.port = v.parse().ok();
                            }
                            continue;
                        }
                    };
                }
            }
            _ => {}
        }
        None
    }
}

/// Parse config text (no `Include` expansion).
pub fn parse_ssh_config(text: &str) -> Vec<SshHost> {
    let mut p = Parser::default();
    for l in text.trim_start_matches('\u{feff}').lines() {
        p.line(l, "");
    }
    p.hosts
}

/// Read `path` (default `%USERPROFILE%\.ssh\config`) following `Include` [P1]. A missing file is an
/// empty list; unreadable included files are skipped.
pub fn read_ssh_config(path: Option<&Path>) -> io::Result<Vec<SshHost>> {
    let ssh_dir = ut_fs::home_dir().join(".ssh");
    read_ssh_config_in(path, &ssh_dir)
}

pub(crate) fn read_ssh_config_in(path: Option<&Path>, ssh_dir: &Path) -> io::Result<Vec<SshHost>> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(|| ssh_dir.join("config"));
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut p = Parser::default();
    let mut stack = vec![fs::canonicalize(&path).unwrap_or_else(|_| path.clone())];
    feed(&mut p, &path, &bytes, ssh_dir, &mut stack);
    Ok(p.hosts)
}

fn feed(p: &mut Parser, path: &Path, bytes: &[u8], ssh_dir: &Path, stack: &mut Vec<PathBuf>) {
    let text = String::from_utf8_lossy(bytes);
    let src = path.to_string_lossy();
    for l in text.trim_start_matches('\u{feff}').lines() {
        let Some(patterns) = p.line(l, &src) else { continue };
        for pat in patterns {
            for f in glob_include(&pat, ssh_dir) {
                // OpenSSH also caps the depth at 16; the stack doubles as the cycle check.
                let Ok(canon) = fs::canonicalize(&f) else { continue };
                if stack.len() >= 16 || stack.contains(&canon) {
                    continue;
                }
                let Ok(b) = fs::read(&canon) else { continue };
                stack.push(canon);
                feed(p, &f, &b, ssh_dir, stack);
                stack.pop();
            }
        }
    }
}

/// Include patterns: `~` expands to the home directory, relative paths are relative to `~/.ssh`.
fn glob_include(pat: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let expanded = expand_tilde(pat);
    let p = Path::new(&expanded);
    let p = if p.is_absolute() { p.to_path_buf() } else { ssh_dir.join(p) };
    glob(&p)
}

/// Expand `*` / `?` in any path component; matches are files, sorted per directory.
fn glob(p: &Path) -> Vec<PathBuf> {
    let mut cur = vec![PathBuf::new()];
    for comp in p.components() {
        let name = comp.as_os_str().to_string_lossy();
        if !name.contains(['*', '?']) {
            cur.iter_mut().for_each(|c| c.push(comp));
            continue;
        }
        let mut next = Vec::new();
        for base in &cur {
            let Ok(rd) = fs::read_dir(base) else { continue };
            let mut names: Vec<_> = rd
                .filter_map(Result::ok)
                .map(|e| e.file_name())
                .filter(|n| {
                    let n = n.to_string_lossy();
                    (!n.starts_with('.') || name.starts_with('.')) && wildmatch(&name, &n)
                })
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|n| base.join(n)));
        }
        cur = next;
    }
    cur.retain(|f| f.is_file());
    cur
}

/// Case-insensitive `*` / `?` matcher.
fn wildmatch(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let s: Vec<char> = s.to_lowercase().chars().collect();
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None::<usize>, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// `"<ssh>" <alias>` (§8.7 [P1]: ssh applies the user's whole configuration itself).
pub fn host_to_command_line(ssh_exe: &Path, h: &SshHost) -> String {
    let alias = if h.alias.contains(char::is_whitespace) { format!("\"{}\"", h.alias) } else { h.alias.clone() };
    format!("\"{}\" {alias}", ssh_exe.display())
}

/// HostName / User / Port for the session description, e.g. `bob@10.1.2.3:2222` (empty when the entry
/// has none of them).
pub fn host_description(h: &SshHost) -> String {
    if h.hostname.is_none() && h.user.is_none() && h.port.is_none() {
        return String::new();
    }
    let host = h.hostname.as_deref().unwrap_or(&h.alias);
    let mut s = match &h.user {
        Some(u) => format!("{u}@{host}"),
        None => host.to_string(),
    };
    if let Some(p) = h.port {
        s.push_str(&if host.contains(':') { format!(" port {p}") } else { format!(":{p}") });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(hs: &[SshHost]) -> Vec<&str> {
        hs.iter().map(|h| h.alias.as_str()).collect()
    }

    #[test]
    fn basic_keys_case_insensitive_and_separators() {
        let hs = parse_ssh_config(
            "# comment\n\nHOST web\n  hostname=10.0.0.1\n  USER = bob\n  Port 2222\n  IdentityFile \"~/.ssh/my key\"\n  ProxyJump jump\nhost db\n\tHostName\tdb.local\n",
        );
        assert_eq!(aliases(&hs), ["web", "db"]);
        let w = &hs[0];
        assert_eq!(w.hostname.as_deref(), Some("10.0.0.1"));
        assert_eq!(w.user.as_deref(), Some("bob"));
        assert_eq!(w.port, Some(2222));
        assert_eq!(w.identity_file.as_deref(), Some("~/.ssh/my key"));
        assert_eq!(w.proxy_jump.as_deref(), Some("jump"));
        assert_eq!(hs[1].hostname.as_deref(), Some("db.local"));
        assert_eq!(hs[1].user, None);
    }

    #[test]
    fn multi_pattern_host_line_gives_one_entry_per_pattern() {
        let hs = parse_ssh_config("Host a b \"c d\" e*\n  User u\n");
        assert_eq!(aliases(&hs), ["a", "b", "c d"]);
        assert!(hs.iter().all(|h| h.user.as_deref() == Some("u")));
    }

    #[test]
    fn wildcard_only_and_negated_hosts_are_skipped() {
        let hs = parse_ssh_config("Host *\n  User all\nHost *.example.com ?x !bad\n  User x\nHost real !neg\n  Port 1\n");
        assert_eq!(aliases(&hs), ["real"]);
        assert_eq!(hs[0].port, Some(1));
        assert_eq!(hs[0].user, None); // `Host *` defaults are not merged
    }

    #[test]
    fn match_blocks_do_not_leak() {
        let hs = parse_ssh_config("Host a\n  User one\nMatch host b\n  User leaked\n  Port 9\nHost c\n  User three\n");
        assert_eq!(aliases(&hs), ["a", "c"]);
        assert_eq!(hs[0].user.as_deref(), Some("one"));
        assert_eq!(hs[0].port, None);
        assert_eq!(hs[1].user.as_deref(), Some("three"));
    }

    #[test]
    fn first_value_wins_and_comments() {
        let hs = parse_ssh_config("Host a # trailing comment\n  User first\n  User second # c\n  Port nope\n");
        assert_eq!(aliases(&hs), ["a"]);
        assert_eq!(hs[0].user.as_deref(), Some("first"));
        assert_eq!(hs[0].port, None);
    }

    #[test]
    fn bom_and_crlf() {
        let hs = parse_ssh_config("\u{feff}Host a\r\n  HostName h\r\n");
        assert_eq!((hs[0].alias.as_str(), hs[0].hostname.as_deref()), ("a", Some("h")));
    }

    #[test]
    fn unsafe_aliases_are_skipped() {
        let hs = parse_ssh_config("Host -oProxyCommand=x ok\n");
        assert_eq!(aliases(&hs), ["ok"]);
    }

    #[test]
    fn wildmatch_cases() {
        assert!(wildmatch("*.conf", "A.CONF"));
        assert!(wildmatch("a?c*", "abcdef"));
        assert!(wildmatch("*", ""));
        assert!(!wildmatch("a?c", "ac"));
        assert!(!wildmatch("*.conf", "x.confx"));
    }

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ut ssh config {n} {}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn include_globs_relative_to_ssh_dir_and_cycles() {
        let ssh = tmp("include");
        fs::create_dir_all(ssh.join("conf.d")).unwrap();
        fs::write(ssh.join("config"), "Include conf.d/*.conf\nInclude \"extra file\"\nInclude missing/*\nHost main\n  User m\n").unwrap();
        fs::write(ssh.join("conf.d/b.conf"), "Host b\n  HostName bh\n").unwrap();
        fs::write(ssh.join("conf.d/a.conf"), "Host a1 a2\n  Port 22\n").unwrap();
        fs::write(ssh.join("conf.d/ignored.txt"), "Host nope\n").unwrap();
        fs::write(ssh.join("extra file"), "Host spaced\nInclude config\n").unwrap(); // cycle back to config
        let hs = read_ssh_config_in(None, &ssh).unwrap();
        assert_eq!(aliases(&hs), ["a1", "a2", "b", "spaced", "main"]);
        assert!(hs[0].source_file.ends_with("a.conf"));
        assert!(hs[4].source_file.ends_with("config"));
        assert_eq!(hs[3].user, None);
    }

    #[test]
    fn include_in_the_middle_keeps_block_context() {
        let ssh = tmp("ctx");
        fs::write(ssh.join("config"), "Host a\n  Include more\n  Port 5\n").unwrap();
        fs::write(ssh.join("more"), "  User fromfile\n").unwrap();
        let hs = read_ssh_config_in(None, &ssh).unwrap();
        assert_eq!((hs[0].user.as_deref(), hs[0].port), (Some("fromfile"), Some(5)));
    }

    #[test]
    fn absolute_include_and_self_include() {
        let ssh = tmp("abs");
        let other = tmp("abs-other");
        let f = other.join("hosts");
        fs::write(&f, "Host far\n").unwrap();
        fs::write(ssh.join("config"), format!("Include \"{}\"\nInclude config\nHost near\n", f.display())).unwrap();
        let hs = read_ssh_config_in(None, &ssh).unwrap();
        assert_eq!(aliases(&hs), ["far", "near"]);
    }

    #[test]
    fn missing_file_is_empty() {
        let ssh = tmp("missing");
        assert!(read_ssh_config_in(None, &ssh).unwrap().is_empty());
        assert!(read_ssh_config_in(Some(&ssh.join("nope")), &ssh).unwrap().is_empty());
    }

    #[test]
    fn command_line_and_description() {
        let h = SshHost { alias: "web".into(), hostname: Some("10.0.0.1".into()), user: Some("bob".into()), port: Some(2222), ..Default::default() };
        let exe = Path::new(r"C:\Program Files\Git\usr\bin\ssh.exe");
        assert_eq!(host_to_command_line(exe, &h), r#""C:\Program Files\Git\usr\bin\ssh.exe" web"#);
        assert_eq!(host_description(&h), "bob@10.0.0.1:2222");
        let only_alias = SshHost { alias: "x".into(), ..Default::default() };
        assert_eq!(host_description(&only_alias), "");
        let spaced = SshHost { alias: "my host".into(), ..Default::default() };
        assert!(host_to_command_line(exe, &spaced).ends_with(r#" "my host""#));
        let v6 = SshHost { alias: "v6".into(), hostname: Some("::1".into()), port: Some(22), ..Default::default() };
        assert_eq!(host_description(&v6), "::1 port 22");
        assert_eq!(serde_json::to_value(&h).unwrap()["identityFile"], serde_json::Value::Null);
    }
}
