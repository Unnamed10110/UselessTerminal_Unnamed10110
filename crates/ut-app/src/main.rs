#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod app;
mod browser;
mod chrome;
mod clipboard;
mod core;
mod debug;
mod drops;
mod fonts;
mod icons;
mod kit;
mod palette;
mod panels;
mod quake;
mod settings_ui;
mod sidebar;
mod single;
mod sys;
mod tab;
mod theme;
mod winhooks;

use app::{App, StartArgs};

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Runs even while the window is hidden (Quake mode / tray): hotkey, tray and forwarded launches.
        self.handle_native(ctx);
        self.debug_toggle(ctx);
        self.poll_second_instance(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self) {
        self.shutdown();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = self.theme.ui.chrome_bg;
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
    }
}

fn main() -> eframe::Result {
    tracing_subscriber_init();
    debug::log("main: start");
    let args: Vec<String> = std::env::args().collect();
    let ctx_cell: std::sync::Arc<parking_lot::Mutex<Option<egui::Context>>> = Default::default();
    let cc2 = ctx_cell.clone();
    let repaint = move || {
        if let Some(c) = cc2.lock().as_ref() {
            c.request_repaint();
        }
    };
    let rx_args = match single::acquire(&args, repaint) {
        single::Instance::Forwarded => return Ok(()),
        single::Instance::First(rx) => rx,
    };

    debug::log("main: single-instance acquired");
    let core = core::Core::new();
    debug::log("main: core ready");
    let (ws, _) = core.load_window_state();
    let (monitors, primary) = winhooks::monitors();
    let scale = system_scale();
    let mut vp = egui::ViewportBuilder::default()
        .with_title("Useless Terminal")
        .with_decorations(false)
        .with_min_inner_size([480.0, 360.0])
        .with_inner_size([1200.0, 800.0])
        .with_maximized(ws.maximized);
    if let Some(i) = winhooks::window_icon() {
        vp = vp.with_icon(i);
    }
    // Restore clamped to the connected monitors (never an off-screen window, §16.3).
    if let (Some(saved), false) = (ws.bounds, monitors.is_empty()) {
        let r = ut_core::clamp_bounds(saved, &monitors, primary);
        vp = vp.with_position([r.x as f32 / scale, r.y as f32 / scale]).with_inner_size([r.width as f32 / scale, r.height as f32 / scale]);
    }
    debug::log("main: run_native");
    let opts = eframe::NativeOptions { viewport: vp, ..Default::default() };
    let start = StartArgs::parse(&args);
    eframe::run_native(
        "Useless Terminal",
        opts,
        Box::new(move |cc| {
            debug::log("main: creation callback");
            *ctx_cell.lock() = Some(cc.egui_ctx.clone());
            let mut app = App::new(cc, core, start);
            debug::log("main: app created");
            app.second_instance_rx = Some(rx_args);
            cc.egui_ctx.set_zoom_factor(app.cfg.ui.scale.clamp(0.75, 2.0) as f32);
            Ok(Box::new(app))
        }),
    )
}

fn system_scale() -> f32 {
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    (unsafe { GetDpiForSystem() } as f32 / 96.0).max(1.0)
}

fn tracing_subscriber_init() {
    // Diagnostics go to stderr when `UT_LOG` is set (release builds have no console).
    if std::env::var_os("UT_LOG").is_some() {
        eprintln!("UT_LOG enabled");
    }
}
