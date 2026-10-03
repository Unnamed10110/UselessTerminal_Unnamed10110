//! Process helpers: snapshots, descendants, command lines, PE subsystem. Used for the close policy (§4.8),
//! SSH target discovery (§10.4), the "is something running" check (§7.9) and the foreground name (§7.5).
//! All of this is synchronous and must run off the UI thread.

use std::ffi::c_void;
use std::io::{Read, Seek, SeekFrom};
use windows::core::PWSTR;
use windows::Wdk::System::Threading::{NtQueryInformationProcess, PROCESSINFOCLASS};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    /// Image file name, e.g. `pwsh.exe`.
    pub name: String,
}

pub fn snapshot() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push(ProcInfo {
                pid: e.th32ProcessID,
                ppid: e.th32ParentProcessID,
                name: String::from_utf16_lossy(&e.szExeFile[..len]),
            });
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// All descendants of `root` (breadth first, excluding `root`), from an existing snapshot.
pub fn descendants_in(all: &[ProcInfo], root: u32) -> Vec<ProcInfo> {
    let mut found: Vec<ProcInfo> = Vec::new();
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for c in all.iter().filter(|c| c.ppid == p && c.pid != root) {
            // Guard against PID reuse making a cycle.
            if !found.iter().any(|f| f.pid == c.pid) {
                found.push(c.clone());
                frontier.push(c.pid);
            }
        }
    }
    found
}

pub fn descendants(root: u32) -> Vec<ProcInfo> {
    descendants_in(&snapshot(), root)
}

fn is_console_host(name: &str) -> bool {
    name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("OpenConsole.exe")
}

/// True when the shell has a descendant other than the console host — "something is running" for shells
/// without OSC 133 integration (§7.9).
pub fn has_busy_children(shell_pid: u32) -> bool {
    descendants(shell_pid).iter().any(|p| !is_console_host(&p.name))
}

/// Name of the most recently started (highest pid is a good-enough proxy) non-conhost leaf descendant,
/// or the shell itself — the "foreground process" shown in the status bar [P1].
pub fn foreground_name(shell_pid: u32) -> Option<String> {
    let all = snapshot();
    let d = descendants_in(&all, shell_pid);
    let leaf = d
        .iter()
        .filter(|p| !is_console_host(&p.name) && !d.iter().any(|c| c.ppid == p.pid))
        .max_by_key(|p| p.pid)
        .map(|p| p.name.clone());
    leaf.or_else(|| all.iter().find(|p| p.pid == shell_pid).map(|p| p.name.clone()))
}

/// Full command line of a process via `NtQueryInformationProcess(ProcessCommandLineInformation = 60)`.
pub fn command_line(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let class = PROCESSINFOCLASS(60);
        let mut len = 0u32;
        let _ = NtQueryInformationProcess(h, class, std::ptr::null_mut(), 0, &mut len);
        let result = if len < 16 || len > 1 << 20 {
            None
        } else {
            let mut buf = vec![0u8; len as usize + 8];
            let st = NtQueryInformationProcess(h, class, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut len);
            if st.0 < 0 {
                None
            } else {
                // UNICODE_STRING { u16 Length; u16 MaximumLength; (pad) PWSTR Buffer } followed by the chars.
                let length = u16::from_le_bytes([buf[0], buf[1]]) as usize / 2;
                let ptr = usize::from_le_bytes(buf[8..16].try_into().ok()?) as *const u16;
                let base = buf.as_ptr() as usize;
                // The buffer pointer refers into our own buffer.
                if (ptr as usize) >= base && (ptr as usize) + length * 2 <= base + buf.len() {
                    Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, length)))
                } else {
                    None
                }
            }
        };
        let _ = CloseHandle(h);
        result
    }
}

/// Image path of a pid.
pub fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let r = image_path_h(h);
        let _ = CloseHandle(h);
        r
    }
}

unsafe fn image_path_h(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut n = buf.len() as u32;
    QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).ok()?;
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

pub const SUBSYSTEM_GUI: u16 = 2;
pub const SUBSYSTEM_CUI: u16 = 3;

/// PE `Subsystem` field of an executable (2 = GUI, 3 = console).
pub fn pe_subsystem(path: &str) -> Option<u16> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut dos = [0u8; 0x40];
    f.read_exact(&mut dos).ok()?;
    if &dos[..2] != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(dos[0x3C..0x40].try_into().ok()?) as u64;
    let mut hdr = [0u8; 24 + 70];
    f.seek(SeekFrom::Start(pe)).ok()?;
    f.read_exact(&mut hdr).ok()?;
    if &hdr[..4] != b"PE\0\0" {
        return None;
    }
    // Optional header starts after the 4-byte signature + 20-byte COFF header; Subsystem is at +68 in
    // both PE32 and PE32+.
    Some(u16::from_le_bytes([hdr[24 + 68], hdr[24 + 69]]))
}

/// Terminate the given pids that are console-subsystem executables (GUI apps such as `code .` or
/// `notepad` launched from the shell are deliberately spared, §4.8).
pub fn terminate_console_processes(pids: &[u32]) -> usize {
    let mut killed = 0;
    for &pid in pids {
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid) else {
                continue;
            };
            let cui = image_path_h(h).and_then(|p| pe_subsystem(&p)) == Some(SUBSYSTEM_CUI);
            if cui && TerminateProcess(h, 1).is_ok() {
                killed += 1;
            }
            let _ = CloseHandle(h);
        }
    }
    killed
}

/// Terminate the given pids unconditionally.
pub fn terminate_all(pids: &[u32]) {
    for &pid in pids {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                let _ = TerminateProcess(h, 1);
                let _ = CloseHandle(h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_contains_self_and_command_line() {
        let me = std::process::id();
        let all = snapshot();
        assert!(all.iter().any(|p| p.pid == me));
        let cl = command_line(me).expect("own command line");
        assert!(cl.to_lowercase().contains(".exe"), "{cl}");
    }

    #[test]
    fn subsystem_of_system_exes() {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        assert_eq!(pe_subsystem(&format!("{sys}\\System32\\cmd.exe")), Some(SUBSYSTEM_CUI));
        assert_eq!(pe_subsystem(&format!("{sys}\\System32\\notepad.exe")), Some(SUBSYSTEM_GUI));
    }

    #[test]
    fn descendants_walk() {
        let all = vec![
            ProcInfo { pid: 1, ppid: 0, name: "a".into() },
            ProcInfo { pid: 2, ppid: 1, name: "b".into() },
            ProcInfo { pid: 3, ppid: 2, name: "c".into() },
            ProcInfo { pid: 9, ppid: 8, name: "x".into() },
        ];
        let d: Vec<u32> = descendants_in(&all, 1).iter().map(|p| p.pid).collect();
        assert_eq!(d.len(), 2);
        assert!(d.contains(&2) && d.contains(&3));
    }
}
