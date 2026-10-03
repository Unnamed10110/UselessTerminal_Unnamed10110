//! The egui terminal widget: draws the emulator grid and turns mouse/IME input into selection, scrolling,
//! mouse reports and link clicks. Keyboard input does NOT come through here (see `input`).

use crate::boxdraw;
use crate::links::{link_at, Link, LinkKind};
use crate::search::Match;
use crate::session::{CursorSetting, Palette, TermSession};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{point_to_viewport, viewport_to_point, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use egui::{Color32, CursorIcon, Event, FontId, Id, PointerButton, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use std::time::{Duration, Instant};

const BLINK: Duration = Duration::from_millis(530);

pub struct ViewOptions<'a> {
    pub font: FontId,
    pub bold: Option<FontId>,
    pub italic: Option<FontId>,
    pub bold_italic: Option<FontId>,
    /// Multiplier on the font's natural row height (`terminal.lineHeight`).
    pub line_height: f32,
    /// Extra pixels added to every cell's width (`terminal.letterSpacing`, may be negative).
    pub letter_spacing: f32,
    pub padding: Vec2,
    pub palette: &'a Palette,
    pub bg_color: Color32,
    /// A background image is drawn behind the terminal: do not paint the default background.
    pub transparent_bg: bool,
    pub selection_bg: Color32,
    pub selection_fg: Option<Color32>,
    pub cursor_color: Color32,
    pub cursor_style: CursorSetting,
    pub cursor_blink: bool,
    /// This pane is the tab's focused pane (solid cursor; otherwise a hollow block).
    pub focused: bool,
    pub bold_is_bright: bool,
    pub copy_on_select: bool,
    pub matches: &'a [Match],
    pub current_match: Option<usize>,
    /// Draw the scrollbar thumb + failed-command marks.
    pub scrollbar: bool,
    /// Overview strip on the right edge (`terminal.minimap`, §5.13); its width is taken from the fit width.
    pub minimap: bool,
    /// CRT overlay (`terminal.crt`, §5.14).
    pub crt: bool,
}

#[derive(Default)]
pub struct ViewOutput {
    /// The user clicked into the pane.
    pub clicked: bool,
    pub context_menu: Option<Pos2>,
    pub link_click: Option<Link>,
    /// Ctrl + wheel notches (+1 up / -1 down).
    pub zoom: i32,
    pub cell: (f32, f32),
    pub hovered: bool,
    pub has_focus: bool,
}

#[derive(Clone, Copy)]
struct SCell {
    ch: char,
    fg: Color32,
    bg: Color32,
    bg_default: bool,
    flags: Flags,
}

/// Padding (points) between a pane's edge and its cell grid.
pub const PADDING: Vec2 = Vec2::new(4.0, 2.0);
/// Width of the scrollbar column.
pub const SCROLLBAR_WIDTH: f32 = 8.0;

/// How many cells fit in a pane of `size` points. The renderer sizes the emulator with this on every frame and the app
/// sizes the PTY at spawn with it: they MUST agree, or the shell starts one column wider than the pane and is resized
/// after it has drawn its prompt (garbled prompts and PSReadLine lists, spec §23.1).
pub fn cells_that_fit(size: Vec2, padding: Vec2, scrollbar: bool, minimap: bool, cell: (f32, f32)) -> (i32, i32) {
    let w = size.x - 2.0 * padding.x - if scrollbar { SCROLLBAR_WIDTH } else { 0.0 } - if minimap { crate::effects::MINIMAP_WIDTH } else { 0.0 };
    let h = size.y - 2.0 * padding.y;
    ((w / cell.0).floor() as i32, (h / cell.1).floor() as i32)
}

/// The cell a pane lays out with: [`measure`] plus `terminal.letterSpacing`. Spawn sizing and drawing MUST agree on it,
/// otherwise the shell starts at one width and is resized to another a frame later (garbled first prompt).
pub fn cell_size(ctx: &egui::Context, font: &FontId, line_height: f32, letter_spacing: f32) -> (f32, f32) {
    let (cw, ch) = measure(ctx, font, line_height);
    ((cw + letter_spacing).max(2.0), ch)
}

/// Cell size in points for a font + line-height multiplier, snapped to physical pixels vertically.
pub fn measure(ctx: &egui::Context, font: &FontId, line_height: f32) -> (f32, f32) {
    let ppp = ctx.pixels_per_point();
    let g = ctx.fonts_mut(|f| f.layout_no_wrap("M".repeat(32), font.clone(), Color32::WHITE));
    let cw = (g.size().x / 32.0).max(1.0);
    let ch = ((g.size().y * line_height) * ppp).round().max(1.0) / ppp;
    (cw, ch)
}

pub struct TermView {
    id: Id,
    cell: (f32, f32),
    metric_key: (u32, u32, u32, u32),
    drag: Option<(SelectionType, Point, Side)>,
    pub nav_anchor: Option<i64>,
    scroll_acc: f32,
    zoom_acc: f32,
    blink_epoch: Instant,
    last_frames: u64,
    sb_drag: bool,
    /// OS "client area animations" setting, re-read every 2 s (the CRT flicker honours it).
    anim: (Instant, bool),
    last_mouse_cell: Option<(usize, usize)>,
}

impl TermView {
    pub fn new(id: Id) -> Self {
        Self {
            anim: (Instant::now() - Duration::from_secs(10), true),
            id,
            cell: (8.0, 16.0),
            metric_key: (0, 0, 0, 0),
            drag: None,
            nav_anchor: None,
            scroll_acc: 0.0,
            zoom_acc: 0.0,
            blink_epoch: Instant::now(),
            last_frames: 0,
            sb_drag: false,
            last_mouse_cell: None,
        }
    }

    pub fn id(&self) -> Id {
        self.id
    }

    pub fn cell_size(&self) -> (f32, f32) {
        self.cell
    }

    fn metrics(&mut self, ui: &Ui, o: &ViewOptions) -> (f32, f32) {
        let ppp = ui.ctx().pixels_per_point();
        let key = (o.font.size.to_bits(), o.line_height.to_bits(), ppp.to_bits(), o.letter_spacing.to_bits());
        if key != self.metric_key || self.cell.0 <= 1.0 {
            self.cell = cell_size(ui.ctx(), &o.font, o.line_height, o.letter_spacing);
            self.metric_key = key;
        }
        self.cell
    }

    /// Draw the session into the remaining space of `ui`, handle mouse + IME input.
    pub fn show(&mut self, ui: &mut Ui, s: &TermSession, o: &ViewOptions) -> ViewOutput {
        let mut out = ViewOutput::default();
        let size = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let resp = ui.interact(rect, self.id, Sense::click_and_drag());
        let (cw, ch) = self.metrics(ui, o);
        out.cell = (cw, ch);
        s.set_cell_size(cw, ch);

        let inner = Rect::from_min_max(rect.min + o.padding, rect.max - Vec2::new(o.padding.x + if o.scrollbar { SCROLLBAR_WIDTH } else { 0.0 } + if o.minimap { crate::effects::MINIMAP_WIDTH } else { 0.0 }, o.padding.y));
        let (c, r) = cells_that_fit(rect.size(), o.padding, o.scrollbar, o.minimap, (cw, ch));
        let (cols, rows) = (c.max(2) as u16, r.max(1) as u16);
        s.resize(cols, rows);
        s.inner.clear_dirty();

        // ---- focus
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
            ui.memory_mut(|m| m.request_focus(self.id));
            out.clicked = true;
        }
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(self.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true });
        });
        let has_focus = ui.memory(|m| m.has_focus(self.id));
        out.has_focus = has_focus;
        out.hovered = resp.hovered();
        if has_focus {
            // Place the IME composition window at the cursor.
            ui.output_mut(|po| {
                po.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Terminal, rect: inner, cursor_rect: Rect::from_min_size(inner.min, Vec2::new(cw, ch)), should_interrupt_composition: false });
            });
        }

        // ---- snapshot (short lock)
        let frames = s.inner.frames.load(std::sync::atomic::Ordering::Relaxed);
        if frames != self.last_frames {
            self.last_frames = frames;
            self.blink_epoch = Instant::now(); // typing echo / output: show a solid cursor
        }
        let snap = snapshot(s, o, cols as usize, rows as usize);

        // ---- mouse
        let origin = inner.min;
        let to_cell = |p: Pos2| -> (usize, usize, Side) {
            let x = (p.x - origin.x).max(0.0);
            let y = (p.y - origin.y).max(0.0);
            let col = ((x / cw) as usize).min(cols as usize - 1);
            let row = ((y / ch) as usize).min(rows as usize - 1);
            let side = if (x / cw).fract() > 0.5 { Side::Right } else { Side::Left };
            (col, row, side)
        };
        let to_point = |col: usize, row: usize| viewport_to_point(snap.display_offset, Point::new(row, Column(col)));
        let ptr = ui.input(|i| i.pointer.hover_pos());
        let mods = ui.input(|i| i.modifiers);
        let mouse_mode = snap.mode.intersects(TermMode::MOUSE_MODE) && !mods.shift;
        let in_inner = ptr.is_some_and(|p| inner.contains(p));

        let mut hover_link: Option<(usize, Link)> = None;
        if let (Some(p), true) = (ptr, in_inner && resp.hovered()) {
            let (col, row, _) = to_cell(p);
            let text = row_text(&snap, row);
            if let Some(l) = link_at(&text, col) {
                hover_link = Some((row, l));
            }
            if hover_link.is_none() {
                hover_link = hyperlink_at(s, to_point(col, row)).map(|u| (row, Link { start: col, end: col + 1, text: u, kind: LinkKind::Url }));
            }
        }

        if mouse_mode {
            self.report_mouse(ui, s, &snap, &resp, in_inner, &to_cell);
        } else {
            self.selection(ui, s, &snap, o, &resp, inner, &to_cell, &to_point, hover_link.as_ref().map(|(_, l)| l), &mut out);
        }

        // wheel: scroll / zoom
        if resp.hovered() {
            let (scroll, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
            if ctrl {
                self.zoom_acc += scroll;
                if self.zoom_acc.abs() > 40.0 {
                    out.zoom = if self.zoom_acc > 0.0 { 1 } else { -1 };
                    self.zoom_acc = 0.0;
                }
            } else if scroll != 0.0 {
                self.scroll_acc += scroll / ch;
                let lines = self.scroll_acc.trunc() as i32;
                if lines != 0 {
                    self.scroll_acc -= lines as f32;
                    self.wheel(s, &snap, lines, &resp, in_inner, ptr, &to_cell);
                }
            }
        }

        // IME commit text goes to the shell.
        for ev in ui.input(|i| i.events.clone()) {
            if let Event::Ime(egui::ImeEvent::Commit(t)) = ev {
                if has_focus && !t.is_empty() {
                    s.write(t.as_bytes());
                }
            }
        }

        if hover_link.is_some() && mods.ctrl {
            ui.output_mut(|po| po.cursor_icon = CursorIcon::PointingHand);
        } else if resp.hovered() && !mouse_mode {
            ui.output_mut(|po| po.cursor_icon = CursorIcon::Text);
        }

        // ---- paint
        let painter = ui.painter_at(rect);
        let ppp = ui.ctx().pixels_per_point();
        if !o.transparent_bg {
            painter.rect_filled(rect, 0.0, o.bg_color);
        }
        paint_grid(&painter, &snap, o, inner.min, cw, ch, ppp, hover_link.as_ref(), mods.ctrl);
        paint_matches(&painter, s, &snap, o, inner.min, cw, ch);
        let blink_on = !o.cursor_blink || !o.focused || ((self.blink_epoch.elapsed().as_millis() / BLINK.as_millis()) % 2 == 0);
        paint_cursor(&painter, &snap, o, inner.min, cw, ch, blink_on, has_focus || o.focused);
        if o.cursor_blink && o.focused && snap.cursor.is_some() {
            ui.ctx().request_repaint_after(BLINK);
        }
        if o.scrollbar {
            self.scrollbar(ui, s, rect, &painter);
        }
        if o.minimap {
            self.minimap(ui, s, rect, ch, o.scrollbar, &painter);
        }
        if o.crt {
            if self.anim.0.elapsed() > Duration::from_secs(2) {
                self.anim = (Instant::now(), crate::effects::animations_enabled());
            }
            let flicker = self.anim.1;
            let dim = flicker && (self.blink_epoch.elapsed().as_millis() / 150) % 2 == 1;
            crate::effects::paint_crt(&painter, rect, dim);
            if flicker {
                ui.ctx().request_repaint_after(Duration::from_millis(150));
            }
        }
        out
    }

    // ------------------------------------------------------------------ minimap
    fn minimap(&mut self, ui: &Ui, s: &TermSession, rect: Rect, ch: f32, scrollbar: bool, painter: &egui::Painter) {
        let right = rect.right() - if scrollbar { SCROLLBAR_WIDTH } else { 0.0 };
        let strip = Rect::from_min_max(Pos2::new(right - crate::effects::MINIMAP_WIDTH, rect.top()), Pos2::new(right, rect.bottom()));
        crate::effects::paint_minimap(painter, strip, &s.minimap_samples(strip.height(), ch));
        let r = ui.interact(strip, self.id.with("minimap"), Sense::click_and_drag());
        if r.clicked() || r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                s.scroll_to_ratio(((p.y - strip.top()) / strip.height().max(1.0)).clamp(0.0, 1.0));
                self.nav_anchor = None;
            }
        }
    }

    // ------------------------------------------------------------------ selection
    #[allow(clippy::too_many_arguments)]
    fn selection(
        &mut self,
        ui: &Ui,
        s: &TermSession,
        snap: &Snap,
        o: &ViewOptions,
        resp: &egui::Response,
        inner: Rect,
        to_cell: &dyn Fn(Pos2) -> (usize, usize, Side),
        to_point: &dyn Fn(usize, usize) -> Point,
        link: Option<&Link>,
        out: &mut ViewOutput,
    ) {
        let alt = ui.input(|i| i.modifiers.alt);
        let ctrl = ui.input(|i| i.modifiers.ctrl);
        if resp.clicked_by(PointerButton::Primary) {
            if ctrl {
                if let Some(l) = link {
                    out.link_click = Some(l.clone());
                    return;
                }
            }
            s.clear_selection();
        }
        if resp.secondary_clicked() {
            out.context_menu = ui.input(|i| i.pointer.interact_pos());
        }
        let start_sel = |ty: SelectionType, p: Pos2| {
            let (c, r, side) = to_cell(p);
            let pt = to_point(c, r);
            let mut term = s.inner.term.lock();
            term.selection = Some(Selection::new(ty, pt, side));
            drop(term);
            s.inner.clear_dirty();
            (ty, pt, side)
        };
        if resp.double_clicked_by(PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                start_sel(SelectionType::Semantic, p);
                if o.copy_on_select {
                    copy_selection(s);
                }
            }
        } else if resp.triple_clicked_by(PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                start_sel(SelectionType::Lines, p);
                if o.copy_on_select {
                    copy_selection(s);
                }
            }
        } else if resp.drag_started_by(PointerButton::Primary) {
            if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                self.drag = Some(start_sel(if alt { SelectionType::Block } else { SelectionType::Simple }, p));
            }
        }
        if resp.dragged_by(PointerButton::Primary) {
            if let (Some(_), Some(p)) = (self.drag, ui.input(|i| i.pointer.interact_pos())) {
                // Auto-scroll while dragging above/below the pane.
                if p.y < inner.top() {
                    s.scroll(Scroll::Delta(1));
                } else if p.y > inner.bottom() {
                    s.scroll(Scroll::Delta(-1));
                }
                let (c, r, side) = to_cell(Pos2::new(p.x, p.y.clamp(inner.top(), inner.bottom() - 1.0)));
                let pt = to_point(c, r);
                if let Some(sel) = s.inner.term.lock().selection.as_mut() {
                    sel.update(pt, side);
                }
                ui.ctx().request_repaint();
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
            if o.copy_on_select {
                copy_selection(s);
            }
        }
        let _ = snap;
    }

    // ---------------------------------------------------------------- mouse reporting
    fn report_mouse(&mut self, ui: &Ui, s: &TermSession, snap: &Snap, resp: &egui::Response, in_inner: bool, to_cell: &dyn Fn(Pos2) -> (usize, usize, Side)) {
        if !resp.hovered() && !resp.dragged() {
            return;
        }
        let mode = snap.mode;
        let sgr = mode.contains(TermMode::SGR_MOUSE);
        let drag = mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION);
        let motion = mode.contains(TermMode::MOUSE_MOTION);
        let mut held: Option<u8> = None;
        for ev in ui.input(|i| i.events.clone()) {
            match ev {
                Event::PointerButton { pos, button, pressed, modifiers } if in_inner || !pressed => {
                    let b = match button {
                        PointerButton::Primary => 0,
                        PointerButton::Middle => 1,
                        PointerButton::Secondary => 2,
                        _ => continue,
                    };
                    let (c, r, _) = to_cell(pos);
                    s.write(&mouse_seq(sgr, b + mod_bits(modifiers), c, r, pressed));
                    held = pressed.then_some(b);
                    self.last_mouse_cell = Some((c, r));
                    if pressed {
                        ui.memory_mut(|m| m.request_focus(self.id));
                    }
                }
                Event::PointerMoved(pos) if in_inner => {
                    let (c, r, _) = to_cell(pos);
                    if self.last_mouse_cell == Some((c, r)) {
                        continue;
                    }
                    self.last_mouse_cell = Some((c, r));
                    let down = ui.input(|i| i.pointer.primary_down()).then_some(0u8).or_else(|| ui.input(|i| i.pointer.middle_down()).then_some(1)).or_else(|| ui.input(|i| i.pointer.secondary_down()).then_some(2));
                    if let Some(b) = down {
                        if drag {
                            s.write(&mouse_seq(sgr, b + 32 + mod_bits(ui.input(|i| i.modifiers)), c, r, true));
                        }
                    } else if motion {
                        s.write(&mouse_seq(sgr, 35 + mod_bits(ui.input(|i| i.modifiers)), c, r, true));
                    }
                }
                _ => {}
            }
        }
        let _ = held;
    }

    fn wheel(&mut self, s: &TermSession, snap: &Snap, lines: i32, resp: &egui::Response, in_inner: bool, ptr: Option<Pos2>, to_cell: &dyn Fn(Pos2) -> (usize, usize, Side)) {
        let mode = snap.mode;
        let shift = resp.ctx.input(|i| i.modifiers.shift);
        if mode.intersects(TermMode::MOUSE_MODE) && !shift {
            if let (Some(p), true) = (ptr, in_inner) {
                let (c, r, _) = to_cell(p);
                let sgr = mode.contains(TermMode::SGR_MOUSE);
                let b = if lines > 0 { 64 } else { 65 };
                for _ in 0..lines.abs() {
                    s.write(&mouse_seq(sgr, b, c, r, true));
                }
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let app = mode.contains(TermMode::APP_CURSOR);
            let seq: &[u8] = match (lines > 0, app) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            for _ in 0..lines.abs() {
                s.write(seq);
            }
        } else {
            s.scroll(Scroll::Delta(lines));
            self.nav_anchor = None;
        }
    }

    // ------------------------------------------------------------------ scrollbar
    fn scrollbar(&mut self, ui: &Ui, s: &TermSession, rect: Rect, painter: &egui::Painter) {
        let (hist, off, rows, marks, a_top) = {
            let term = s.inner.term.lock();
            let g = term.grid();
            (g.history_size(), g.display_offset(), term.screen_lines(), s.marks(), s.inner.track.lock().a_top)
        };
        if hist == 0 {
            return;
        }
        let sb = Rect::from_min_max(Pos2::new(rect.right() - 8.0, rect.top()), rect.right_bottom());
        let total = (hist + rows) as f32;
        let h = sb.height();
        let thumb_h = (rows as f32 / total * h).clamp(16.0, h);
        let thumb_top = sb.top() + (hist - off) as f32 / total * h;
        let id = self.id.with("scrollbar");
        let r = ui.interact(sb, id, Sense::click_and_drag());
        if r.drag_started() {
            self.sb_drag = true;
        }
        if r.dragged() || r.clicked() {
            if let Some(p) = r.interact_pointer_pos() {
                let frac = ((p.y - sb.top() - thumb_h / 2.0) / (h - thumb_h).max(1.0)).clamp(0.0, 1.0);
                let want = ((1.0 - frac) * hist as f32).round() as i32;
                s.scroll(Scroll::Delta(want - off as i32));
                self.nav_anchor = None;
            }
        }
        if r.drag_stopped() {
            self.sb_drag = false;
        }
        let active = r.hovered() || self.sb_drag || off > 0;
        let col = Color32::from_white_alpha(if active { 90 } else { 40 });
        painter.rect_filled(Rect::from_min_size(Pos2::new(sb.left() + 2.0, thumb_top), Vec2::new(4.0, thumb_h)), 2.0, col);
        // failed commands
        let top_abs = a_top - hist as i64;
        for m in marks.iter().filter(|m| m.done && m.exit.is_some_and(|c| c != 0)) {
            let f = (m.abs - top_abs) as f32 / total;
            if (0.0..=1.0).contains(&f) {
                let y = sb.top() + f * h;
                painter.rect_filled(Rect::from_min_size(Pos2::new(sb.left(), y), Vec2::new(8.0, 2.0)), 0.0, Color32::from_rgb(255, 60, 90));
            }
        }
    }
}

fn mod_bits(m: egui::Modifiers) -> u8 {
    (m.shift as u8) * 4 + (m.alt as u8) * 8 + (m.ctrl as u8) * 16
}

/// SGR (`ESC[<b;x;yM/m`) or legacy X10 (`ESC[M` + 3 bytes) mouse report; coordinates are 1-based.
fn mouse_seq(sgr: bool, b: u8, col: usize, row: usize, pressed: bool) -> Vec<u8> {
    if sgr {
        format!("\x1b[<{};{};{}{}", b, col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
    } else {
        let b = if pressed { b } else { 3 | (b & !3) };
        vec![0x1b, b'[', b'M', 32 + b, (32 + col + 1).min(255) as u8, (32 + row + 1).min(255) as u8]
    }
}

fn copy_selection(s: &TermSession) {
    if let Some(t) = s.selection_text() {
        if let Ok(mut c) = arboard::Clipboard::new() {
            let _ = c.set_text(t);
        }
    }
}

fn hyperlink_at(s: &TermSession, p: Point) -> Option<String> {
    let term = s.inner.term.lock();
    if p.line.0 < -(term.grid().history_size() as i32) || p.line.0 >= term.screen_lines() as i32 || p.column.0 >= term.columns() {
        return None;
    }
    term.grid()[p].hyperlink().map(|h| h.uri().to_string())
}

// ------------------------------------------------------------------------------------ snapshot

struct Snap {
    cols: usize,
    rows: usize,
    cells: Vec<SCell>,
    display_offset: usize,
    cursor: Option<(usize, usize, CursorShape)>,
    /// Selection as per-row inclusive column spans (viewport row → (c0, c1)).
    sel: Vec<Option<(usize, usize)>>,
    mode: TermMode,
}

fn snapshot(s: &TermSession, o: &ViewOptions, cols: usize, rows: usize) -> Snap {
    let term = s.inner.term.lock();
    let content = term.renderable_content();
    let off = content.display_offset;
    let colors: &Colors = content.colors;
    let default = SCell { ch: ' ', fg: to32(o.palette.fg), bg: Color32::TRANSPARENT, bg_default: true, flags: Flags::empty() };
    let mut cells = vec![default; cols * rows];
    for ic in content.display_iter {
        let Some(vp) = point_to_viewport(off, ic.point) else { continue };
        if vp.line >= rows || vp.column.0 >= cols {
            continue;
        }
        let cell = ic.cell;
        let fl = cell.flags;
        let mut fg = resolve(cell.fg, colors, o.palette, fl, o.bold_is_bright);
        let mut bg_c = resolve(cell.bg, colors, o.palette, Flags::empty(), false);
        let mut bg_default = matches!(cell.bg, Color::Named(NamedColor::Background));
        if fl.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg_c);
            bg_default = false;
        }
        if fl.contains(Flags::HIDDEN) {
            fg = bg_c;
        }
        cells[vp.line * cols + vp.column.0] = SCell { ch: cell.c, fg, bg: bg_c, bg_default, flags: fl };
    }
    let cursor = point_to_viewport(off, content.cursor.point)
        .filter(|p| p.line < rows && p.column.0 < cols && content.cursor.shape != CursorShape::Hidden)
        .map(|p| (p.column.0, p.line, content.cursor.shape));
    let mut sel = vec![None; rows];
    if let Some(r) = content.selection {
        for (row, slot) in sel.iter_mut().enumerate() {
            let line = Line(row as i32 - off as i32);
            let (a, b) = (r.start, r.end);
            if line < a.line || line > b.line {
                continue;
            }
            let (c0, c1) = if r.is_block {
                (a.column.0.min(b.column.0), a.column.0.max(b.column.0))
            } else {
                let c0 = if line == a.line { a.column.0 } else { 0 };
                let c1 = if line == b.line { b.column.0 } else { cols - 1 };
                (c0, c1)
            };
            *slot = Some((c0.min(cols - 1), c1.min(cols - 1)));
        }
    }
    Snap { cols, rows, cells, display_offset: off, cursor, sel, mode: content.mode }
}

fn to32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn dim(c: [u8; 3]) -> [u8; 3] {
    [(c[0] as u16 * 2 / 3) as u8, (c[1] as u16 * 2 / 3) as u8, (c[2] as u16 * 2 / 3) as u8]
}

fn resolve(c: Color, colors: &Colors, pal: &Palette, flags: Flags, bold_bright: bool) -> Color32 {
    let rgb = match c {
        Color::Spec(r) => [r.r, r.g, r.b],
        Color::Named(n) => {
            let mut i = n as usize;
            if bold_bright && flags.contains(Flags::BOLD) && i < 8 {
                i += 8;
            }
            if let Some(r) = colors[i] {
                [r.r, r.g, r.b]
            } else {
                match i {
                    0..=15 => pal.ansi[i],
                    259..=266 => dim(pal.ansi[i - 259]),
                    268 => dim(pal.fg),
                    256 | 267 => pal.fg,
                    257 => pal.bg,
                    258 => pal.cursor,
                    _ => pal.fg,
                }
            }
        }
        Color::Indexed(i) => {
            let mut i = i as usize;
            if bold_bright && flags.contains(Flags::BOLD) && i < 8 {
                i += 8;
            }
            colors[i].map(|r| [r.r, r.g, r.b]).unwrap_or_else(|| pal.indexed(i))
        }
    };
    let already_dim = matches!(c, Color::Named(n) if (259..=266).contains(&(n as usize)));
    let rgb = if flags.contains(Flags::DIM) && !already_dim { dim(rgb) } else { rgb };
    to32(rgb)
}

fn row_text(s: &Snap, row: usize) -> String {
    let mut t = String::with_capacity(s.cols);
    for c in 0..s.cols {
        let cell = &s.cells[row * s.cols + c];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        t.push(if cell.ch == '\0' { ' ' } else { cell.ch });
    }
    t
}

// ---------------------------------------------------------------------------------------- paint

#[allow(clippy::too_many_arguments)]
fn paint_grid(p: &egui::Painter, s: &Snap, o: &ViewOptions, origin: Pos2, cw: f32, ch: f32, ppp: f32, hover: Option<&(usize, Link)>, ctrl: bool) {
    let snap = |v: f32| (v * ppp).round() / ppp;
    for row in 0..s.rows {
        let y = snap(origin.y + row as f32 * ch);
        let y1 = snap(origin.y + (row + 1) as f32 * ch);
        let sel = s.sel[row];
        // ---- backgrounds (merge adjacent equal colours)
        let mut c = 0;
        while c < s.cols {
            let cell = &s.cells[row * s.cols + c];
            let selected = sel.is_some_and(|(a, b)| c >= a && c <= b);
            let bg = if selected { Some(o.selection_bg) } else if !cell.bg_default { Some(cell.bg) } else { None };
            let Some(bg) = bg else {
                c += 1;
                continue;
            };
            let mut e = c + 1;
            while e < s.cols {
                let n = &s.cells[row * s.cols + e];
                let nsel = sel.is_some_and(|(a, b)| e >= a && e <= b);
                let nbg = if nsel { Some(o.selection_bg) } else if !n.bg_default { Some(n.bg) } else { None };
                if nbg != Some(bg) {
                    break;
                }
                e += 1;
            }
            let x0 = snap(origin.x + c as f32 * cw);
            let x1 = snap(origin.x + e as f32 * cw);
            p.rect_filled(Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, y1)), 0.0, bg);
            c = e;
        }
        // ---- text
        let mut run = String::new();
        let mut run_col = 0usize;
        let mut run_style: Option<(Color32, bool, bool)> = None;
        let flush = |run: &mut String, run_col: usize, style: Option<(Color32, bool, bool)>| {
            if run.is_empty() {
                return;
            }
            if let Some((fg, bold, italic)) = style {
                let font = match (bold, italic) {
                    (true, true) => o.bold_italic.as_ref().or(o.bold.as_ref()),
                    (true, false) => o.bold.as_ref(),
                    (false, true) => o.italic.as_ref(),
                    _ => None,
                }
                .unwrap_or(&o.font);
                let g = p.layout_no_wrap(std::mem::take(run), font.clone(), fg);
                let ty = y + ((y1 - y) - g.size().y) / 2.0;
                p.galley(Pos2::new(origin.x + run_col as f32 * cw, ty), g, fg);
            }
            run.clear();
        };
        let mut c = 0;
        while c < s.cols {
            let cell = &s.cells[row * s.cols + c];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                c += 1;
                continue;
            }
            let selected = sel.is_some_and(|(a, b)| c >= a && c <= b);
            let fg = if selected { o.selection_fg.unwrap_or(cell.fg) } else { cell.fg };
            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            let cells_w = if wide { 2.0 } else { 1.0 };
            let x0 = snap(origin.x + c as f32 * cw);
            let ch_ = if cell.ch == '\0' { ' ' } else { cell.ch };
            let bold = cell.flags.contains(Flags::BOLD);
            let italic = cell.flags.contains(Flags::ITALIC);
            if !cell.flags.contains(Flags::HIDDEN) && ch_ != ' ' {
                let cellr = Rect::from_min_max(Pos2::new(x0, y), Pos2::new(snap(origin.x + (c as f32 + cells_w) * cw), y1));
                if is_custom(ch_) {
                    flush(&mut run, run_col, run_style.take());
                    boxdraw::draw(p, ch_, cellr, fg, ppp);
                } else if ch_.is_ascii() && !wide {
                    let style = (fg, bold, italic);
                    if run_style != Some(style) {
                        flush(&mut run, run_col, run_style.take());
                        run_col = c;
                        run_style = Some(style);
                    }
                    run.push(ch_);
                } else {
                    flush(&mut run, run_col, run_style.take());
                    let font = match (bold, italic) {
                        (true, _) => o.bold.as_ref().unwrap_or(&o.font),
                        (_, true) => o.italic.as_ref().unwrap_or(&o.font),
                        _ => &o.font,
                    };
                    let g = p.layout_no_wrap(ch_.to_string(), font.clone(), fg);
                    let ty = y + ((y1 - y) - g.size().y) / 2.0;
                    // Centre wide glyphs in their two cells, keep narrow ones at the cell origin.
                    let gx = if wide { x0 + (cw * 2.0 - g.size().x).max(0.0) / 2.0 } else { x0 };
                    p.galley(Pos2::new(gx, ty), g, fg);
                }
            } else if ch_ == ' ' && run_style.is_some() {
                // keep spaces inside a run so following glyphs stay on the grid
                let style = run_style.unwrap();
                if !run.is_empty() && c + 1 < s.cols && s.cells[row * s.cols + c + 1].ch != ' ' && s.cells[row * s.cols + c + 1].fg == style.0 {
                    run.push(' ');
                } else {
                    flush(&mut run, run_col, run_style.take());
                }
            } else {
                flush(&mut run, run_col, run_style.take());
            }
            // decorations
            let uf = cell.flags;
            if uf.intersects(Flags::ALL_UNDERLINES) {
                let yy = y1 - 1.5;
                p.line_segment([Pos2::new(x0, yy), Pos2::new(x0 + cw * cells_w, yy)], Stroke::new(1.0, fg));
                if uf.contains(Flags::DOUBLE_UNDERLINE) {
                    p.line_segment([Pos2::new(x0, yy - 2.0), Pos2::new(x0 + cw * cells_w, yy - 2.0)], Stroke::new(1.0, fg));
                }
            }
            if uf.contains(Flags::STRIKEOUT) {
                let yy = (y + y1) / 2.0;
                p.line_segment([Pos2::new(x0, yy), Pos2::new(x0 + cw * cells_w, yy)], Stroke::new(1.0, fg));
            }
            c += if wide { 2 } else { 1 };
        }
        flush(&mut run, run_col, run_style.take());
        if let Some((hr, l)) = hover {
            if *hr == row {
                let (a, b) = (l.start.min(s.cols), l.end.min(s.cols));
                let yy = y1 - 1.0;
                let col = if ctrl { Color32::from_rgb(120, 190, 255) } else { Color32::from_white_alpha(120) };
                p.line_segment([Pos2::new(origin.x + a as f32 * cw, yy), Pos2::new(origin.x + b as f32 * cw, yy)], Stroke::new(1.0, col));
            }
        }
    }
}

fn is_custom(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259F | 0xE0B0..=0xE0B3)
}

fn paint_matches(p: &egui::Painter, s: &TermSession, snap: &Snap, o: &ViewOptions, origin: Pos2, cw: f32, ch: f32) {
    if o.matches.is_empty() {
        return;
    }
    let (top, rows) = s.view_top_abs();
    for (i, m) in o.matches.iter().enumerate() {
        let row = m.abs - top;
        if row < 0 || row >= rows as i64 || row as usize >= snap.rows {
            continue;
        }
        let cur = o.current_match == Some(i);
        let r = Rect::from_min_max(
            Pos2::new(origin.x + m.c0 as f32 * cw, origin.y + row as f32 * ch),
            Pos2::new(origin.x + (m.c1 + 1) as f32 * cw, origin.y + (row + 1) as f32 * ch),
        );
        let (fill, stroke) = if cur { (Color32::from_rgba_unmultiplied(255, 122, 0, 110), Color32::WHITE) } else { (Color32::from_rgba_unmultiplied(255, 212, 0, 70), Color32::from_rgb(255, 212, 0)) };
        p.rect_filled(r, 0.0, fill);
        p.rect_stroke(r, 0.0, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_cursor(p: &egui::Painter, s: &Snap, o: &ViewOptions, origin: Pos2, cw: f32, ch: f32, blink_on: bool, focused: bool) {
    let Some((c, r, shape)) = s.cursor else { return };
    let cell = &s.cells[r * s.cols + c];
    let wide = cell.flags.contains(Flags::WIDE_CHAR);
    let w = if wide { cw * 2.0 } else { cw };
    let rect = Rect::from_min_size(Pos2::new(origin.x + c as f32 * cw, origin.y + r as f32 * ch), Vec2::new(w, ch));
    if !focused {
        p.rect_stroke(rect, 0.0, Stroke::new(1.0, o.cursor_color), egui::StrokeKind::Inside);
        return;
    }
    if !blink_on {
        return;
    }
    match shape {
        CursorShape::Block => {
            p.rect_filled(rect, 0.0, o.cursor_color);
            // Re-draw the glyph under the cursor in the background colour so it stays readable.
            if cell.ch != ' ' && cell.ch != '\0' && !is_custom(cell.ch) {
                let g = p.layout_no_wrap(cell.ch.to_string(), o.font.clone(), o.bg_color);
                let ty = rect.top() + (ch - g.size().y) / 2.0;
                p.galley(Pos2::new(rect.left(), ty), g, o.bg_color);
            }
        }
        CursorShape::Underline => {
            p.rect_filled(Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - 2.0), rect.right_bottom()), 0.0, o.cursor_color);
        }
        CursorShape::HollowBlock => {
            p.rect_stroke(rect, 0.0, Stroke::new(1.0, o.cursor_color), egui::StrokeKind::Inside);
        }
        _ => {
            p.rect_filled(Rect::from_min_size(rect.min, Vec2::new(2.0, ch)), 0.0, o.cursor_color);
        }
    }
}

