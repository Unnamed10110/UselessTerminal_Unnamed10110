//! Chord capture for the keybindings editor and the Quake hotkey (§15.3, §7.8).
//!
//! The running app matches shortcuts on Win32 virtual keys with `GetKeyState`-style modifiers (`ut_term::input`),
//! so the chord is recorded from exactly that state instead of from egui's lossy key events (egui-winit turns
//! Ctrl+C/X/V/Insert/Delete into clipboard events, drops text under AltGr and reports layout-dependent logical
//! keys). What is recorded is therefore what the router will later match, on every keyboard layout.
//!
//! Everything but [`real_state`] is pure and unit-tested with a fake keyboard.

use ut_core::Chord;
use ut_term::input::vks_for;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    /// Right Alt together with Ctrl: Windows' AltGr. It produces text, never a shortcut (§15.1, §5.5).
    pub altgr: bool,
}

impl Mods {
    pub fn any(&self) -> bool {
        self.ctrl || self.shift || self.alt || self.win
    }

    /// `Ctrl+Shift+` style prefix of the modifiers currently held (the live hint while recording).
    pub fn prefix(&self) -> String {
        [(self.ctrl, "Ctrl+"), (self.shift, "Shift+"), (self.alt, "Alt+"), (self.win, "Win+")].iter().filter(|(on, _)| *on).map(|(_, n)| *n).collect()
    }
}

/// What a recorded chord is for: app shortcuts exclude Win chords, the global hotkey needs a "real" modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    App,
    Global,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Nothing usable yet (only modifiers are down).
    Wait,
    Cancel,
    Chord(Chord),
    /// A key the model cannot or must not bind; the message is shown and recording continues.
    Reject(String),
}

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;
const VK_RMENU: u16 = 0xA5;
const VK_ESCAPE: u16 = 0x1B;

/// Key names that map to exactly one virtual key (the inverse table of `ut_term::input::vks_for`; `Arrow` is a wildcard).
const NAMED: [&str; 26] = [
    "Tab", "Enter", "Esc", "Space", "Backspace", "Delete", "Insert", "Home", "End", "PageUp", "PageDown", "Up", "Down", "Left", "Right", "Comma", "Period", "Minus", "Equal", "Backquote", "Slash", "Backslash",
    "BracketLeft", "BracketRight", "Semicolon", "Quote",
];

/// Shift, Ctrl, Alt, Win, their left/right variants and the lock keys: held down, never "the key" of a chord.
pub fn is_modifier(vk: u16) -> bool {
    matches!(vk, VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN | 0xA0..=0xA5 | 0x14 | 0x90 | 0x91)
}

/// Virtual key -> canonical chord key name (`0x54` -> `T`, `0xBC` -> `Comma`); the inverse of `vks_for`.
pub fn key_name(vk: u16) -> Option<String> {
    let cands: Vec<String> = match vk {
        0x30..=0x39 | 0x41..=0x5A => vec![(vk as u8 as char).to_string()],
        0x60..=0x69 => vec![format!("Numpad{}", vk - 0x60)],
        0x70..=0x87 => vec![format!("F{}", vk - 0x6F)],
        _ => NAMED.iter().map(|s| s.to_string()).collect(),
    };
    cands.into_iter().find(|n| vks_for(n) == [vk])
}

/// Chords Windows (or the shell's expectations) own: they never reach the app, or must not be taken over.
const RESERVED: [(&str, &str); 9] = [
    ("Alt+F4", "Alt+F4 closes the window (reserved by Windows)."),
    ("Alt+Tab", "Alt+Tab is the Windows task switcher."),
    ("Shift+Alt+Tab", "Shift+Alt+Tab is the Windows task switcher."),
    ("Alt+Esc", "Alt+Esc is reserved by Windows."),
    ("Alt+Space", "Alt+Space opens the window menu (reserved by Windows)."),
    ("Ctrl+Esc", "Ctrl+Esc opens the Start menu (reserved by Windows)."),
    ("Ctrl+Shift+Esc", "Ctrl+Shift+Esc opens the Task Manager (reserved by Windows)."),
    ("Ctrl+Alt+Delete", "Ctrl+Alt+Delete is reserved by Windows."),
    ("Ctrl+C", "Ctrl+C interrupts the running program (and already copies when text is selected)."),
];

pub fn reserved(c: &Chord) -> Option<&'static str> {
    let s = c.to_string();
    RESERVED.iter().find(|(r, _)| *r == s).map(|(_, why)| *why)
}

/// A key that produces text (or edits it): alone or with Shift it would swallow normal typing.
fn types_text(key: &str) -> bool {
    let k = key.as_bytes();
    (k.len() == 1 && k[0].is_ascii_alphanumeric()) || key.starts_with("Numpad") || matches!(key, "Space" | "Backspace" | "Enter" | "Tab" | "Esc" | "Comma" | "Period" | "Minus" | "Equal" | "Backquote" | "Slash" | "Backslash" | "BracketLeft" | "BracketRight" | "Semicolon" | "Quote")
}

/// Turn a key press into a verdict (§15.1): AltGr never matches, `Win` is for the global hotkey only, reserved
/// chords are refused, and a shortcut must not hijack plain typing.
pub fn interpret(vk: u16, m: Mods, scope: Scope) -> Outcome {
    if is_modifier(vk) {
        return Outcome::Wait;
    }
    if vk == VK_ESCAPE && !m.any() && !m.altgr {
        return Outcome::Cancel;
    }
    let Some(key) = key_name(vk) else { return Outcome::Reject("That key can't be used in a shortcut.".into()) };
    if m.altgr {
        return Outcome::Reject("AltGr combinations type characters, so they never trigger shortcuts. Use Ctrl+Alt with the left Alt key.".into());
    }
    if m.win && scope == Scope::App {
        return Outcome::Reject("Win chords are reserved for the global Quake hotkey.".into());
    }
    let chord = Chord { ctrl: m.ctrl, shift: m.shift, alt: m.alt, win: m.win, key };
    if let Some(why) = reserved(&chord) {
        return Outcome::Reject(why.into());
    }
    let strong = m.ctrl || m.alt || m.win;
    let ok = match scope {
        Scope::Global => strong,
        // F-keys alone are fine; Shift+navigation keys too (Shift+PageUp, Shift+Insert); text keys need Ctrl/Alt.
        Scope::App => strong || (m.shift && !types_text(&chord.key)) || (chord.key.starts_with('F') && chord.key.len() > 1 && !types_text(&chord.key)),
    };
    if !ok {
        let why = match scope {
            Scope::Global => "A global hotkey needs Ctrl, Alt or Win so it doesn't steal normal typing in every app.",
            Scope::App => "Add Ctrl or Alt: a bare key (or Shift+key) would swallow normal typing.",
        };
        return Outcome::Reject(why.into());
    }
    Outcome::Chord(chord)
}

/// Modifier state exactly as the router computes it (`ut_term::input::mods`): AltGr = Ctrl + right Alt.
pub fn mods_with(state: &impl Fn(u16) -> i16) -> Mods {
    let down = |vk| state(vk) < 0;
    let (ctrl, shift, alt) = (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU));
    let altgr = ctrl && down(VK_RMENU);
    Mods { ctrl: ctrl && !altgr, shift, alt: alt && !altgr, win: down(VK_LWIN) || down(VK_RWIN), altgr }
}

/// Edge detector over the keyboard: returns the virtual key that went down since the last poll, with the modifiers.
pub struct Poller {
    prev: Vec<bool>,
}

impl Default for Poller {
    fn default() -> Self {
        Self { prev: vec![false; 256] }
    }
}

impl Poller {
    /// Start recording: keys that are already down (e.g. the Enter that activated the "Record" button) don't count
    /// until they are released and pressed again.
    pub fn prime(&mut self) {
        self.prime_with(&real_state);
    }

    pub fn poll(&mut self) -> Option<(u16, Mods)> {
        self.poll_with(&real_state)
    }

    pub fn mods(&self) -> Mods {
        mods_with(&real_state)
    }

    pub fn prime_with(&mut self, state: &impl Fn(u16) -> i16) {
        for vk in 0..256u16 {
            self.prev[vk as usize] = state(vk) < 0;
        }
    }

    pub fn poll_with(&mut self, state: &impl Fn(u16) -> i16) -> Option<(u16, Mods)> {
        let mut hit = None;
        for vk in 8..255u16 {
            let down = state(vk) < 0;
            if down && !self.prev[vk as usize] && !is_modifier(vk) && hit.is_none() {
                hit = Some(vk);
            }
            self.prev[vk as usize] = down;
        }
        hit.map(|vk| (vk, mods_with(state)))
    }
}

fn real_state(vk: u16) -> i16 {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { GetAsyncKeyState(vk as i32) }
}

/// A "press a shortcut" session in the UI: owns the keyboard poller and the last rejection message.
#[derive(Default)]
pub struct Recorder {
    pub active: bool,
    pub notice: Option<String>,
    poller: Poller,
}

impl Recorder {
    pub fn start(&mut self) {
        self.active = true;
        self.notice = None;
        self.poller.prime();
    }

    pub fn stop(&mut self) {
        self.active = false;
    }

    /// The modifiers held right now (`Ctrl+Shift+`), shown while recording.
    pub fn held(&self) -> String {
        self.poller.mods().prefix()
    }

    /// Call once per frame while `active`. While recording no egui widget may react to the keys (Tab would move the
    /// focus, Enter/Space would press the focused button): an invisible focus sink swallows them. Returns the
    /// final verdict (`Chord` or `Cancel`); rejected keys set [`Recorder::notice`] and recording continues.
    pub fn step(&mut self, ui: &mut egui::Ui, scope: Scope) -> Option<Outcome> {
        if !self.active {
            return None;
        }
        let id = egui::Id::new("chord-recorder-sink");
        let r = ui.interact(egui::Rect::from_min_size(ui.cursor().min, egui::Vec2::ZERO), id, egui::Sense::focusable_noninteractive());
        let _ = r;
        ui.memory_mut(|m| {
            m.request_focus(id);
            m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true });
        });
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(8));
        if ui.ctx().input(|i| i.viewport().focused) == Some(false) {
            return None; // keys typed into another window are not ours
        }
        let (vk, m) = self.poller.poll()?;
        match interpret(vk, m, scope) {
            Outcome::Wait => None,
            Outcome::Reject(why) => {
                self.notice = Some(why);
                None
            }
            done => {
                self.active = false;
                Some(done)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    fn mods(ctrl: bool, shift: bool, alt: bool) -> Mods {
        Mods { ctrl, shift, alt, ..Default::default() }
    }

    #[test]
    fn key_names_invert_the_routers_table() {
        for (vk, name) in [(0x54, "T"), (0x31, "1"), (0xBC, "Comma"), (0xBB, "Equal"), (0xC0, "Backquote"), (0x70, "F1"), (0x87, "F24"), (0x60, "Numpad0"), (0x69, "Numpad9"), (0x21, "PageUp"), (0x26, "Up"), (0x09, "Tab"), (0x1B, "Esc")] {
            assert_eq!(key_name(vk).as_deref(), Some(name), "{vk:#x}");
        }
        // every nameable key round-trips through the router's own mapping, so a recorded chord always matches
        let mut n = 0;
        for vk in 0..256u16 {
            if let Some(name) = key_name(vk) {
                assert_eq!(vks_for(&name), vec![vk], "{name}");
                assert!(name.parse::<Chord>().is_ok(), "{name} must be a valid chord key");
                n += 1;
            }
        }
        assert_eq!(n, 10 + 26 + 10 + 24 + NAMED.len(), "digits, letters, numpad, F1-F24 and the named keys");
        assert_eq!(key_name(0xE2), None, "ISO extra key");
        assert_eq!(key_name(0xAF), None, "volume key");
    }

    #[test]
    fn modifiers_alone_wait_and_bare_esc_cancels() {
        assert_eq!(interpret(0x11, mods(true, false, false), Scope::App), Outcome::Wait);
        assert_eq!(interpret(0xA2, mods(true, false, false), Scope::App), Outcome::Wait);
        assert_eq!(interpret(0x1B, Mods::default(), Scope::App), Outcome::Cancel);
        assert!(matches!(interpret(0x1B, mods(true, false, false), Scope::App), Outcome::Reject(_)), "Ctrl+Esc is the Start menu");
        assert!(matches!(interpret(0x1B, mods(false, true, false), Scope::App), Outcome::Reject(_)), "Shift+Esc would swallow Esc handling");
    }

    #[test]
    fn app_chords_are_canonical() {
        let ok = |vk, m| match interpret(vk, m, Scope::App) {
            Outcome::Chord(c) => c.to_string(),
            o => panic!("{o:?}"),
        };
        assert_eq!(ok(0x54, mods(true, false, false)), "Ctrl+T");
        assert_eq!(ok(0x54, mods(true, true, true)), "Ctrl+Shift+Alt+T");
        assert_eq!(ok(0xBB, mods(false, true, true)), "Shift+Alt+Equal");
        assert_eq!(ok(0x26, mods(true, false, true)), "Ctrl+Alt+Up");
        assert_eq!(ok(0x21, mods(false, true, false)), "Shift+PageUp");
        assert_eq!(ok(0x2D, mods(false, true, false)), "Shift+Insert");
        assert_eq!(ok(0x7B, mods(false, false, false)), "F12");
        assert_eq!(ok(0x69, mods(true, false, true)), "Ctrl+Alt+Numpad9");
        assert_eq!(ok(0xBC, mods(true, false, false)), "Ctrl+Comma");
        // the canonical text parses back to the same chord
        assert!("Ctrl+Shift+Alt+T".parse::<Chord>().is_ok());
    }

    #[test]
    fn typing_keys_need_ctrl_or_alt() {
        for vk in [0x41, 0x35, 0x20, 0xBC, 0x0D, 0x08] {
            assert!(matches!(interpret(vk, Mods::default(), Scope::App), Outcome::Reject(_)), "{vk:#x} bare");
            assert!(matches!(interpret(vk, mods(false, true, false), Scope::App), Outcome::Reject(_)), "{vk:#x} + Shift");
            if vk != 0x20 {
                assert!(matches!(interpret(vk, mods(false, false, true), Scope::App), Outcome::Chord(_)), "{vk:#x} + Alt");
            }
            assert!(matches!(interpret(vk, mods(true, false, false), Scope::App), Outcome::Chord(_)), "{vk:#x} + Ctrl");
        }
    }

    #[test]
    fn altgr_and_win_and_reserved_are_refused() {
        let altgr = Mods { ctrl: false, alt: false, altgr: true, ..Default::default() };
        let Outcome::Reject(m) = interpret(0x32, altgr, Scope::App) else { panic!() };
        assert!(m.contains("AltGr"), "{m}");
        assert!(matches!(interpret(0x32, altgr, Scope::Global), Outcome::Reject(_)));
        let win = Mods { win: true, ..Default::default() };
        assert!(matches!(interpret(0xC0, win, Scope::App), Outcome::Reject(_)), "Win only for the global hotkey");
        match interpret(0xC0, win, Scope::Global) {
            Outcome::Chord(c) => assert_eq!(c.to_string(), "Win+Backquote"),
            o => panic!("{o:?}"),
        }
        assert!(matches!(interpret(0x73, mods(false, false, true), Scope::App), Outcome::Reject(_)), "Alt+F4");
        assert!(matches!(interpret(0x09, mods(false, false, true), Scope::App), Outcome::Reject(_)), "Alt+Tab");
        assert!(matches!(interpret(0x43, mods(true, false, false), Scope::App), Outcome::Reject(_)), "Ctrl+C");
        assert!(matches!(interpret(0x43, mods(true, true, false), Scope::App), Outcome::Chord(_)), "Ctrl+Shift+C is the copy default");
    }

    #[test]
    fn global_hotkeys_need_a_strong_modifier_and_register() {
        assert!(matches!(interpret(0x7B, Mods::default(), Scope::Global), Outcome::Reject(_)));
        assert!(matches!(interpret(0x41, mods(false, true, false), Scope::Global), Outcome::Reject(_)));
        for vk in 0..256u16 {
            if let Outcome::Chord(c) = interpret(vk, mods(true, false, false), Scope::Global) {
                if reserved(&c).is_none() && vk != 0x5B {
                    assert!(crate::winhooks::parse_hotkey(&c.to_string()).is_ok(), "global-hotkey cannot parse {c}");
                }
            }
        }
    }

    #[test]
    fn modifier_state_matches_the_router() {
        let keys = |held: &'static [u16]| move |vk: u16| if held.contains(&vk) { -32768i16 } else { 0 };
        assert_eq!(mods_with(&keys(&[VK_CONTROL, VK_SHIFT])), Mods { ctrl: true, shift: true, ..Default::default() });
        assert_eq!(mods_with(&keys(&[VK_CONTROL, VK_MENU])), Mods { ctrl: true, alt: true, ..Default::default() }, "left Ctrl+Alt is a chord");
        assert_eq!(mods_with(&keys(&[VK_CONTROL, VK_MENU, VK_RMENU])), Mods { altgr: true, ..Default::default() }, "Ctrl + right Alt is AltGr");
        assert!(mods_with(&keys(&[VK_RWIN])).win);
        assert_eq!(mods_with(&keys(&[VK_CONTROL, VK_SHIFT])).prefix(), "Ctrl+Shift+");
        assert_eq!(Mods::default().prefix(), "");
    }

    #[test]
    fn poller_reports_each_press_once_and_ignores_held_keys() {
        let held = RefCell::new(HashSet::<u16>::new());
        let state = |vk: u16| if held.borrow().contains(&vk) { -32768i16 } else { 0 };
        let mut p = Poller::default();
        held.borrow_mut().insert(0x0D); // Enter is down when recording starts (it activated the button)
        p.prime_with(&state);
        assert_eq!(p.poll_with(&state), None, "a key held since before recording is not a press");
        held.borrow_mut().remove(&0x0D);
        assert_eq!(p.poll_with(&state), None);
        held.borrow_mut().extend([VK_CONTROL, 0x54]);
        let (vk, m) = p.poll_with(&state).expect("Ctrl+T pressed");
        assert_eq!((vk, m.ctrl), (0x54, true));
        assert_eq!(p.poll_with(&state), None, "still held: no repeat");
        held.borrow_mut().remove(&0x54);
        assert_eq!(p.poll_with(&state), None);
        held.borrow_mut().insert(0x54);
        assert!(p.poll_with(&state).is_some(), "pressed again");
        held.borrow_mut().clear();
        held.borrow_mut().insert(VK_SHIFT);
        assert_eq!(p.poll_with(&state), None, "a lone modifier is not a press");
    }
}
