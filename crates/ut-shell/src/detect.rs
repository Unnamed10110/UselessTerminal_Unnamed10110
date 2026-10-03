//! Shell detection, default profile and executable resolution (§9.1, §9.2).
//! Never spawns a process: WSL distros and Git for Windows come from the registry.

use crate::kind::{file_stem_lower, is_file, split_exe_and_args, ShellKind};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellProfile {
    /// Stable id: `pwsh`, `powershell`, `cmd`, `wsl`, `wsl:<distro>`, `gitbash`.
    pub id: String,
    pub name: String,
    pub path: String,
    pub arguments: String,
    pub color: String,
    pub kind: ShellKind,
    pub is_default: bool,
    /// Executable (quoted if it contains a space) followed by ` {arguments}`.
    pub command: String,
}

/// `"path" args`, quoting the path only when it contains whitespace (§9.1).
pub fn profile_command(path: &str, args: &str) -> String {
    let exe = if path.contains(char::is_whitespace) { format!("\"{path}\"") } else { path.to_string() };
    if args.is_empty() { exe } else { format!("{exe} {args}") }
}

/// A WSL distro from `HKCU\...\Lxss`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Distro {
    pub name: String,
    pub is_default: bool,
}

type Var<'a> = &'a dyn Fn(&str) -> Option<String>;

fn real_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Detects the installed shells, in §9.1 order. Blocking (a few `stat`s and registry reads):
/// run it off the UI thread, normally through [`ShellCache`].
pub fn detect_shells() -> Vec<ShellProfile> {
    detect_in(&real_var, &reg::wsl_distros(), reg::git_install_path().as_deref())
}

pub(crate) fn detect_in(var: Var, distros: &[Distro], git_install: Option<&str>) -> Vec<ShellProfile> {
    let get = |k: &str| var(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let sysroot = get("SystemRoot").or_else(|| get("windir")).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let sys32 = sysroot.join("System32");
    let pf = get("ProgramFiles");
    let sub = |base: Option<PathBuf>, parts: &[&str]| {
        base.map(|b| parts.iter().fold(b, |p, s| p.join(s)))
    };
    let profile = |id: &str, name: &str, path: &Path, args: &str, color: &str, kind| {
        let path = path.to_string_lossy().into_owned();
        ShellProfile {
            id: id.into(),
            name: name.into(),
            command: profile_command(&path, args),
            path,
            arguments: args.into(),
            color: color.into(),
            kind,
            is_default: false,
        }
    };
    let mut out = Vec::new();

    // 1. PowerShell 7: PATH first, then the well-known installs.
    let pwsh = path_dirs(var)
        .into_iter()
        .map(|d| d.join("pwsh.exe"))
        .chain([
            sub(pf.clone(), &["PowerShell", "7", "pwsh.exe"]),
            sub(pf.clone(), &["PowerShell", "7-preview", "pwsh.exe"]),
            sub(get("LOCALAPPDATA"), &["Microsoft", "WindowsApps", "pwsh.exe"]),
            sub(get("USERPROFILE"), &[".dotnet", "tools", "pwsh.exe"]),
        ].into_iter().flatten())
        .find(|p| is_file(p));
    let has_pwsh = pwsh.is_some();
    if let Some(p) = pwsh {
        out.push(profile("pwsh", "PowerShell", &p, "", "#00e5ff", ShellKind::PowerShell));
    }
    // 2. Windows PowerShell
    let ps5 = sys32.join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    if is_file(&ps5) {
        out.push(profile("powershell", "Windows PowerShell", &ps5, "", "#00e5ff", ShellKind::PowerShell));
    }
    // 3. cmd
    let cmd = sys32.join("cmd.exe");
    if is_file(&cmd) {
        out.push(profile("cmd", "Command Prompt", &cmd, "", "#ffff00", ShellKind::Cmd));
    }
    // 4. WSL: generic + one per distro (default distro first, docker-desktop hidden)
    let wsl = sys32.join("wsl.exe");
    if is_file(&wsl) {
        out.push(profile("wsl", "WSL", &wsl, "", "#ff8800", ShellKind::Wsl));
        let mut ds: Vec<&Distro> = distros
            .iter()
            .filter(|d| !matches!(d.name.to_lowercase().as_str(), "docker-desktop" | "docker-desktop-data"))
            .collect();
        ds.sort_by_key(|d| (!d.is_default, d.name.to_lowercase()));
        for d in ds {
            let args = format!("-d \"{}\"", d.name);
            out.push(profile(&format!("wsl:{}", d.name), &format!("WSL: {}", d.name), &wsl, &args, "#ff8800", ShellKind::Wsl));
        }
    }
    // 5. Git Bash: registry InstallPath, then Program Files.
    let gitbash = git_install
        .map(|p| PathBuf::from(p).join("bin").join("bash.exe"))
        .into_iter()
        .chain(sub(pf, &["Git", "bin", "bash.exe"]))
        .chain(sub(get("ProgramFiles(x86)"), &["Git", "bin", "bash.exe"]))
        .find(|p| is_file(p));
    if let Some(p) = gitbash {
        out.push(profile("gitbash", "Git Bash", &p, "--login -i", "#ff003c", ShellKind::Posix));
    }

    // Default: pwsh, else Windows PowerShell; nothing found at all → cmd (§9.1 step 7).
    if out.is_empty() {
        out.push(profile("cmd", "Command Prompt", &cmd, "", "#ffff00", ShellKind::Cmd));
    }
    let default_id = if has_pwsh { "pwsh" } else { "powershell" };
    let i = out.iter().position(|p| p.id == default_id).or_else(|| out.iter().position(|p| p.id == "cmd")).unwrap_or(0);
    out[i].is_default = true;
    out
}

/// `shells.defaultProfile`: `"auto"` (or empty) → the profile flagged default (else the
/// first); anything else → the profile with that id (case-insensitive). An unknown id gives
/// `None`: the caller may look it up among saved sessions or fall back to `"auto"`.
pub fn resolve_default<'a>(setting: &str, profiles: &'a [ShellProfile]) -> Option<&'a ShellProfile> {
    let s = setting.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
        return profiles.iter().find(|p| p.is_default).or(profiles.first());
    }
    profiles.iter().find(|p| p.id.eq_ignore_ascii_case(s))
}

fn path_dirs(var: Var) -> Vec<PathBuf> {
    var("PATH")
        .map(|p| std::env::split_paths(&OsString::from(p)).filter(|d| !d.as_os_str().is_empty()).collect())
        .unwrap_or_default()
}

/// §9.2: resolves the executable of a command line to an existing file.
pub fn resolve_exe(cmdline: &str) -> Option<PathBuf> {
    resolve_exe_in(cmdline, &real_var, &std::env::current_dir().unwrap_or_default())
}

/// Tries, in order: the exact path, the current directory, `System32`,
/// `System32\WindowsPowerShell\v1.0` (powershell only), then each `PATH` entry; every
/// location with the name as given and then with each `PATHEXT` extension. A name with a
/// directory part is only tried exactly and under the current directory.
pub(crate) fn resolve_exe_in(cmdline: &str, var: Var, cwd: &Path) -> Option<PathBuf> {
    let (name, _) = split_exe_and_args(cmdline);
    if name.is_empty() {
        return None;
    }
    let pathext = var("PATHEXT").filter(|v| !v.is_empty()).unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    let exts: Vec<&str> = pathext.split(';').filter(|e| !e.is_empty()).collect();
    let hit = |base: PathBuf| -> Option<PathBuf> {
        if is_file(&base) {
            return Some(base);
        }
        exts.iter().find_map(|e| {
            let mut s = base.clone().into_os_string();
            s.push(e.to_lowercase());
            let p = PathBuf::from(s);
            is_file(&p).then_some(p)
        })
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if !name.contains(['\\', '/', ':']) {
        let sys32 = var("SystemRoot").map(|r| PathBuf::from(r).join("System32")).unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32"));
        if file_stem_lower(&name) == "powershell" {
            dirs.push(sys32.join("WindowsPowerShell").join("v1.0"));
        }
        dirs.insert(0, sys32);
        dirs.extend(path_dirs(var));
    }
    hit(PathBuf::from(&name))
        .or_else(|| hit(cwd.join(&name)))
        .or_else(|| dirs.into_iter().find_map(|d| hit(d.join(&name))))
}

/// Cached detection result: re-detects only when forced or older than 60 s (§9.1).
/// Holds its lock while detecting, so concurrent callers share one run.
pub struct ShellCache {
    inner: Mutex<Option<(Instant, Vec<ShellProfile>)>>,
}

impl ShellCache {
    pub const MAX_AGE: Duration = Duration::from_secs(60);

    pub const fn new() -> Self {
        Self { inner: Mutex::new(None) }
    }

    pub fn get(&self, force: bool) -> Vec<ShellProfile> {
        self.get_with(force, Instant::now(), detect_shells)
    }

    pub(crate) fn get_with(&self, force: bool, now: Instant, detect: impl FnOnce() -> Vec<ShellProfile>) -> Vec<ShellProfile> {
        let mut g = self.inner.lock();
        match &*g {
            Some((at, v)) if !force && now.saturating_duration_since(*at) <= Self::MAX_AGE => v.clone(),
            _ => {
                let v = detect();
                *g = Some((now, v.clone()));
                v
            }
        }
    }
}

impl Default for ShellCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
mod reg {
    use super::Distro;
    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::*;

    const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

    fn get_string(root: HKEY, subkey: &str, name: &str) -> Option<String> {
        let (sk, nm) = (HSTRING::from(subkey), HSTRING::from(name));
        let mut bytes = 0u32;
        unsafe {
            if RegGetValueW(root, &sk, &nm, RRF_RT_REG_SZ, None, None, Some(&mut bytes)) != ERROR_SUCCESS {
                return None;
            }
            let mut buf = vec![0u16; bytes as usize / 2 + 1];
            bytes = (buf.len() * 2) as u32;
            if RegGetValueW(root, &sk, &nm, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes)) != ERROR_SUCCESS {
                return None;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            Some(String::from_utf16_lossy(&buf[..end]))
        }
    }

    fn subkeys(root: HKEY, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(root, &HSTRING::from(path), None, KEY_READ, &mut key) != ERROR_SUCCESS {
                return out;
            }
            for i in 0.. {
                let mut buf = [0u16; 256];
                let mut len = buf.len() as u32;
                if RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) != ERROR_SUCCESS {
                    break;
                }
                out.push(String::from_utf16_lossy(&buf[..len as usize]));
            }
            let _ = RegCloseKey(key);
        }
        out
    }

    /// `HKCU\...\Lxss\{GUID}\DistributionName`; `DefaultDistribution` names the default GUID.
    pub fn wsl_distros() -> Vec<Distro> {
        let default = get_string(HKEY_CURRENT_USER, LXSS, "DefaultDistribution").unwrap_or_default();
        subkeys(HKEY_CURRENT_USER, LXSS)
            .into_iter()
            .filter_map(|guid| {
                let name = get_string(HKEY_CURRENT_USER, &format!(r"{LXSS}\{guid}"), "DistributionName")?;
                Some(Distro { is_default: guid.eq_ignore_ascii_case(&default), name })
            })
            .collect()
    }

    /// `HKLM\SOFTWARE\GitForWindows\InstallPath` (per-user installs: HKCU).
    pub fn git_install_path() -> Option<String> {
        [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
            .into_iter()
            .find_map(|r| get_string(r, r"SOFTWARE\GitForWindows", "InstallPath"))
            .filter(|s| !s.is_empty())
    }
}

#[cfg(not(windows))]
mod reg {
    use super::Distro;
    pub fn wsl_distros() -> Vec<Distro> {
        Vec::new()
    }
    pub fn git_install_path() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    /// A fake Windows tree under a temp dir with spaces, plus an env lookup into it.
    struct Fake {
        root: PathBuf,
        env: HashMap<String, String>,
    }
    impl Fake {
        fn new(tag: &str) -> Fake {
            let root = std::env::temp_dir().join(format!("ut shell detect {tag} {}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let r = |s: &str| s.split('/').fold(root.clone(), |p, c| p.join(c)).to_string_lossy().into_owned();
            let env = [
                ("SYSTEMROOT", r("Windows")),
                ("PROGRAMFILES", r("Program Files")),
                ("PROGRAMFILES(X86)", r("Program Files (x86)")),
                ("LOCALAPPDATA", r("Users/me/AppData/Local")),
                ("USERPROFILE", r("Users/me")),
                ("PATH", format!("{};{}", r("bin one"), r("bin two"))),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
            Fake { root, env }
        }
        fn touch(&self, rel: &str) -> PathBuf {
            let p = rel.split('/').fold(self.root.clone(), |p, s| p.join(s));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"").unwrap();
            p
        }
        fn var(&self) -> impl Fn(&str) -> Option<String> + '_ {
            |k| self.env.get(&k.to_uppercase()).cloned()
        }
        fn detect(&self, distros: &[Distro], git: Option<&str>) -> Vec<ShellProfile> {
            detect_in(&self.var(), distros, git)
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn ids(v: &[ShellProfile]) -> Vec<&str> {
        v.iter().map(|p| p.id.as_str()).collect()
    }
    fn d(name: &str, is_default: bool) -> Distro {
        Distro { name: name.into(), is_default }
    }

    #[test]
    fn nothing_found_falls_back_to_cmd_default() {
        let f = Fake::new("empty");
        let v = f.detect(&[], None);
        assert_eq!(ids(&v), ["cmd"]);
        assert!(v[0].is_default);
        assert_eq!(v[0].kind, ShellKind::Cmd);
        assert_eq!(v[0].color, "#ffff00");
    }

    #[test]
    fn full_set_in_order_with_pwsh_default() {
        let f = Fake::new("full");
        let pwsh = f.touch("Program Files/PowerShell/7/pwsh.exe");
        let ps5 = f.touch("Windows/System32/WindowsPowerShell/v1.0/powershell.exe");
        f.touch("Windows/System32/cmd.exe");
        f.touch("Windows/System32/wsl.exe");
        f.touch("Program Files/Git/bin/bash.exe");
        let v = f.detect(&[d("Ubuntu-22.04", false), d("docker-desktop", false), d("Debian", true), d("docker-desktop-data", false)], None);
        assert_eq!(ids(&v), ["pwsh", "powershell", "cmd", "wsl", "wsl:Debian", "wsl:Ubuntu-22.04", "gitbash"]);
        assert_eq!(v.iter().filter(|p| p.is_default).count(), 1);
        assert!(v[0].is_default);
        // quoted because of the space in "Program Files"
        assert_eq!(v[0].command, format!("\"{}\"", pwsh.display()));
        assert_eq!(v[0].path, pwsh.to_string_lossy());
        assert_eq!(v[0].color, "#00e5ff");
        assert_eq!(v[1].path, ps5.to_string_lossy());
        let wsl_d = &v[5];
        assert_eq!(wsl_d.name, "WSL: Ubuntu-22.04");
        assert_eq!(wsl_d.arguments, "-d \"Ubuntu-22.04\"");
        assert!(wsl_d.command.ends_with(" -d \"Ubuntu-22.04\""), "{}", wsl_d.command);
        assert_eq!(wsl_d.kind, ShellKind::Wsl);
        assert_eq!(wsl_d.color, "#ff8800");
        let gb = &v[6];
        assert_eq!((gb.arguments.as_str(), gb.color.as_str(), gb.kind), ("--login -i", "#ff003c", ShellKind::Posix));
        assert!(gb.command.ends_with("bash.exe\" --login -i"));
        // every command re-detects to the profile's own kind
        for p in &v {
            assert_eq!(crate::detect_kind(&p.command), p.kind, "{}", p.command);
        }
    }

    #[test]
    fn windows_powershell_is_default_without_pwsh() {
        let f = Fake::new("ps5");
        f.touch("Windows/System32/WindowsPowerShell/v1.0/powershell.exe");
        f.touch("Windows/System32/cmd.exe");
        let v = f.detect(&[], None);
        assert_eq!(ids(&v), ["powershell", "cmd"]);
        assert!(v[0].is_default && !v[1].is_default);
    }

    #[test]
    fn cmd_only_is_default_and_no_wsl_without_exe() {
        let f = Fake::new("cmdonly");
        f.touch("Windows/System32/cmd.exe");
        let v = f.detect(&[d("Ubuntu", true)], None);
        assert_eq!(ids(&v), ["cmd"]);
        assert!(v[0].is_default);
    }

    #[test]
    fn pwsh_search_order_path_first() {
        let f = Fake::new("pwshorder");
        f.touch("Program Files/PowerShell/7/pwsh.exe");
        let on_path = f.touch("bin two/pwsh.exe");
        let v = f.detect(&[], None);
        assert_eq!(v[0].path, on_path.to_string_lossy());
        // without PATH: Program Files 7, then 7-preview, then WindowsApps, then dotnet tools
        for rel in [
            "Program Files/PowerShell/7-preview/pwsh.exe",
            "Users/me/AppData/Local/Microsoft/WindowsApps/pwsh.exe",
            "Users/me/.dotnet/tools/pwsh.exe",
        ] {
            let g = Fake::new("pwshorder2");
            let want = g.touch(rel);
            let mut env = g.env.clone();
            env.remove("PATH");
            let v = detect_in(&|k| env.get(&k.to_uppercase()).cloned(), &[], None);
            assert_eq!(v[0].id, "pwsh", "{rel}");
            assert_eq!(v[0].path, want.to_string_lossy());
        }
        let g = Fake::new("pwshorder3");
        let first = g.touch("Program Files/PowerShell/7/pwsh.exe");
        g.touch("Program Files/PowerShell/7-preview/pwsh.exe");
        g.touch("Users/me/.dotnet/tools/pwsh.exe");
        assert_eq!(g.detect(&[], None)[0].path, first.to_string_lossy());
    }

    #[test]
    fn git_bash_registry_path_beats_program_files() {
        let f = Fake::new("git");
        f.touch("Program Files/Git/bin/bash.exe");
        let reg = f.touch("My Git/bin/bash.exe");
        let reg_dir = f.root.join("My Git");
        let v = f.detect(&[], Some(&reg_dir.to_string_lossy()));
        let gb = v.iter().find(|p| p.id == "gitbash").unwrap();
        assert_eq!(gb.path, reg.to_string_lossy());
        // x86 fallback
        let g = Fake::new("git86");
        let x86 = g.touch("Program Files (x86)/Git/bin/bash.exe");
        let v = g.detect(&[], Some(r"C:\does\not\exist"));
        assert_eq!(v.iter().find(|p| p.id == "gitbash").unwrap().path, x86.to_string_lossy());
    }

    #[test]
    fn resolve_default_setting() {
        let f = Fake::new("resolve");
        f.touch("Program Files/PowerShell/7/pwsh.exe");
        f.touch("Windows/System32/cmd.exe");
        f.touch("Windows/System32/wsl.exe");
        let v = f.detect(&[d("Ubuntu", false)], None);
        assert_eq!(resolve_default("auto", &v).unwrap().id, "pwsh");
        assert_eq!(resolve_default("", &v).unwrap().id, "pwsh");
        assert_eq!(resolve_default("AUTO", &v).unwrap().id, "pwsh");
        assert_eq!(resolve_default("cmd", &v).unwrap().id, "cmd");
        assert_eq!(resolve_default("wsl:Ubuntu", &v).unwrap().name, "WSL: Ubuntu");
        assert!(resolve_default("no-such-session-id", &v).is_none());
        assert!(resolve_default("auto", &[]).is_none());
    }

    #[test]
    fn profile_serializes_camel_case() {
        let f = Fake::new("serde");
        let v = f.detect(&[], None);
        let j = serde_json::to_value(&v[0]).unwrap();
        assert_eq!(j["isDefault"], true);
        assert_eq!(j["kind"], "cmd");
        assert!(j.get("is_default").is_none());
    }

    #[test]
    fn resolve_exe_order_and_forms() {
        let f = Fake::new("exe");
        let bin = f.touch("bin one/tool.EXE");
        let sys = f.touch("Windows/System32/cmd.exe");
        let ps = f.touch("Windows/System32/WindowsPowerShell/v1.0/powershell.exe");
        let cwd = f.root.join("work dir");
        let cwd_tool = f.touch("work dir/tool.exe");
        let var = f.var();
        // PATHEXT supplies its own extension casing; Windows paths are case-insensitive
        let norm = |p: Option<PathBuf>| p.map(|p| p.to_string_lossy().to_lowercase());
        let r = |c: &str| norm(resolve_exe_in(c, &var, &cwd));
        let l = |p: &PathBuf| Some(p.to_string_lossy().to_lowercase());

        // cwd beats System32/PATH
        assert_eq!(r("tool -x"), l(&cwd_tool));
        assert_eq!(r("tool.exe"), l(&cwd_tool));
        // System32, PATHEXT applied, quotes trimmed
        assert_eq!(r("cmd /c dir"), l(&sys));
        assert_eq!(r("\"cmd\""), l(&sys));
        assert_eq!(r("CMD.EXE"), l(&sys));
        // powershell: the v1.0 subfolder
        assert_eq!(r("powershell -NoLogo"), l(&ps));
        assert_eq!(r("pwsh"), None);
        // PATH entry with spaces (different cwd so the cwd copy does not win)
        let other = f.root.join("elsewhere");
        assert_eq!(norm(resolve_exe_in("tool", &var, &other)), l(&bin));
        // quoted exact path with spaces + arguments
        let q = format!("\"{}\" --login -i", f.root.join("work dir").join("tool.exe").display());
        assert_eq!(r(&q), l(&f.root.join("work dir").join("tool.exe")));
        // unquoted spaced path resolves by adding words until a file exists
        let spaced = f.touch("My Apps - X/sub dir/app");
        let u = format!("{} --flag", spaced.display());
        assert_eq!(r(&u), l(&spaced));
        // names with a directory part do not search PATH
        assert_eq!(resolve_exe_in(r"sub\tool", &var, &other), None);
        assert_eq!(r(""), None);
        assert_eq!(r("nope.exe"), None);
    }

    #[test]
    fn cache_ttl_and_force() {
        let c = ShellCache::new();
        let t0 = Instant::now();
        let mut runs = 0;
        let mk = |n: &mut i32| {
            *n += 1;
            vec![ShellProfile {
                id: format!("run{n}"),
                name: String::new(),
                path: String::new(),
                arguments: String::new(),
                color: String::new(),
                kind: ShellKind::Unknown,
                is_default: true,
                command: String::new(),
            }]
        };
        assert_eq!(c.get_with(false, t0, || mk(&mut runs))[0].id, "run1");
        assert_eq!(c.get_with(false, t0 + Duration::from_secs(59), || mk(&mut runs))[0].id, "run1");
        assert_eq!(c.get_with(false, t0 + Duration::from_secs(60), || mk(&mut runs))[0].id, "run1");
        assert_eq!(c.get_with(false, t0 + Duration::from_secs(61), || mk(&mut runs))[0].id, "run2");
        // the clock restarts at the re-detection
        assert_eq!(c.get_with(false, t0 + Duration::from_secs(100), || mk(&mut runs))[0].id, "run2");
        assert_eq!(c.get_with(true, t0 + Duration::from_secs(101), || mk(&mut runs))[0].id, "run3");
        assert_eq!(runs, 3);
    }

    #[cfg(windows)]
    #[test]
    fn real_system_smoke() {
        // Only invariants that hold on every Windows machine.
        let v = detect_shells();
        assert!(!v.is_empty());
        assert_eq!(v.iter().filter(|p| p.is_default).count(), 1);
        assert!(v.iter().all(|p| !p.id.is_empty() && !p.command.is_empty()));
        assert!(resolve_exe("cmd").is_some());
    }
}
