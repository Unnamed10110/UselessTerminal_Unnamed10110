//! asciicast v2 writer (§12.2). Number formatting never depends on the locale (§23.28, §24 #8), the real
//! size goes in the header, resizes are `"r"` events, and text is decoded with a streaming UTF-8 decoder
//! so a multi-byte character is never split across events (§24 #9).

use std::io::{self, Write};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::utf8::Utf8Stream;

/// Elapsed time since the recording started; injectable for deterministic tests.
pub type Clock = Box<dyn FnMut() -> Duration + Send>;

/// The real clock, starting now.
pub fn system_clock() -> Clock {
    let t0 = Instant::now();
    Box::new(move || t0.elapsed())
}

pub struct CastHeader {
    pub cols: u16,
    pub rows: u16,
    /// Unix seconds when the recording started.
    pub timestamp: i64,
    pub title: String,
    /// Executable name of the shell, e.g. `pwsh`.
    pub shell_name: String,
}

// Field order is the line's key order (serde_json maps are sorted unless `preserve_order` leaks in).
#[derive(Serialize)]
struct Header<'a> {
    version: u8,
    width: u16,
    height: u16,
    timestamp: i64,
    title: &'a str,
    env: Env<'a>,
}

#[derive(Serialize)]
struct Env<'a> {
    #[serde(rename = "TERM")]
    term: &'static str,
    #[serde(rename = "SHELL")]
    shell: &'a str,
}

pub struct CastWriter<W: Write> {
    w: W,
    clock: Clock,
    out: Utf8Stream,
    inp: Utf8Stream,
}

impl<W: Write> CastWriter<W> {
    /// Writes the header line. Wrap a file in a `BufWriter` if per-event syscalls matter.
    pub fn new(mut w: W, h: CastHeader, clock: Clock) -> io::Result<Self> {
        let header = Header {
            version: 2,
            width: h.cols,
            height: h.rows,
            timestamp: h.timestamp,
            title: &h.title,
            env: Env { term: "xterm-256color", shell: &h.shell_name },
        };
        let mut line = serde_json::to_vec(&header)?;
        line.push(b'\n');
        w.write_all(&line)?;
        Ok(Self { w, clock, out: Utf8Stream::default(), inp: Utf8Stream::default() })
    }

    /// PTY output (`"o"`). An incomplete trailing character is held until the next call or `finish`.
    pub fn output(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut s = String::new();
        self.out.push(bytes, &mut s);
        self.event("o", &s)
    }

    /// [P1] Typed input (`"i"`), only when `recording.captureInput` is on.
    pub fn input(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut s = String::new();
        self.inp.push(bytes, &mut s);
        self.event("i", &s)
    }

    /// `[t,"r","COLSxROWS"]`
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.event("r", &format!("{cols}x{rows}"))
    }

    /// Flushes held partial characters (as U+FFFD), flushes the writer and hands it back.
    pub fn finish(mut self) -> io::Result<W> {
        let mut s = String::new();
        self.out.finish(&mut s);
        self.event("o", &s)?;
        s.clear();
        self.inp.finish(&mut s);
        self.event("i", &s)?;
        self.w.flush()?;
        Ok(self.w)
    }

    fn event(&mut self, code: &str, data: &str) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let t = (self.clock)();
        // Integer formatting: exact, and `.` regardless of the user's locale.
        let line = format!("[{}.{:06},\"{code}\",{}]\n", t.as_secs(), t.subsec_micros(), serde_json::to_string(data)?);
        self.w.write_all(line.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(cols: u16, rows: u16) -> CastHeader {
        CastHeader { cols, rows, timestamp: 1_700_000_000, title: "my \"tab\"".into(), shell_name: "pwsh".into() }
    }

    /// A clock that advances 250 ms per reading.
    fn ticking() -> Clock {
        let mut n = 0;
        Box::new(move || {
            n += 250;
            Duration::from_millis(n)
        })
    }

    fn lines(w: &CastWriter<Vec<u8>>) -> Vec<String> {
        String::from_utf8(w.w.clone()).unwrap().lines().map(String::from).collect()
    }

    #[test]
    fn header_has_the_exact_shape_with_real_size() {
        let w = CastWriter::new(Vec::new(), header(97, 41), ticking()).unwrap();
        assert_eq!(
            lines(&w)[0],
            r#"{"version":2,"width":97,"height":41,"timestamp":1700000000,"title":"my \"tab\"","env":{"TERM":"xterm-256color","SHELL":"pwsh"}}"#
        );
    }

    #[test]
    fn events_use_a_dot_with_six_decimals_and_are_valid_json() {
        let mut w = CastWriter::new(Vec::new(), header(80, 24), ticking()).unwrap();
        w.output(b"hi\x1b[31m \"q\" \\ \r\n\t").unwrap();
        w.resize(120, 30).unwrap();
        w.input(b"ls\r").unwrap();
        let l = lines(&w);
        assert_eq!(l[1], r#"[0.250000,"o","hi\u001b[31m \"q\" \\ \r\n\t"]"#);
        assert_eq!(l[2], r#"[0.500000,"r","120x30"]"#);
        assert_eq!(l[3], r#"[0.750000,"i","ls\r"]"#);
        for line in &l {
            let v: serde_json::Value = serde_json::from_str(line).expect(line);
            if !v.is_array() {
                continue;
            }
            assert!(v[0].is_f64(), "{line}"); // `0.250000`, never `0,250000`
            assert!(line.starts_with("[0."), "{line}");
        }
        assert!(String::from_utf8(w.w).unwrap().lines().skip(1).all(|l| !l.contains(",2") && !l.contains(",5")));
    }

    #[test]
    fn time_formatting_is_exact_beyond_a_minute() {
        let clock: Clock = Box::new(|| Duration::new(3725, 1_500_000)); // 1 ms 500 us
        let mut w = CastWriter::new(Vec::new(), header(80, 24), clock).unwrap();
        w.output(b"x").unwrap();
        assert_eq!(lines(&w)[1], r#"[3725.001500,"o","x"]"#);
    }

    #[test]
    fn utf8_split_across_reads_stays_in_one_event() {
        let b = "a\u{e9}\u{20ac}\u{1f600}".as_bytes();
        for i in 0..=b.len() {
            let mut w = CastWriter::new(Vec::new(), header(80, 24), ticking()).unwrap();
            w.output(&b[..i]).unwrap();
            w.output(&b[i..]).unwrap();
            let out = String::from_utf8(w.finish().unwrap()).unwrap();
            let text: String = out
                .lines()
                .skip(1)
                .map(|l| serde_json::from_str::<(f64, String, String)>(l).unwrap().2)
                .collect();
            assert_eq!(text, "a\u{e9}\u{20ac}\u{1f600}", "split {i}");
            assert!(!out.contains('\u{fffd}'), "split {i}");
        }
    }

    #[test]
    fn partial_tail_is_held_then_flushed_by_finish() {
        let mut w = CastWriter::new(Vec::new(), header(80, 24), ticking()).unwrap();
        w.output(b"ok\xe2\x82").unwrap();
        assert_eq!(lines(&w).len(), 2); // header + "ok"
        assert!(lines(&w)[1].contains("\"ok\""));
        w.output(b"").unwrap(); // nothing to say, no empty event
        assert_eq!(lines(&w).len(), 2);
        let out = String::from_utf8(w.finish().unwrap()).unwrap();
        assert_eq!(out.lines().count(), 3);
        assert!(out.lines().last().unwrap().ends_with(&format!("\"o\",\"{}\"]", char::REPLACEMENT_CHARACTER)));
    }

    #[test]
    fn writer_errors_are_returned_not_panicked() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(CastWriter::new(Broken, header(80, 24), ticking()).is_err());
    }
}
