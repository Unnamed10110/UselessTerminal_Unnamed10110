//! System font loading for egui: the terminal font stack (with bold/italic faces) and the UI font.

use egui::{FontData, FontDefinitions, FontFamily, FontId};
use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use std::sync::{Arc, OnceLock};

fn db() -> &'static Database {
    static DB: OnceLock<Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut d = Database::new();
        d.load_system_fonts();
        d
    })
}

/// `'Cascadia Code', 'Cascadia Mono', Consolas, monospace` → `["Cascadia Code", "Cascadia Mono", "Consolas"]`.
pub fn parse_stack(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().trim_matches(['\'', '"']).trim().to_string())
        .filter(|p| !p.is_empty() && !matches!(p.to_ascii_lowercase().as_str(), "monospace" | "sans-serif" | "serif" | "system-ui"))
        .collect()
}

/// Installed font families, sorted case-insensitively, with the "monospaced" flag of the family's regular face
/// (the settings window's family picker, §14.3). Built once from the shared system font database.
pub fn families() -> &'static [(String, bool)] {
    static LIST: OnceLock<Vec<(String, bool)>> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut map: std::collections::BTreeMap<String, bool> = Default::default();
        for f in db().faces() {
            if let Some((name, _)) = f.families.first() {
                let e = map.entry(name.clone()).or_insert(false);
                *e |= f.monospaced;
            }
        }
        let mut v: Vec<(String, bool)> = map.into_iter().filter(|(n, _)| !n.starts_with('@') && !n.trim().is_empty()).collect();
        v.sort_by_key(|(n, _)| n.to_lowercase());
        v
    })
}

fn load(name: &str, weight: u16, italic: bool) -> Option<FontData> {
    let q = Query { families: &[Family::Name(name)], weight: Weight(weight), stretch: Stretch::Normal, style: if italic { Style::Italic } else { Style::Normal } };
    let d = db();
    let id = d.query(&q)?;
    d.with_face_data(id, |data, index| {
        let mut f = FontData::from_owned(data.to_vec());
        f.index = index;
        f
    })
}

#[derive(Clone)]
pub struct Fonts {
    pub regular: FontId,
    pub bold: FontId,
    pub italic: FontId,
    pub bold_italic: FontId,
    /// Names from the stack that could not be found (shown in the settings UI).
    pub missing: Vec<String>,
}

/// Fallbacks appended to every family: symbols, then the usual Windows monospace fonts.
const FALLBACKS: [&str; 5] = ["Segoe UI Symbol", "Cascadia Mono", "Consolas", "Segoe UI Emoji", "MS Gothic"];

/// Build and install the font definitions. `size` is in points (CSS-px equivalent).
pub fn install(ctx: &egui::Context, stack: &str, ui_family: &str, weight: u16, size: f32) -> Fonts {
    let mut defs = FontDefinitions::default();
    let names = parse_stack(stack);
    let all: Vec<String> = names.iter().cloned().chain(FALLBACKS.iter().map(|s| s.to_string())).collect();
    let bold_w = (weight + 250).min(900);
    let mut missing = Vec::new();
    // egui's bundled fonts stay at the end of every family so something always renders.
    let default_tail = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    for (family_name, w, italic) in [("term", weight, false), ("term-bold", bold_w, false), ("term-italic", weight, true), ("term-bold-italic", bold_w, true)] {
        let mut keys: Vec<String> = Vec::new();
        for (i, n) in all.iter().enumerate() {
            let key = format!("{family_name}-{i}-{n}");
            match load(n, w, italic).or_else(|| italic.then(|| load(n, w, false)).flatten()) {
                Some(data) => {
                    defs.font_data.insert(key.clone(), Arc::new(data));
                    keys.push(key);
                }
                None if family_name == "term" && i < names.len() => missing.push(n.clone()),
                None => {}
            }
        }
        keys.extend(default_tail.iter().cloned());
        let fam = if family_name == "term" { FontFamily::Monospace } else { FontFamily::Name(family_name.into()) };
        defs.families.insert(fam, keys);
    }
    // UI font.
    let mut ui_keys = Vec::new();
    for (i, n) in std::iter::once(ui_family).chain(["Segoe UI", "Segoe UI Symbol", "Segoe UI Emoji", "Arial"]).enumerate() {
        if let Some(d) = load(n, 400, false) {
            let key = format!("ui-{i}-{n}");
            defs.font_data.insert(key.clone(), Arc::new(d));
            ui_keys.push(key);
        }
    }
    let tail = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    ui_keys.extend(tail);
    defs.families.insert(FontFamily::Proportional, ui_keys);
    ctx.set_fonts(defs);
    let f = |n: &str| FontId::new(size, FontFamily::Name(n.into()));
    Fonts { regular: FontId::new(size, FontFamily::Monospace), bold: f("term-bold"), italic: f("term-italic"), bold_italic: f("term-bold-italic"), missing }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_parsing() {
        assert_eq!(parse_stack("'Cascadia Code', \"Cascadia Mono\", Consolas, 'Courier New', monospace"), vec!["Cascadia Code", "Cascadia Mono", "Consolas", "Courier New"]);
        assert!(parse_stack("monospace").is_empty());
    }

    #[test]
    fn consolas_loads_on_windows() {
        // Consolas ships with Windows; this proves the fontdb → egui path works end to end.
        assert!(load("Consolas", 400, false).is_some() || load("Segoe UI", 400, false).is_some());
    }
}
