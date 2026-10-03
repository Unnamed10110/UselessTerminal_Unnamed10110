//! OS-facing helpers: opening links and files (§5.10, §18.2), elevated launches.

use std::path::{Path, PathBuf};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// ShellExecute "open" — for URLs and for files the user explicitly asked to open.
pub fn shell_open(target: &str) -> Result<(), String> {
    let r = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    if r.0 as usize > 32 { Ok(()) } else { Err(format!("Could not open {target} (code {})", r.0 as usize)) }
}

fn explorer(arg: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer.exe").raw_arg(arg).spawn();
}

const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

pub fn url_scheme(url: &str) -> Option<String> {
    let scheme = url.split(':').next()?.to_ascii_lowercase();
    let plain = url.contains(':') && !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    (plain && !url.chars().any(|c| c.is_control())).then_some(scheme)
}

/// Whether a URL may be opened without asking: only http/https/mailto (§18.2).
pub fn is_safe_url(url: &str) -> bool {
    url_scheme(url).is_some_and(|s| ALLOWED_SCHEMES.contains(&s.as_str()))
}

const EXECUTABLE_EXTS: [&str; 14] = ["exe", "bat", "cmd", "ps1", "vbs", "lnk", "com", "msi", "scr", "js", "jse", "wsf", "hta", "reg"];

/// Open a clicked file path (§5.10): folders → Explorer; missing file → its parent; executables are never run
/// (the parent folder opens with the file selected); anything else → the editor, else the default app.
pub fn open_path(path: &str, line: Option<u32>, cwd: Option<&str>, editor_template: &str) -> Result<(), String> {
    let mut p = PathBuf::from(path.trim().trim_matches('"'));
    if p.is_relative() {
        if let Some(c) = cwd.filter(|c| Path::new(c).is_dir()) {
            p = Path::new(c).join(&p);
        }
    }
    let s = p.to_string_lossy().into_owned();
    if s.contains('"') || s.contains('\n') {
        return Err("Unsupported path.".into());
    }
    if p.is_dir() {
        explorer(&format!("\"{s}\""));
        return Ok(());
    }
    if !p.exists() {
        let parent = p.parent().filter(|d| d.is_dir()).ok_or("The path does not exist.")?;
        explorer(&format!("\"{}\"", parent.display()));
        return Ok(());
    }
    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if EXECUTABLE_EXTS.contains(&ext.as_str()) {
        explorer(&format!("/select,\"{s}\""));
        return Ok(());
    }
    if !run_editor(editor_template, &s, line.unwrap_or(1)) {
        shell_open(&s)?;
    }
    Ok(())
}

/// Run the `links.editorCommand` template (`code --goto "{file}:{line}"`) without a shell: tokenise, substitute,
/// resolve the program on PATH; `.cmd`/`.bat` shims (code.cmd) go through `cmd.exe /d /c`.
fn run_editor(template: &str, file: &str, line: u32) -> bool {
    use std::os::windows::process::CommandExt;
    let mut toks: Vec<String> = Vec::new();
    let (mut cur, mut quoted, mut any) = (String::new(), false, false);
    for c in template.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        toks.push(cur);
    }
    let toks: Vec<String> = toks.into_iter().map(|t| t.replace("{file}", file).replace("{line}", &line.to_string())).collect();
    let Some((prog, args)) = toks.split_first() else { return false };
    let Some(exe) = ut_shell::resolve_exe(prog) else { return false };
    const NO_WINDOW: u32 = 0x0800_0000;
    let ext = exe.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let mut cmd;
    if ext == "cmd" || ext == "bat" {
        cmd = std::process::Command::new("cmd.exe");
        cmd.args(["/d", "/c"]).arg(&exe).args(args);
    } else {
        cmd = std::process::Command::new(&exe);
        cmd.args(args);
    }
    cmd.creation_flags(NO_WINDOW).spawn().is_ok()
}

/// Label of a physical key on the CURRENT keyboard layout (VK_OEM_3 is "Ñ" on es-ES, §7.8).
pub fn key_label(code: &str) -> String {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyNameTextW, MapVirtualKeyW, MAPVK_VK_TO_VSC};
    let vk: u32 = match code {
        "Backquote" => 0xC0,
        "Minus" => 0xBD,
        "Equal" => 0xBB,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "Semicolon" => 0xBA,
        "Quote" => 0xDE,
        "BracketLeft" => 0xDB,
        "BracketRight" => 0xDD,
        "Backslash" => 0xDC,
        _ => return code.to_string(),
    };
    unsafe {
        let sc = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
        let mut buf = [0u16; 32];
        let n = GetKeyNameTextW((sc << 16) as i32, &mut buf);
        if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { code.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_schemes_are_safe() {
        assert!(is_safe_url("https://example.com/x"));
        assert!(is_safe_url("mailto:a@b.c"));
        assert!(!is_safe_url("file:///C:/Windows/System32/cmd.exe"));
        assert!(!is_safe_url("ms-settings:privacy"));
        assert!(!is_safe_url("C:\\x"));
        assert!(!is_safe_url("javascript:alert(1)"));
    }
}
