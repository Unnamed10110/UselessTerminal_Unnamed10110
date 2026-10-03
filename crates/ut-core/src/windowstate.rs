//! Window state (§16.3): model, sanitising, bounds clamping against the connected monitors,
//! atomic load/save, and the legacy WPF conversion (Appendix C.6). Autosave timers and the
//! "never save before restore finished" guards live in the app, not here.
//!
//! Coordinates are whatever space the caller uses for both the saved bounds and the monitor
//! work areas (the app uses physical pixels). Legacy WPF files hold DIPs, which equal physical
//! pixels only at 100% scaling.

use crate::color::{from_wpf, normalize_hex};
use crate::{lenient, LoadStatus};
use serde::{de::Deserializer, Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use ut_fs::{read_json, ReadJson};

pub const SIDEBAR_RANGE: (u32, u32) = (160, 900);
pub const BROWSER_RANGE: (u32, u32) = (250, 1600);
pub const MAX_PANES: usize = 16;
const MIN_SIZE: (i32, i32) = (480, 360);
const DEFAULT_SIZE: (i32, i32) = (1200, 800);
/// §16.3: at least this much of the window stays visible horizontally.
const MIN_VISIBLE_X: i64 = 120;
/// Height of the custom title bar (§7.1): it must stay reachable.
const TITLE_BAR: i64 = 30;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// [P1] Binary split tree of a tab's panes (§7.4).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SplitDir {
    /// `a` left, `b` right (Split Right).
    #[default]
    Right,
    /// `a` top, `b` bottom (Split Down).
    Down,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PaneLayout {
    Leaf {
        pane_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Split {
        dir: SplitDir,
        /// Share of the first child, 0.05..=0.95.
        ratio: f64,
        a: Box<PaneLayout>,
        b: Box<PaneLayout>,
    },
}

impl PaneLayout {
    pub fn pane_count(&self) -> usize {
        match self {
            PaneLayout::Leaf { .. } => 1,
            PaneLayout::Split { a, b, .. } => a.pane_count() + b.pane_count(),
        }
    }

    fn clamp_ratios(&mut self) {
        if let PaneLayout::Split { ratio, a, b, .. } = self {
            *ratio = if ratio.is_finite() { ratio.clamp(0.05, 0.95) } else { 0.5 };
            a.clamp_ratios();
            b.clamp_ratios();
        }
    }
}

/// Parse a field that may be malformed without invalidating its container.
fn tolerant<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    Ok(Value::deserialize(d).ok().and_then(|v| serde_json::from_value(v).ok()))
}

/// Restore each tab independently (§16.3, §23.24): a bad entry is dropped, the rest survive.
fn tolerant_tabs<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<TabState>, D::Error> {
    Ok(Vec::<Value>::deserialize(d)?.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TabState {
    pub title: String,
    pub title_locked: bool,
    pub command: String,
    pub session_id: Option<String>,
    /// The local live cwd if it exists locally, otherwise the original directory (caller's job).
    pub cwd: Option<String>,
    pub starting_command: Option<String>,
    pub color: Option<String>,
    pub pinned: bool,
    pub group: Option<String>,
    pub read_only: bool,
    #[serde(deserialize_with = "tolerant", skip_serializing_if = "Option::is_none")]
    pub layout: Option<PaneLayout>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowState {
    pub schema_version: u32,
    /// Restore bounds (Normal state); `None` until a valid one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    pub maximized: bool,
    pub sidebar_open: bool,
    pub sidebar_width: u32,
    pub browser_open: bool,
    pub browser_width: u32,
    pub active_tab_index: usize,
    #[serde(deserialize_with = "tolerant_tabs")]
    pub tabs: Vec<TabState>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            schema_version: 3,
            bounds: None,
            maximized: false,
            sidebar_open: true,
            sidebar_width: 260,
            browser_open: false,
            browser_width: 500,
            active_tab_index: 0,
            tabs: vec![],
        }
    }
}

fn opt(s: &mut Option<String>) {
    *s = s.take().filter(|s| !s.trim().is_empty());
}

impl WindowState {
    /// Deserialize leniently (bad leaves fall back to defaults, bad tabs are dropped), then sanitise.
    pub fn from_value(v: &Value) -> WindowState {
        let mut s: WindowState = lenient(v);
        s.sanitize();
        s
    }

    /// Never keeps NaN/zero/absurd bounds (§16.3), out-of-range panel widths fall back to their
    /// defaults, the active index points at an existing tab, tab fields are tidied.
    pub fn sanitize(&mut self) {
        self.schema_version = 3;
        self.bounds = self.bounds.filter(|r| {
            let sane = |v: i32| v.unsigned_abs() <= 100_000;
            r.width > 0 && r.height > 0 && [r.x, r.y, r.width, r.height].into_iter().all(sane)
        });
        if let Some(r) = &mut self.bounds {
            r.width = r.width.max(MIN_SIZE.0);
            r.height = r.height.max(MIN_SIZE.1);
        }
        if !(SIDEBAR_RANGE.0..=SIDEBAR_RANGE.1).contains(&self.sidebar_width) {
            self.sidebar_width = 260;
        }
        if !(BROWSER_RANGE.0..=BROWSER_RANGE.1).contains(&self.browser_width) {
            self.browser_width = 500;
        }
        self.active_tab_index = self.active_tab_index.min(self.tabs.len().saturating_sub(1));
        for t in &mut self.tabs {
            opt(&mut t.session_id);
            opt(&mut t.cwd);
            opt(&mut t.starting_command);
            opt(&mut t.group); // empty = no group (§7.3: it must be possible to clear it)
            t.color = t.color.take().and_then(|c| normalize_hex(&c));
            if t.layout.as_ref().is_some_and(|l| l.pane_count() > MAX_PANES) {
                t.layout = None;
            }
            if let Some(l) = &mut t.layout {
                l.clamp_ratios();
            }
        }
    }

    /// Atomic write (callers that debounce use `DebouncedWriter::queue_json` instead).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        ut_fs::write_json_atomic(path, self)
    }

    /// Convert the legacy C.6 object. `Renamed` -> `titleLocked`, `HighlightColor` -> `color`.
    pub fn from_legacy(v: &Value) -> WindowState {
        let f = |k: &str| v.get(k).and_then(Value::as_f64).filter(|n| n.is_finite());
        let b = |k: &str| v.get(k).and_then(Value::as_bool);
        let mut s = WindowState::default();
        if let (Some(x), Some(y), Some(w), Some(h)) = (f("Left"), f("Top"), f("Width"), f("Height")) {
            let r = |n: f64| n.round().clamp(-1e6, 1e6) as i32;
            s.bounds = Some(Rect { x: r(x), y: r(y), width: r(w), height: r(h) });
        }
        s.maximized = b("IsMaximized").unwrap_or(false);
        s.sidebar_open = b("SessionPanelOpen").unwrap_or(s.sidebar_open);
        s.sidebar_width = f("SessionPanelWidth").map_or(s.sidebar_width, |n| n.round().clamp(0.0, 1e6) as u32);
        s.active_tab_index = f("ActiveTabIndex").map_or(0, |n| n.round().clamp(0.0, 1e6) as usize);
        s.tabs = v
            .get("Tabs")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(legacy_tab).collect())
            .unwrap_or_default();
        s.sanitize();
        s
    }
}

fn legacy_tab(t: &Value) -> Option<TabState> {
    let o = t.as_object()?;
    let s = |k: &str| o.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    Some(TabState {
        title: s("Title").unwrap_or_default(),
        title_locked: o.get("Renamed").and_then(Value::as_bool).unwrap_or(false),
        command: s("Command").unwrap_or_default(),
        cwd: s("WorkingDirectory"),
        starting_command: s("StartingCommand"),
        color: s("HighlightColor").and_then(|c| from_wpf(&c)),
        ..Default::default()
    })
}

/// Load `windowstate.json`. Missing/corrupt -> defaults (the status says which). A legacy WPF
/// file is backed up as `*.legacy-bak.json`, converted and rewritten as v3.
pub fn load(path: &Path) -> (WindowState, LoadStatus) {
    let v = match read_json::<Value>(path) {
        ReadJson::Missing => return (WindowState::default(), LoadStatus::Missing),
        ReadJson::Corrupt { error } => return (WindowState::default(), LoadStatus::Corrupt { error }),
        ReadJson::Ok(v) if !v.is_object() => {
            return (WindowState::default(), LoadStatus::Corrupt { error: "windowstate.json must contain a JSON object".into() })
        }
        ReadJson::Ok(v) => v,
    };
    let legacy = v.get("schemaVersion").is_none()
        && v.as_object().is_some_and(|m| m.keys().any(|k| k.starts_with(|c: char| c.is_ascii_uppercase())));
    if !legacy {
        return (WindowState::from_value(&v), LoadStatus::Loaded);
    }
    let s = WindowState::from_legacy(&v);
    let status = match ut_fs::backup_copy(path, "legacy") {
        Err(e) => LoadStatus::Corrupt { error: format!("cannot back up legacy window state: {e}") },
        Ok(bak) => match s.save(path) {
            Ok(()) => LoadStatus::Migrated { backup: bak.display().to_string() },
            Err(e) => LoadStatus::Corrupt { error: format!("cannot write migrated window state: {e}") },
        },
    };
    (s, status)
}

// ------------------------------------------------------------------------------- bounds

fn overlap(a: &Rect, b: &Rect) -> i64 {
    let w = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width)) - i64::from(a.x).max(i64::from(b.x));
    let h = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height)) - i64::from(a.y).max(i64::from(b.y));
    if w > 0 && h > 0 { w * h } else { 0 }
}

/// A window of `size` centred in `mon`, shrunk to `mon - 80 px` per axis when it does not fit.
fn centred(size: (i32, i32), mon: &Rect) -> Rect {
    let fit = |s: i32, m: i32| if s > m { (m - 80).max(1) } else { s };
    let (w, h) = (fit(size.0, mon.width), fit(size.1, mon.height));
    Rect { x: mon.x + (mon.width - w) / 2, y: mon.y + (mon.height - h) / 2, width: w, height: h }
}

/// First-run bounds: 1200x800 centred on the primary monitor's work area (§7.1).
pub fn default_bounds(monitors: &[Rect], primary: usize) -> Rect {
    match monitors.get(primary).or(monitors.first()) {
        Some(m) => centred(DEFAULT_SIZE, m),
        None => Rect { x: 100, y: 100, width: DEFAULT_SIZE.0, height: DEFAULT_SIZE.1 },
    }
}

/// §16.3 restore rules against the **work areas** of the connected monitors:
/// a window on no monitor is centred on the primary one; one larger than its monitor shrinks to
/// the monitor size minus 80 px; at least 120 px stay visible horizontally and the title bar stays
/// reachable. `monitors` empty -> `saved` unchanged.
pub fn clamp_bounds(saved: Rect, monitors: &[Rect], primary: usize) -> Rect {
    let Some(prim) = monitors.get(primary).or(monitors.first()) else { return saved };
    let Some(mon) = monitors.iter().filter(|m| overlap(&saved, m) > 0).max_by_key(|m| overlap(&saved, m)) else {
        return centred((saved.width, saved.height), prim); // entirely off-screen (e.g. monitor unplugged)
    };
    let fit = |s: i32, m: i32| if s > m { (m - 80).max(1) } else { s };
    let (w, h) = (fit(saved.width, mon.width), fit(saved.height, mon.height));
    let vis = MIN_VISIBLE_X.min(i64::from(w));
    let (left, right) = (i64::from(mon.x), i64::from(mon.x) + i64::from(mon.width));
    let x = i64::from(saved.x).max(left + vis - i64::from(w)).min(right - vis);
    let y = i64::from(saved.y).min(i64::from(mon.y) + i64::from(mon.height) - TITLE_BAR).max(i64::from(mon.y));
    Rect { x: x as i32, y: y as i32, width: w, height: h }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn r(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect { x, y, width, height }
    }
    const M1: Rect = Rect { x: 0, y: 0, width: 1920, height: 1040 }; // work area (taskbar excluded)
    const M2: Rect = Rect { x: 1920, y: 0, width: 2560, height: 1400 };
    const LEFT: Rect = Rect { x: -1600, y: 100, width: 1600, height: 860 };

    #[test]
    fn inside_is_untouched() {
        assert_eq!(clamp_bounds(r(100, 50, 1200, 800), &[M1], 0), r(100, 50, 1200, 800));
        assert_eq!(clamp_bounds(r(-1500, 150, 1000, 700), &[LEFT, M1], 1), r(-1500, 150, 1000, 700));
    }

    #[test]
    fn disconnected_monitor_centres_on_primary() {
        // was on monitor 2 (x 2500..), which is now unplugged
        let c = clamp_bounds(r(2500, 200, 1200, 800), &[M1], 0);
        assert_eq!(c, r(360, 120, 1200, 800));
        // same window with both monitors present stays where it was
        assert_eq!(clamp_bounds(r(2500, 200, 1200, 800), &[M1, M2], 0), r(2500, 200, 1200, 800));
        // way above the screen
        assert_eq!(clamp_bounds(r(100, -5000, 800, 600), &[M1], 0), r(560, 220, 800, 600));
        // centred on a primary that is not at the origin
        assert_eq!(clamp_bounds(r(-9000, 0, 800, 600), &[M1, M2], 1), r(1920 + 880, 400, 800, 600));
    }

    #[test]
    fn larger_than_monitor_shrinks_by_80() {
        assert_eq!(clamp_bounds(r(0, 0, 3000, 2000), &[M1], 0), r(0, 0, 1840, 960));
        assert_eq!(clamp_bounds(r(0, 0, 2000, 700), &[M1], 0), r(0, 0, 1840, 700), "only the oversized axis");
        // off-screen AND too big: shrunk, then centred on primary
        assert_eq!(clamp_bounds(r(9000, 9000, 3000, 2000), &[M1], 0), r(40, 40, 1840, 960));
    }

    #[test]
    fn keeps_120px_visible_and_titlebar_reachable() {
        // only 70 px on the primary monitor -> pulled in so 120 px show
        assert_eq!(clamp_bounds(r(1850, 100, 800, 600), &[M1], 0), r(1800, 100, 800, 600));
        // sticks out on the left with 50 px visible
        assert_eq!(clamp_bounds(r(-750, 100, 800, 600), &[M1], 0), r(-680, 100, 800, 600));
        // title bar above the work area
        assert_eq!(clamp_bounds(r(100, -20, 800, 600), &[M1], 0).y, 0);
        // title bar below the bottom edge
        assert_eq!(clamp_bounds(r(100, 1035, 800, 600), &[M1], 0).y, 1040 - 30);
        // narrow window: min(120, width) rule
        assert_eq!(clamp_bounds(r(1900, 10, 100, 600), &[M1], 0).x, 1820);
    }

    #[test]
    fn picks_the_monitor_with_most_overlap_and_tolerates_odd_input() {
        let c = clamp_bounds(r(1800, 100, 1600, 900), &[M1, M2], 0); // mostly on M2
        assert_eq!(c, r(1800, 100, 1600, 900));
        assert_eq!(clamp_bounds(r(5, 5, 100, 100), &[], 0), r(5, 5, 100, 100));
        assert_eq!(clamp_bounds(r(100, 100, 800, 600), &[M1], 7), r(100, 100, 800, 600), "bad primary index");
        let big = r(i32::MAX - 10, i32::MIN + 10, i32::MAX, i32::MAX);
        let _ = clamp_bounds(big, &[M1], 0); // no overflow, no panic
        assert_eq!(default_bounds(&[M1], 0), r(360, 120, 1200, 800));
        assert_eq!(default_bounds(&[], 0).width, 1200);
        assert_eq!(default_bounds(&[r(0, 0, 1024, 728)], 0), r(40, 40, 944, 648));
    }

    #[test]
    fn sanitize_rejects_bad_bounds_and_widths() {
        for b in [json!({"x": 10, "y": 10, "width": 0, "height": 500}), json!({"x": 10, "width": 800}),
            json!({"x": 10, "y": 10, "width": 800, "height": -1}), json!({"x": 10000000, "y": 0, "width": 800, "height": 600})] {
            assert_eq!(WindowState::from_value(&json!({"bounds": b})).bounds, None, "{b}");
        }
        let s = WindowState::from_value(&json!({"bounds": {"x": -5, "y": 7, "width": 100, "height": 100},
            "sidebarWidth": 50, "browserWidth": 99999, "activeTabIndex": 9}));
        assert_eq!(s.bounds, Some(r(-5, 7, 480, 360)), "below the minimum size is raised");
        assert_eq!((s.sidebar_width, s.browser_width, s.active_tab_index), (260, 500, 0));
        let s = WindowState::from_value(&json!({"sidebarWidth": 300, "browserWidth": 700, "sidebarOpen": false}));
        assert_eq!((s.sidebar_width, s.browser_width, s.sidebar_open), (300, 700, false));
    }

    #[test]
    fn tabs_are_tolerant_and_tidied() {
        let s = WindowState::from_value(&json!({"activeTabIndex": 2, "tabs": [
            {"title": "a", "command": "pwsh.exe", "color": "#00FF44", "group": "", "cwd": " ", "pinned": true},
            "garbage", 42, {"title": 5}, {"title": "b", "titleLocked": true, "readOnly": true, "color": "red",
              "layout": {"type": "nonsense"}, "sessionId": "s1"}]}));
        assert_eq!(s.tabs.len(), 2, "{:?}", s.tabs);
        assert_eq!(s.tabs[0].color.as_deref(), Some("#00ff44"));
        assert_eq!((s.tabs[0].group.clone(), s.tabs[0].cwd.clone()), (None, None));
        assert!(s.tabs[0].pinned && s.tabs[1].title_locked && s.tabs[1].read_only);
        assert_eq!(s.tabs[1].color, None);
        assert_eq!(s.tabs[1].layout, None, "a malformed layout does not drop the tab");
        assert_eq!(s.tabs[1].session_id.as_deref(), Some("s1"));
        assert_eq!(s.active_tab_index, 1, "clamped to the last tab");
        assert_eq!(WindowState::from_value(&json!({"tabs": "nope"})).tabs.len(), 0);
        assert_eq!(WindowState::from_value(&json!({"tabs": []})).active_tab_index, 0);
    }

    fn leaf(id: &str) -> PaneLayout {
        PaneLayout::Leaf { pane_id: id.into(), profile: Some("pwsh".into()), command: None, cwd: Some("C:\\a b".into()) }
    }

    #[test]
    fn layout_tree_roundtrip_and_limits() {
        let l = PaneLayout::Split { dir: SplitDir::Down, ratio: 0.3, a: Box::new(leaf("p1")), b: Box::new(leaf("p2")) };
        let st = WindowState { tabs: vec![TabState { title: "t".into(), layout: Some(l.clone()), ..Default::default() }], ..Default::default() };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["tabs"][0]["layout"]["type"], "split");
        assert_eq!(v["tabs"][0]["layout"]["a"]["paneId"], "p1");
        assert_eq!(v["tabs"][0]["layout"]["dir"], "down");
        assert_eq!(WindowState::from_value(&v).tabs[0].layout, Some(l));
        // ratio clamp
        let bad = PaneLayout::Split { dir: SplitDir::Right, ratio: 7.0, a: Box::new(leaf("a")), b: Box::new(leaf("b")) };
        let s = WindowState { tabs: vec![TabState { layout: Some(bad), ..Default::default() }], ..Default::default() };
        let got = WindowState::from_value(&serde_json::to_value(&s).unwrap());
        assert!(matches!(got.tabs[0].layout, Some(PaneLayout::Split { ratio, .. }) if ratio == 0.95));
        // more than 16 panes -> layout dropped, tab kept
        let mut big = leaf("0");
        for i in 1..=MAX_PANES {
            big = PaneLayout::Split { dir: SplitDir::Right, ratio: 0.5, a: Box::new(big), b: Box::new(leaf(&i.to_string())) };
        }
        assert_eq!(big.pane_count(), 17);
        let s = WindowState { tabs: vec![TabState { layout: Some(big), ..Default::default() }], ..Default::default() };
        let got = WindowState::from_value(&serde_json::to_value(&s).unwrap());
        assert_eq!((got.tabs.len(), got.tabs[0].layout.is_none()), (1, true));
    }

    #[test]
    fn load_save_and_legacy_conversion() {
        let d = std::env::temp_dir().join(format!("ut core ws {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("with space").join("windowstate.json");
        assert_eq!(load(&f).1, LoadStatus::Missing);
        let st = WindowState { bounds: Some(r(10, 20, 900, 700)), maximized: true, tabs: vec![TabState { title: "x".into(), ..Default::default() }], ..Default::default() };
        st.save(&f).unwrap();
        assert_eq!(load(&f), (st, LoadStatus::Loaded));
        std::fs::write(&f, "{ nope").unwrap();
        assert!(matches!(load(&f).1, LoadStatus::Corrupt { .. }));

        std::fs::write(&f, r##"{"Left": 100.4, "Top": -32000, "Width": 1000, "Height": 700.6, "IsMaximized": true,
            "SessionPanelOpen": false, "SessionPanelWidth": 300, "ActiveTabIndex": 1,
            "Tabs": [{"Title": "one", "Command": "pwsh.exe", "WorkingDirectory": "C:\\Users\\a b", "StartingCommand": "ls",
            "HighlightColor": "#FF00FF44", "Renamed": true}, {"Title": "two", "Command": "cmd.exe", "HighlightColor": "", "Renamed": false}, 5]}"##).unwrap();
        let (s, status) = load(&f);
        assert!(matches!(status, LoadStatus::Migrated { .. }), "{status:?}");
        assert_eq!(s.bounds, Some(r(100, -32000, 1000, 701)));
        assert!(s.maximized && !s.sidebar_open);
        assert_eq!((s.sidebar_width, s.active_tab_index, s.tabs.len()), (300, 1, 2));
        assert_eq!(s.tabs[0].cwd.as_deref(), Some("C:\\Users\\a b"));
        assert_eq!((s.tabs[0].title_locked, s.tabs[0].color.as_deref()), (true, Some("#00ff44")));
        assert_eq!(s.tabs[0].starting_command.as_deref(), Some("ls"));
        assert_eq!(s.tabs[1].color, None);
        assert!(f.with_file_name("windowstate.legacy-bak.json").exists());
        assert_eq!(load(&f).1, LoadStatus::Loaded);
        // the -32000 "minimised" sentinel never reaches the screen
        let c = clamp_bounds(s.bounds.unwrap(), &[M1], 0);
        assert_eq!(c, r(460, 169, 1000, 701));
    }
}
