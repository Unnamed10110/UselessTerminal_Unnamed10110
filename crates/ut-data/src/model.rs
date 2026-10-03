//! Session / folder / snippet data model (§8.1), command-line building and environment-text parsing.

use crate::{new_id, DataError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DEFAULT_COLOR: &str = "#00ff44";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Integration {
    Off,
    /// Also the fallback for unknown literals in hand-edited files.
    #[default]
    #[serde(other)]
    Auto,
}

/// A saved shell profile (§8.1). Missing JSON fields take the documented defaults.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Executable; for Windows Terminal imports it may hold a whole command line.
    pub shell_path: String,
    pub arguments: String,
    pub working_directory: String,
    pub starting_command: String,
    pub color_tag: String,
    pub icon_override: String,
    pub folder_id: Option<String>,
    pub sort_order: i32,
    pub theme_background: String,
    pub theme_preset: String,
    /// 0 = global size, otherwise 8..=32.
    pub font_size: i32,
    /// `KEY=VALUE` lines.
    pub environment: String,
    pub integration: Integration,
    pub run_as_admin: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: "New Session".into(),
            description: String::new(),
            shell_path: String::new(),
            arguments: String::new(),
            working_directory: String::new(),
            starting_command: String::new(),
            color_tag: DEFAULT_COLOR.into(),
            icon_override: String::new(),
            folder_id: None,
            sort_order: 0,
            theme_background: String::new(),
            theme_preset: String::new(),
            font_size: 0,
            environment: String::new(),
            integration: Integration::Auto,
            run_as_admin: false,
        }
    }
}

impl Session {
    /// The command line to hand to `CreateProcessW` (§8.1 `GetFullCommand`).
    ///
    /// * blank `arguments`: `shellPath` trimmed, quoted only when it has a space, is not already
    ///   quoted and the whole string is an existing file. A string that is not a file is a full
    ///   command line (the Windows Terminal import case) and must stay verbatim.
    /// * otherwise: `"<shellPath>" <arguments>` (an already-quoted path is not quoted twice).
    pub fn full_command(&self) -> String {
        let path = self.shell_path.trim();
        let quoted = path.starts_with('"');
        if self.arguments.trim().is_empty() {
            if path.contains(' ') && !quoted && Path::new(path).is_file() {
                format!("\"{path}\"")
            } else {
                path.to_string()
            }
        } else if quoted {
            format!("{path} {}", self.arguments)
        } else {
            format!("\"{path}\" {}", self.arguments)
        }
    }

    /// Copy of every field with a fresh id (§24 #18). The name is kept; the UI may rename.
    pub fn duplicate(&self) -> Session {
        Session { id: new_id(), ..self.clone() }
    }

    /// Name and shell path are required (§8.3).
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(DataError::Invalid("Name is required.".into()));
        }
        if self.shell_path.trim().is_empty() {
            return Err(DataError::Invalid("Shell path is required.".into()));
        }
        Ok(())
    }

    /// Normalise dialog input: font size outside 8..=32 becomes 0, a blank colour becomes the default.
    pub fn sanitize(&mut self) {
        if self.font_size != 0 && !(8..=32).contains(&self.font_size) {
            self.font_size = 0;
        }
        if self.color_tag.trim().is_empty() {
            self.color_tag = DEFAULT_COLOR.into();
        }
    }

    /// Environment overrides with `%VAR%` expanded against `base` (§8.1 [P1]).
    pub fn resolved_env(&self, base: &[(String, String)]) -> Vec<(String, String)> {
        expand_env_values(parse_env(&self.environment), base)
    }

    /// Working directory with `%VAR%` / `~` expanded; blank means `%USERPROFILE%`.
    pub fn resolved_cwd(&self, env: &[(String, String)]) -> String {
        match self.working_directory.trim() {
            "" => ut_fs::home_dir().to_string_lossy().into_owned(),
            w => expand_vars(w, env),
        }
    }
}

/// Root-level folder (§8.1): one level of nesting.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub sort_order: i32,
}

impl Default for Folder {
    fn default() -> Self {
        Self { id: new_id(), name: "New Folder".into(), sort_order: 0 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Snippet {
    pub id: String,
    pub name: String,
    pub command: String,
    pub append_enter: bool,
    pub sort_order: i32,
}

impl Default for Snippet {
    fn default() -> Self {
        Self { id: new_id(), name: String::new(), command: String::new(), append_enter: true, sort_order: 0 }
    }
}

// ------------------------------------------------------------------ environment text

/// Parse `KEY=VALUE` lines (§8.1). Lines are split on `\n` with `\r` trimmed; empty lines and lines whose
/// first `=` is at index <= 0 are skipped (a blank key after trimming too, since Windows rejects it).
/// Keys are case-insensitive: first-seen position and spelling, last value wins.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        let Some(eq) = line.find('=').filter(|&i| i > 0) else { continue };
        let (key, value) = (line[..eq].trim(), line[eq + 1..].trim());
        if key.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(k, _)| ieq(k, key)) {
            Some(e) => e.1 = value.to_string(),
            None => out.push((key.to_string(), value.to_string())),
        }
    }
    out
}

/// Expand `%VAR%` in each value against the earlier entries of `pairs` first, then `base`
/// (so `PATH=%PATH%;C:\tools` appends to the inherited PATH). Unknown variables are left as written.
pub fn expand_env_values(pairs: Vec<(String, String)>, base: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        let v = expand_with(&v, |n| env_get(&out, n).or_else(|| env_get(base, n)).map(String::from));
        out.push((k, v));
    }
    out
}

/// A problem on one line of the environment editor (§8.3 [P1]); `line` is 1-based.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvProblem {
    pub line: usize,
    pub message: String,
}

/// Per-line problems for the UI. Parsing itself never fails: these lines are simply ignored by `parse_env`.
pub fn validate_env(text: &str) -> Vec<EnvProblem> {
    let mut problems = Vec::new();
    let mut seen: Vec<(&str, usize)> = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        let (n, line) = (i + 1, line.trim_end_matches('\r'));
        if line.trim().is_empty() {
            continue;
        }
        let mut problem = |m: &str| problems.push(EnvProblem { line: n, message: m.into() });
        match line.find('=') {
            None => problem("Expected KEY=VALUE (no '=' found); this line is ignored."),
            Some(eq) if eq == 0 || line[..eq].trim().is_empty() => problem("Missing variable name before '='; this line is ignored."),
            Some(eq) => {
                let key = line[..eq].trim();
                match seen.iter().find(|(k, _)| ieq(k, key)) {
                    Some((_, first)) => problem(&format!("Duplicate of line {first}; the last value wins.")),
                    None => seen.push((key, n)),
                }
            }
        }
    }
    problems
}

// ------------------------------------------------------------------ variable expansion

fn ieq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_uppercase() == b.to_uppercase()
}

/// Case-insensitive lookup; the last entry wins in unsorted lists.
pub fn env_get<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().rev().find(|(k, _)| ieq(k, name)).map(|(_, v)| v.as_str())
}

pub(crate) fn expand_with(s: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%').filter(|&j| j > 0).and_then(|j| lookup(&after[..j]).map(|v| (v, j))) {
            Some((v, j)) => {
                out.push_str(&v);
                rest = &after[j + 1..];
            }
            // Unknown or empty name: keep the '%' literally and rescan, so `100% of %HOME%` still works.
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Expand `%VAR%` references (case-insensitive; unknown ones stay as written).
pub fn expand_percent(s: &str, env: &[(String, String)]) -> String {
    expand_with(s, |n| env_get(env, n).map(String::from))
}

/// Expand a leading `~` (alone or followed by `/` or `\`) to the home directory, then `%VAR%` (§8.1 [P1]).
pub fn expand_vars(s: &str, env: &[(String, String)]) -> String {
    match s.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            let home = env_get(env, "USERPROFILE")
                .map_or_else(|| ut_fs::home_dir().to_string_lossy().into_owned(), String::from);
            home + &expand_percent(rest, env)
        }
        _ => expand_percent(s, env),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn sess(path: &str, args: &str) -> Session {
        Session { shell_path: path.into(), arguments: args.into(), ..Default::default() }
    }

    /// A real file under a directory with spaces, like the user's OneDrive tree (§23.29).
    fn spaced_file(tag: &str) -> String {
        let d = std::env::temp_dir()
            .join(format!("ut-data-model-{tag}-{}", std::process::id()))
            .join("OneDrive - BEPSA DEL PARAGUAY SAECA");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("tool.exe");
        std::fs::write(&f, b"x").unwrap();
        f.to_string_lossy().into_owned()
    }

    #[test]
    fn defaults_and_serde() {
        let s: Session = serde_json::from_str("{}").unwrap();
        assert_eq!(s.id.len(), 32);
        assert!(s.id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!((s.name.as_str(), s.color_tag.as_str(), s.integration, s.font_size), ("New Session", "#00ff44", Integration::Auto, 0));
        let j = serde_json::to_value(&s).unwrap();
        for k in ["shellPath", "workingDirectory", "startingCommand", "colorTag", "iconOverride", "folderId", "sortOrder",
                  "themeBackground", "themePreset", "fontSize", "environment", "integration", "runAsAdmin"] {
            assert!(j.get(k).is_some(), "missing {k}");
        }
        assert_eq!(j["integration"], "auto");
        assert!(j["folderId"].is_null());
        let off: Session = serde_json::from_str(r#"{"integration":"off","runAsAdmin":true}"#).unwrap();
        assert_eq!(off.integration, Integration::Off);
        assert!(off.run_as_admin);
        let odd: Session = serde_json::from_str(r#"{"integration":"banana"}"#).unwrap();
        assert_eq!(odd.integration, Integration::Auto);
        assert!(Snippet::default().append_enter);
        assert_eq!(Folder::default().name, "New Folder");
    }

    #[test]
    fn full_command_blank_args() {
        // WT import: whole command line stays verbatim (§24 #19).
        assert_eq!(sess("wsl.exe -d Ubuntu", "").full_command(), "wsl.exe -d Ubuntu");
        assert_eq!(sess(r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo"#, "").full_command(), r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo"#);
        assert_eq!(sess("  pwsh.exe  ", "  ").full_command(), "pwsh.exe");
        assert_eq!(sess("", "").full_command(), "");
        // Not a file and not quoted: left alone (it is a command line, not a path).
        assert_eq!(sess(r"C:\no such dir\tool.exe", "").full_command(), r"C:\no such dir\tool.exe");
    }

    #[test]
    fn full_command_quotes_existing_spaced_file() {
        let f = spaced_file("quote");
        assert!(f.contains("OneDrive - BEPSA DEL PARAGUAY SAECA"));
        assert_eq!(sess(&f, "").full_command(), format!("\"{f}\""));
        assert_eq!(sess(&format!("\"{f}\""), "").full_command(), format!("\"{f}\""));
        assert_eq!(sess(&format!("  {f} "), "").full_command(), format!("\"{f}\""));
    }

    #[test]
    fn full_command_with_args() {
        let p = r"C:\Users\x\OneDrive - BEPSA DEL PARAGUAY SAECA\tool.exe";
        assert_eq!(sess(p, "-a b").full_command(), format!("\"{p}\" -a b"));
        assert_eq!(sess("pwsh.exe", "-NoLogo").full_command(), r#""pwsh.exe" -NoLogo"#);
        assert_eq!(sess(&format!("\"{p}\""), "-a").full_command(), format!("\"{p}\" -a"));
    }

    #[test]
    fn duplicate_copies_every_field() {
        let s = Session {
            id: "a".repeat(32),
            name: "n".into(),
            description: "d".into(),
            shell_path: "p".into(),
            arguments: "a".into(),
            working_directory: "w".into(),
            starting_command: "c".into(),
            color_tag: "#123456".into(),
            icon_override: "i.ico".into(),
            folder_id: Some("f".into()),
            sort_order: 7,
            theme_background: "#000001".into(),
            theme_preset: "Dracula".into(),
            font_size: 14,
            environment: "A=1\nB=2".into(),
            integration: Integration::Off,
            run_as_admin: true,
        };
        let d = s.duplicate();
        assert_ne!(d.id, s.id);
        assert_eq!(d.id.len(), 32);
        assert_eq!(Session { id: s.id.clone(), ..d }, s);
    }

    #[test]
    fn validate_and_sanitize() {
        assert!(sess("", "").validate().is_err());
        assert!(Session { name: " ".into(), ..sess("x", "") }.validate().is_err());
        assert!(sess("x", "").validate().is_ok());
        let mut s = Session { font_size: 99, color_tag: " ".into(), ..sess("x", "") };
        s.sanitize();
        assert_eq!((s.font_size, s.color_tag.as_str()), (0, "#00ff44"));
        for ok in [0, 8, 32] {
            let mut s = Session { font_size: ok, ..sess("x", "") };
            s.sanitize();
            assert_eq!(s.font_size, ok);
        }
        // A colour outside the swatch list is kept as is (§24 #17).
        let mut s = Session { color_tag: "#123abc".into(), ..sess("x", "") };
        s.sanitize();
        assert_eq!(s.color_tag, "#123abc");
    }

    #[test]
    fn parse_env_rules() {
        let t = "A=1\r\n\r\n=bad\n noeq\n  b =  two words  \nA=3\n x= \n =\n  =z\nPath=C:\\a=b\nPATH=last";
        assert_eq!(
            parse_env(t),
            kv(&[("A", "3"), ("b", "two words"), ("x", ""), ("Path", "last")])
        );
        // case-insensitive duplicate: first position/spelling, last value
        assert_eq!(parse_env("Foo=1\nBAR=2\nfoo=3"), kv(&[("Foo", "3"), ("BAR", "2")]));
        assert!(parse_env("").is_empty());
        assert_eq!(parse_env("K=a=b"), kv(&[("K", "a=b")]));
    }

    #[test]
    fn expand_rules() {
        let env = kv(&[("USERPROFILE", r"C:\Users\x y"), ("Path", r"C:\bin")]);
        assert_eq!(expand_percent("%userprofile%\\src", &env), r"C:\Users\x y\src");
        assert_eq!(expand_percent("%NOPE%\\a", &env), "%NOPE%\\a");
        assert_eq!(expand_percent("100% of %PATH%", &env), r"100% of C:\bin");
        assert_eq!(expand_percent("%%", &env), "%%");
        assert_eq!(expand_percent("a%", &env), "a%");
        assert_eq!(expand_vars("~", &env), r"C:\Users\x y");
        assert_eq!(expand_vars("~/src", &env), r"C:\Users\x y/src");
        assert_eq!(expand_vars(r"~\%PATH%", &env), r"C:\Users\x y\C:\bin");
        assert_eq!(expand_vars("~foo", &env), "~foo");
        assert_eq!(expand_vars("a~", &env), "a~");
        // no USERPROFILE in env: falls back to the real home
        assert_eq!(expand_vars("~", &[]), ut_fs::home_dir().to_string_lossy());
    }

    #[test]
    fn env_values_expand_against_base_and_earlier_entries() {
        let base = kv(&[("PATH", r"C:\bin"), ("HOME", "h")]);
        let out = expand_env_values(parse_env("PATH=%PATH%;C:\\tools\nX=%path%|%HOME%\nY=%PATH%\nZ=%UNKNOWN%"), &base);
        assert_eq!(
            out,
            kv(&[("PATH", r"C:\bin;C:\tools"), ("X", r"C:\bin;C:\tools|h"), ("Y", r"C:\bin;C:\tools"), ("Z", "%UNKNOWN%")])
        );
        let s = Session { environment: "A=%HOME%".into(), ..Default::default() };
        assert_eq!(s.resolved_env(&base), kv(&[("A", "h")]));
    }

    #[test]
    fn resolved_cwd() {
        let env = kv(&[("USERPROFILE", r"C:\Users\x")]);
        let mut s = Session::default();
        assert_eq!(s.resolved_cwd(&env), ut_fs::home_dir().to_string_lossy());
        s.working_directory = r"  %USERPROFILE%\repos  ".into();
        assert_eq!(s.resolved_cwd(&env), r"C:\Users\x\repos");
        s.working_directory = "~/p".into();
        assert_eq!(s.resolved_cwd(&env), r"C:\Users\x/p");
    }

    #[test]
    fn validate_env_reports_lines() {
        let p = validate_env("A=1\n\nnoeq\n=x\nB=2\r\na=3\n  =y");
        let lines: Vec<usize> = p.iter().map(|p| p.line).collect();
        assert_eq!(lines, vec![3, 4, 6, 7]);
        assert!(p[2].message.contains("line 1"));
        assert!(validate_env("A=1\nB=\n").is_empty());
    }
}
