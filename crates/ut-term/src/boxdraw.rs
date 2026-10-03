//! Pixel-exact drawing of box-drawing, block-element and Powerline glyphs (spec §5.2 `customGlyphs`): drawn as
//! rectangles/polygons filling the whole cell so lines connect without gaps whatever the font does.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke};

/// (left, right, up, down) arm styles for U+2500..=U+257F: 0 none, 1 light, 2 heavy, 3 double.
/// Dashed variants are drawn solid.
#[rustfmt::skip]
const BOX: [[u8; 4]; 128] = [
    [1,1,0,0],[2,2,0,0],[0,0,1,1],[0,0,2,2],[1,1,0,0],[2,2,0,0],[0,0,1,1],[0,0,2,2], // 2500-2507
    [1,1,0,0],[2,2,0,0],[0,0,1,1],[0,0,2,2],                                         // 2508-250B
    [0,1,0,1],[0,2,0,1],[0,1,0,2],[0,2,0,2],                                         // 250C-250F ┌┍┎┏
    [1,0,0,1],[2,0,0,1],[1,0,0,2],[2,0,0,2],                                         // 2510-2513 ┐┑┒┓
    [0,1,1,0],[0,2,1,0],[0,1,2,0],[0,2,2,0],                                         // 2514-2517 └┕┖┗
    [1,0,1,0],[2,0,1,0],[1,0,2,0],[2,0,2,0],                                         // 2518-251B ┘┙┚┛
    [0,1,1,1],[0,2,1,1],[0,1,2,1],[0,1,1,2],[0,1,2,2],[0,2,2,1],[0,2,1,2],[0,2,2,2], // 251C-2523 ├┝┞┟┠┡┢┣
    [1,0,1,1],[2,0,1,1],[1,0,2,1],[1,0,1,2],[1,0,2,2],[2,0,2,1],[2,0,1,2],[2,0,2,2], // 2524-252B ┤┥┦┧┨┩┪┫
    [1,1,0,1],[2,1,0,1],[1,2,0,1],[2,2,0,1],[1,1,0,2],[2,1,0,2],[1,2,0,2],[2,2,0,2], // 252C-2533 ┬┭┮┯┰┱┲┳
    [1,1,1,0],[2,1,1,0],[1,2,1,0],[2,2,1,0],[1,1,2,0],[2,1,2,0],[1,2,2,0],[2,2,2,0], // 2534-253B ┴┵┶┷┸┹┺┻
    [1,1,1,1],[2,1,1,1],[1,2,1,1],[2,2,1,1],                                         // 253C-253F ┼┽┾┿
    [1,1,2,1],[1,1,1,2],[1,1,2,2],                                                   // 2540-2542 ╀╁╂
    [2,1,2,1],[1,2,2,1],[2,1,1,2],[1,2,1,2],                                         // 2543-2546 ╃╄╅╆
    [2,2,2,1],[2,2,1,2],[2,1,2,2],[1,2,2,2],[2,2,2,2],                               // 2547-254B ╇╈╉╊╋
    [1,1,0,0],[2,2,0,0],[0,0,1,1],[0,0,2,2],                                         // 254C-254F dashed
    [3,3,0,0],[0,0,3,3],                                                             // 2550 ═ 2551 ║
    [0,3,0,1],[0,1,0,3],[0,3,0,3],[3,0,0,1],[1,0,0,3],[3,0,0,3],                     // 2552-2557 ╒╓╔╕╖╗
    [0,3,1,0],[0,1,3,0],[0,3,3,0],[3,0,1,0],[1,0,3,0],[3,0,3,0],                     // 2558-255D ╘╙╚╛╜╝
    [0,3,1,1],[0,1,3,3],[0,3,3,3],[3,0,1,1],[1,0,3,3],[3,0,3,3],                     // 255E-2563 ╞╟╠╡╢╣
    [3,3,0,1],[1,1,0,3],[3,3,0,3],[3,3,1,0],[1,1,3,0],[3,3,3,0],                     // 2564-2569 ╤╥╦╧╨╩
    [3,3,1,1],[1,1,3,3],[3,3,3,3],                                                   // 256A-256C ╪╫╬
    [0,1,0,1],[1,0,0,1],[1,0,1,0],[0,1,1,0],                                         // 256D-2570 rounded ╭╮╯╰
    [0,0,0,0],[0,0,0,0],[0,0,0,0],                                                   // 2571-2573 diagonals (separate)
    [1,0,0,0],[0,0,1,0],[0,1,0,0],[0,0,0,1],                                         // 2574-2577 ╴╵╶╷
    [2,0,0,0],[0,0,2,0],[0,2,0,0],[0,0,0,2],                                         // 2578-257B ╸╹╺╻
    [1,2,0,0],[0,0,1,2],[2,1,0,0],[0,0,2,1],                                         // 257C-257F ╼╽╾╿
];

/// Draw `ch` into `cell` (physical-pixel aligned by the caller) if it is a custom glyph. Returns false otherwise.
pub fn draw(p: &Painter, ch: char, cell: Rect, fg: Color32, ppp: f32) -> bool {
    let c = ch as u32;
    match c {
        0x2500..=0x257F => {
            draw_box(p, c, cell, fg, ppp);
            true
        }
        0x2580..=0x259F => {
            draw_block(p, c, cell, fg);
            true
        }
        0xE0B0 | 0xE0B2 | 0xE0B1 | 0xE0B3 => {
            draw_powerline(p, c, cell, fg);
            true
        }
        _ => false,
    }
}

fn snap(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

fn draw_box(p: &Painter, c: u32, cell: Rect, fg: Color32, ppp: f32) {
    if matches!(c, 0x2571..=0x2573) {
        let s = Stroke::new((1.0 / ppp).max(cell.height() / 14.0), fg);
        if c != 0x2572 {
            p.line_segment([cell.left_bottom(), cell.right_top()], s);
        }
        if c != 0x2571 {
            p.line_segment([cell.left_top(), cell.right_bottom()], s);
        }
        return;
    }
    let [l, r, u, d] = BOX[(c - 0x2500) as usize];
    let light = (cell.height() / 14.0).max(1.0 / ppp).round_to(1.0 / ppp);
    let thick = |s: u8| if s == 2 { light * 2.0 } else { light };
    let (cx, cy) = (snap(cell.center().x, ppp), snap(cell.center().y, ppp));
    // The widest arm decides how far the other direction's arms must extend to meet cleanly.
    let hmax = thick(l.max(r)) + if l.max(r) == 3 { light * 2.0 } else { 0.0 };
    let vmax = thick(u.max(d)) + if u.max(d) == 3 { light * 2.0 } else { 0.0 };
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| {
        p.rect_filled(Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1)), 0.0, fg);
    };
    // Horizontal arms.
    let horiz = |style: u8, left: bool| {
        if style == 0 {
            return;
        }
        let (x0, x1) = if left { (cell.left(), cx + vmax / 2.0) } else { (cx - vmax / 2.0, cell.right()) };
        if style == 3 {
            for dy in [-light, light] {
                rect(x0, cy + dy - light / 2.0, x1, cy + dy + light / 2.0);
            }
        } else {
            let t = thick(style);
            rect(x0, cy - t / 2.0, x1, cy + t / 2.0);
        }
    };
    let vert = |style: u8, up: bool| {
        if style == 0 {
            return;
        }
        let (y0, y1) = if up { (cell.top(), cy + hmax / 2.0) } else { (cy - hmax / 2.0, cell.bottom()) };
        if style == 3 {
            for dx in [-light, light] {
                rect(cx + dx - light / 2.0, y0, cx + dx + light / 2.0, y1);
            }
        } else {
            let t = thick(style);
            rect(cx - t / 2.0, y0, cx + t / 2.0, y1);
        }
    };
    horiz(l, true);
    horiz(r, false);
    vert(u, true);
    vert(d, false);
}

trait RoundTo {
    fn round_to(self, step: f32) -> f32;
}
impl RoundTo for f32 {
    fn round_to(self, step: f32) -> f32 {
        (self / step).round() * step
    }
}

fn draw_block(p: &Painter, c: u32, cell: Rect, fg: Color32) {
    let (w, h) = (cell.width(), cell.height());
    let at = |x0: f32, y0: f32, x1: f32, y1: f32, col: Color32| {
        p.rect_filled(Rect::from_min_max(Pos2::new(cell.left() + x0 * w, cell.top() + y0 * h), Pos2::new(cell.left() + x1 * w, cell.top() + y1 * h)), 0.0, col);
    };
    match c {
        0x2580 => at(0.0, 0.0, 1.0, 0.5, fg),
        0x2581..=0x2588 => {
            let k = (c - 0x2580) as f32 / 8.0;
            at(0.0, 1.0 - k, 1.0, 1.0, fg)
        }
        0x2589..=0x258F => {
            let k = (0x2590 - c) as f32 / 8.0;
            at(0.0, 0.0, k, 1.0, fg)
        }
        0x2590 => at(0.5, 0.0, 1.0, 1.0, fg),
        0x2591..=0x2593 => {
            let a = [64u8, 128, 192][(c - 0x2591) as usize];
            at(0.0, 0.0, 1.0, 1.0, Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), a))
        }
        0x2594 => at(0.0, 0.0, 1.0, 0.125, fg),
        0x2595 => at(0.875, 0.0, 1.0, 1.0, fg),
        0x2596..=0x259F => {
            // Quadrant bitmask: UL = 1, UR = 2, LL = 4, LR = 8.
            let mask: u8 = match c {
                0x2596 => 4,
                0x2597 => 8,
                0x2598 => 1,
                0x2599 => 1 | 4 | 8,
                0x259A => 1 | 8,
                0x259B => 1 | 2 | 4,
                0x259C => 1 | 2 | 8,
                0x259D => 2,
                0x259E => 2 | 4,
                _ => 2 | 4 | 8,
            };
            for (bit, x, y) in [(1u8, 0.0, 0.0), (2, 0.5, 0.0), (4, 0.0, 0.5), (8, 0.5, 0.5)] {
                if mask & bit != 0 {
                    at(x, y, x + 0.5, y + 0.5, fg);
                }
            }
        }
        _ => {}
    }
}

fn draw_powerline(p: &Painter, c: u32, cell: Rect, fg: Color32) {
    let (l, r, t, b) = (cell.left(), cell.right(), cell.top(), cell.bottom());
    let m = (t + b) / 2.0;
    match c {
        0xE0B0 => p.add(Shape::convex_polygon(vec![Pos2::new(l, t), Pos2::new(r, m), Pos2::new(l, b)], fg, Stroke::NONE)),
        0xE0B2 => p.add(Shape::convex_polygon(vec![Pos2::new(r, t), Pos2::new(l, m), Pos2::new(r, b)], fg, Stroke::NONE)),
        0xE0B1 => p.add(Shape::line(vec![Pos2::new(l, t), Pos2::new(r, m), Pos2::new(l, b)], Stroke::new(1.0, fg))),
        _ => p.add(Shape::line(vec![Pos2::new(r, t), Pos2::new(l, m), Pos2::new(r, b)], Stroke::new(1.0, fg))),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_the_basic_shapes() {
        assert_eq!(BOX[0x00], [1, 1, 0, 0]); // ─
        assert_eq!(BOX[0x02], [0, 0, 1, 1]); // │
        assert_eq!(BOX[0x0C], [0, 1, 0, 1]); // ┌
        assert_eq!(BOX[0x18], [1, 0, 1, 0]); // ┘
        assert_eq!(BOX[0x3C], [1, 1, 1, 1]); // ┼
        assert_eq!(BOX[0x4B], [2, 2, 2, 2]); // ╋
        assert_eq!(BOX[0x50], [3, 3, 0, 0]); // ═
        assert_eq!(BOX[0x6C], [3, 3, 3, 3]); // ╬
        assert_eq!(BOX[0x7F], [0, 0, 2, 1]); // ╿
        assert_eq!(BOX.len(), 128);
    }
}
