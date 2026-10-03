//! Backend pane-state scanner (§3.5 step 3, §4.4, §6): cheap, synchronous and streaming. It extracts
//! title, cwd, shell-integration turns, bell, OSC 52 and alt-screen changes from raw PTY bytes and never
//! decodes the stream as UTF-8 (only OSC payloads, which are buffered and capped).

use std::time::Instant;

use base64::{
    alphabet,
    engine::{general_purpose::GeneralPurposeConfig, DecodePaddingMode, GeneralPurpose},
    Engine,
};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};

use crate::parser::{Handler, Parser};

/// §6.1: ordinary OSC payloads are capped at 8 KiB (longer ones are dropped).
const OSC_MAX: usize = 8 << 10;
/// §6.1: OSC 52 clipboard writes are capped at 1 MiB *decoded*.
pub const OSC52_MAX_DECODED: usize = 1 << 20;
/// Raw payload cap for OSC 52: base64 of the decoded cap plus the `52;<targets>;` prefix.
const OSC52_MAX_RAW: usize = OSC52_MAX_DECODED / 3 * 4 + 64;

// Lenient on padding: emitters disagree about `=`.
const B64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// `terminal.osc52` setting (§6.1). Default: write allowed, read denied.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Osc52 {
    Off,
    #[default]
    Write,
    ReadWrite,
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Used for the "local host" rule of §6.3.
    pub computer_name: String,
    pub osc52: Osc52,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ShellKind {
    PromptStart,
    InputStart,
    CommandStart,
    CommandEnd,
}

/// Something the pane's byte stream said. `Shell` events are emitted only for transitions that actually
/// fire (§6.2), so duplicates from prompt themes that also emit OSC 133 never reach the app.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    /// OSC 0/2.
    Title { title: String },
    /// OSC 7 (§6.3) or one of the aliases (9;9, 633;P;Cwd=, 1337;CurrentDir=; those have no host: local).
    /// A non-local host is only reported (§10.6); the title fallback is the app's job.
    Cwd { path: String, host: String, local: bool },
    /// OSC 133/633 A/B/C/D. `exit_code`/`duration_ms` are only set on `CommandEnd`; a bare `D` has no exit
    /// code and `duration_ms` needs a preceding `C`.
    Shell { kind: ShellKind, exit_code: Option<i32>, duration_ms: Option<u64> },
    /// BEL that is not an OSC terminator.
    Bell,
    /// OSC 52 write, base64-decoded (≤ 1 MiB). Never empty.
    Osc52Write { data: Vec<u8> },
    /// OSC 52 `?`. Only emitted when `Config::osc52 == ReadWrite`.
    Osc52ReadRequest,
    /// [P2] OSC 9;4: `state` 0 remove, 1 normal, 2 error, 3 indeterminate, 4 warning; `value` 0..=100.
    Progress { state: u8, value: u8 },
    /// [P2] OSC 9;text / OSC 777;notify;title;body.
    Notify { title: String, body: String },
    /// DECSET/DECRST 1049, 1047, 47 (only on change), or RIS while active.
    AltScreen { active: bool },
    /// At most once per `feed` call that saw any bytes, so the app can count output bursts (§12.3).
    Output,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    None,
    Prompt,
    Input,
    Running,
    Done,
}

/// Backend half of §6.2's turn (the line markers live in the frontend).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Turn {
    pub phase: Phase,
    pub exit_code: Option<i32>,
    /// Set by `C`; cleared by `A`.
    pub started_at: Option<Instant>,
    pub duration_ms: Option<u64>,
}

struct Inner {
    cfg: Config,
    /// OSC payload being accumulated (kept only while within its cap).
    buf: Vec<u8>,
    overflow: bool,
    alt: bool,
    turn: Turn,
}

pub struct Scanner {
    parser: Parser,
    st: Inner,
}

impl Scanner {
    pub fn new(cfg: Config) -> Self {
        let st = Inner { cfg, buf: Vec::new(), overflow: false, alt: false, turn: Turn::default() };
        Self { parser: Parser::default(), st }
    }

    /// Scans one PTY frame. State survives any chunk split. `now` stamps `C` and times `D`.
    pub fn feed(&mut self, bytes: &[u8], now: Instant, out: &mut Vec<Event>) {
        self.feed_at(bytes, now, out, None);
    }

    /// Like [`feed`](Self::feed), additionally pushing to `offsets` (parallel to `out`, for the events this
    /// call adds) the offset into `bytes` just after the sequence that produced each event (0 for `Output`).
    /// Lets an emulator advance up to an event and sample its own state (e.g. the cursor row at OSC 133;A).
    pub fn feed_at(&mut self, bytes: &[u8], now: Instant, out: &mut Vec<Event>, mut offsets: Option<&mut Vec<usize>>) {
        if bytes.is_empty() {
            return;
        }
        out.push(Event::Output);
        if let Some(o) = offsets.as_deref_mut() {
            o.push(0);
        }
        self.parser.advance(&mut Ctx { st: &mut self.st, out, now, offs: offsets, pos: 0 }, bytes);
    }

    pub fn turn(&self) -> &Turn {
        &self.st.turn
    }

    pub fn alt_screen(&self) -> bool {
        self.st.alt
    }
}

struct Ctx<'a> {
    st: &'a mut Inner,
    out: &'a mut Vec<Event>,
    now: Instant,
    offs: Option<&'a mut Vec<usize>>,
    pos: usize,
}

impl Ctx<'_> {
    fn emit(&mut self, e: Event) {
        if let Some(o) = self.offs.as_deref_mut() {
            o.push(self.pos);
        }
        self.out.push(e);
    }
}

impl Handler for Ctx<'_> {
    fn pos(&mut self, after: usize) {
        self.pos = after;
    }

    fn control(&mut self, b: u8) {
        if b == 0x07 {
            self.emit(Event::Bell);
        }
    }

    fn esc(&mut self, fin: u8) {
        if fin == b'c' {
            self.set_alt(false); // RIS
        }
    }

    fn csi(&mut self, params: &[u8], fin: u8) {
        if let (Some(modes), b'h' | b'l') = (params.strip_prefix(b"?"), fin) {
            for m in modes.split(|&c| c == b';') {
                if matches!(m, b"1049" | b"1047" | b"47") {
                    self.set_alt(fin == b'h');
                }
            }
        }
    }

    fn osc_start(&mut self) {
        self.st.buf.clear();
        self.st.overflow = false;
    }

    fn osc_bytes(&mut self, mut run: &[u8]) {
        let st = &mut *self.st;
        if st.overflow {
            return;
        }
        if st.buf.len() < 3 {
            // Learn the OSC number before choosing the cap (the first run may hold the whole payload).
            let n = (3 - st.buf.len()).min(run.len());
            st.buf.extend_from_slice(&run[..n]);
            run = &run[n..];
        }
        let cap = if st.buf.starts_with(b"52;") { OSC52_MAX_RAW } else { OSC_MAX };
        if st.buf.len() + run.len() > cap {
            st.overflow = true;
            st.buf = Vec::new(); // an unterminated flood must not stay allocated
        } else {
            st.buf.extend_from_slice(run);
        }
    }

    fn osc_end(&mut self, ok: bool) {
        if !ok || self.st.overflow {
            return;
        }
        let buf = std::mem::take(&mut self.st.buf);
        self.osc(&buf);
        if buf.capacity() <= 2 * OSC_MAX {
            self.st.buf = buf; // keep the allocation unless OSC 52 blew it up
        }
    }
}

impl Ctx<'_> {
    fn set_alt(&mut self, on: bool) {
        if self.st.alt != on {
            self.st.alt = on;
            self.emit(Event::AltScreen { active: on });
        }
    }

    fn osc(&mut self, p: &[u8]) {
        let (num, rest) = split(p);
        match num {
            b"0" | b"2" => self.emit(Event::Title { title: lossy(rest) }),
            b"7" => {
                let (path, host, local) = normalize_osc7(&lossy(rest), &self.st.cfg.computer_name);
                self.cwd(path, host, local);
            }
            b"9" => self.osc9(rest),
            b"52" => self.osc52(rest),
            b"133" => self.shell(rest, false),
            b"633" => self.shell(rest, true),
            b"777" => {
                let (cmd, args) = split(rest);
                if cmd == b"notify" {
                    let (title, body) = split(args);
                    self.emit(Event::Notify { title: lossy(title), body: lossy(body) });
                }
            }
            b"1337" => {
                if let Some(p) = rest.strip_prefix(b"CurrentDir=") {
                    self.cwd_plain(p);
                }
            }
            _ => {}
        }
    }

    fn cwd(&mut self, path: String, host: String, local: bool) {
        if !path.is_empty() {
            self.emit(Event::Cwd { path, host, local });
        }
    }

    /// The aliases carry a plain path (ConEmu/WT may quote it), not a `file://` URL.
    fn cwd_plain(&mut self, p: &[u8]) {
        let p = lossy(p);
        self.cwd(drive_path(p.trim_matches('"')), String::new(), true);
    }

    fn osc9(&mut self, rest: &[u8]) {
        let (sub, args) = split(rest);
        match sub {
            b"9" => self.cwd_plain(args),
            b"4" => {
                let mut f = args.split(|&c| c == b';').map(|f| std::str::from_utf8(f).ok()?.parse::<u8>().ok());
                if let Some(Some(state @ 0..=4)) = f.next() {
                    let value = f.next().flatten().unwrap_or(0).min(100);
                    self.emit(Event::Progress { state, value });
                }
            }
            b"" => {}
            // Other ConEmu sub-commands (`9;12` prompt mark, `9;1;ms` sleep, ...) are not notifications.
            s if s.iter().all(u8::is_ascii_digit) => {}
            _ => self.emit(Event::Notify { title: String::new(), body: lossy(rest) }),
        }
    }

    fn osc52(&mut self, rest: &[u8]) {
        let mode = self.st.cfg.osc52;
        let (_targets, data) = split(rest);
        if mode == Osc52::Off {
            return;
        }
        if data == b"?" {
            if mode == Osc52::ReadWrite {
                self.emit(Event::Osc52ReadRequest);
            }
            return;
        }
        // An empty payload means "clear the clipboard" in xterm; we never wipe the user's clipboard.
        match B64.decode(data) {
            Ok(d) if !d.is_empty() && d.len() <= OSC52_MAX_DECODED => self.emit(Event::Osc52Write { data: d }),
            _ => {}
        }
    }

    /// OSC 133 / 633 (`vscode`: also `P;Cwd=`). §6.2: transitions fire only from their predecessor.
    fn shell(&mut self, rest: &[u8], vscode: bool) {
        let (kind, args) = split(rest);
        let kind = match kind {
            b"A" => ShellKind::PromptStart,
            b"B" => ShellKind::InputStart,
            b"C" => ShellKind::CommandStart,
            b"D" => ShellKind::CommandEnd,
            b"P" if vscode => {
                if let Some(p) = args.strip_prefix(b"Cwd=") {
                    self.cwd_plain(p);
                }
                return;
            }
            _ => return,
        };
        let (alt, now) = (self.st.alt, self.now);
        let t = &mut self.st.turn;
        match kind {
            ShellKind::PromptStart if !alt && t.phase != Phase::Prompt => {
                *t = Turn { phase: Phase::Prompt, ..Turn::default() }
            }
            ShellKind::InputStart if t.phase == Phase::Prompt => t.phase = Phase::Input,
            ShellKind::CommandStart if t.phase == Phase::Input => {
                t.phase = Phase::Running;
                t.started_at = Some(now);
            }
            ShellKind::CommandEnd if t.phase != Phase::Done => {
                t.phase = Phase::Done;
                // `D;0;extra` is tolerated by cutting at `;`; a bare `D` means "no exit code".
                t.exit_code = parse_exit(split(args).0);
                t.duration_ms = t.started_at.map(|s| now.saturating_duration_since(s).as_millis() as u64);
            }
            _ => return,
        }
        let (exit_code, duration_ms) =
            if kind == ShellKind::CommandEnd { (t.exit_code, t.duration_ms) } else { (None, None) };
        self.emit(Event::Shell { kind, exit_code, duration_ms });
    }
}

/// `(before first ';', after it)`; the second part is empty when there is no `;`.
fn split(b: &[u8]) -> (&[u8], &[u8]) {
    match b.iter().position(|&c| c == b';') {
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (b, &[]),
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Windows exit codes show up both signed and unsigned (`3221225786` == `-1073741510`).
fn parse_exit(f: &[u8]) -> Option<i32> {
    let v: i64 = std::str::from_utf8(f).ok()?.parse().ok()?;
    i32::try_from(v).ok().or_else(|| u32::try_from(v).ok().map(|u| u as i32))
}

/// Rules 4 and 5 of §6.3: `/C:/x` or `\C:\x` → `C:\x`, and drive paths get backslashes.
fn drive_path(p: &str) -> String {
    let drive = |s: &[u8]| {
        s.len() >= 2 && s[0].is_ascii_alphabetic() && s[1] == b':' && s.get(2).is_none_or(|c| matches!(c, b'/' | b'\\'))
    };
    let p = match p.as_bytes() {
        [b'/' | b'\\', rest @ ..] if drive(rest) => &p[1..],
        _ => p,
    };
    if !drive(p.as_bytes()) {
        return p.to_string();
    }
    let mut r = p.replace('/', "\\");
    if r.len() == 2 {
        r.push('\\'); // bare `C:` is drive-relative; the root is what the shell meant
    }
    r
}

/// §6.3: normalises the value of OSC 7 into `(path, host, local)`.
/// `file:///C:/x` and `file:///C:\x` → `C:\x`; `file://host/home/u` → (`/home/u`, `host`);
/// Git Bash `/c/Users/x` stays POSIX. The host is local if it is empty, `localhost`, `127.0.0.1`, `::1`
/// or the computer name (case-insensitive). Without a `file://` prefix the whole value is the path.
pub fn normalize_osc7(raw: &str, computer_name: &str) -> (String, String, bool) {
    let (host, path) = match raw.get(..7).filter(|p| p.eq_ignore_ascii_case("file://")) {
        Some(_) => {
            let rest = &raw[7..];
            // The host is whatever precedes the first slash, unless the path starts right away.
            rest.split_at(rest.find(['/', '\\']).unwrap_or(rest.len()))
        }
        None => ("", raw),
    };
    // A failed decode (e.g. `%E9`, not UTF-8) keeps the raw text.
    let path = percent_decode_str(path).decode_utf8().map_or_else(|_| path.to_string(), |p| p.into_owned());
    let h = host.trim_start_matches('[').trim_end_matches(']');
    let local = h.is_empty()
        || ["localhost", "127.0.0.1", "::1"].iter().any(|l| h.eq_ignore_ascii_case(l))
        || (!computer_name.is_empty() && h.eq_ignore_ascii_case(computer_name));
    (drive_path(&path), host.to_string(), local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg(osc52: Osc52) -> Config {
        Config { computer_name: "MyPC".into(), osc52 }
    }

    /// Feeds each chunk and returns the events without `Output`.
    fn run(s: &mut Scanner, chunks: &[&[u8]]) -> Vec<Event> {
        let t0 = Instant::now();
        let mut out = Vec::new();
        for c in chunks {
            s.feed(c, t0, &mut out);
        }
        out.retain(|e| *e != Event::Output);
        out
    }

    fn scan(chunks: &[&[u8]]) -> Vec<Event> {
        run(&mut Scanner::new(cfg(Osc52::Write)), chunks)
    }

    fn title(t: &str) -> Event {
        Event::Title { title: t.into() }
    }

    fn cwd(path: &str, host: &str, local: bool) -> Event {
        Event::Cwd { path: path.into(), host: host.into(), local }
    }

    fn sh(kind: ShellKind, exit_code: Option<i32>, duration_ms: Option<u64>) -> Event {
        Event::Shell { kind, exit_code, duration_ms }
    }

    #[test]
    fn osc7_forms() {
        let n = |s: &str| normalize_osc7(s, "MyPC");
        let t = |p: &str, h: &str, l: bool| (p.to_string(), h.to_string(), l);
        assert_eq!(n("file:///C:/x"), t("C:\\x", "", true));
        assert_eq!(n("file:///C:\\x"), t("C:\\x", "", true)); // cmd
        assert_eq!(n("file:///C:/Users/me/My%20Docs"), t("C:\\Users\\me\\My Docs", "", true));
        assert_eq!(n("file://host/home/u"), t("/home/u", "host", false));
        assert_eq!(n("file://HOST/home/u%20x"), t("/home/u x", "HOST", false));
        assert_eq!(n("file://mypc/home/u"), t("/home/u", "mypc", true)); // computer name, case-insensitive
        assert_eq!(n("file://localhost/home/u"), t("/home/u", "localhost", true));
        assert_eq!(n("file://127.0.0.1/home/u"), t("/home/u", "127.0.0.1", true));
        assert_eq!(n("file://[::1]/home/u"), t("/home/u", "[::1]", true));
        // Git Bash stays POSIX; the file-drop code maps it (§11.2).
        assert_eq!(n("file:///c/Users/x"), t("/c/Users/x", "", true));
        // Bad escapes keep the raw text: a lone `%`, an invalid hex pair, bytes that are not UTF-8.
        assert_eq!(n("file:///C:/100%").0, "C:\\100%");
        assert_eq!(n("file:///C:/a%zzb").0, "C:\\a%zzb");
        assert_eq!(n("file:///C:/caf%E9").0, "C:\\caf%E9");
        assert_eq!(n("file:///C:").0, "C:\\");
        assert_eq!(n("file://host").0, "");
        // No prefix: the value is already a path.
        assert_eq!(n("C:\\x"), t("C:\\x", "", true));
        assert_eq!(n("/home/u"), t("/home/u", "", true));
    }

    #[test]
    fn osc7_event_and_empty_computer_name() {
        let e = scan(&[b"\x1b]7;file://srv/home/u\x07"]);
        assert_eq!(e, [cwd("/home/u", "srv", false)]);
        // An empty computer name must not make every host "local".
        assert!(!normalize_osc7("file://srv/x", "").2);
        // `;` inside a path survives.
        assert_eq!(scan(&[b"\x1b]7;file:///C:/a;b\x1b\\"]), [cwd("C:\\a;b", "", true)]);
    }

    #[test]
    fn cwd_aliases() {
        assert_eq!(scan(&[b"\x1b]9;9;\"C:\\Users\\me\"\x07"]), [cwd("C:\\Users\\me", "", true)]);
        assert_eq!(scan(&[b"\x1b]9;9;C:\\a;b\x1b\\"]), [cwd("C:\\a;b", "", true)]);
        assert_eq!(scan(&[b"\x1b]633;P;Cwd=C:/Users/me\x07"]), [cwd("C:\\Users\\me", "", true)]);
        assert_eq!(scan(&[b"\x1b]1337;CurrentDir=/home/u\x07"]), [cwd("/home/u", "", true)]);
        assert_eq!(scan(&[b"\x1b]1337;RemoteHost=a@b\x07\x1b]633;E;ls\x07"]), []);
    }

    #[test]
    fn title_and_terminators() {
        assert_eq!(scan(&[b"\x1b]0;hello\x07"]), [title("hello")]);
        assert_eq!(scan(&[b"\x1b]2;hello\x1b\\"]), [title("hello")]);
        // Semicolons are kept, even more than vte's 16-param limit.
        let many = "a;".repeat(40);
        assert_eq!(scan(&[format!("\x1b]0;{many}\x07").as_bytes()]), [title(&many)]);
        assert_eq!(scan(&[b"\x1b]0;\x07"]), [title("")]);
        // Non-ASCII titles, split in the middle of a character.
        let t = "caf\u{e9} \u{20ac} \u{1f600}";
        let full = [&b"\x1b]0;"[..], t.as_bytes(), b"\x07"].concat();
        for i in 0..full.len() {
            assert_eq!(scan(&[&full[..i], &full[i..]]), [title(t)], "split {i}");
        }
        // Invalid UTF-8 in a title is replaced, never a panic.
        assert_eq!(scan(&[b"\x1b]0;a\xffb\x07"]), [title("a\u{fffd}b")]);
    }

    #[test]
    fn bell_vs_osc_terminator() {
        assert_eq!(scan(&[b"\x07"]), [Event::Bell]);
        assert_eq!(scan(&[b"a\x07b\x07"]), [Event::Bell, Event::Bell]);
        assert_eq!(scan(&[b"\x1b]0;t\x07"]), [title("t")]); // terminator: no bell
        assert_eq!(scan(&[b"\x1b]0;t\x1b\\"]), [title("t")]);
        assert_eq!(scan(&[b"\x1b]0;t\x07\x07"]), [title("t"), Event::Bell]);
        assert_eq!(scan(&[b"\x1b[31m\x07"]), [Event::Bell]);
        // BEL inside a DCS/APC string is payload, not a bell.
        assert_eq!(scan(&[b"\x1bPq\x07\x1b\\\x1b_a\x07b\x1b\\"]), []);
    }

    #[test]
    fn cancelled_osc_emits_nothing_and_stream_recovers() {
        assert_eq!(scan(&[b"\x1b]0;lost\x18ok\x1b]0;kept\x07"]), [title("kept")]);
        assert_eq!(scan(&[b"\x1b]0;lost\x1aok"]), []);
        // ESC followed by something else ends the OSC and starts a new sequence.
        assert_eq!(scan(&[b"\x1b]0;t\x1b[?1049h"]), [title("t"), Event::AltScreen { active: true }]);
    }

    #[test]
    fn osc_cap_8k_and_no_unbounded_buffering() {
        let mut s = Scanner::new(cfg(Osc52::Write));
        let fits = format!("\x1b]0;{}\x07", "a".repeat(OSC_MAX - 2));
        assert_eq!(run(&mut s, &[fits.as_bytes()]), [title(&"a".repeat(OSC_MAX - 2))]);
        let big = format!("\x1b]0;{}\x07", "a".repeat(OSC_MAX));
        assert_eq!(run(&mut s, &[big.as_bytes()]), []);
        // An unterminated flood stays bounded.
        let mut chunks = vec![b"\x1b]0;".to_vec()];
        chunks.extend((0..100).map(|_| vec![b'x'; 64 << 10]));
        let refs: Vec<&[u8]> = chunks.iter().map(|c| c.as_slice()).collect();
        assert_eq!(run(&mut s, &refs), []);
        assert!(s.st.buf.capacity() < 64 << 10);
        // The BEL terminates the flooded OSC (dropped, no bell); the next OSC works.
        assert_eq!(run(&mut s, &[b"\x07\x1b]0;back\x07"]), [title("back")]);
    }

    fn osc52(data: &[u8]) -> Vec<u8> {
        [&b"\x1b]52;c;"[..], B64.encode(data).as_bytes(), b"\x07"].concat()
    }

    #[test]
    fn osc52_write_read_off_and_caps() {
        let w = |d: &[u8]| Event::Osc52Write { data: d.to_vec() };
        assert_eq!(scan(&[&osc52(b"hello")]), [w(b"hello")]);
        assert_eq!(scan(&[b"\x1b]52;;aGk\x1b\\"]), [w(b"hi")]); // empty targets, unpadded
        assert_eq!(scan(&[b"\x1b]52;c;!!!not base64\x07"]), []);
        assert_eq!(scan(&[b"\x1b]52;c;\x07"]), []);
        // The 1 MiB cap is on the decoded size; the 8 KiB OSC cap does not apply.
        let max = vec![b'z'; OSC52_MAX_DECODED];
        assert_eq!(scan(&[&osc52(&max)]), [w(&max)]);
        assert_eq!(scan(&[&osc52(&vec![b'z'; OSC52_MAX_DECODED + 1])]), []);
        assert_eq!(scan(&[&osc52(&vec![b'z'; 3 * OSC52_MAX_DECODED])]), []);
        // Split in a few places.
        let msg = osc52(b"split me");
        assert_eq!(scan(&[&msg[..3], &msg[3..6], &msg[6..]]), [w(b"split me")]);
        // Read: denied unless readwrite. Off drops everything.
        let q = b"\x1b]52;c;?\x07";
        assert_eq!(scan(&[q]), []);
        assert_eq!(run(&mut Scanner::new(cfg(Osc52::ReadWrite)), &[q]), [Event::Osc52ReadRequest]);
        assert_eq!(run(&mut Scanner::new(cfg(Osc52::ReadWrite)), &[&osc52(b"x")]), [w(b"x")]);
        assert_eq!(run(&mut Scanner::new(cfg(Osc52::Off)), &[q, &osc52(b"x")]), []);
    }

    #[test]
    fn osc52_setting_serde_literals() {
        assert_eq!(serde_json::to_string(&Osc52::ReadWrite).unwrap(), "\"readwrite\"");
        assert_eq!(serde_json::from_str::<Osc52>("\"off\"").unwrap(), Osc52::Off);
    }

    #[test]
    fn turn_state_machine_full_cycle_with_duration() {
        let mut s = Scanner::new(cfg(Osc52::Write));
        let t0 = Instant::now();
        let mut out = Vec::new();
        let at = |ms| t0 + Duration::from_millis(ms);
        s.feed(b"\x1b]133;A\x07", at(0), &mut out);
        assert_eq!(s.turn().phase, Phase::Prompt);
        s.feed(b"\x1b]133;B\x07", at(10), &mut out);
        assert_eq!(s.turn().phase, Phase::Input);
        s.feed(b"\x1b]133;C\x07", at(1000), &mut out);
        assert_eq!(s.turn().phase, Phase::Running);
        assert_eq!(s.turn().started_at, Some(at(1000)));
        s.feed(b"\x1b]133;D;7\x07", at(3500), &mut out);
        assert_eq!(s.turn().phase, Phase::Done);
        assert_eq!((s.turn().exit_code, s.turn().duration_ms), (Some(7), Some(2500)));
        out.retain(|e| *e != Event::Output);
        assert_eq!(
            out,
            [
                sh(ShellKind::PromptStart, None, None),
                sh(ShellKind::InputStart, None, None),
                sh(ShellKind::CommandStart, None, None),
                sh(ShellKind::CommandEnd, Some(7), Some(2500)),
            ]
        );
        // A new prompt starts a fresh turn.
        out.clear();
        s.feed(b"\x1b]133;A\x07", at(4000), &mut out);
        assert_eq!(*s.turn(), Turn { phase: Phase::Prompt, ..Turn::default() });
    }

    #[test]
    fn duplicate_d_is_dropped_and_prompt_theme_cannot_double_count() {
        use ShellKind::*;
        // Our integration and a prompt theme (oh-my-posh/starship) both emit every marker.
        let e = scan(&[b"\x1b]133;A\x07\x1b]133;A\x07\x1b]133;B\x07\x1b]133;B\x07"]);
        assert_eq!(e, [sh(PromptStart, None, None), sh(InputStart, None, None)]);
        let e = scan(&[b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;D;0\x07"]);
        let ends = e.iter().filter(|e| matches!(e, Event::Shell { kind: CommandEnd, .. })).count();
        let starts = e.iter().filter(|e| matches!(e, Event::Shell { kind: CommandStart, .. })).count();
        assert_eq!((ends, starts), (1, 1));
    }

    #[test]
    fn b_before_a_and_c_before_b_are_ignored() {
        let mut s = Scanner::new(cfg(Osc52::Write));
        assert_eq!(run(&mut s, &[b"\x1b]133;B\x07"]), []);
        assert_eq!(s.turn().phase, Phase::None);
        assert_eq!(run(&mut s, &[b"\x1b]133;C\x07"]), []);
        assert_eq!(s.turn().phase, Phase::None);
        run(&mut s, &[b"\x1b]133;A\x07"]);
        assert_eq!(run(&mut s, &[b"\x1b]133;C\x07"]), []); // C needs input
        assert_eq!(s.turn().phase, Phase::Prompt);
        assert_eq!(s.turn().started_at, None);
    }

    #[test]
    fn a_is_ignored_on_alt_screen() {
        let mut s = Scanner::new(cfg(Osc52::Write));
        let e = run(&mut s, &[b"\x1b[?1049h\x1b]133;A\x07"]);
        assert_eq!(e, [Event::AltScreen { active: true }]);
        assert_eq!(s.turn().phase, Phase::None);
        let e = run(&mut s, &[b"\x1b[?1049l\x1b]133;A\x07"]);
        assert_eq!(e, [Event::AltScreen { active: false }, sh(ShellKind::PromptStart, None, None)]);
    }

    #[test]
    fn bare_d_and_d_with_extras() {
        use ShellKind::CommandEnd as D;
        assert_eq!(scan(&[b"\x1b]133;D\x07"]), [sh(D, None, None)]);
        assert_eq!(scan(&[b"\x1b]133;D;\x07"]), [sh(D, None, None)]);
        assert_eq!(scan(&[b"\x1b]133;D;0;extra\x07"]), [sh(D, Some(0), None)]);
        assert_eq!(scan(&[b"\x1b]133;D;-1\x07"]), [sh(D, Some(-1), None)]);
        assert_eq!(scan(&[b"\x1b]133;D;1;aid=3\x1b\\"]), [sh(D, Some(1), None)]);
        assert_eq!(scan(&[b"\x1b]133;D;abc\x07"]), [sh(D, None, None)]);
        // Windows exit codes, signed or unsigned.
        assert_eq!(scan(&[b"\x1b]133;D;3221225786\x07"]), [sh(D, Some(-1073741510), None)]);
        assert_eq!(scan(&[b"\x1b]133;D;99999999999\x07"]), [sh(D, None, None)]);
        // D without C has no duration even after a prompt (cmd emits only A/B and a bare D, §23.10).
        let e = scan(&[b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;D\x07"]);
        assert_eq!(e[2], sh(D, None, None));
    }

    #[test]
    fn osc633_aliases_133() {
        use ShellKind::*;
        let e = scan(&[b"\x1b]633;A\x07\x1b]633;B\x07\x1b]633;C\x07\x1b]633;E;ls\x07\x1b]633;D;3\x07"]);
        assert_eq!(
            e,
            [
                sh(PromptStart, None, None),
                sh(InputStart, None, None),
                sh(CommandStart, None, None),
                sh(CommandEnd, Some(3), Some(0)),
            ]
        );
        // Extra fields on A (kitty/iTerm style) are fine; unknown letters are ignored.
        assert_eq!(scan(&[b"\x1b]133;A;k=i\x07\x1b]133;Z\x07"]), [sh(PromptStart, None, None)]);
    }

    #[test]
    fn alt_screen_modes() {
        let on = Event::AltScreen { active: true };
        let off = Event::AltScreen { active: false };
        assert_eq!(scan(&[b"\x1b[?1049h"]), std::slice::from_ref(&on));
        assert_eq!(scan(&[b"\x1b[?1049h\x1b[?1049h\x1b[?1049l"]), [on.clone(), off.clone()]); // only changes
        assert_eq!(scan(&[b"\x1b[?1047h\x1b[?1047l"]), [on.clone(), off.clone()]);
        assert_eq!(scan(&[b"\x1b[?47h\x1b[?47l"]), [on.clone(), off.clone()]);
        assert_eq!(scan(&[b"\x1b[?2004;1049h"]), std::slice::from_ref(&on));
        assert_eq!(scan(&[b"\x1b[?25h\x1b[?2004h\x1b[1049h\x1b[?10490h"]), []); // other modes, not private
        assert_eq!(scan(&[b"\x1b[?1049h\x1bc"]), [on, off]); // RIS leaves the alt screen
    }

    #[test]
    fn progress_and_notify() {
        let p = |state, value| Event::Progress { state, value };
        assert_eq!(scan(&[b"\x1b]9;4;1;42\x07"]), [p(1, 42)]);
        assert_eq!(scan(&[b"\x1b]9;4;0\x07"]), [p(0, 0)]);
        assert_eq!(scan(&[b"\x1b]9;4;2;250\x07"]), [p(2, 100)]);
        assert_eq!(scan(&[b"\x1b]9;4;9;1\x07\x1b]9;4\x07"]), []);
        let n = |t: &str, b: &str| Event::Notify { title: t.into(), body: b.into() };
        assert_eq!(scan(&[b"\x1b]9;Build done; 3 warnings\x07"]), [n("", "Build done; 3 warnings")]);
        assert_eq!(scan(&[b"\x1b]777;notify;Title;Body; more\x07"]), [n("Title", "Body; more")]);
        // ConEmu sub-commands are not notifications.
        assert_eq!(scan(&[b"\x1b]9;12\x07\x1b]9;1;500\x07\x1b]9;\x07"]), []);
    }

    #[test]
    fn output_once_per_feed() {
        let mut s = Scanner::new(cfg(Osc52::Write));
        let mut out = Vec::new();
        s.feed(b"", Instant::now(), &mut out);
        assert!(out.is_empty());
        s.feed(b"hello\r\nworld\x07\x1b]0;t\x07", Instant::now(), &mut out);
        assert_eq!(out.iter().filter(|e| **e == Event::Output).count(), 1);
        s.feed(b"x", Instant::now(), &mut out);
        assert_eq!(out.iter().filter(|e| **e == Event::Output).count(), 2);
    }

    #[test]
    fn events_serialize_camel_case_with_type_tag() {
        let j = |e: &Event| serde_json::to_string(e).unwrap();
        assert_eq!(
            j(&sh(ShellKind::CommandEnd, Some(1), Some(2500))),
            r#"{"type":"shell","kind":"commandEnd","exitCode":1,"durationMs":2500}"#
        );
        assert_eq!(j(&cwd("C:\\x", "", true)), r#"{"type":"cwd","path":"C:\\x","host":"","local":true}"#);
        assert_eq!(j(&Event::AltScreen { active: true }), r#"{"type":"altScreen","active":true}"#);
        assert_eq!(j(&Event::Osc52ReadRequest), r#"{"type":"osc52ReadRequest"}"#);
        let back: Event = serde_json::from_str(&j(&title("x"))).unwrap();
        assert_eq!(back, title("x"));
    }

    /// A stream mixing every sequence kind, split at every offset (and byte by byte) must give identical
    /// events: the scanner keeps state across arbitrary chunk boundaries (§4.4).
    #[test]
    fn chunk_split_invariance() {
        let mut stream = Vec::new();
        for part in [
            &b"plain text \xe2\x82\xac\r\n"[..],
            b"\x1b]133;D;0\x07\x1b]133;A\x1b\\\x1b]7;file:///C:/My%20Docs\x1b\\",
            b"\x1b]0;caf\xc3\xa9 \xe2\x82\xac title\x07\x1b[?1049h\x1b]133;A\x07\x1b[?1049l",
            b"\x1b]133;A\x07\x1b]133;B\x07\x1b[38;2;1;2;3m\x07\x1b]133;C\x07",
            b"\x1b]9;4;1;50\x07\x1b]9;9;\"C:\\x\"\x1b\\\x1b]52;c;aGVsbG8=\x07\x1b]133;D;5\x07",
            b"\x1bPq#0;2;0;0;0\x1b\\\x1b_apc\x07x\x1b\\\x1b]0;a\x18b\x1b]777;notify;t;b\x07\x1bc",
        ] {
            stream.extend_from_slice(part);
        }
        let want = scan(&[&stream]);
        assert!(want.len() >= 14, "{want:?}");
        for i in 0..=stream.len() {
            assert_eq!(scan(&[&stream[..i], &stream[i..]]), want, "split at {i}");
        }
        assert_eq!(scan(&stream.chunks(1).collect::<Vec<_>>()), want);
    }

    #[test]
    fn chunk_split_invariance_keeps_turn_timing() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let (mut s, mut out) = (Scanner::new(cfg(Osc52::Write)), Vec::new());
        for b in b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07" {
            s.feed(&[*b], at(100), &mut out);
        }
        for b in b"\x1b]133;D;0\x1b\\" {
            s.feed(&[*b], at(600), &mut out);
        }
        assert_eq!(s.turn().duration_ms, Some(500));
    }

    /// `cargo test -p ut-vt-scan --release -- --ignored --nocapture throughput`
    #[test]
    #[ignore]
    fn throughput_plain_text() {
        let line = format!("{}\r\n", "The quick brown fox jumps over the lazy dog 0123456789 ".repeat(2));
        let frame: Vec<u8> = line.as_bytes().iter().copied().cycle().take(64 << 10).collect();
        let mut s = Scanner::new(Config::default());
        let (mut out, now) = (Vec::new(), Instant::now());
        let total = 1usize << 30;
        let t = Instant::now();
        for _ in 0..total / frame.len() {
            out.clear();
            s.feed(&frame, now, &mut out);
        }
        let mbs = total as f64 / (1 << 20) as f64 / t.elapsed().as_secs_f64();
        println!("scanner throughput: {mbs:.0} MiB/s");
        if !cfg!(debug_assertions) {
            assert!(mbs >= 200.0, "{mbs} MiB/s");
        }
    }
}
