//! Quake drop-down mode (§7.8, P1): the window slides down from the top of the monitor under the cursor, full work-area
//! width and `quake.heightPercent` tall, stays on top while shown, and optionally hides when it loses focus.

use crate::app::App;
use egui::{Context, Pos2, Vec2, ViewportCommand, WindowLevel};
use std::time::{Duration, Instant};
use ut_core::Rect;

const SLIDE: Duration = Duration::from_millis(120);

/// A running slide-in, in egui points.
pub struct Slide {
    start: Instant,
    x: f32,
    top: f32,
    h: f32,
}

/// Work area → the drop-down's rectangle (same units): full width, `percent` of the height (10–100).
pub fn dropdown_rect(work: Rect, percent: u32) -> Rect {
    let h = (work.height as i64 * percent.clamp(10, 100) as i64 / 100) as i32;
    Rect { x: work.x, y: work.y, width: work.width, height: h }
}

/// Window top at progress `t` (0 = just above the screen, 1 = in place), ease-out cubic.
pub fn slide_y(top: f32, h: f32, t: f32) -> f32 {
    let e = 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
    top - h * (1.0 - e)
}

fn pixels_to_points(r: Rect, ppp: f32) -> (f32, f32, f32, f32) {
    (r.x as f32 / ppp, r.y as f32 / ppp, r.width as f32 / ppp, r.height as f32 / ppp)
}

impl App {
    /// Show as a drop-down on the monitor under the cursor. `false` = no monitor found (caller shows normally).
    pub fn quake_show(&mut self, ctx: &Context) -> bool {
        let Some(work) = crate::winhooks::monitor_under_cursor() else { return false };
        let r = dropdown_rect(work, self.cfg.quake.height_percent);
        let ppp = ctx.native_pixels_per_point().unwrap_or(1.0).max(0.5);
        let (x, y, w, h) = pixels_to_points(r, ppp);
        ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(w, h)));
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(x, slide_y(y, h, 0.0))));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.quake_slide = Some(Slide { start: Instant::now(), x, top: y, h });
        self.quake_on = true;
        self.quake_was_focused = false;
        ctx.request_repaint();
        true
    }

    /// Back to a normal window: drop "always on top" and the drop-down geometry.
    pub fn quake_leave(&mut self, ctx: &Context) {
        if !self.quake_on {
            return;
        }
        self.quake_on = false;
        self.quake_slide = None;
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::Normal));
        if let Some(b) = self.normal_bounds {
            let ppp = ctx.native_pixels_per_point().unwrap_or(1.0).max(0.5);
            let (x, y, w, h) = pixels_to_points(b, ppp);
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(w, h)));
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(x, y)));
        }
    }

    /// Per frame: advance the slide, and hide on blur.
    pub fn quake_tick(&mut self, ctx: &Context) {
        if !self.quake_on {
            return;
        }
        if let Some(s) = &self.quake_slide {
            let t = s.start.elapsed().as_secs_f32() / SLIDE.as_secs_f32();
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(s.x, slide_y(s.top, s.h, t))));
            if t >= 1.0 {
                self.quake_slide = None;
            } else {
                ctx.request_repaint();
            }
        }
        if self.window_focused {
            self.quake_was_focused = true;
        } else if self.cfg.quake.hide_on_blur && self.quake_was_focused && self.quake_slide.is_none() && !self.minimized {
            self.hide_window(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropdown_is_full_width_and_a_percentage_of_the_height() {
        let work = Rect { x: -1920, y: 0, width: 1920, height: 1040 };
        let r = dropdown_rect(work, 50);
        assert_eq!((r.x, r.y, r.width, r.height), (-1920, 0, 1920, 520));
        assert_eq!(dropdown_rect(work, 0).height, 104, "clamped to 10%");
        assert_eq!(dropdown_rect(work, 400).height, 1040, "clamped to 100%");
    }

    #[test]
    fn slide_starts_above_the_screen_and_ends_in_place() {
        assert_eq!(slide_y(0.0, 500.0, 0.0), -500.0);
        assert_eq!(slide_y(0.0, 500.0, 1.0), 0.0);
        assert_eq!(slide_y(40.0, 500.0, 2.0), 40.0, "overshoot is clamped");
        let mid = slide_y(0.0, 500.0, 0.5);
        assert!(mid > -250.0 && mid < 0.0, "ease-out is past the halfway point at t=0.5: {mid}");
    }
}
