//! ANSI-stripping sink for session logs (§12.1, §24 #7, #9): keeps only printable text, LF and TAB.
//!
//! CSI, OSC, DCS, APC, PM and SOS sequences are dropped with their payloads and terminators (the WPF
//! regex let `ESC ]` through, so OSC titles and BELs leaked into logs). CR LF becomes LF, and a bare CR
//! discards the pending partial line so spinners and progress bars log only their final state [P1].

use crate::parser::{Handler, Parser};
use crate::utf8::Utf8Stream;

/// A line that never ends (binary output) is released after this many bytes instead of growing forever.
const MAX_LINE: usize = 64 << 10;

#[derive(Default)]
struct Inner {
    utf8: Utf8Stream,
    /// The current line, held until its LF so a later bare CR can discard it.
    line: String,
    /// A CR was seen and the next thing decides whether it was half of CR LF or a bare CR.
    cr: bool,
    tmp: String,
}

#[derive(Default)]
pub struct LogStripper {
    parser: Parser,
    st: Inner,
}

impl LogStripper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends the plain text of `bytes` to `out`. Complete lines are released at their LF, so the text of
    /// an unfinished last line only appears once it is finished (or on [`finish`](Self::finish)).
    /// Any chunk split is fine, including inside an escape sequence or a multi-byte character.
    pub fn feed(&mut self, bytes: &[u8], out: &mut String) {
        self.parser.advance(&mut Sink { st: &mut self.st, out }, bytes);
    }

    /// End of log: releases the unfinished line (a trailing CR is not an overwrite) and any held partial char
    /// as U+FFFD.
    pub fn finish(&mut self, out: &mut String) {
        let mut sink = Sink { st: &mut self.st, out };
        sink.flush_utf8();
        sink.st.cr = false;
        sink.out.push_str(&sink.st.line);
        sink.st.line.clear();
    }
}

struct Sink<'a> {
    st: &'a mut Inner,
    out: &'a mut String,
}

impl Sink<'_> {
    /// Anything printable after a CR proves it was a bare CR: the line is being overwritten.
    fn resolve_cr(&mut self) {
        if std::mem::take(&mut self.st.cr) {
            self.st.line.clear();
        }
    }

    fn put(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.resolve_cr();
        // C1 controls (U+0080..U+009F) are not printable.
        self.st.line.extend(s.chars().filter(|c| !('\u{80}'..='\u{9f}').contains(c)));
        if self.st.line.len() > MAX_LINE {
            self.out.push_str(&self.st.line);
            self.st.line.clear();
        }
    }

    fn flush_utf8(&mut self) {
        let mut s = std::mem::take(&mut self.st.tmp);
        self.st.utf8.finish(&mut s);
        self.put(&s);
        s.clear();
        self.st.tmp = s;
    }
}

impl Handler for Sink<'_> {
    fn text(&mut self, run: &[u8]) {
        let mut s = std::mem::take(&mut self.st.tmp);
        self.st.utf8.push(run, &mut s);
        self.put(&s);
        s.clear();
        self.st.tmp = s;
    }

    fn control(&mut self, b: u8) {
        // A control byte can never continue a UTF-8 sequence: settle a held partial char first.
        self.flush_utf8();
        match b {
            b'\n' => {
                self.st.cr = false; // CR LF -> LF
                self.st.line.push('\n');
                self.out.push_str(&self.st.line);
                self.st.line.clear();
            }
            b'\r' => {
                self.resolve_cr(); // CR CR: the first one was bare
                self.st.cr = true;
            }
            b'\t' => {
                self.resolve_cr();
                self.st.line.push('\t');
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(chunks: &[&[u8]]) -> String {
        let (mut s, mut out) = (LogStripper::new(), String::new());
        for c in chunks {
            s.feed(c, &mut out);
        }
        s.finish(&mut out);
        out
    }

    #[test]
    fn wpf_regression_osc_title_leaves_nothing() {
        assert_eq!(strip(&[b"\x1b]0;title\x07"]), "");
        assert_eq!(strip(&[b"\x1b]0;title\x1b\\"]), "");
        assert_eq!(strip(&[b"a\x1b]0;title\x07b"]), "ab");
        assert_eq!(strip(&[b"\x1b]7;file:///C:/x\x07\x1b]133;A\x07prompt> \x1b]133;B\x07"]), "prompt> ");
        // An unterminated OSC swallows the rest, but never panics or leaks the escape.
        assert_eq!(strip(&[b"keep\n\x1b]0;never ends"]), "keep\n");
    }

    #[test]
    fn drops_csi_dcs_apc_pm_sos_and_other_escapes() {
        assert_eq!(strip(&[b"\x1b[31mred\x1b[0m \x1b[1;38;2;1;2;3mx\x1b[?25l\x1b[2K\x1b[10;20H!"]), "red x!");
        assert_eq!(strip(&[b"a\x1bPq#0;2;0;0;0~-\x1b\\b"]), "ab"); // DCS (sixel)
        assert_eq!(strip(&[b"a\x1b_Gf=24;AAAA\x1b\\b"]), "ab"); // APC (kitty graphics)
        assert_eq!(strip(&[b"a\x1b^private\x1b\\b\x1bXsos\x1b\\c"]), "abc"); // PM, SOS
        assert_eq!(strip(&[b"a\x1b(Bb\x1b7c\x1b8d\x1b=e\x1b>f\x1b\\g"]), "abcdefg");
        // OSC 8 hyperlinks keep their text only.
        assert_eq!(strip(&[b"\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"]), "link");
        // A BEL inside a DCS is payload; it must not end the string and print what follows.
        assert_eq!(strip(&[b"a\x1bPx\x07y\x1b\\b"]), "ab");
        // CAN/SUB cancel a sequence.
        assert_eq!(strip(&[b"a\x1b[1\x18b\x1b]0;t\x1ac"]), "abc");
    }

    #[test]
    fn keeps_only_printable_lf_and_tab() {
        assert_eq!(strip(&[b"a\tb\x00\x01\x07\x08\x0b\x0c\x0e\x7fc\n"]), "a\tbc\n");
        assert_eq!(strip(&["a\u{85}b\u{9b}c".as_bytes()]), "abc"); // C1 as UTF-8
        assert_eq!(strip(&["h\u{e9}llo \u{20ac} \u{1f600}\n".as_bytes()]), "h\u{e9}llo \u{20ac} \u{1f600}\n");
    }

    #[test]
    fn crlf_becomes_lf_and_bare_cr_discards_the_line() {
        assert_eq!(strip(&[b"a\r\nb\r\n"]), "a\nb\n");
        assert_eq!(strip(&[b"a\nb\n"]), "a\nb\n");
        assert_eq!(strip(&[b"10%\r20%\r100%\ndone\n"]), "100%\ndone\n");
        assert_eq!(strip(&[b"\r\n\r\n"]), "\n\n");
        assert_eq!(strip(&[b"abc\r\r\ndef\n"]), "\ndef\n"); // CR CR LF: the first CR was bare
        // The decision waits for the next byte, even across feeds.
        assert_eq!(strip(&[b"ab\r", b"\ncd\n"]), "ab\ncd\n");
        assert_eq!(strip(&[b"ab\r", b"cd\n"]), "cd\n");
        // Sequences between CR and the text do not decide anything.
        assert_eq!(strip(&[b"old\r\x1b[2K\x1b[0mnew\n"]), "new\n");
        assert_eq!(strip(&[b"old\r\x1b[0m\ntail\n"]), "old\ntail\n");
        // A spinner redraw keeps only the finished state; a tab after CR is overwrite too.
        assert_eq!(strip(&[b"|\r/\r-\r\\\rdone\n"]), "done\n");
        assert_eq!(strip(&[b"x\r\ty\n"]), "\ty\n");
        // The unfinished last line is kept at the end (no data loss), a trailing CR is not an overwrite.
        assert_eq!(strip(&[b"PS C:\\> "]), "PS C:\\> ");
        assert_eq!(strip(&[b"last\r"]), "last");
    }

    #[test]
    fn utf8_split_across_feeds_is_never_fffd() {
        let s = "h\u{e9}\u{20ac}\u{1f600}\n";
        let b = s.as_bytes();
        for i in 0..=b.len() {
            assert_eq!(strip(&[&b[..i], &b[i..]]), s, "split {i}");
        }
        assert_eq!(strip(&b.chunks(1).collect::<Vec<_>>()), s);
        // Genuinely invalid bytes become one U+FFFD each, in order.
        assert_eq!(strip(&[b"a\xffb\n"]), "a\u{fffd}b\n");
        assert_eq!(strip(&[b"a\xe2\x82\nb\n"]), "a\u{fffd}\nb\n"); // truncated char before a control
        assert_eq!(strip(&[b"tail \xe2\x82"]), "tail \u{fffd}");
    }

    /// Property-style: a stream with every kind of sequence, split at every offset and byte by byte.
    #[test]
    fn split_sequences_at_every_boundary() {
        let stream: &[u8] = b"start \x1b[31mred\x1b[0m\x1b]0;title\x07\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ \
            \x1bPq#0;2;0;0;0\x1b\\\x1b_apc\x1b\\\x1b^pm\x1b\\\x1bXsos\x1b\\\x1b(B\x1b7caf\xc3\xa9\r\nspin\r\
            \x1b[2Kdone\tTAB\x1b]133;D;0\x07\n\x1b[?1049h\x1b[?1049l\x07end\r";
        let want = strip(&[stream]);
        assert_eq!(want, "start redlink caf\u{e9}\ndone\tTAB\nend");
        for i in 0..=stream.len() {
            assert_eq!(strip(&[&stream[..i], &stream[i..]]), want, "split at {i}");
        }
        assert_eq!(strip(&stream.chunks(1).collect::<Vec<_>>()), want);
    }

    #[test]
    fn non_ascii_aborts_a_sequence_and_is_kept() {
        assert_eq!(strip(&["a\x1b[1\u{e9}b".as_bytes()]), "a\u{e9}b");
        assert_eq!(strip(&["a\x1b\u{e9}b".as_bytes()]), "a\u{e9}b");
    }

    #[test]
    fn endless_line_is_released_in_pieces() {
        let (mut s, mut out) = (LogStripper::new(), String::new());
        for _ in 0..8 {
            s.feed(&vec![b'x'; 40 << 10], &mut out);
        }
        assert!(out.len() >= 128 << 10 && s.st.line.len() <= MAX_LINE);
        s.finish(&mut out);
        assert_eq!(out.len(), 8 * (40 << 10));
    }
}
