#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assets;
mod browser;
mod clipboard;
mod cmd_data;
mod cmd_settings;
mod drops;
mod launch;
mod pane;
mod state;
mod sys;
mod taps;
mod win;

fn main() {
    app::run();
}
