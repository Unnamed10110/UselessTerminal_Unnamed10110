//! Painter-drawn glyphs and text helpers for the sidebar (no icon font dependency, like `chrome::draw_glyph`).

use crate::theme;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Ui, Vec2};
use std::ops::Range;

#[derive(Clone, Copy)]
pub enum G {
    Folder,
    /// Points right when closed, down when open.
    Chevron { open: bool },
    Edit,
    Trash,
    Plus,
    More,
    Close,
    Bolt,
}

pub fn glyph(p: &Painter, g: G, r: Rect, c: Color32) {
    let s = Stroke::new(1.3, c);
    let (cx, cy) = (r.center().x, r.center().y);
    let k = r.width().min(r.height()) / 2.0 - 1.0;
    let pt = |x: f32, y: f32| Pos2::new(cx + k * x, cy + k * y);
    let line = |pts: &[(f32, f32)], closed: bool| {
        let mut v: Vec<Pos2> = pts.iter().map(|&(x, y)| pt(x, y)).collect();
        if closed {
            v.push(v[0]);
        }
        p.add(Shape::line(v, s));
    };
    match g {
        G::Folder => line(&[(-1.0, -0.6), (-0.25, -0.6), (0.1, -0.3), (1.0, -0.3), (1.0, 0.7), (-1.0, 0.7)], true),
        G::Chevron { open: false } => line(&[(-0.3, -0.6), (0.3, 0.0), (-0.3, 0.6)], false),
        G::Chevron { open: true } => line(&[(-0.6, -0.3), (0.0, 0.3), (0.6, -0.3)], false),
        G::Edit => {
            line(&[(-0.7, 0.7), (-0.55, 0.2), (0.35, -0.7), (0.75, -0.3), (-0.15, 0.6)], true);
            line(&[(0.15, -0.5), (0.55, -0.1)], false);
        }
        G::Trash => {
            line(&[(-0.8, -0.55), (0.8, -0.55)], false);
            line(&[(-0.3, -0.55), (-0.3, -0.85), (0.3, -0.85), (0.3, -0.55)], false);
            line(&[(-0.6, -0.55), (-0.5, 0.85), (0.5, 0.85), (0.6, -0.55)], false);
            line(&[(-0.2, -0.2), (-0.2, 0.5)], false);
            line(&[(0.2, -0.2), (0.2, 0.5)], false);
        }
        G::Plus => {
            line(&[(-0.7, 0.0), (0.7, 0.0)], false);
            line(&[(0.0, -0.7), (0.0, 0.7)], false);
        }
        G::More => {
            for dx in [-0.65, 0.0, 0.65] {
                p.circle_filled(pt(dx, 0.0), 1.3, c);
            }
        }
        G::Close => {
            line(&[(-0.55, -0.55), (0.55, 0.55)], false);
            line(&[(-0.55, 0.55), (0.55, -0.55)], false);
        }
        G::Bolt => line(&[(0.25, -0.9), (-0.55, 0.15), (0.0, 0.15), (-0.25, 0.9), (0.55, -0.15), (0.0, -0.15)], true),
    }
}

/// A button-sized hit rect with a hover background and a glyph; the caller decides what a click does.
pub fn glyph_button(ui: &mut Ui, rect: Rect, id: egui::Id, g: G, fg: Color32, hover_fg: Color32, hover_bg: Color32, tip: &str) -> bool {
    let resp = ui.interact(rect, id, egui::Sense::click());
    let hot = resp.hovered();
    if hot {
        ui.painter().rect_filled(rect, 4.0, hover_bg);
    }
    glyph(ui.painter(), g, Rect::from_center_size(rect.center(), Vec2::splat(rect.width().min(rect.height()) - 6.0)), if hot { hover_fg } else { fg });
    resp.on_hover_text(tip).clicked()
}

/// The generic shell glyph shown when an executable has no extractable icon.
pub fn console(p: &Painter, r: Rect, c: Color32) {
    crate::chrome::draw_glyph(p, crate::chrome::Glyph::Console, r, c);
}

/// Collapsible section header: chevron, title, optional count pill and an optional "+" button.
/// Returns (the title row was clicked, the "+" was clicked).
pub fn section_header(ui: &mut Ui, th: &theme::Ui, id: &str, title: &str, count: Option<usize>, open: bool, add_tip: Option<&str>) -> (bool, bool) {
    let w = ui.available_width();
    let (_, rect) = ui.allocate_space(Vec2::new(w, 28.0));
    let add_w = if add_tip.is_some() { 26.0 } else { 0.0 };
    let toggle = Rect::from_min_max(Pos2::new(rect.left() + 6.0, rect.top() + 2.0), Pos2::new(rect.right() - 6.0 - add_w, rect.bottom() - 2.0));
    let resp = ui.interact(toggle, ui.id().with(("sb-sec", id)), egui::Sense::click());
    let p = ui.painter().clone();
    if resp.hovered() {
        p.rect_filled(toggle, 4.0, th.hover_bg);
    }
    glyph(&p, G::Chevron { open }, Rect::from_center_size(Pos2::new(toggle.left() + 10.0, toggle.center().y), Vec2::splat(14.0)), th.muted);
    let g = p.layout_no_wrap(title.to_uppercase(), FontId::proportional(11.5), th.accent);
    let tx = toggle.left() + 22.0;
    p.galley(Pos2::new(tx, toggle.center().y - g.size().y / 2.0), g.clone(), th.accent);
    if let Some(n) = count {
        let t = p.layout_no_wrap(n.to_string(), FontId::proportional(11.0), th.accent);
        let pill = Rect::from_min_size(Pos2::new(tx + g.size().x + 8.0, toggle.center().y - 8.0), Vec2::new((t.size().x + 12.0).max(18.0), 16.0));
        p.rect_filled(pill, 8.0, th.accent_dim);
        p.galley(pill.center() - t.size() / 2.0, t, th.accent);
    }
    let mut add = false;
    if let Some(tip) = add_tip {
        let r = Rect::from_center_size(Pos2::new(rect.right() - 6.0 - 11.0, rect.center().y), Vec2::splat(22.0));
        add = glyph_button(ui, r, ui.id().with(("sb-sec-add", id)), G::Plus, th.muted, th.foreground, th.hover_bg, tip);
    }
    (resp.clicked(), add)
}

/// One-line text that is cut with an ellipsis at `max_w`, with the `hl` byte ranges marked.
pub fn job(text: &str, hl: &[Range<usize>], font: FontId, color: Color32, hl_fg: Color32, hl_bg: Color32, max_w: f32) -> LayoutJob {
    let mut j = LayoutJob::default();
    let plain = TextFormat { font_id: font.clone(), color, ..Default::default() };
    let hit = TextFormat { font_id: font, color: hl_fg, background: hl_bg, ..Default::default() };
    let mut i = 0;
    for r in hl {
        if r.start < i || r.end > text.len() {
            continue;
        }
        if r.start > i {
            j.append(&text[i..r.start], 0.0, plain.clone());
        }
        j.append(&text[r.clone()], 0.0, hit.clone());
        i = r.end;
    }
    if i < text.len() || text.is_empty() {
        j.append(&text[i..], 0.0, plain);
    }
    j.wrap = TextWrapping::truncate_at_width(max_w.max(1.0));
    j
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_splits_text_around_the_highlights() {
        let f = FontId::proportional(12.0);
        let j = job("Build Server", &[7..9, 10..12], f.clone(), Color32::WHITE, Color32::RED, Color32::BLUE, 100.0);
        let parts: Vec<(&str, Color32)> = j.sections.iter().map(|s| (&j.text[s.byte_range.start.0..s.byte_range.end.0], s.format.color)).collect();
        assert_eq!(parts, [("Build S", Color32::WHITE), ("er", Color32::RED), ("v", Color32::WHITE), ("er", Color32::RED)]);
        // no highlight / empty text / bad ranges never panic
        assert_eq!(job("abc", &[], f.clone(), Color32::WHITE, Color32::RED, Color32::BLUE, 10.0).text, "abc");
        assert_eq!(job("", &[], f.clone(), Color32::WHITE, Color32::RED, Color32::BLUE, 10.0).text, "");
        assert_eq!(job("abc", &[2..99], f, Color32::WHITE, Color32::RED, Color32::BLUE, 10.0).text, "abc");
    }
}
