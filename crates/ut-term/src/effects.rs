//! Optional pane overlays: the minimap (§5.13) and CRT mode (§5.14). Both are painted on top of the terminal with the
//! egui painter; the numbers (bar widths, brightness, scanline pitch, vignette) are the spec's.

use crate::session::TermSession;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Color32, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

pub const MINIMAP_WIDTH: f32 = 48.0;

/// What the minimap draws: one length per sampled line, plus where the viewport is (indices over the whole buffer).
pub struct MiniSamples {
    pub total: usize,
    pub step: usize,
    pub lens: Vec<u16>,
    pub view_top: usize,
    pub view_rows: usize,
}

/// `step = floor(total / min(total, ceil(height / lineHeight)))`, at least 1 (§5.13).
pub fn sample_step(total: usize, height: f32, line_height: f32) -> usize {
    let wanted = ((height / line_height.max(1.0)).ceil() as usize).max(1);
    (total / total.min(wanted).max(1)).max(1)
}

/// Bar width: `len / 120 × (w − 2)`, capped at the strip.
pub fn bar_width(len: u16, w: f32) -> f32 {
    (len as f32 / 120.0 * (w - 2.0)).min(w - 2.0)
}

/// Bar brightness: `min(255, 60 + 3·len)`.
pub fn brightness(len: u16) -> u8 {
    (60 + 3 * len as u32).min(255) as u8
}

/// The display offset (lines scrolled up from the bottom) that centres `ratio` of the buffer in the viewport.
pub fn offset_for_ratio(ratio: f32, total: usize, rows: usize, hist: usize) -> i32 {
    let top = (ratio.clamp(0.0, 1.0) * total as f32 - rows as f32 / 2.0).round() as i64;
    (hist as i64 - top).clamp(0, hist as i64) as i32
}

impl TermSession {
    /// Sample the buffer for the minimap: the length (last non-blank column) of every `step`-th line.
    pub fn minimap_samples(&self, height: f32, line_height: f32) -> MiniSamples {
        let term = self.inner.term.lock();
        let g = term.grid();
        let (hist, rows, cols) = (g.history_size(), term.screen_lines(), term.columns());
        let total = hist + rows;
        let step = sample_step(total, height, line_height);
        let mut lens = Vec::with_capacity(total / step + 1);
        for i in (0..total).step_by(step) {
            let row = &g[Line(i as i32 - hist as i32)];
            let len = (0..cols).rev().find(|&c| row[Column(c)].c != ' ' && row[Column(c)].c != '\0').map_or(0, |c| c + 1);
            lens.push(len as u16);
        }
        MiniSamples { total, step, lens, view_top: hist - g.display_offset(), view_rows: rows }
    }

    /// Jump so that `ratio` (0 = oldest line, 1 = newest) of the buffer is in the middle of the viewport.
    pub fn scroll_to_ratio(&self, ratio: f32) {
        let mut term = self.inner.term.lock();
        let (hist, rows) = (term.grid().history_size(), term.screen_lines());
        let want = offset_for_ratio(ratio, hist + rows, rows, hist);
        let cur = term.grid().display_offset() as i32;
        term.scroll_display(Scroll::Delta(want - cur));
        drop(term);
        self.inner.wake();
    }
}

/// Draw the overview into `strip` (the click/drag handling is the caller's).
pub fn paint_minimap(p: &Painter, strip: Rect, m: &MiniSamples) {
    p.rect_filled(strip, 0.0, Color32::from_black_alpha(60));
    let h = strip.height();
    let per = h / m.total.max(1) as f32;
    for (k, &len) in m.lens.iter().enumerate() {
        if len == 0 {
            continue;
        }
        let y = strip.top() + (k * m.step) as f32 * per;
        let b = brightness(len);
        p.rect_filled(
            Rect::from_min_size(Pos2::new(strip.left() + 1.0, y), Vec2::new(bar_width(len, strip.width()), (m.step as f32 * per).max(1.0))),
            0.0,
            Color32::from_rgba_unmultiplied(b, b, b, 128),
        );
    }
    let vy = strip.top() + m.view_top as f32 * per;
    let vh = (m.view_rows as f32 * per).max(4.0);
    p.rect_stroke(Rect::from_min_size(Pos2::new(strip.left(), vy.min(strip.bottom() - vh)), Vec2::new(strip.width(), vh)), 0.0, Stroke::new(1.0, Color32::from_white_alpha(77)), StrokeKind::Inside);
}

/// CRT overlay: 1 px scanlines every 3 px (alpha .15), a vignette from 60 % of the radius to .45 at the corners, and an
/// optional flicker frame (alpha .02 darkening, i.e. opacity .98). The colours are neutral black on purpose: a coloured
/// glow washed out every theme's ANSI colours in the WPF version (§5.14). The text glow is not implemented.
pub fn paint_crt(p: &Painter, rect: Rect, flicker_dim: bool) {
    let mut lines = Mesh::default();
    let mut y = rect.top();
    while y < rect.bottom() {
        let r = Rect::from_min_max(Pos2::new(rect.left(), y), Pos2::new(rect.right(), (y + 1.0).min(rect.bottom())));
        lines.add_colored_rect(r, Color32::from_black_alpha(38));
        y += 3.0;
    }
    p.add(lines);
    p.add(vignette(rect));
    if flicker_dim {
        p.rect_filled(rect, 0.0, Color32::from_black_alpha(5));
    }
}

/// Vignette alpha (0–255) at a point given as `[-1, 1]` offsets from the centre: 0 inside 60 % of the radius, .45 at the corners.
pub fn vignette_alpha(dx: f32, dy: f32) -> u8 {
    let r = (dx * dx + dy * dy).sqrt() / std::f32::consts::SQRT_2;
    (((r - 0.6) / 0.4).clamp(0.0, 1.0) * 0.45 * 255.0).round() as u8
}

fn vignette(rect: Rect) -> Mesh {
    const NX: usize = 24;
    const NY: usize = 14;
    let mut mesh = Mesh::default();
    for j in 0..=NY {
        for i in 0..=NX {
            let (u, v) = (i as f32 / NX as f32, j as f32 / NY as f32);
            let pos = Pos2::new(rect.left() + u * rect.width(), rect.top() + v * rect.height());
            mesh.vertices.push(Vertex { pos, uv: WHITE_UV, color: Color32::from_black_alpha(vignette_alpha(u * 2.0 - 1.0, v * 2.0 - 1.0)) });
        }
    }
    for j in 0..NY {
        for i in 0..NX {
            let a = (j * (NX + 1) + i) as u32;
            let b = a + 1;
            let c = a + (NX + 1) as u32;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    mesh
}

/// Whether the OS wants client-area animations (the flicker honours "reduce motion", §5.14 [P1]).
pub fn animations_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut on = windows::core::BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    on.as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_step_follows_the_spec_formula() {
        assert_eq!(sample_step(10_000, 800.0, 16.0), 200, "50 samples for 10k lines");
        assert_eq!(sample_step(30, 800.0, 16.0), 1, "short buffers are sampled line by line");
        assert_eq!(sample_step(0, 800.0, 16.0), 1);
    }

    #[test]
    fn bars_and_brightness() {
        assert_eq!(bar_width(60, 48.0), 23.0);
        assert_eq!(bar_width(500, 48.0), 46.0, "capped at the strip");
        assert_eq!(brightness(0), 60);
        assert_eq!(brightness(10), 90);
        assert_eq!(brightness(200), 255);
    }

    #[test]
    fn click_ratio_centres_the_viewport() {
        // 100 history + 40 rows: clicking the middle of the buffer shows lines 50..90 (top index 50 → offset 50)
        assert_eq!(offset_for_ratio(0.5, 140, 40, 100), 100 - (70 - 20));
        assert_eq!(offset_for_ratio(0.0, 140, 40, 100), 100, "top of the scrollback");
        assert_eq!(offset_for_ratio(1.0, 140, 40, 100), 0, "live screen");
    }

    #[test]
    fn pane_fit_subtracts_padding_scrollbar_and_minimap() {
        use crate::render::{cells_that_fit, PADDING};
        let size = Vec2::new(760.0, 480.0);
        // (760 - 2*4 - 8) / 8 = 93 columns, (480 - 2*2) / 16 = 29 rows
        assert_eq!(cells_that_fit(size, PADDING, true, false, (8.0, 16.0)), (93, 29));
        // the minimap takes another 48 px = 6 columns
        assert_eq!(cells_that_fit(size, PADDING, true, true, (8.0, 16.0)), (87, 29));
        assert_eq!(cells_that_fit(size, PADDING, false, false, (8.0, 16.0)).0, 94);
    }

    #[test]
    fn vignette_is_clear_in_the_middle_and_darkest_in_the_corners() {
        assert_eq!(vignette_alpha(0.0, 0.0), 0);
        assert_eq!(vignette_alpha(0.5, 0.5), 0);
        assert_eq!(vignette_alpha(1.0, 1.0), 115, "0.45 * 255");
        assert!(vignette_alpha(1.0, 0.0) < vignette_alpha(1.0, 1.0));
    }
}
