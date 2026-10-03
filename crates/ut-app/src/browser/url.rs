//! Pure logic of the browser panel (§17.2): address-bar rules, search URL, quick links, last-URL persistence and the
//! pixel maths for the child webview. Nothing here touches egui state, a window or the web engine.

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use std::path::{Path, PathBuf};

pub const DEFAULT_URL: &str = "https://chatgpt.com";

/// §17.2 (not yet configurable: `settings.json` has no field for the list).
pub const QUICK_LINKS: [(&str, &str); 7] = [
    ("ChatGPT", "https://chatgpt.com"),
    ("DeepSeek", "https://chat.deepseek.com"),
    ("Claude", "https://claude.ai"),
    ("Gemini", "https://gemini.google.com"),
    ("Copilot", "https://copilot.microsoft.com"),
    ("Perplexity", "https://www.perplexity.ai"),
    ("Grok", "https://grok.com"),
];

/// JavaScript's `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is escaped.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'!').remove(b'~').remove(b'*').remove(b'\'').remove(b'(').remove(b')');

pub fn search_url(query: &str) -> String {
    format!("https://www.google.com/search?q={}", utf8_percent_encode(query, COMPONENT))
}

/// Address-bar rules (§17.2); `None` = ignore the input.
/// * empty / blank → ignored,
/// * contains `://` → used as typed,
/// * contains a `.` and no whitespace → `https://` is prepended,
/// * anything else → a Google search.
pub fn normalize_input(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if t.contains("://") {
        return Some(t.to_string());
    }
    if t.contains('.') && !t.chars().any(char::is_whitespace) {
        return Some(format!("https://{t}"));
    }
    Some(search_url(t))
}

/// The panel only ever navigates to web pages (`file:`, `javascript:`, app protocols are refused).
pub fn is_web_url(url: &str) -> bool {
    matches!(crate::sys::url_scheme(url).as_deref(), Some("http" | "https"))
}

// -------------------------------------------------------------------------------------------- persistence

/// `%APPDATA%\UselessTerminal\browser.json` (`UT_APPDATA` honoured): `{"lastUrl": "..."}`.
pub fn state_path() -> PathBuf {
    ut_fs::app_data_dir().join("browser.json")
}

#[derive(serde::Deserialize, Default)]
struct State {
    #[serde(rename = "lastUrl")]
    last_url: Option<String>,
}

/// The page to open first: the saved last URL when it is a web address, else ChatGPT. A missing or corrupt file
/// is never an error and is never touched.
pub fn load_last_url(path: &Path) -> String {
    match ut_fs::read_json::<State>(path) {
        ut_fs::ReadJson::Ok(s) => s.last_url.filter(|u| is_web_url(u)).unwrap_or_else(|| DEFAULT_URL.into()),
        _ => DEFAULT_URL.into(),
    }
}

/// What to persist for a visited URL; non-web pages (`about:blank`, `data:`, …) are not remembered.
pub fn state_value(url: &str) -> Option<serde_json::Value> {
    is_web_url(url).then(|| serde_json::json!({ "lastUrl": url }))
}

// ------------------------------------------------------------------------------------------- shortcuts

/// While the page has the keyboard, Windows hands every key to the WebView2 and the app's own key router never sees
/// it. These app shortcuts are matched on the page's accelerator events instead, so Ctrl+Shift+B still closes the
/// panel from inside a chat. Anything a page may use for editing or that touches the terminal (copy, paste, close pane,
/// search, zoom, and Ctrl+B = bold in rich editors, i.e. `togglePanel`) is deliberately not forwarded.
pub const PAGE_SHORTCUTS: [&str; 6] = ["toggleBrowser", "commandPalette", "settings", "newTab", "nextTab", "prevTab"];

/// A keymap entry resolved to virtual keys (see `ut_term::input::Binding`).
#[derive(Clone, Debug, PartialEq)]
pub struct Shortcut {
    pub action: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub vks: Vec<u16>,
}

/// The action bound to this exact chord, if it is a forwardable one. A chord needs Ctrl or Alt (a bare key is text),
/// and AltGr (reported as Ctrl+Alt) is text too.
#[cfg_attr(not(feature = "browser-panel"), allow(dead_code))] // only the web engine calls it
pub fn match_shortcut(list: &[Shortcut], vk: u16, ctrl: bool, shift: bool, alt: bool, altgr: bool) -> Option<&str> {
    if altgr {
        return None;
    }
    list.iter().find(|s| (s.ctrl || s.alt) && s.ctrl == ctrl && s.shift == shift && s.alt == alt && s.vks.contains(&vk)).map(|s| s.action.as_str())
}

// ----------------------------------------------------------------------------------------------- bounds

/// A rectangle in physical pixels, relative to the parent window's client area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Gap kept free of the child window on the panel's left and right edges. A child HWND swallows the mouse, so
/// without it the splitter's grab zone (egui: 5 px each side of the edge) and the frameless window's resize border
/// (5 px, `chrome::resize_zones`) would be dead where the page is.
pub const EDGE_GUTTER: f32 = 5.0;

/// The page's area inside the panel body.
pub fn view_rect(body: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(body.left() + EDGE_GUTTER, body.top()), egui::pos2(body.right() - EDGE_GUTTER, body.bottom()))
}

/// egui points → physical pixels. Both edges are rounded (not the size) so neighbours tile without gaps;
/// `None` when the area is too small or not finite (the webview is then hidden instead of resized to nothing).
pub fn phys_bounds(r: egui::Rect, ppp: f32) -> Option<Bounds> {
    if !(r.min.x.is_finite() && r.min.y.is_finite() && r.max.x.is_finite() && r.max.y.is_finite() && ppp.is_finite() && ppp > 0.0) {
        return None;
    }
    let (x0, y0, x1, y1) = ((r.min.x * ppp).round(), (r.min.y * ppp).round(), (r.max.x * ppp).round(), (r.max.y * ppp).round());
    let (w, h) = (x1 - x0, y1 - y0);
    (w >= 8.0 && h >= 8.0).then_some(Bounds { x: x0 as i32, y: y0 as i32, w: w as u32, h: h as u32 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, Rect};

    #[test]
    fn address_bar_rules() {
        assert_eq!(normalize_input(""), None);
        assert_eq!(normalize_input("   \t "), None);
        assert_eq!(normalize_input("example.com").as_deref(), Some("https://example.com"));
        assert_eq!(normalize_input("  example.com/a?b=c  ").as_deref(), Some("https://example.com/a?b=c"));
        assert_eq!(normalize_input("localhost:3000"), Some(search_url("localhost:3000")), "no dot → search");
        assert_eq!(normalize_input("http://localhost:3000/x").as_deref(), Some("http://localhost:3000/x"));
        assert_eq!(normalize_input("ftp://h/f").as_deref(), Some("ftp://h/f"), "`://` is used as typed (navigation filters schemes)");
        assert_eq!(normalize_input("what is rust 1.9").as_deref(), Some("https://www.google.com/search?q=what%20is%20rust%201.9"), "dot but a space → search");
        assert_eq!(normalize_input("rust").as_deref(), Some("https://www.google.com/search?q=rust"));
    }

    #[test]
    fn search_query_is_encoded_like_encode_uri_component() {
        assert_eq!(search_url("a b&c=d"), "https://www.google.com/search?q=a%20b%26c%3Dd");
        assert_eq!(search_url("ñandú"), "https://www.google.com/search?q=%C3%B1and%C3%BA");
        assert_eq!(search_url("100% (ok) !~*'-_."), "https://www.google.com/search?q=100%25%20(ok)%20!~*'-_.");
        assert_eq!(search_url("a/b?c#d+e"), "https://www.google.com/search?q=a%2Fb%3Fc%23d%2Be");
    }

    #[test]
    fn only_web_pages_are_navigable() {
        for ok in ["https://example.com", "http://127.0.0.1:8000/x", "HTTPS://EXAMPLE.COM"] {
            assert!(is_web_url(ok), "{ok}");
        }
        for bad in ["file:///C:/Windows/win.ini", "javascript:alert(1)", "data:text/html,hi", "about:blank", "ms-settings:privacy", "mailto:a@b.c", "example.com", ""] {
            assert!(!is_web_url(bad), "{bad}");
        }
    }

    #[test]
    fn quick_links_match_the_spec() {
        let names: Vec<_> = QUICK_LINKS.iter().map(|l| l.0).collect();
        assert_eq!(names, ["ChatGPT", "DeepSeek", "Claude", "Gemini", "Copilot", "Perplexity", "Grok"]);
        assert!(QUICK_LINKS.iter().all(|l| is_web_url(l.1)));
        assert_eq!(QUICK_LINKS[0].1, DEFAULT_URL);
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ut-browser-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn last_url_roundtrips_through_the_debounced_writer() {
        let d = tmp("persist");
        let f = d.join("browser.json");
        assert_eq!(load_last_url(&f), DEFAULT_URL, "first run");
        let w = ut_fs::DebouncedWriter::new(|p, e| panic!("{p:?}: {e}"));
        w.queue_json(&f, std::time::Duration::from_secs(60), &state_value("https://example.com/a?b=1").unwrap());
        w.flush_all();
        assert_eq!(load_last_url(&f), "https://example.com/a?b=1");
        // the on-disk format is the legacy one
        assert_eq!(std::fs::read_to_string(&f).unwrap().replace([' ', '\n', '\r'], ""), r#"{"lastUrl":"https://example.com/a?b=1"}"#);
    }

    #[test]
    fn bad_state_files_fall_back_to_the_default_and_are_left_alone() {
        let d = tmp("badstate");
        let f = d.join("browser.json");
        for body in ["{ nope", r#"{"lastUrl": 5}"#, r#"{"lastUrl": "file:///C:/x"}"#, r#"{"lastUrl": "javascript:alert(1)"}"#, "{}"] {
            std::fs::write(&f, body).unwrap();
            assert_eq!(load_last_url(&f), DEFAULT_URL, "{body}");
            assert_eq!(std::fs::read_to_string(&f).unwrap(), body, "the file is never rewritten on load");
        }
    }

    #[test]
    fn non_web_urls_are_not_remembered() {
        assert!(state_value("about:blank").is_none());
        assert!(state_value("data:text/html,x").is_none());
        assert!(state_value("https://claude.ai/chat/1").is_some());
    }

    fn sc(action: &str, ctrl: bool, shift: bool, alt: bool, vks: &[u16]) -> Shortcut {
        Shortcut { action: action.into(), ctrl, shift, alt, vks: vks.to_vec() }
    }

    #[test]
    fn page_shortcuts_match_exact_chords_only() {
        let l = [sc("toggleBrowser", true, true, false, &[0x42]), sc("movePane", true, true, false, &[0x25, 0x26]), sc("bare", false, false, false, &[0x41]), sc("nextTab", true, false, false, &[0x09])];
        assert_eq!(match_shortcut(&l, 0x42, true, true, false, false), Some("toggleBrowser"));
        assert_eq!(match_shortcut(&l, 0x42, true, false, false, false), None, "Ctrl+B is not Ctrl+Shift+B");
        assert_eq!(match_shortcut(&l, 0x42, true, true, true, false), None, "extra Alt");
        assert_eq!(match_shortcut(&l, 0x26, true, true, false, false), Some("movePane"), "any of the chord's virtual keys");
        assert_eq!(match_shortcut(&l, 0x41, false, false, false, false), None, "a bare key is text, never a shortcut");
        assert_eq!(match_shortcut(&l, 0x09, true, false, false, false), Some("nextTab"));
        assert_eq!(match_shortcut(&l, 0x42, true, true, false, true), None, "AltGr produces text");
        assert_eq!(match_shortcut(&[], 0x42, true, true, false, false), None);
    }

    #[test]
    fn only_safe_actions_are_forwarded_from_the_page() {
        for unsafe_ in ["copy", "paste", "closePane", "search", "zoomIn", "zoomOut", "zoomReset", "exportBuffer", "splitRight", "togglePanel"] {
            assert!(!PAGE_SHORTCUTS.contains(&unsafe_), "{unsafe_} must stay with the page");
        }
        assert!(PAGE_SHORTCUTS.contains(&"toggleBrowser"));
    }

    #[test]
    fn bounds_scale_and_tile() {
        let r = Rect::from_min_max(pos2(600.5, 90.0), pos2(1195.0, 760.25));
        assert_eq!(phys_bounds(r, 1.0), Some(Bounds { x: 601, y: 90, w: 594, h: 670 }));
        // 150 % DPI: edges are rounded, so a neighbour starting at `r.max` starts exactly where this one ends
        let b = phys_bounds(r, 1.5).unwrap();
        assert_eq!((b.x, b.y), (901, 135));
        assert_eq!(b.x as u32 + b.w, (1195.0f32 * 1.5).round() as u32);
        assert_eq!(b.y as u32 + b.h, (760.25f32 * 1.5).round() as u32);
    }

    #[test]
    fn degenerate_bounds_hide_instead_of_resizing_to_nothing() {
        assert_eq!(phys_bounds(Rect::from_min_max(pos2(10.0, 10.0), pos2(14.0, 400.0)), 1.0), None, "4 px wide");
        assert_eq!(phys_bounds(Rect::from_min_max(pos2(10.0, 10.0), pos2(400.0, 12.0)), 1.0), None, "2 px tall");
        assert_eq!(phys_bounds(Rect::from_min_max(pos2(f32::NAN, 0.0), pos2(100.0, 100.0)), 1.0), None);
        assert_eq!(phys_bounds(Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)), 0.0), None);
        assert_eq!(phys_bounds(Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)), f32::INFINITY), None);
        // inverted rect (collapsing panel animation) → no negative sizes
        assert_eq!(phys_bounds(Rect::from_min_max(pos2(100.0, 100.0), pos2(50.0, 50.0)), 1.0), None);
    }

    #[test]
    fn page_area_leaves_the_splitter_and_resize_gutters() {
        let body = Rect::from_min_max(pos2(700.0, 100.0), pos2(1200.0, 780.0));
        let v = view_rect(body);
        assert_eq!((v.left(), v.right(), v.top(), v.bottom()), (700.0 + EDGE_GUTTER, 1200.0 - EDGE_GUTTER, 100.0, 780.0));
        assert!(EDGE_GUTTER >= 5.0, "egui's splitter grab radius and chrome::resize_zones are both 5 px");
    }
}
