//! Native window integration: monitors, taskbar flash, ShareX scroll bridge (WM_VSCROLL), global Quake hotkey,
//! tray icon, single-instance forwarding.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use ut_core::Rect;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{FlashWindowEx, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY, WM_VSCROLL};

// ------------------------------------------------------------------------------------------ monitors

/// Work areas of the connected monitors in physical pixels, and the primary one's index (§16.3).
pub fn monitors() -> (Vec<Rect>, usize) {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let v = &mut *(data.0 as *mut Vec<(Rect, bool)>);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(m, &mut mi).as_bool() {
            let w = mi.rcWork;
            v.push((Rect { x: w.left, y: w.top, width: w.right - w.left, height: w.bottom - w.top }, mi.dwFlags & 1 != 0 /* MONITORINFOF_PRIMARY */));
        }
        BOOL(1)
    }
    let mut v: Vec<(Rect, bool)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    let primary = v.iter().position(|(_, p)| *p).unwrap_or(0);
    (v.into_iter().map(|(r, _)| r).collect(), primary)
}

/// Work area (physical px) of the monitor under the mouse cursor, else the primary one.
pub fn monitor_under_cursor() -> Option<Rect> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    let at = unsafe { GetCursorPos(&mut p).is_ok() };
    let (mons, primary) = monitors();
    mons.iter()
        .find(|r| at && p.x >= r.x && p.x < r.x + r.width && p.y >= r.y && p.y < r.y + r.height)
        .or_else(|| mons.get(primary))
        .cloned()
}

/// Flash the taskbar button until the window comes to the foreground (§12.3).
pub fn flash(hwnd: isize) {
    let fi = FLASHWINFO { cbSize: size_of::<FLASHWINFO>() as u32, hwnd: HWND(hwnd as *mut _), dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG, uCount: 0, dwTimeout: 0 };
    unsafe {
        let _ = FlashWindowEx(&fi);
    }
}

// ------------------------------------------------------------------------------------ scroll bridge

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAction {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
}

struct Bridge {
    queue: Mutex<VecDeque<ScrollAction>>,
    repaint: Box<dyn Fn() + Send + Sync>,
}

static BRIDGE: OnceLock<Arc<Bridge>> = OnceLock::new();

unsafe extern "system" fn scroll_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if msg == WM_VSCROLL {
        let a = match wp.0 & 0xFFFF {
            0 => Some(ScrollAction::LineUp),
            1 => Some(ScrollAction::LineDown),
            2 => Some(ScrollAction::PageUp),
            3 => Some(ScrollAction::PageDown),
            6 => Some(ScrollAction::Top),
            7 => Some(ScrollAction::Bottom),
            _ => None,
        };
        if let (Some(a), Some(b)) = (a, BRIDGE.get()) {
            b.queue.lock().push_back(a);
            (b.repaint)();
            return LRESULT(0);
        }
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

/// ShareX's "Windows message" scrolling capture sends WM_VSCROLL to the window (§7.11).
pub fn install_scroll_bridge(hwnd: isize, repaint: impl Fn() + Send + Sync + 'static) {
    let _ = BRIDGE.set(Arc::new(Bridge { queue: Mutex::new(VecDeque::new()), repaint: Box::new(repaint) }));
    unsafe {
        let _ = SetWindowSubclass(HWND(hwnd as *mut _), Some(scroll_proc), 0xA11CE, 0);
    }
}

pub fn take_scroll_actions() -> Vec<ScrollAction> {
    BRIDGE.get().map(|b| b.queue.lock().drain(..).collect()).unwrap_or_default()
}

// --------------------------------------------------------------------------------------- hotkey

pub struct Hotkey {
    manager: Option<global_hotkey::GlobalHotKeyManager>,
    current: Option<global_hotkey::hotkey::HotKey>,
}

/// `Win+Backquote`, `Ctrl+Alt+T`, … → global-hotkey syntax.
pub fn parse_hotkey(s: &str) -> Result<global_hotkey::hotkey::HotKey, String> {
    let norm: Vec<String> = s
        .split('+')
        .map(|p| match p.trim().to_ascii_lowercase().as_str() {
            "win" | "windows" | "meta" => "super".to_string(),
            "ctrl" | "control" => "control".to_string(),
            _ => p.trim().to_string(),
        })
        .collect();
    norm.join("+").parse().map_err(|e| format!("Invalid hotkey '{s}': {e}"))
}

impl Hotkey {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(move |_| repaint()));
        Self { manager: global_hotkey::GlobalHotKeyManager::new().ok(), current: None }
    }

    /// (Re)register; `Err(message)` is shown as a non-blocking warning (§7.8).
    pub fn register(&mut self, spec: &str) -> Result<(), String> {
        let m = self.manager.as_ref().ok_or("Global hotkeys are unavailable.")?;
        if let Some(old) = self.current.take() {
            let _ = m.unregister(old);
        }
        let hk = parse_hotkey(spec)?;
        m.register(hk).map_err(|e| format!("Could not register {spec}: {e}"))?;
        self.current = Some(hk);
        Ok(())
    }

    /// True if the hotkey was pressed since the last call.
    pub fn pressed(&self) -> bool {
        let Some(hk) = self.current else { return false };
        let mut hit = false;
        while let Ok(e) = global_hotkey::GlobalHotKeyEvent::receiver().try_recv() {
            hit |= e.id == hk.id() && e.state == global_hotkey::HotKeyState::Pressed;
        }
        hit
    }
}

// ------------------------------------------------------------------------------------------- tray

pub enum TrayCmd {
    Toggle,
    NewTab,
    Settings,
    Exit,
    Show,
}

pub struct Tray {
    _icon: Option<tray_icon::TrayIcon>,
    ids: [tray_icon::menu::MenuId; 4],
}

impl Tray {
    pub fn new(repaint: impl Fn() + Send + Sync + Clone + 'static) -> Self {
        use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
        let toggle = MenuItem::new("Show / Hide", true, None);
        let new = MenuItem::new("New Tab", true, None);
        let settings = MenuItem::new("Settings", true, None);
        let exit = MenuItem::new("Exit", true, None);
        let menu = Menu::new();
        let _ = menu.append_items(&[&toggle, &new, &PredefinedMenuItem::separator(), &settings, &PredefinedMenuItem::separator(), &exit]);
        let r1 = repaint.clone();
        tray_icon::menu::MenuEvent::set_event_handler(Some(move |_| r1()));
        tray_icon::TrayIconEvent::set_event_handler(Some(move |_| repaint()));
        let icon = load_icon();
        let t = tray_icon::TrayIconBuilder::new().with_tooltip("Useless Terminal").with_menu(Box::new(menu)).with_menu_on_left_click(false);
        let t = match icon {
            Some(i) => t.with_icon(i),
            None => t,
        };
        Tray { _icon: t.build().ok(), ids: [toggle.id().clone(), new.id().clone(), settings.id().clone(), exit.id().clone()] }
    }

    pub fn poll(&self) -> Vec<TrayCmd> {
        let mut out = Vec::new();
        while let Ok(e) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if e.id == self.ids[0] {
                out.push(TrayCmd::Toggle);
            } else if e.id == self.ids[1] {
                out.push(TrayCmd::NewTab);
            } else if e.id == self.ids[2] {
                out.push(TrayCmd::Settings);
            } else if e.id == self.ids[3] {
                out.push(TrayCmd::Exit);
            }
        }
        while let Ok(e) = tray_icon::TrayIconEvent::receiver().try_recv() {
            if let tray_icon::TrayIconEvent::DoubleClick { .. } = e {
                out.push(TrayCmd::Show);
            }
        }
        out
    }
}

fn load_icon() -> Option<tray_icon::Icon> {
    let img = image::load_from_memory(include_bytes!("../assets/icon.png")).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).ok()
}

pub fn window_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../assets/icon.png")).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}
