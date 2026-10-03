//! Sidebar logic that needs no egui context (so it is unit-tested): the visible rows, selection ranges, keyboard
//! stepping, search highlighting and the §8.2 drag-and-drop target maths.

use egui::{Pos2, Rect};
use std::ops::Range;
use ut_data::{Half, Session, Target, Tree};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Folder,
    Session,
}

/// One visible row, in display order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub id: String,
    pub kind: Kind,
    /// The folder a session is shown under (`None` at the root and in search results).
    pub parent: Option<String>,
}

/// Rows of the tree: folders with their sessions (when open), then the root sessions; or, while searching, the flat
/// list of hits (§8.2).
pub fn flatten(tree: &Tree, is_open: impl Fn(&str) -> bool, hits: Option<&[Session]>) -> Vec<Row> {
    if let Some(h) = hits {
        return h.iter().map(|s| Row { id: s.id.clone(), kind: Kind::Session, parent: None }).collect();
    }
    let mut out = Vec::new();
    for f in &tree.folders {
        out.push(Row { id: f.folder.id.clone(), kind: Kind::Folder, parent: None });
        if is_open(&f.folder.id) {
            out.extend(f.sessions.iter().map(|s| Row { id: s.id.clone(), kind: Kind::Session, parent: Some(f.folder.id.clone()) }));
        }
    }
    out.extend(tree.sessions.iter().map(|s| Row { id: s.id.clone(), kind: Kind::Session, parent: None }));
    out
}

pub fn index_of(rows: &[Row], id: &str) -> Option<usize> {
    rows.iter().position(|r| r.id == id)
}

/// Sessions from `a` to `b` inclusive (either order) in row order; folders in between are skipped, so a folder
/// never becomes part of a multi-selection.
pub fn session_range(rows: &[Row], a: &str, b: &str) -> Vec<String> {
    let (Some(i), Some(j)) = (index_of(rows, a), index_of(rows, b)) else { return vec![] };
    rows[i.min(j)..=i.max(j)].iter().filter(|r| r.kind == Kind::Session).map(|r| r.id.clone()).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Up,
    Down,
    Home,
    End,
}

/// Row the cursor moves to; no cursor yet means "the first row" for either arrow.
pub fn step(rows: &[Row], cursor: Option<&str>, s: Step) -> Option<usize> {
    let last = rows.len().checked_sub(1)?;
    let at = cursor.and_then(|c| index_of(rows, c));
    Some(match s {
        Step::Home => 0,
        Step::End => last,
        Step::Down => at.map_or(0, |i| (i + 1).min(last)),
        Step::Up => at.map_or(0, |i| i.saturating_sub(1)),
    })
}

// ------------------------------------------------------------------------------------------------ command text

/// What is searched and shown while filtering: the path and the arguments, as typed.
pub fn long_command(s: &Session) -> String {
    format!("{} {}", s.shell_path.trim(), s.arguments.trim()).trim().to_string()
}

/// The line under a session's name in the tree: the executable's file name plus the arguments, because the folders of a
/// long path would push the arguments out of a narrow sidebar. A shell path that is really a whole command line (Windows
/// Terminal imports) is shown as it is. `is_file` tells those apart without this module touching the disk.
pub fn short_command(s: &Session, is_file: impl Fn(&str) -> bool) -> String {
    let path = s.shell_path.trim();
    let exe = path.trim_matches('"');
    let file = || exe.rsplit(['\\', '/']).next().unwrap_or(exe).to_string();
    match s.arguments.trim() {
        "" if is_file(exe) => file(),
        "" => path.to_string(),
        args => format!("{} {args}", file()),
    }
}

// ------------------------------------------------------------------------------------------------ search

/// Byte ranges of the case-insensitive occurrences of `q` in `text` (for highlighting). Empty when the lower-cased text
/// no longer lines up with the original (exotic case folding): no highlight beats a wrong one.
pub fn match_ranges(text: &str, q: &str) -> Vec<Range<usize>> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let lo = text.to_lowercase();
    if lo.len() != text.len() {
        return vec![];
    }
    lo.match_indices(&q).map(|(i, m)| i..i + m.len()).filter(|r| text.is_char_boundary(r.start) && text.is_char_boundary(r.end)).collect()
}

/// A hit that is only in the description would otherwise show no highlight at all.
pub fn only_in_description(s: &Session, command: &str, q: &str) -> bool {
    !q.trim().is_empty() && match_ranges(&s.name, q).is_empty() && match_ranges(command, q).is_empty() && !match_ranges(&s.description, q).is_empty()
}

// ------------------------------------------------------------------------------------------- drag and drop

/// Where a row was drawn this frame. `outer` of a folder includes its children; `head` is the header row only (§8.2:
/// the top/bottom test uses the header, not the whole folder).
#[derive(Clone, Debug)]
pub enum Slot {
    Folder { id: String, head: Rect, outer: Rect },
    Session { id: String, rect: Rect },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Before,
    After,
    Into,
    Root,
}

/// A resolved drop: what to hand to `SessionStore::move_items`, and where to draw the indicator.
#[derive(Clone, Debug, PartialEq)]
pub struct DropAt {
    pub target: Target,
    pub half: Half,
    pub mode: Mode,
    /// `Before`: its top edge gets the line, `After`: its bottom edge, `Into`: the whole rect is highlighted.
    pub rect: Rect,
}

pub enum Dragged<'a> {
    Folder(&'a str),
    Sessions(&'a [String]),
}

/// The §8.2 drop table for the pointer at `p`. `area` is the tree's visible rect (outside it nothing is a target).
/// `None` = no-op (over the dragged items themselves, or outside the tree).
pub fn drop_at(slots: &[Slot], area: Rect, p: Pos2, dragged: Dragged) -> Option<DropAt> {
    if !area.contains(p) {
        return None;
    }
    let half_of = |r: Rect| if p.y < r.center().y { Half::Top } else { Half::Bottom };
    // Children sit inside their folder's `outer`, so look at the most specific thing under the pointer first.
    let session = slots.iter().find_map(|s| match s {
        Slot::Session { id, rect } if rect.contains(p) => Some((id.as_str(), *rect)),
        _ => None,
    });
    let head = slots.iter().find_map(|s| match s {
        Slot::Folder { id, head, outer } if head.contains(p) => Some((id.as_str(), *head, *outer)),
        _ => None,
    });
    let edge = slots.iter().find_map(|s| match s {
        Slot::Folder { id, head, outer } if outer.contains(p) => Some((id.as_str(), *head)),
        _ => None,
    });
    match dragged {
        Dragged::Folder(me) => {
            if let Some((id, head, outer)) = head {
                if id == me {
                    return None;
                }
                let half = half_of(head);
                let (mode, rect) = if half == Half::Top { (Mode::Before, head) } else { (Mode::After, outer) };
                return Some(DropAt { target: Target::Folder(id.into()), half, mode, rect });
            }
            // Over its own children: dropping a folder on itself is a no-op rather than "move to the end".
            if edge.is_some_and(|(id, _)| id == me) {
                return None;
            }
            Some(DropAt { target: Target::Root, half: Half::Bottom, mode: Mode::Root, rect: area })
        }
        Dragged::Sessions(ids) => {
            if let Some((id, rect)) = session {
                if ids.iter().any(|d| d == id) {
                    return None;
                }
                let half = half_of(rect);
                let mode = if half == Half::Top { Mode::Before } else { Mode::After };
                return Some(DropAt { target: Target::Session(id.into()), half, mode, rect });
            }
            if let Some((id, head, _)) = head {
                return Some(DropAt { target: Target::Folder(id.into()), half: Half::Top, mode: Mode::Into, rect: head });
            }
            if let Some((id, head)) = edge {
                return Some(DropAt { target: Target::FolderEdge(id.into()), half: Half::Top, mode: Mode::Into, rect: head });
            }
            Some(DropAt { target: Target::Root, half: Half::Bottom, mode: Mode::Root, rect: area })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};
    use ut_data::{Folder, TreeFolder};

    fn s(id: &str) -> Session {
        Session { id: id.into(), name: id.into(), shell_path: "x.exe".into(), ..Default::default() }
    }

    fn tree() -> Tree {
        let f = |id: &str, kids: &[&str]| TreeFolder { folder: Folder { id: id.into(), name: id.into(), sort_order: 0 }, sessions: kids.iter().map(|k| s(k)).collect() };
        Tree { folders: vec![f("A", &["a1", "a2"]), f("B", &["b1"])], sessions: vec![s("r1"), s("r2")] }
    }

    fn ids(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn flatten_follows_the_tree_and_skips_collapsed_children() {
        let t = tree();
        assert_eq!(ids(&flatten(&t, |_| true, None)), ["A", "a1", "a2", "B", "b1", "r1", "r2"]);
        assert_eq!(ids(&flatten(&t, |f| f != "A", None)), ["A", "B", "b1", "r1", "r2"]);
        let rows = flatten(&t, |_| true, None);
        assert_eq!(rows[1].parent.as_deref(), Some("A"));
        assert_eq!(rows[5].parent, None);
        assert_eq!(rows[0].kind, Kind::Folder);
    }

    #[test]
    fn flatten_search_is_a_flat_list_of_sessions() {
        let hits = [s("b1"), s("a2")];
        let rows = flatten(&tree(), |_| true, Some(&hits));
        assert_eq!(ids(&rows), ["b1", "a2"]);
        assert!(rows.iter().all(|r| r.kind == Kind::Session && r.parent.is_none()));
    }

    #[test]
    fn range_selection_skips_folders_and_works_backwards() {
        let rows = flatten(&tree(), |_| true, None);
        assert_eq!(session_range(&rows, "a1", "b1"), ["a1", "a2", "b1"]);
        assert_eq!(session_range(&rows, "r1", "a2"), ["a2", "b1", "r1"]);
        assert_eq!(session_range(&rows, "a1", "a1"), ["a1"]);
        assert!(session_range(&rows, "a1", "nope").is_empty());
    }

    #[test]
    fn keyboard_stepping_clamps_and_starts_at_the_top() {
        let rows = flatten(&tree(), |_| true, None);
        assert_eq!(step(&rows, None, Step::Down), Some(0));
        assert_eq!(step(&rows, None, Step::Up), Some(0));
        assert_eq!(step(&rows, Some("A"), Step::Up), Some(0));
        assert_eq!(step(&rows, Some("a1"), Step::Down), Some(2));
        assert_eq!(step(&rows, Some("r2"), Step::Down), Some(6));
        assert_eq!(step(&rows, Some("b1"), Step::Home), Some(0));
        assert_eq!(step(&rows, Some("b1"), Step::End), Some(6));
        assert_eq!(step(&[], Some("x"), Step::Down), None);
        // a cursor on a row that vanished behaves like no cursor
        assert_eq!(step(&rows, Some("gone"), Step::Down), Some(0));
    }

    #[test]
    fn highlight_ranges_are_case_insensitive_and_char_safe() {
        assert_eq!(match_ranges("Build Server", "ER"), vec![7..9, 10..12]);
        assert_eq!(match_ranges("Build Server", "  er "), vec![7..9, 10..12]);
        assert_eq!(match_ranges("aXbxc", "x"), vec![1..2, 3..4]);
        assert_eq!(match_ranges("  Süß  ", " süß "), vec![2..7]);
        assert!(match_ranges("abc", "  ").is_empty());
        assert!(match_ranges("abc", "zz").is_empty());
        // 'İ' lower-cases to a longer string: highlighting is skipped instead of cutting a char in half
        assert!(match_ranges("İstanbul", "stan").is_empty());
    }

    #[test]
    fn command_lines_for_display_and_search() {
        let mk = |p: &str, a: &str| Session { shell_path: p.into(), arguments: a.into(), ..Default::default() };
        let file = |p: &str| p.ends_with(".exe");
        // arguments: file name + arguments (the long path hides them otherwise)
        assert_eq!(short_command(&mk(r"C:\Windows\System32\OpenSSH\ssh.exe", "build@10.0.0.5"), file), "ssh.exe build@10.0.0.5");
        assert_eq!(short_command(&mk(r#""C:\Program Files\Git\bin\bash.exe""#, "--login -i"), file), "bash.exe --login -i");
        assert_eq!(short_command(&mk("ssh", " host "), file), "ssh host");
        // no arguments: a plain executable path shrinks to its name, a whole command line stays as written
        assert_eq!(short_command(&mk(r"C:\Windows\System32\cmd.exe", ""), file), "cmd.exe");
        assert_eq!(short_command(&mk("wsl.exe -d Ubuntu", ""), file), "wsl.exe -d Ubuntu");
        assert_eq!(short_command(&mk(r#"  "C:\x y\z.exe" -a "#, ""), file), r#""C:\x y\z.exe" -a"#);
        // search text is path + arguments, trimmed
        assert_eq!(long_command(&mk(" a.exe ", " -x ")), "a.exe -x");
        assert_eq!(long_command(&mk("a.exe", "")), "a.exe");
    }

    #[test]
    fn description_only_hits() {
        let mut a = s("a");
        a.name = "Alpha".into();
        a.description = "the build box".into();
        assert!(only_in_description(&a, "pwsh.exe", "build"));
        assert!(!only_in_description(&a, "pwsh.exe", "alp"));
        assert!(!only_in_description(&a, "build.exe", "build"));
        assert!(!only_in_description(&a, "pwsh.exe", ""));
    }

    // Geometry: tree area 0..300 x 0..300. Folder A head 0..30, children 30..90 (two 30px cards), folder B head
    // 100..130 with one child 130..160, root sessions r1 170..200 and r2 200..230.
    fn rect(top: f32, bottom: f32) -> Rect {
        Rect::from_min_max(pos2(0.0, top), pos2(200.0, bottom))
    }

    fn slots() -> Vec<Slot> {
        vec![
            Slot::Folder { id: "A".into(), head: rect(0.0, 30.0), outer: rect(0.0, 90.0) },
            Slot::Session { id: "a1".into(), rect: rect(30.0, 60.0) },
            Slot::Session { id: "a2".into(), rect: rect(60.0, 90.0) },
            Slot::Folder { id: "B".into(), head: rect(100.0, 130.0), outer: rect(100.0, 160.0) },
            Slot::Session { id: "b1".into(), rect: rect(130.0, 160.0) },
            Slot::Session { id: "r1".into(), rect: rect(170.0, 200.0) },
            Slot::Session { id: "r2".into(), rect: rect(200.0, 230.0) },
        ]
    }

    fn area() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(300.0, 300.0))
    }

    fn at(y: f32, d: Dragged) -> Option<DropAt> {
        drop_at(&slots(), area(), pos2(50.0, y), d)
    }

    fn sel(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sessions_on_a_folder_header_go_into_it() {
        let d = sel(&["r1"]);
        for y in [5.0, 25.0] {
            let r = at(y, Dragged::Sessions(&d)).unwrap();
            assert_eq!((r.target, r.mode, r.rect), (Target::Folder("A".into()), Mode::Into, rect(0.0, 30.0)));
        }
    }

    #[test]
    fn sessions_on_a_session_split_by_halves_and_follow_its_folder() {
        let d = sel(&["r1"]);
        let top = at(32.0, Dragged::Sessions(&d)).unwrap();
        assert_eq!((top.target, top.half, top.mode), (Target::Session("a1".into()), Half::Top, Mode::Before));
        let bottom = at(88.0, Dragged::Sessions(&d)).unwrap();
        assert_eq!((bottom.target, bottom.half, bottom.mode), (Target::Session("a2".into()), Half::Bottom, Mode::After));
        assert_eq!(bottom.rect, rect(60.0, 90.0));
    }

    #[test]
    fn dropping_on_a_dragged_item_is_a_no_op() {
        let d = sel(&["a1", "r1"]);
        assert!(at(40.0, Dragged::Sessions(&d)).is_none());
        assert!(at(180.0, Dragged::Sessions(&d)).is_none());
        // but its neighbour is fine
        assert!(at(70.0, Dragged::Sessions(&d)).is_some());
    }

    #[test]
    fn sessions_on_empty_space_go_to_the_root_end() {
        let d = sel(&["a1"]);
        for y in [95.0, 165.0, 260.0] {
            let r = at(y, Dragged::Sessions(&d)).unwrap();
            assert_eq!((r.target, r.mode, r.rect), (Target::Root, Mode::Root, area()), "y={y}");
        }
        // outside the tree: nothing
        assert!(drop_at(&slots(), area(), pos2(50.0, 400.0), Dragged::Sessions(&d)).is_none());
        assert!(drop_at(&slots(), area(), pos2(-5.0, 20.0), Dragged::Sessions(&d)).is_none());
    }

    #[test]
    fn a_folders_children_area_outside_any_card_is_a_folder_edge() {
        let d = sel(&["r1"]);
        // between B's header and child there is no gap in real layouts; use the lower part of B's box
        let mut sl = slots();
        sl.retain(|s| !matches!(s, Slot::Session { id, .. } if id == "b1"));
        let r = drop_at(&sl, area(), pos2(50.0, 145.0), Dragged::Sessions(&d)).unwrap();
        assert_eq!((r.target, r.mode, r.rect), (Target::FolderEdge("B".into()), Mode::Into, rect(100.0, 130.0)));
    }

    #[test]
    fn folders_use_the_header_row_for_before_and_after() {
        let top = at(105.0, Dragged::Folder("A")).unwrap();
        assert_eq!((top.target, top.half, top.mode, top.rect), (Target::Folder("B".into()), Half::Top, Mode::Before, rect(100.0, 130.0)));
        // the bottom half of the HEADER (y 115..130) counts as "after" even though the box is 60px tall
        let bottom = at(125.0, Dragged::Folder("A")).unwrap();
        assert_eq!((bottom.target, bottom.half, bottom.mode), (Target::Folder("B".into()), Half::Bottom, Mode::After));
        assert_eq!(bottom.rect, rect(100.0, 160.0), "the line goes under the whole folder");
    }

    #[test]
    fn a_folder_over_anything_else_moves_to_the_end_but_never_onto_itself() {
        // over a root session / empty space / another folder's child: "anything else"
        for y in [180.0, 260.0, 145.0] {
            let r = at(y, Dragged::Folder("A")).unwrap();
            assert_eq!((r.target, r.mode), (Target::Root, Mode::Root), "y={y}");
        }
        assert!(at(10.0, Dragged::Folder("A")).is_none(), "own header");
        assert!(at(45.0, Dragged::Folder("A")).is_none(), "own children");
    }
}
