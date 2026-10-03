//! URL and file-path detection in one terminal row (spec §5.10). Offsets are in `char`s of the row text.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Url,
    Path { line: Option<u32>, col: Option<u32> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// `char` offsets into the row text, end exclusive.
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub kind: LinkKind,
}

fn url_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?:https?|ftp|file|mailto):[^\s<>"'`]+|www\.[^\s<>"'`]+"#).unwrap())
}

/// Order matters: quoted paths first (they may contain spaces), then drive / UNC / relative / POSIX.
fn path_res() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        [
            r#""((?:[A-Za-z]:[\\/]|\\\\|\.{1,2}[\\/]|~[\\/])[^"\r\n]*)""#,
            r#"[A-Za-z]:[\\/][^\s"'<>|*?:]*"#,
            r#"\\\\[\w.$-]+\\[^\s"'<>|*?:]*"#,
            r#"(?:\.{1,2}[\\/]|~/)[^\s"'<>|*?:]+"#,
            r#"/[\w.@+\-]+(?:/[\w.@+\-]*)+"#,
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

fn trim_trailing(s: &str) -> &str {
    s.trim_end_matches(['.', ',', ';', ')', ']', '}', '>', '!', '?', ':'])
}

fn suffix_pos(rest: &str) -> Option<(u32, Option<u32>, usize)> {
    let r = rest.strip_prefix(':')?;
    let d1: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
    let line: u32 = d1.parse().ok()?;
    let mut len = 1 + d1.len();
    let mut col = None;
    if let Some(r2) = r[d1.len()..].strip_prefix(':') {
        let d2: String = r2.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(c) = d2.parse() {
            col = Some(c);
            len += 1 + d2.len();
        }
    }
    Some((line, col, len))
}

/// All links in `text`, URLs first. Path matches overlapping a URL are dropped.
pub fn find_links(text: &str) -> Vec<Link> {
    let byte_to_char = |b: usize| text[..b].chars().count();
    let mut out: Vec<Link> = Vec::new();
    for m in url_re().find_iter(text) {
        let t = trim_trailing(m.as_str());
        if t.len() < 5 {
            continue;
        }
        let start = byte_to_char(m.start());
        out.push(Link { start, end: start + t.chars().count(), text: t.to_string(), kind: LinkKind::Url });
    }
    for re in path_res() {
        for caps in re.captures_iter(text) {
            let whole = caps.get(0).unwrap();
            let (g, quoted) = match caps.get(1) {
                Some(g) => (g, true),
                None => (whole, false),
            };
            let raw = if quoted { g.as_str() } else { trim_trailing(g.as_str()) };
            if raw.chars().count() < 3 {
                continue;
            }
            let start = byte_to_char(g.start());
            let mut end = start + raw.chars().count();
            let (mut line, mut col) = (None, None);
            if let Some((l, c, len)) = suffix_pos(&text[g.start() + raw.len()..]) {
                line = Some(l);
                col = c;
                end += len;
            }
            if out.iter().any(|o| start < o.end && end > o.start) {
                continue;
            }
            let shown: String = text.chars().skip(start).take(end - start).collect();
            out.push(Link { start, end, text: shown, kind: LinkKind::Path { line, col } });
            // keep the bare path for opening
            if let Some(l) = out.last_mut() {
                l.text = raw.to_string();
            }
        }
    }
    out.sort_by_key(|l| l.start);
    out
}

pub fn link_at(text: &str, col: usize) -> Option<Link> {
    find_links(text).into_iter().find(|l| col >= l.start && col < l.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_trailing_punctuation() {
        let l = find_links("see https://example.com/a?b=1, and www.foo.org.");
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].text, "https://example.com/a?b=1");
        assert_eq!(l[1].text, "www.foo.org");
    }

    #[test]
    fn windows_paths_with_line_and_col() {
        let l = find_links(r"error in C:\src\app\main.rs:42:7: expected");
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].text, r"C:\src\app\main.rs");
        assert_eq!(l[0].kind, LinkKind::Path { line: Some(42), col: Some(7) });
    }

    #[test]
    fn quoted_paths_with_spaces_and_unc_and_posix() {
        let l = find_links(r#"open "C:\Users\a b\OneDrive - X\f.txt" or \\srv\share\x.log or /usr/local/bin/ls"#);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].text, r"C:\Users\a b\OneDrive - X\f.txt");
        assert_eq!(l[1].text, r"\\srv\share\x.log");
        assert_eq!(l[2].text, "/usr/local/bin/ls");
    }

    #[test]
    fn url_wins_over_path_inside_it() {
        let l = find_links("https://example.com/usr/local/x");
        assert_eq!(l.len(), 1);
        assert!(matches!(l[0].kind, LinkKind::Url));
    }

    #[test]
    fn offsets_are_char_based() {
        let l = find_links("éé https://x.io/ab");
        assert_eq!(l[0].start, 3);
        assert_eq!(link_at("éé https://x.io/ab", 4).unwrap().text, "https://x.io/ab");
    }
}
