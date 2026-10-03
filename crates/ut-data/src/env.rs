//! Child-process environment layering (§4.3). The UTF-16 block serialisation lives in `ut-pty`.

use crate::model::expand_with;
use std::collections::BTreeMap;

type Kv = (String, String);

/// Layer 1 of §4.3. Without `TERM`, remote shells reached over SSH suppress all colour.
pub fn default_env(version: &str) -> Vec<Kv> {
    [("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("TERM_PROGRAM", "UselessTerminal"), ("TERM_PROGRAM_VERSION", version)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The current process environment (non-Unicode entries are skipped).
pub fn process_env() -> Vec<Kv> {
    std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect()
}

/// Merge layers, later ones overriding earlier ones (§4.3): defaults, parent environment, shell-integration
/// variables, per-session overrides (these always win, even over `TERM`). Keys are case-insensitive and the
/// winning layer's spelling is kept. The result is sorted case-insensitively, as a Windows environment block requires.
pub fn layer_env(defaults: &[Kv], parent: &[Kv], integration: &[Kv], session: &[Kv]) -> Vec<Kv> {
    let mut m: BTreeMap<String, Kv> = BTreeMap::new();
    for (k, v) in defaults.iter().chain(parent).chain(integration).chain(session).filter(|(k, _)| !k.is_empty()) {
        m.insert(k.to_uppercase(), (k.clone(), v.clone()));
    }
    m.into_values().collect()
}

/// One value read from an `Environment` registry key.
#[cfg(any(windows, test))]
pub(crate) struct RegValue {
    pub name: String,
    pub value: String,
    /// `REG_EXPAND_SZ`: contains `%VAR%` references.
    pub expand: bool,
}

/// Overlay registry environments on the process environment: machine values, then user values
/// (REG_EXPAND_SZ expanded against what is known so far); `PATH` becomes `machine;user`.
#[cfg(any(windows, test))]
pub(crate) fn merge_registry_env(process: Vec<Kv>, machine: Vec<RegValue>, user: Vec<RegValue>) -> Vec<Kv> {
    let mut m: BTreeMap<String, Kv> = process.into_iter().map(|(k, v)| (k.to_uppercase(), (k, v))).collect();
    let mut path: Vec<String> = Vec::new();
    for rv in machine.into_iter().chain(user) {
        let value = if rv.expand { expand_with(&rv.value, |n| m.get(&n.to_uppercase()).map(|e| e.1.clone())) } else { rv.value };
        if rv.name.eq_ignore_ascii_case("PATH") {
            path.push(value.trim_matches(';').to_string());
        } else {
            m.insert(rv.name.to_uppercase(), (rv.name, value));
        }
    }
    path.retain(|p| !p.is_empty());
    if !path.is_empty() {
        let key = m.get("PATH").map_or_else(|| "Path".to_string(), |e| e.0.clone());
        m.insert("PATH".into(), (key, path.join(";")));
    }
    m.into_values().collect()
}

/// [P1] Rebuild the parent environment from `HKCU\Environment` and
/// `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` over the process environment, so new
/// tabs see PATH changes without restarting the app (`terminal.refreshEnvironment`). Unreadable keys are skipped.
#[cfg(windows)]
pub fn fresh_parent_env() -> Vec<Kv> {
    use windows::core::w;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let machine = read_registry(HKEY_LOCAL_MACHINE, w!("SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment"));
    let user = read_registry(HKEY_CURRENT_USER, w!("Environment"));
    merge_registry_env(process_env(), machine, user)
}

#[cfg(windows)]
fn read_registry(root: windows::Win32::System::Registry::HKEY, sub: windows::core::PCWSTR) -> Vec<RegValue> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegCloseKey, RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW, HKEY, KEY_READ, REG_EXPAND_SZ, REG_SZ};
    let mut out = Vec::new();
    // SAFETY: plain registry reads into buffers sized from RegQueryInfoKeyW; every length is passed back in.
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(root, sub, None, KEY_READ, &mut key) != ERROR_SUCCESS {
            return out;
        }
        let (mut count, mut max_name, mut max_data) = (0u32, 0u32, 0u32);
        let info = RegQueryInfoKeyW(key, None, None, None, None, None, None, Some(&mut count), Some(&mut max_name), Some(&mut max_data), None, None);
        if info == ERROR_SUCCESS {
            let mut name = vec![0u16; max_name as usize + 1];
            let mut data = vec![0u8; max_data as usize + 2];
            for i in 0..count {
                let (mut name_len, mut data_len, mut ty) = (name.len() as u32, data.len() as u32, 0u32);
                let r = RegEnumValueW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut name_len, None, Some(&mut ty), Some(data.as_mut_ptr()), Some(&mut data_len));
                if r != ERROR_SUCCESS || name_len == 0 || (ty != REG_SZ.0 && ty != REG_EXPAND_SZ.0) {
                    continue;
                }
                let wide: Vec<u16> = data[..data_len as usize].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
                out.push(RegValue {
                    name: String::from_utf16_lossy(&name[..name_len as usize]),
                    value: String::from_utf16_lossy(&wide).trim_end_matches('\0').to_string(),
                    expand: ty == REG_EXPAND_SZ.0,
                });
            }
        }
        let _ = RegCloseKey(key);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(p: &[(&str, &str)]) -> Vec<Kv> {
        p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn get<'a>(e: &'a [Kv], k: &str) -> Option<&'a str> {
        e.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    fn rv(name: &str, value: &str, expand: bool) -> RegValue {
        RegValue { name: name.into(), value: value.into(), expand }
    }

    #[test]
    fn term_is_always_present_unless_overridden() {
        let e = layer_env(&default_env("1.2.3"), &[], &[], &[]);
        assert_eq!(get(&e, "TERM"), Some("xterm-256color"));
        assert_eq!(get(&e, "COLORTERM"), Some("truecolor"));
        assert_eq!(get(&e, "TERM_PROGRAM"), Some("UselessTerminal"));
        assert_eq!(get(&e, "TERM_PROGRAM_VERSION"), Some("1.2.3"));
        // an inherited TERM beats the default, a session TERM beats everything
        let e = layer_env(&default_env("1"), &kv(&[("TERM", "vt100")]), &[], &[]);
        assert_eq!(get(&e, "TERM"), Some("vt100"));
        let e = layer_env(&default_env("1"), &kv(&[("TERM", "vt100")]), &kv(&[("TERM", "dumb")]), &kv(&[("TERM", "screen")]));
        assert_eq!(get(&e, "TERM"), Some("screen"));
    }

    #[test]
    fn precedence_and_case_insensitivity() {
        let e = layer_env(
            &kv(&[("A", "d"), ("B", "d"), ("C", "d"), ("D", "d")]),
            &kv(&[("a", "p"), ("B", "p"), ("C", "p")]),
            &kv(&[("b", "i"), ("C", "i")]),
            &kv(&[("c", "s")]),
        );
        // winner's spelling and value; one entry per case-insensitive key
        assert_eq!(e.iter().filter(|(k, _)| k.eq_ignore_ascii_case("a")).count(), 1);
        assert_eq!(get(&e, "a"), Some("p"));
        assert_eq!(get(&e, "b"), Some("i"));
        assert_eq!(get(&e, "c"), Some("s"));
        assert_eq!(get(&e, "D"), Some("d"));
        assert_eq!(get(&e, "A"), None);
    }

    #[test]
    fn sorted_case_insensitively_and_skips_empty_keys() {
        let e = layer_env(&[], &kv(&[("b", "1"), ("A", "2"), ("c", "3"), ("", "x")]), &[], &[]);
        let keys: Vec<&str> = e.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["A", "b", "c"]);
    }

    #[test]
    fn registry_merge_expands_and_joins_path() {
        let process = kv(&[("SystemRoot", r"C:\Windows"), ("Path", r"C:\stale"), ("USERNAME", "u"), ("USERPROFILE", r"C:\Users\u")]);
        let machine = vec![rv("Path", r"%SystemRoot%\system32;C:\m;", true), rv("FOO", "machine", false), rv("EXP", r"%SystemRoot%\x", true)];
        let user = vec![rv("PATH", r"%USERPROFILE%\bin", true), rv("foo", "user", false), rv("NEW", "1", false)];
        let e = layer_env(&[], &merge_registry_env(process, machine, user), &[], &[]);
        assert_eq!(get(&e, "Path"), Some(r"C:\Windows\system32;C:\m;C:\Users\u\bin"));
        assert_eq!(get(&e, "foo"), Some("user"));
        assert_eq!(get(&e, "EXP"), Some(r"C:\Windows\x"));
        assert_eq!(get(&e, "NEW"), Some("1"));
        assert_eq!(get(&e, "USERNAME"), Some("u")); // process-only variables survive
        // no registry PATH: the process PATH stays
        let e = merge_registry_env(kv(&[("Path", "keep")]), vec![], vec![rv("X", "1", false)]);
        assert_eq!(get(&e, "Path"), Some("keep"));
    }

    #[cfg(windows)]
    #[test]
    fn fresh_parent_env_reads_the_registry() {
        let e = fresh_parent_env();
        let path = e.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")).map(|(_, v)| v.clone()).unwrap_or_default();
        assert!(path.contains(';') && !path.contains(";;"), "PATH = {path}");
        assert!(e.iter().any(|(k, _)| k.eq_ignore_ascii_case("SystemRoot")));
        assert!(e.iter().any(|(k, _)| k.eq_ignore_ascii_case("TEMP")));
    }
}
