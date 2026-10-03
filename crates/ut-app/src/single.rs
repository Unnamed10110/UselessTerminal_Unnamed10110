//! Single instance (§16.2): a named mutex decides who is first; a second launch forwards its arguments over a named
//! pipe to the running instance, which opens a new tab with them. The names include a hash of the data directory so
//! an instance started with `UT_APPDATA` (tests) never collides with the installed one.

use std::sync::mpsc::{channel, Receiver};
use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{ReadFile, PIPE_ACCESS_INBOUND};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
use windows::Win32::System::Threading::CreateMutexW;

fn suffix() -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in ut_fs::app_data_dir().to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

pub enum Instance {
    /// We are the first instance; forwarded argument lists arrive here.
    First(Receiver<Vec<String>>),
    /// Another instance got our arguments; exit.
    Forwarded,
}

pub fn acquire(args: &[String], repaint: impl Fn() + Send + Sync + 'static) -> Instance {
    let sfx = suffix();
    let mutex = HSTRING::from(format!("Local\\UselessTerminal-{sfx}"));
    let pipe = format!("\\\\.\\pipe\\UselessTerminal-{sfx}");
    unsafe {
        let h = CreateMutexW(None, true, &mutex);
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        match h {
            Ok(_) if !exists => {} // keep the handle open for the process lifetime
            _ => {
                // Forward and exit. The pipe may not be listening yet: retry briefly.
                let payload = args.join("\0");
                // OPEN_EXISTING, not CREATE_ALWAYS (`fs::write`): the pipe is created by the first instance.
                for _ in 0..20 {
                    let sent = std::fs::OpenOptions::new().write(true).open(&pipe).and_then(|mut f| std::io::Write::write_all(&mut f, payload.as_bytes()));
                    match sent {
                        Ok(()) => {
                            crate::debug::log("single: arguments forwarded");
                            return Instance::Forwarded;
                        }
                        Err(e) => crate::debug::log(&format!("single: forward to {pipe} failed: {e}")),
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                return Instance::Forwarded;
            }
        }
    }
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || serve(&pipe, tx, repaint))
        .ok();
    Instance::First(rx)
}

fn serve(pipe: &str, tx: std::sync::mpsc::Sender<Vec<String>>, repaint: impl Fn()) {
    let name = HSTRING::from(pipe);
    loop {
        unsafe {
            let h = CreateNamedPipeW(&name, PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES, 4096, 4096, 0, None);
            if h.is_invalid() || h == INVALID_HANDLE_VALUE {
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }
            let _ = ConnectNamedPipe(h, None);
            let data = read_all(h);
            let _ = DisconnectNamedPipe(h);
            let _ = CloseHandle(h);
            if !data.is_empty() {
                let args: Vec<String> = String::from_utf8_lossy(&data).split('\0').map(String::from).collect();
                let _ = tx.send(args);
                repaint();
            }
        }
    }
}

unsafe fn read_all(h: HANDLE) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut n = 0u32;
        if ReadFile(h, Some(&mut buf), Some(&mut n), None).is_err() || n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
        if out.len() > 1 << 20 {
            break;
        }
    }
    out
}
