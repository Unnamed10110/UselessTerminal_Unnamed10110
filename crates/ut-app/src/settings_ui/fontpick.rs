//! Installed-font picker (§14.3): searchable, optionally filtered to monospace families, plus the helpers that turn a
//! picked family into the CSS-style font stack stored in `terminal.fontFamily`.

use crate::fonts;
use egui::{Id, Key, PopupCloseBehavior, RichText, ScrollArea, Ui};

/// `'Name', Consolas, 'Courier New', monospace` — the shape of the default stack (§5.2) with the picked family first.
pub fn stack_for(name: &str) -> String {
    let name = name.replace(['\'', '"'], "");
    let mut parts = vec![format!("'{}'", name.trim())];
    for fallback in ["Consolas", "Courier New"] {
        if !name.trim().eq_ignore_ascii_case(fallback) {
            parts.push(if fallback.contains(' ') { format!("'{fallback}'") } else { fallback.to_string() });
        }
    }
    parts.push("monospace".into());
    parts.join(", ")
}

/// The first family of a stack, as shown on the picker button.
pub fn primary(stack: &str) -> String {
    fonts::parse_stack(stack).into_iter().next().unwrap_or_else(|| "monospace".into())
}

/// Case-insensitive substring filter over `(family, monospaced)` entries.
pub fn filter<'a>(list: &'a [(String, bool)], query: &str, mono_only: bool) -> Vec<&'a (String, bool)> {
    let q = query.trim().to_lowercase();
    list.iter().filter(|(n, mono)| (!mono_only || *mono) && (q.is_empty() || n.to_lowercase().contains(&q))).collect()
}

/// A combo box listing the installed families. `mono_default`: start with the monospace filter on (terminal font).
/// Returns the family the user clicked.
pub fn picker(ui: &mut Ui, id: Id, current: &str, mono_default: bool) -> Option<String> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(240.0)
        .height(380.0)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.set_min_width(300.0);
            let (qid, mid) = (id.with("query"), id.with("mono"));
            let mut query: String = ui.data(|d| d.get_temp(qid)).unwrap_or_default();
            let mut mono: bool = ui.data(|d| d.get_temp(mid)).unwrap_or(mono_default);
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut query).hint_text("Search fonts…").desired_width(180.0));
                if ui.memory(|m| m.focused().is_none()) {
                    r.request_focus();
                }
                ui.checkbox(&mut mono, "Monospace only");
            });
            ui.data_mut(|d| {
                d.insert_temp(qid, query.clone());
                d.insert_temp(mid, mono);
            });
            let all = fonts::families();
            let shown = filter(all, &query, mono);
            ui.label(RichText::new(format!("{} of {} families", shown.len(), all.len())).small().weak());
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
            ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show_rows(ui, row_h, shown.len(), |ui, range| {
                for (name, is_mono) in shown[range].iter().copied() {
                    let text = if *is_mono { name.clone() } else { format!("{name}  ·  proportional") };
                    if ui.selectable_label(name == current, text).clicked() {
                        picked = Some(name.clone());
                        ui.close();
                    }
                }
            });
            if ui.input(|i| i.key_pressed(Key::Enter)) && shown.len() == 1 {
                picked = Some(shown[0].0.clone());
                ui.close();
            }
        });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_puts_the_pick_first_and_keeps_fallbacks() {
        assert_eq!(stack_for("JetBrains Mono"), "'JetBrains Mono', Consolas, 'Courier New', monospace");
        assert_eq!(stack_for("Consolas"), "'Consolas', 'Courier New', monospace");
        assert_eq!(stack_for("Weird 'Name'"), "'Weird Name', Consolas, 'Courier New', monospace");
        assert_eq!(fonts::parse_stack(&stack_for("Cascadia Code")), vec!["Cascadia Code", "Consolas", "Courier New"]);
    }

    #[test]
    fn primary_is_the_first_real_family() {
        assert_eq!(primary("'Cascadia Code', 'Cascadia Mono', Consolas, monospace"), "Cascadia Code");
        assert_eq!(primary("monospace"), "monospace");
        assert_eq!(primary("  "), "monospace");
    }

    #[test]
    fn filter_by_query_and_monospace_flag() {
        let l: Vec<(String, bool)> = [("Arial", false), ("Cascadia Code", true), ("Consolas", true), ("Segoe UI", false)].iter().map(|(n, m)| (n.to_string(), *m)).collect();
        assert_eq!(filter(&l, "", true).len(), 2);
        assert_eq!(filter(&l, "", false).len(), 4);
        assert_eq!(filter(&l, "CAS", false).len(), 1);
        assert_eq!(filter(&l, "ui", true).len(), 0);
        assert_eq!(filter(&l, " con ", true)[0].0, "Consolas");
    }

    #[test]
    fn installed_fonts_enumerate_on_windows() {
        let f = fonts::families();
        assert!(f.windows(2).all(|w| w[0].0.to_lowercase() <= w[1].0.to_lowercase()), "sorted");
        assert!(f.iter().any(|(n, _)| n == "Consolas" || n == "Segoe UI"), "a stock Windows font is present");
        assert!(f.iter().any(|(_, mono)| *mono), "at least one monospace family is flagged");
    }
}
