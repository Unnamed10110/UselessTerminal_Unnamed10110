//! Scrollback search (spec §5.11): regex / case / whole-word over history + screen rows.

use crate::session::TermSession;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use regex::RegexBuilder;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOpts {
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Absolute row (see `session` docs).
    pub abs: i64,
    /// First and last (inclusive) column of the match.
    pub c0: usize,
    pub c1: usize,
}

const MAX_MATCHES: usize = 5000;

impl TermSession {
    /// All matches, oldest first. An invalid regex yields no matches.
    pub fn search(&self, query: &str, o: SearchOpts) -> Vec<Match> {
        if query.is_empty() {
            return vec![];
        }
        let mut pat = if o.regex { query.to_string() } else { regex::escape(query) };
        if o.whole_word {
            pat = format!(r"\b(?:{pat})\b");
        }
        let Ok(re) = RegexBuilder::new(&pat).case_insensitive(!o.case_sensitive).size_limit(1 << 20).build() else { return vec![] };
        let term = self.inner.term.lock();
        let a_top = self.inner.track.lock().a_top;
        let grid = term.grid();
        let hist = grid.history_size() as i32;
        let cols = grid.columns();
        let mut out = Vec::new();
        let (mut text, mut colmap) = (String::new(), Vec::<usize>::new());
        for line in -hist..grid.screen_lines() as i32 {
            text.clear();
            colmap.clear();
            let row = &grid[Line(line)];
            for c in 0..cols {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                // Map every byte of this char to its column so match byte offsets convert back.
                for _ in 0..ch.len_utf8() {
                    colmap.push(c);
                }
                text.push(ch);
            }
            if text.trim().is_empty() {
                continue;
            }
            for m in re.find_iter(&text) {
                if m.is_empty() {
                    continue;
                }
                out.push(Match { abs: a_top + line as i64, c0: colmap[m.start()], c1: colmap[m.end() - 1] });
                if out.len() >= MAX_MATCHES {
                    return out;
                }
            }
        }
        out
    }

    /// Scroll so that absolute row `abs` is visible (roughly centred).
    pub fn reveal_abs(&self, abs: i64) {
        let mut term = self.inner.term.lock();
        let a_top = self.inner.track.lock().a_top;
        let rows = term.screen_lines() as i64;
        let hist = term.grid().history_size() as i64;
        let cur_top = a_top - term.grid().display_offset() as i64;
        if abs >= cur_top && abs < cur_top + rows {
            return;
        }
        let want_top = abs - rows / 2;
        let offset = (a_top - want_top).clamp(0, hist) as i32;
        let cur = term.grid().display_offset() as i32;
        term.scroll_display(Scroll::Delta(offset - cur));
        drop(term);
        self.inner.wake();
    }
}
