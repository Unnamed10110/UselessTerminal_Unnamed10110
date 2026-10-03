use crate::env::build_env_block;
use crate::flow::Flow;
use crate::{proc, CloseMode, Pty, Sink, SpawnConfig, SpawnError};
use parking_lot::Mutex;
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{s, w, HRESULT, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{SetConsoleCtrlHandler, COORD, HPCON};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
    QueryInformationJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList,
    ResumeThread, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTUPINFOEXW, STARTUPINFOW, STARTF_USESTDHANDLES,
};

const RESIZE_THROTTLE: Duration = Duration::from_millis(16);
const READ_BUF: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct H(HANDLE);
// Kernel handles are plain integers usable from any thread.
unsafe impl Send for H {}
unsafe impl Sync for H {}

/// Closes the handle on drop unless `release()`d.
struct Owned(HANDLE);
impl Owned {
    fn release(mut self) -> HANDLE {
        std::mem::take(&mut self.0)
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type CloseFn = unsafe extern "system" fn(HPCON);

/// ConPTY entry points: kernel32's, or a bundled `conpty.dll` (§2.1) when it loads.
struct Api {
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
}

impl Api {
    fn load(bundled: Option<&Path>) -> Result<Api, SpawnError> {
        unsafe {
            if let Some(p) = bundled {
                if let Ok(lib) = LoadLibraryW(&HSTRING::from(p.as_os_str())) {
                    if let Some(a) = Self::from_module(lib) {
                        return Ok(a);
                    }
                }
                tracing::warn!("bundled conpty.dll failed to load; using the system ConPTY");
            }
            let k32 = GetModuleHandleW(w!("kernel32.dll"))
                .map_err(|e| SpawnError::Unsupported(e.message()))?;
            Self::from_module(k32).ok_or_else(|| {
                SpawnError::Unsupported("kernel32 has no CreatePseudoConsole (needs Windows 10 1809+)".into())
            })
        }
    }

    unsafe fn from_module(m: HMODULE) -> Option<Api> {
        let c = GetProcAddress(m, s!("CreatePseudoConsole"))?;
        let r = GetProcAddress(m, s!("ResizePseudoConsole"))?;
        let x = GetProcAddress(m, s!("ClosePseudoConsole"))?;
        Some(Api {
            create: std::mem::transmute::<_, CreateFn>(c),
            resize: std::mem::transmute::<_, ResizeFn>(r),
            close: std::mem::transmute::<_, CloseFn>(x),
        })
    }
}

enum Msg {
    Write(Vec<u8>),
    Resize,
    Stop,
}

pub struct ConPty {
    pid: u32,
    api: Api,
    hpc: Mutex<Option<HPCON>>,
    process: H,
    job: Option<H>,
    tx: Sender<Msg>,
    flow: Flow,
    disposed: AtomicBool,
    /// Set once the process has exited: lets the reader drain what is left without credits.
    exiting: AtomicBool,
    alive: AtomicBool,
    exit: Mutex<Option<u32>>,
    /// `cols << 16 | rows` most recently requested.
    desired: AtomicU32,
}

fn os(stage: &'static str) -> impl Fn(windows::core::Error) -> SpawnError {
    move |e| SpawnError::Os { stage, message: e.message().trim().to_string() }
}

pub fn spawn(cfg: SpawnConfig, sink: Arc<dyn Sink>) -> Result<Arc<ConPty>, SpawnError> {
    // A parent that ignores Ctrl+C (CI runners, IDE launchers, `start /b`) hands that flag to every child it creates,
    // which would make ^C unable to interrupt anything in the pane. Children must start with the default handling.
    static CTRL_C: std::sync::Once = std::sync::Once::new();
    CTRL_C.call_once(|| unsafe {
        let _ = SetConsoleCtrlHandler(None, false);
    });
    let api = Api::load(cfg.conpty_dll.as_deref())?;
    let size = COORD { X: cfg.cols.max(1) as i16, Y: cfg.rows.max(1) as i16 };
    unsafe {
        // 1. Pipes: `in` (we write → ConPTY reads), `out` (ConPTY writes → we read, 64 KiB).
        let (mut in_r, mut in_w, mut out_r, mut out_w) = Default::default();
        CreatePipe(&mut in_r, &mut in_w, None, 0).map_err(os("CreatePipe(in)"))?;
        let (in_r, in_w) = (Owned(in_r), Owned(in_w));
        CreatePipe(&mut out_r, &mut out_w, None, READ_BUF as u32).map_err(os("CreatePipe(out)"))?;
        let (out_r, out_w) = (Owned(out_r), Owned(out_w));

        // 2. Pseudo-console. No INHERIT_CURSOR, no WIN32_INPUT_MODE (xterm.js doesn't speak it).
        let mut hpc = HPCON::default();
        (api.create)(size, in_r.0, out_w.0, 0, &mut hpc).ok().map_err(os("CreatePseudoConsole"))?;
        let pcon = PconGuard { api: &api, hpc: Some(hpc) };
        // 3. The pseudo-console owns these ends now.
        drop(in_r);
        drop(out_w);

        // 4. Job object: enumerates the tree; deliberately WITHOUT KILL_ON_JOB_CLOSE (§4.8).
        let job = CreateJobObjectW(None, PCWSTR::null()).ok().map(Owned);

        // 5. Attribute list carrying the HPCON *value*.
        let mut attr_size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut attr_size);
        let mut attr_buf = vec![0u8; attr_size];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(Some(attrs), 1, None, &mut attr_size).map_err(os("InitializeProcThreadAttributeList"))?;
        let _attrs_guard = AttrGuard(attrs);
        UpdateProcThreadAttribute(
            attrs,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(hpc.0 as *const c_void),
            size_of::<HPCON>(),
            None,
            None,
        )
        .map_err(os("UpdateProcThreadAttribute"))?;

        // 6. CreateProcessW — suspended, verbatim command line, our env block.
        let mut cmd: Vec<u16> = cfg.command_line.encode_utf16().chain(Some(0)).collect();
        let env = build_env_block(&cfg.env);
        let cwd = cfg.cwd.as_ref().map(|p| HSTRING::from(p.as_os_str()));
        let si = STARTUPINFOEXW {
            // USESTDHANDLES with NULL handles: without it a child inherits OUR redirected stdio instead of
            // the pseudo-console's (visible whenever the parent has no console, e.g. under `cargo test`).
            StartupInfo: STARTUPINFOW {
                cb: size_of::<STARTUPINFOEXW>() as u32,
                dwFlags: STARTF_USESTDHANDLES,
                ..Default::default()
            },
            lpAttributeList: attrs,
        };
        let mut pi = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            Some(env.as_ptr() as *const c_void),
            cwd.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            &si.StartupInfo,
            &mut pi,
        )
        .map_err(|e| SpawnError::Os {
            stage: "CreateProcess",
            message: format!("{} (command line: {})", e.message().trim(), cfg.command_line),
        })?;
        let (process, thread) = (Owned(pi.hProcess), Owned(pi.hThread));

        // 7. Job assignment failure is not a spawn failure (§23.3): drop the job and carry on.
        let job = job.and_then(|j| match AssignProcessToJobObject(j.0, process.0) {
            Ok(()) => Some(j),
            Err(e) => {
                tracing::warn!("AssignProcessToJobObject failed: {e}; continuing without a job");
                None
            }
        });

        // 8. Resume on EVERY path — a suspended shell is a silent hang (§23.2).
        if ResumeThread(thread.0) == u32::MAX {
            let _ = TerminateProcess(process.0, 1);
            return Err(SpawnError::Os { stage: "ResumeThread", message: "could not resume the new process".into() });
        }
        drop(thread);

        let hpc_val = pcon.release();
        let (tx, rx) = channel();
        let pty = Arc::new(ConPty {
            pid: pi.dwProcessId,
            api,
            hpc: Mutex::new(hpc_val),
            process: H(process.release()),
            job: job.map(|j| H(j.release())),
            tx,
            flow: Flow::new(cfg.high_water, cfg.low_water),
            disposed: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            exit: Mutex::new(None),
            desired: AtomicU32::new((size.X as u32) << 16 | size.Y as u32),
        });

        // 9. Threads.
        let (done_tx, done_rx) = channel();
        let (r, w_, wt) = (pty.clone(), pty.clone(), pty.clone());
        let (out_r, in_w) = (H(out_r.release()), H(in_w.release()));
        let sink_r = sink.clone();
        let spawn_thread = |name: &str, f: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new().name(name.into()).spawn(f).map(|_| ()).map_err(|e| SpawnError::Os {
                stage: "thread spawn",
                message: e.to_string(),
            })
        };
        let started = spawn_thread("pty-reader", Box::new(move || r.reader_loop(out_r, sink_r, done_tx)))
            .and_then(|_| spawn_thread("pty-writer", Box::new(move || w_.writer_loop(in_w, rx))))
            .and_then(|_| spawn_thread("pty-exit", Box::new(move || wt.exit_watcher(done_rx, sink))));
        if let Err(e) = started {
            pty.close(CloseMode::Force, true);
            return Err(e);
        }
        Ok(pty)
    }
}

struct PconGuard<'a> {
    api: &'a Api,
    hpc: Option<HPCON>,
}
impl PconGuard<'_> {
    fn release(mut self) -> Option<HPCON> {
        self.hpc.take()
    }
}
impl Drop for PconGuard<'_> {
    fn drop(&mut self) {
        if let Some(h) = self.hpc.take() {
            unsafe { (self.api.close)(h) }
        }
    }
}

struct AttrGuard(LPPROC_THREAD_ATTRIBUTE_LIST);
impl Drop for AttrGuard {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.0) }
    }
}

impl ConPty {
    fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::Acquire) || self.exiting.load(Ordering::Acquire)
    }

    fn reader_loop(self: Arc<Self>, out: H, sink: Arc<dyn Sink>, done: Sender<()>) {
        let mut buf = vec![0u8; READ_BUF];
        loop {
            let mut n = 0u32;
            // Blocking read; BROKEN_PIPE (ClosePseudoConsole + conhost exit) is the EOF.
            if unsafe { ReadFile(out.0, Some(&mut buf[..]), Some(&mut n), None) }.is_err() {
                break;
            }
            let mut len = n as usize;
            // Coalesce whatever is already waiting — but never wait just to coalesce.
            while len > 0 && len < buf.len() {
                let mut avail = 0u32;
                if unsafe { PeekNamedPipe(out.0, None, 0, None, Some(&mut avail), None) }.is_err() || avail == 0 {
                    break;
                }
                let want = (avail as usize).min(buf.len() - len);
                let mut m = 0u32;
                match unsafe { ReadFile(out.0, Some(&mut buf[len..len + want]), Some(&mut m), None) } {
                    Ok(()) if m > 0 => len += m as usize,
                    _ => break,
                }
            }
            if len == 0 || self.disposed.load(Ordering::Acquire) {
                continue; // keep draining so ClosePseudoConsole can finish
            }
            // Credit BEFORE delivery: an ack can race the delivery otherwise.
            self.flow.add(len);
            sink.frame(&buf[..len]);
            self.flow.wait_drained(|| self.is_disposed());
        }
        unsafe {
            let _ = CloseHandle(out.0);
        }
        let _ = done.send(());
    }

    fn writer_loop(self: Arc<Self>, input: H, rx: Receiver<Msg>) {
        let mut last_resize = Instant::now() - RESIZE_THROTTLE;
        let mut applied = self.desired.load(Ordering::Relaxed);
        let mut pending = false;
        loop {
            let wait = if pending {
                (last_resize + RESIZE_THROTTLE).saturating_duration_since(Instant::now())
            } else {
                Duration::from_secs(3600)
            };
            match rx.recv_timeout(wait) {
                Ok(Msg::Write(bytes)) => {
                    if !self.disposed.load(Ordering::Acquire) {
                        write_all(input.0, &bytes);
                    }
                }
                Ok(Msg::Resize) => pending = true,
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if pending && last_resize.elapsed() >= RESIZE_THROTTLE {
                pending = false;
                last_resize = Instant::now();
                let want = self.desired.load(Ordering::Relaxed);
                if want != applied {
                    applied = want;
                    if let Some(h) = *self.hpc.lock() {
                        let size = COORD { X: (want >> 16) as i16, Y: (want & 0xFFFF) as i16 };
                        if let Err(e) = unsafe { (self.api.resize)(h, size) }.ok() {
                            tracing::debug!("ResizePseudoConsole failed: {e}");
                        }
                    }
                }
            }
        }
        unsafe {
            let _ = CloseHandle(input.0);
        }
    }

    fn exit_watcher(self: Arc<Self>, reader_done: Receiver<()>, sink: Arc<dyn Sink>) {
        let mut code = 0u32;
        unsafe {
            WaitForSingleObject(self.process.0, INFINITE);
            let _ = GetExitCodeProcess(self.process.0, &mut code);
        }
        *self.exit.lock() = Some(code);
        self.alive.store(false, Ordering::Release);
        self.exiting.store(true, Ordering::Release);
        self.flow.wake();
        // ClosePseudoConsole while the reader is still draining (§4.7): on older builds it can block
        // until the output pipe is empty. Exit is detected by the process handle, not by pipe EOF.
        self.close_pseudoconsole();
        let _ = reader_done.recv_timeout(Duration::from_secs(3));
        sink.exited(Some(code));
        let _ = self.tx.send(Msg::Stop);
    }

    fn close_pseudoconsole(&self) {
        if let Some(h) = self.hpc.lock().take() {
            unsafe { (self.api.close)(h) }
        }
    }

    /// PIDs in the job; without a job, the shell plus its descendants.
    fn tree_pids(&self) -> Vec<u32> {
        if let Some(job) = self.job {
            let cap = 512usize;
            let mut buf = vec![0usize; 2 + cap + 1];
            let ok = unsafe {
                QueryInformationJobObject(
                    Some(job.0),
                    JobObjectBasicProcessIdList,
                    buf.as_mut_ptr() as *mut c_void,
                    (buf.len() * size_of::<usize>()) as u32,
                    None,
                )
            }
            .is_ok();
            if ok {
                let list = unsafe { &*(buf.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
                let n = list.NumberOfProcessIdsInList as usize;
                let ids = unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), n.min(cap)) };
                return ids.iter().map(|&p| p as u32).collect();
            }
        }
        let mut v: Vec<u32> = proc::descendants(self.pid).iter().map(|p| p.pid).collect();
        v.push(self.pid);
        v
    }
}

fn write_all(h: HANDLE, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let mut n = 0u32;
        if unsafe { WriteFile(h, Some(bytes), Some(&mut n), None) }.is_err() || n == 0 {
            return; // pipe closed: a write after disposal is a silent no-op (§4.5)
        }
        bytes = &bytes[n as usize..];
    }
}

impl Pty for ConPty {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.tx.send(Msg::Write(bytes.to_vec()));
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 || cols > i16::MAX as u16 || rows > i16::MAX as u16 {
            return;
        }
        let want = (cols as u32) << 16 | rows as u32;
        if self.desired.swap(want, Ordering::AcqRel) != want {
            let _ = self.tx.send(Msg::Resize);
        }
    }

    fn ack(&self, bytes: usize) {
        self.flow.ack(bytes);
    }

    fn close(&self, mode: CloseMode, kill_console_tree: bool) {
        // Releases the reader from flow control first, so ClosePseudoConsole cannot deadlock on it.
        self.disposed.store(true, Ordering::Release);
        self.flow.wake();
        match mode {
            CloseMode::Graceful => {
                self.close_pseudoconsole(); // clients get CTRL_CLOSE_EVENT
                unsafe { WaitForSingleObject(self.process.0, 100) }; // PSReadLine needs a moment
                if kill_console_tree {
                    // Only console-subsystem leftovers; GUI apps launched from the shell survive.
                    proc::terminate_console_processes(&self.tree_pids());
                }
            }
            CloseMode::Force => {
                let pids = self.tree_pids();
                proc::terminate_all(&pids);
                self.close_pseudoconsole();
            }
        }
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn exit_code(&self) -> Option<u32> {
        *self.exit.lock()
    }

    fn unacked(&self) -> usize {
        self.flow.unacked()
    }
}

impl Drop for ConPty {
    fn drop(&mut self) {
        unsafe {
            if let Some(h) = self.hpc.get_mut().take() {
                (self.api.close)(h);
            }
            let _ = CloseHandle(self.process.0);
            if let Some(j) = self.job {
                let _ = CloseHandle(j.0);
            }
        }
    }
}
