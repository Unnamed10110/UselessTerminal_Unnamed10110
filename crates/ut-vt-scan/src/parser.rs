//! Minimal streaming VT tokenizer shared by the scanner and the log stripper.
//!
//! Hand-written instead of built on `vte`, because vte 0.15 (std) buffers OSC payloads in an *unbounded*
//! `Vec`, silently drops everything after the 16th `;` of an OSC and exposes no parser state. None of the
//! spec's caps (§6.1: 8 KiB, 1 MiB for OSC 52) nor a fast skip over plain text can be layered on top of it.
//! Semantics follow vte/xterm: C0 controls run inside sequences, ESC always restarts, CAN/SUB abort,
//! BEL or `ESC \` end an OSC, DCS/SOS/PM/APC end only with ST, and C1 bytes (0x80..) are never controls
//! (they are UTF-8 payload).

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    EscInter,
    Csi,
    Osc,
    /// DCS, SOS, PM or APC: skipped until ST.
    Str,
}

/// CSI parameters we keep; longer sequences are ignored (the scanner only needs `?1049;2004h`).
const MAX_CSI: usize = 64;

/// Receives tokens; every method defaults to a no-op so each consumer only implements what it uses.
pub(crate) trait Handler {
    /// A run of bytes with no C0 control, ESC or DEL (may hold partial UTF-8; may end mid-char).
    fn text(&mut self, _run: &[u8]) {}
    /// A C0 control (or DEL) executed in the ground state or inside a sequence. BEL is `0x07`.
    fn control(&mut self, _b: u8) {}
    /// `ESC <final>` with no intermediates and not one of `[ ] P X ^ _`.
    fn esc(&mut self, _fin: u8) {}
    /// A complete CSI sequence: parameter+intermediate bytes (`?1049;1`) and the final byte.
    fn csi(&mut self, _params: &[u8], _fin: u8) {}
    fn osc_start(&mut self) {}
    /// A slice of OSC payload (the bytes between `ESC ]` and the terminator, controls removed).
    fn osc_bytes(&mut self, _run: &[u8]) {}
    /// `ok` is false when the OSC was cancelled by CAN/SUB.
    fn osc_end(&mut self, _ok: bool) {}
    /// Called before a control/escape/CSI/OSC token is dispatched with the number of bytes of the CURRENT
    /// `advance` slice consumed up to and including that token's byte (offset just after it).
    fn pos(&mut self, _after: usize) {}
}

#[derive(Default)]
pub(crate) struct Parser {
    state: State,
    csi: Vec<u8>,
    csi_overflow: bool,
}

impl Parser {
    pub(crate) fn advance<H: Handler>(&mut self, h: &mut H, mut b: &[u8]) {
        let total = b.len();
        while let Some(&c) = b.first() {
            // Length of the run that cannot change state. This is the hot path: plain output is skipped
            // here without touching the state machine.
            let n = match self.state {
                State::Ground => b.iter().position(|&c| c < 0x20 || c == 0x7f),
                State::Osc => b.iter().position(|&c| c < 0x20),
                State::Str => b.iter().position(|&c| matches!(c, 0x18 | 0x1a | 0x1b)),
                _ => Some(0),
            }
            .unwrap_or(b.len());
            if n > 0 {
                match self.state {
                    State::Ground => h.text(&b[..n]),
                    State::Osc => h.osc_bytes(&b[..n]),
                    _ => {}
                }
                b = &b[n..];
            } else {
                h.pos(total - b.len() + 1);
                self.step(h, c);
                b = &b[1..];
            }
        }
    }

    fn step<H: Handler>(&mut self, h: &mut H, c: u8) {
        use State::*;
        match (self.state, c) {
            (Osc, 0x07) => {
                h.osc_end(true);
                self.state = Ground;
            }
            (Osc, 0x1b) => {
                h.osc_end(true);
                self.state = Escape;
            }
            (Osc, 0x18 | 0x1a) => {
                h.osc_end(false);
                self.state = Ground;
            }
            (Osc, _) => {} // other C0 inside an OSC are dropped
            (_, 0x1b) => self.state = Escape,
            (_, 0x18 | 0x1a) => self.state = Ground,
            (Str, _) => {}
            (Ground, _) => h.control(c),
            (Escape, b'[') => {
                self.csi.clear();
                self.csi_overflow = false;
                self.state = Csi;
            }
            (Escape, b']') => {
                h.osc_start();
                self.state = Osc;
            }
            (Escape, b'P' | b'X' | b'^' | b'_') => self.state = Str,
            (Escape | EscInter, 0x20..=0x2f) => self.state = EscInter,
            (Escape, 0x30..=0x7e) => {
                h.esc(c);
                self.state = Ground;
            }
            (EscInter, 0x30..=0x7e) => self.state = Ground,
            (_, 0..=0x1f) => h.control(c),
            (_, 0x7f) => {}
            (Csi, 0x20..=0x3f) => {
                if self.csi.len() < MAX_CSI {
                    self.csi.push(c);
                } else {
                    self.csi_overflow = true;
                }
            }
            (Csi, 0x40..=0x7e) => {
                if !self.csi_overflow {
                    h.csi(&self.csi, c);
                }
                self.state = Ground;
            }
            // A non-ASCII byte aborts the sequence and is reprocessed as text (it may start a UTF-8 char).
            _ => {
                self.state = Ground;
                h.text(&[c]);
            }
        }
    }
}
