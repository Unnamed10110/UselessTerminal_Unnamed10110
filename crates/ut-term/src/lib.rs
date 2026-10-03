//! Terminal session (ConPTY + scanner + emulator grid) and its egui widget.
pub mod boxdraw;
pub mod effects;
pub mod input;
pub mod links;
pub mod render;
pub mod search;
pub mod session;
pub mod taps;

pub use alacritty_terminal::grid::Scroll;
pub use alacritty_terminal::term::TermMode;
pub use links::{Link, LinkKind};
pub use render::{cell_size, cells_that_fit, measure, TermView, ViewOptions, ViewOutput, PADDING, SCROLLBAR_WIDTH};
pub use search::{Match, SearchOpts};

pub use session::*;

#[cfg(test)]
mod session_tests;
