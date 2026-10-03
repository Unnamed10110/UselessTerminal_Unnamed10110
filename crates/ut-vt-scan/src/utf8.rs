//! Streaming UTF-8 decoder: a multi-byte char split across `push` calls is never turned into U+FFFD
//! (§4.4, §24 #9). Genuinely invalid bytes become one U+FFFD per maximal invalid subpart, exactly like
//! `String::from_utf8_lossy` would on the concatenated input.

#[derive(Default, Debug)]
pub struct Utf8Stream {
    rem: [u8; 4],
    len: usize,
}

impl Utf8Stream {
    /// Appends the decoded text of `bytes` to `out`, holding an incomplete trailing char (≤ 3 bytes).
    pub fn push(&mut self, mut bytes: &[u8], out: &mut String) {
        // Complete the char left over from the previous call first.
        while self.len > 0 && !bytes.is_empty() {
            self.rem[self.len] = bytes[0];
            match std::str::from_utf8(&self.rem[..=self.len]) {
                Ok(s) => {
                    out.push_str(s);
                    self.len = 0;
                    bytes = &bytes[1..];
                }
                Err(e) if e.error_len().is_none() => {
                    self.len += 1;
                    bytes = &bytes[1..];
                }
                Err(_) => {
                    out.push('\u{FFFD}');
                    self.len = 0; // `bytes[0]` is reprocessed below
                }
            }
        }
        let mut seen = 0;
        for chunk in bytes.utf8_chunks() {
            out.push_str(chunk.valid());
            let bad = chunk.invalid();
            seen += chunk.valid().len() + bad.len();
            if bad.is_empty() {
                continue;
            }
            // Only the very last chunk can be a prefix of a char that the next call completes.
            if seen == bytes.len() && std::str::from_utf8(bad).is_err_and(|e| e.error_len().is_none()) {
                self.rem[..bad.len()].copy_from_slice(bad);
                self.len = bad.len();
            } else {
                out.push('\u{FFFD}');
            }
        }
    }

    /// End of stream: a held incomplete char becomes one U+FFFD.
    pub fn finish(&mut self, out: &mut String) {
        if self.len > 0 {
            out.push('\u{FFFD}');
            self.len = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&[u8]]) -> String {
        let (mut s, mut out) = (Utf8Stream::default(), String::new());
        for c in chunks {
            s.push(c, &mut out);
        }
        s.finish(&mut out);
        out
    }

    #[test]
    fn split_char_is_not_replaced() {
        let b = "a€b😀".as_bytes();
        for i in 0..=b.len() {
            assert_eq!(decode(&[&b[..i], &b[i..]]), "a€b😀", "split at {i}");
        }
        assert_eq!(decode(&b.chunks(1).collect::<Vec<_>>()), "a€b😀");
    }

    #[test]
    fn matches_lossy_for_garbage_at_every_split() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut data = Vec::new();
        for _ in 0..400 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // Bias towards bytes that stress the decoder: lead/continuation bytes and ASCII.
            data.push([0xE2, 0x82, 0xAC, 0xF0, 0x9F, 0xFF, 0xC3, b'a', 0x80][(x % 9) as usize]);
        }
        let want = String::from_utf8_lossy(&data).into_owned();
        for i in 0..=data.len() {
            assert_eq!(decode(&[&data[..i], &data[i..]]), want, "split at {i}");
        }
        assert_eq!(decode(&data.chunks(1).collect::<Vec<_>>()), want);
    }

    #[test]
    fn invalid_byte_is_one_fffd_and_truncated_tail_is_flushed_by_finish() {
        assert_eq!(decode(&[b"a\xFFb"]), "a\u{FFFD}b");
        assert_eq!(decode(&[b"a\xE2\x82"]), "a\u{FFFD}");
    }
}
