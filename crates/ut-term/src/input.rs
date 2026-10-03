//! Keyboard input for the focused terminal, taken straight from the Win32 message stream.
//!
//! egui-winit is lossy for terminals: it turns Ctrl+C/X/V into Copy/Cut/Paste events, and drops text while Ctrl
//! is held — which is how Windows reports AltGr (es-ES `@ # \ ~ € [ ] { }`). So while a terminal has focus a window
//! subclass sees `WM_KEYDOWN`/`WM_CHAR` first (like Windows Terminal does):
//!
//! * app shortcuts (the live keymap) are matched here and queued as action ids for the app,
//! * special keys / Ctrl combos are encoded xterm-style and written to the PTY,
//! * text (`WM_CHAR`: composed characters, AltGr, dead keys) is written as UTF-8,
//! * IME-processed keys (`VK_PROCESSKEY`) and everything when disabled pass through to winit/egui.

use crate::session::TermSession;
use alacritty_terminal::term::TermMode;
use parking_lot::{Mutex, RwLock};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP};

const SUBCLASS_ID: usize = 0x5554_494E; // "UTIN"

/// One keymap entry, already resolved to virtual-key codes (see [`Binding::new`]).
#[derive(Clone, Debug)]
pub struct Binding {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub keys: Vec<u16>,
    pub action: String,
}

/// Virtual-key codes for a canonical chord key name (`T`, `Comma`, `Numpad1`, `F5`, `Arrow`, …).
pub fn vks_for(key: &str) -> Vec<u16> {
    let b = key.as_bytes();
    if b.len() == 1 && b[0].is_ascii_alphanumeric() {
        return vec![b[0].to_ascii_uppercase() as u16];
    }
    if let Some(n) = key.strip_prefix("Numpad").and_then(|n| n.parse::<u16>().ok()) {
        return vec![VK_NUMPAD0.0 + n];
    }
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return vec![VK_F1.0 + n - 1];
    }
    let v = match key {
        "Tab" => VK_TAB,
        "Enter" => VK_RETURN,
        "Esc" => VK_ESCAPE,
        "Space" => VK_SPACE,
        "Backspace" => VK_BACK,
        "Delete" => VK_DELETE,
        "Insert" => VK_INSERT,
        "Home" => VK_HOME,
        "End" => VK_END,
        "PageUp" => VK_PRIOR,
        "PageDown" => VK_NEXT,
        "Up" => VK_UP,
        "Down" => VK_DOWN,
        "Left" => VK_LEFT,
        "Right" => VK_RIGHT,
        "Arrow" => return vec![VK_LEFT.0, VK_UP.0, VK_RIGHT.0, VK_DOWN.0],
        "Comma" => VK_OEM_COMMA,
        "Period" => VK_OEM_PERIOD,
        "Minus" => VK_OEM_MINUS,
        "Equal" => VK_OEM_PLUS,
        "Backquote" => VK_OEM_3,
        "Slash" => VK_OEM_2,
        "Backslash" => VK_OEM_5,
        "BracketLeft" => VK_OEM_4,
        "BracketRight" => VK_OEM_6,
        "Semicolon" => VK_OEM_1,
        "Quote" => VK_OEM_7,
        _ => return vec![],
    };
    vec![v.0]
}

impl Binding {
    pub fn new(ctrl: bool, shift: bool, alt: bool, key: &str, action: impl Into<String>) -> Self {
        Self { ctrl, shift, alt, keys: vks_for(key), action: action.into() }
    }
}

/// Shared between the UI thread (which configures it every frame) and the window procedure.
pub struct Router {
    /// Only true while a terminal pane has keyboard focus and no overlay/text field wants keys.
    pub enabled: AtomicBool,
    /// Focus is in a text field of the main window (sidebar search, rename…): only app shortcuts that do not
    /// clash with text editing are taken, everything else reaches the field.
    shortcuts_only: AtomicBool,
    /// First = the focused pane (modes, selection); every target receives the bytes (broadcast, §7.4).
    targets: Mutex<Vec<Arc<TermSession>>>,
    readonly: AtomicBool,
    shortcuts: RwLock<Vec<Binding>>,
    actions: Mutex<VecDeque<(String, u16, Mods)>>,
    repaint: Box<dyn Fn() + Send + Sync>,
    state: Mutex<KeyState>,
}

#[derive(Default)]
struct KeyState {
    /// The next `WM_CHAR` belongs to a key we already handled.
    swallow_char: bool,
    high_surrogate: Option<u16>,
}

impl Router {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(false),
            shortcuts_only: AtomicBool::new(false),
            targets: Mutex::new(Vec::new()),
            readonly: AtomicBool::new(false),
            shortcuts: RwLock::new(vec![]),
            actions: Mutex::new(VecDeque::new()),
            repaint: Box::new(repaint),
            state: Mutex::new(KeyState::default()),
        })
    }

    /// `primary` first; `others` get the same bytes verbatim (broadcast).
    pub fn set_targets(&self, targets: Vec<Arc<TermSession>>, readonly: bool) {
        *self.targets.lock() = targets;
        self.readonly.store(readonly, Ordering::Release);
    }

    pub fn set_shortcuts(&self, b: Vec<Binding>) {
        *self.shortcuts.write() = b;
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Release);
    }

    pub fn set_shortcuts_only(&self, on: bool) {
        self.shortcuts_only.store(on, Ordering::Release);
    }

    /// Actions queued by shortcuts since the last call: `(action id, virtual key, modifiers)`.
    pub fn take_actions(&self) -> Vec<(String, u16, Mods)> {
        self.actions.lock().drain(..).collect()
    }

    /// Install on the window. The returned handle keeps the router alive; dropping it does NOT uninstall.
    pub fn install(self: &Arc<Self>, hwnd: isize) -> bool {
        let data = Arc::into_raw(self.clone()) as usize;
        unsafe { SetWindowSubclass(HWND(hwnd as *mut _), Some(subclass_proc), SUBCLASS_ID, data).as_bool() }
    }

    pub fn uninstall(hwnd: isize) {
        unsafe {
            let _ = RemoveWindowSubclass(HWND(hwnd as *mut _), Some(subclass_proc), SUBCLASS_ID);
        }
    }
}

fn down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// AltGr (Ctrl+Alt reported together with a right Alt): produces TEXT, not a shortcut.
    pub altgr: bool,
}

fn mods() -> Mods {
    let (ctrl, shift, alt) = (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU));
    let altgr = ctrl && down(VK_RMENU);
    Mods { ctrl: ctrl && !altgr, shift, alt: alt && !altgr, altgr }
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, data: usize) -> LRESULT {
    let r = &*(data as *const Router);
    if r.enabled.load(Ordering::Acquire) {
        if let Some(handled) = r.message(msg, wp.0, lp.0) {
            if handled {
                return LRESULT(0);
            }
        }
    }
    if r.shortcuts_only.load(Ordering::Acquire) && (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN) && r.global_shortcut(wp.0 as u16) {
        return LRESULT(0);
    }
    if msg == WM_KILLFOCUS {
        *r.state.lock() = KeyState::default();
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

impl Router {
    /// `Some(true)` = handled (swallow), `Some(false)`/`None` = pass through.
    fn message(&self, msg: u32, wp: usize, lp: isize) -> Option<bool> {
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(self.key_down(wp as u16, lp)),
            WM_CHAR | WM_SYSCHAR => Some(self.char_in(wp as u16, msg == WM_SYSCHAR)),
            WM_KEYUP | WM_SYSKEYUP => {
                self.state.lock().swallow_char = false;
                None
            }
            _ => None,
        }
    }

    fn queue(&self, action: &str, vk: u16, m: Mods) {
        self.actions.lock().push_back((action.to_string(), vk, m));
        (self.repaint)();
    }

    /// An app shortcut pressed while a text field has focus. Chords that mean something to text editing (clipboard,
    /// Ctrl+Shift+Arrow word selection, Shift+PageUp) stay with the field.
    fn global_shortcut(&self, vk: u16) -> bool {
        const TEXT_EDITING: [&str; 5] = ["copy", "paste", "movePaneFocus", "scrollPageUp", "scrollPageDown"];
        if vk == VK_PROCESSKEY.0 || vk == VK_LWIN.0 || vk == VK_RWIN.0 {
            return false;
        }
        let m = mods();
        if m.altgr || !(m.ctrl || m.alt) {
            return false; // bare / Shift-only keys are typing
        }
        for b in self.shortcuts.read().iter() {
            if b.ctrl == m.ctrl && b.shift == m.shift && b.alt == m.alt && b.keys.contains(&vk) && !TEXT_EDITING.contains(&b.action.as_str()) {
                self.queue(&b.action, vk, m);
                return true;
            }
        }
        false
    }

    fn key_down(&self, vk: u16, _lp: isize) -> bool {
        if vk == VK_PROCESSKEY.0 || vk == VK_LWIN.0 || vk == VK_RWIN.0 {
            return false; // IME / Windows key: leave to the system
        }
        if matches!(VIRTUAL_KEY(vk), VK_SHIFT | VK_CONTROL | VK_MENU | VK_LSHIFT | VK_RSHIFT | VK_LCONTROL | VK_RCONTROL | VK_LMENU | VK_RMENU | VK_CAPITAL | VK_NUMLOCK | VK_SCROLL) {
            return false;
        }
        let m = mods();
        if !m.altgr {
            for b in self.shortcuts.read().iter() {
                if b.ctrl == m.ctrl && b.shift == m.shift && b.alt == m.alt && b.keys.contains(&vk) {
                    self.state.lock().swallow_char = true;
                    self.queue(&b.action, vk, m);
                    return true;
                }
            }
        }
        let targets = self.targets.lock().clone();
        let Some(term) = targets.first().cloned() else {
            // The focused pane's process exited: Enter restarts it (§4.7), everything else is swallowed.
            if vk == VK_RETURN.0 {
                self.queue("restartPane", vk, mods());
            }
            return true;
        };
        if self.readonly.load(Ordering::Acquire) {
            return true; // read-only pane: drop all input (§7.4)
        }
        let mode = term.mode();
        // Plain Ctrl+C with a selection copies it (else ^C reaches the shell).
        if m.ctrl && !m.shift && !m.alt && vk == b'C' as u16 && term.has_selection() {
            self.state.lock().swallow_char = true;
            self.queue("copy", vk, m);
            return true;
        }
        if let Some(bytes) = encode_key(vk, m, mode) {
            self.state.lock().swallow_char = true;
            for t in &targets {
                t.write(&bytes);
            }
            return true;
        }
        false // text-producing key: WM_CHAR delivers the (composed) character
    }

    fn char_in(&self, c: u16, sys: bool) -> bool {
        let targets = self.targets.lock().clone();
        if targets.is_empty() || self.readonly.load(Ordering::Acquire) {
            return true;
        }
        let mut st = self.state.lock();
        if std::mem::take(&mut st.swallow_char) {
            return true;
        }
        let ch = if (0xD800..0xDC00).contains(&c) {
            st.high_surrogate = Some(c);
            return true;
        } else if (0xDC00..0xE000).contains(&c) {
            match st.high_surrogate.take() {
                Some(h) => char::decode_utf16([h, c]).next().and_then(|r| r.ok()),
                None => None,
            }
        } else {
            char::from_u32(c as u32)
        };
        drop(st);
        let Some(ch) = ch else { return true };
        if (ch as u32) < 0x20 || ch as u32 == 0x7f {
            return true; // control characters are produced by key_down
        }
        let mut buf = [0u8; 4];
        let mut out = Vec::with_capacity(5);
        if sys || (mods().alt) {
            out.push(0x1b); // Alt+key = ESC prefix (not for AltGr: that is a plain character)
        }
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        for t in &targets {
            t.write(&out);
        }
        true
    }
}

/// xterm-style encoding of non-text keys and Ctrl/Alt combos. `None` = the key produces text via `WM_CHAR`.
pub fn encode_key(vk: u16, m: Mods, mode: TermMode) -> Option<Vec<u8>> {
    let app = mode.contains(TermMode::APP_CURSOR);
    // CSI modifier parameter: 1 + shift(1) + alt(2) + ctrl(4)
    let mp = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let has_mod = mp > 1;
    let csi_letter = |l: char| -> Vec<u8> {
        if has_mod {
            format!("\x1b[1;{mp}{l}").into_bytes()
        } else if app {
            format!("\x1bO{l}").into_bytes()
        } else {
            format!("\x1b[{l}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if has_mod { format!("\x1b[{n};{mp}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let esc_if_alt = |mut v: Vec<u8>| -> Vec<u8> {
        if m.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    let v = VIRTUAL_KEY(vk);
    Some(match v {
        VK_UP => csi_letter('A'),
        VK_DOWN => csi_letter('B'),
        VK_RIGHT => csi_letter('C'),
        VK_LEFT => csi_letter('D'),
        VK_HOME => csi_letter('H'),
        VK_END => csi_letter('F'),
        VK_INSERT => tilde(2),
        VK_DELETE => tilde(3),
        VK_PRIOR => tilde(5),
        VK_NEXT => tilde(6),
        VIRTUAL_KEY(0x70..=0x73) => {
            let l = (b'P' + (vk - VK_F1.0) as u8) as char;
            if has_mod { format!("\x1b[1;{mp}{l}").into_bytes() } else { format!("\x1bO{l}").into_bytes() }
        }
        VK_F5 => tilde(15),
        VK_F6 => tilde(17),
        VK_F7 => tilde(18),
        VK_F8 => tilde(19),
        VK_F9 => tilde(20),
        VK_F10 => tilde(21),
        VK_F11 => tilde(23),
        VK_F12 => tilde(24),
        VK_BACK => esc_if_alt(vec![if m.ctrl { 0x08 } else { 0x7f }]),
        VK_RETURN => esc_if_alt(vec![if m.ctrl { b'\n' } else { b'\r' }]),
        VK_TAB => {
            if m.shift { b"\x1b[Z".to_vec() } else { esc_if_alt(vec![b'\t']) }
        }
        VK_ESCAPE => esc_if_alt(vec![0x1b]),
        VK_SPACE if m.ctrl => esc_if_alt(vec![0]),
        VK_SPACE if m.alt => vec![0x1b, b' '],
        _ if m.ctrl && !m.altgr => {
            // Ctrl + letter / digit / punctuation → C0 control.
            let b = match v {
                VIRTUAL_KEY(c @ 0x41..=0x5A) => (c as u8) - 0x40,
                VK_OEM_4 => 0x1b,
                VK_OEM_5 => 0x1c,
                VK_OEM_6 => 0x1d,
                VK_OEM_MINUS => 0x1f,
                VK_OEM_2 => 0x1f,
                VIRTUAL_KEY(c @ 0x32..=0x38) => match c {
                    0x32 => 0x00,
                    0x33 => 0x1b,
                    0x34 => 0x1c,
                    0x35 => 0x1d,
                    0x36 => 0x1e,
                    0x37 => 0x1f,
                    _ => 0x7f,
                },
                _ => return None,
            };
            esc_if_alt(vec![b])
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(ctrl: bool, shift: bool, alt: bool) -> Mods {
        Mods { ctrl, shift, alt, altgr: false }
    }

    #[test]
    fn arrows_and_modifiers() {
        let none = TermMode::empty();
        assert_eq!(encode_key(VK_UP.0, m(false, false, false), none).unwrap(), b"\x1b[A");
        assert_eq!(encode_key(VK_UP.0, m(false, false, false), TermMode::APP_CURSOR).unwrap(), b"\x1bOA");
        assert_eq!(encode_key(VK_LEFT.0, m(true, false, false), none).unwrap(), b"\x1b[1;5D");
        assert_eq!(encode_key(VK_RIGHT.0, m(true, true, false), none).unwrap(), b"\x1b[1;6C");
        assert_eq!(encode_key(VK_DELETE.0, m(false, false, true), none).unwrap(), b"\x1b[3;3~");
    }

    #[test]
    fn controls_and_function_keys() {
        let none = TermMode::empty();
        assert_eq!(encode_key(b'C' as u16, m(true, false, false), none).unwrap(), vec![3]);
        assert_eq!(encode_key(b'C' as u16, m(true, true, false), none).unwrap(), vec![3]);
        assert_eq!(encode_key(b'W' as u16, m(true, false, true), none).unwrap(), vec![0x1b, 0x17]);
        assert_eq!(encode_key(VK_BACK.0, m(false, false, false), none).unwrap(), vec![0x7f]);
        assert_eq!(encode_key(VK_RETURN.0, m(false, false, false), none).unwrap(), vec![b'\r']);
        assert_eq!(encode_key(VK_TAB.0, m(false, true, false), none).unwrap(), b"\x1b[Z");
        assert_eq!(encode_key(VK_F1.0, m(false, false, false), none).unwrap(), b"\x1bOP");
        assert_eq!(encode_key(VK_F5.0, m(false, false, false), none).unwrap(), b"\x1b[15~");
        assert_eq!(encode_key(VK_F12.0, m(false, true, false), none).unwrap(), b"\x1b[24;2~");
    }

    #[test]
    fn plain_and_altgr_text_keys_are_left_to_wm_char() {
        let none = TermMode::empty();
        assert!(encode_key(b'A' as u16, m(false, false, false), none).is_none());
        assert!(encode_key(b'2' as u16, m(false, true, false), none).is_none());
        // AltGr+2 = '@' on es-ES: Ctrl+Alt are both down but `altgr` is set → no control code, no shortcut.
        let ag = Mods { ctrl: false, shift: false, alt: false, altgr: true };
        assert!(encode_key(b'2' as u16, ag, none).is_none());
        assert!(encode_key(VK_OEM_1.0, ag, none).is_none());
    }

    #[test]
    fn chord_keys_resolve_to_virtual_keys() {
        assert_eq!(vks_for("T"), vec![b'T' as u16]);
        assert_eq!(vks_for("Backquote"), vec![VK_OEM_3.0]);
        assert_eq!(vks_for("Numpad3"), vec![VK_NUMPAD3.0]);
        assert_eq!(vks_for("F10"), vec![VK_F10.0]);
        assert_eq!(vks_for("Arrow").len(), 4);
        assert!(vks_for("Nonsense").is_empty());
    }
}
