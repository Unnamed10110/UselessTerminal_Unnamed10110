//! Streaming VT scanner, log stripper and asciicast writer (spec §3.5, §4.4, §6, §12).
//! Everything here is synchronous, allocation-light and never decodes a whole stream as UTF-8.

pub mod cast;
mod parser;
pub mod scanner;
pub mod strip;
pub mod utf8;

pub use cast::{system_clock, CastHeader, CastWriter, Clock};
pub use scanner::{normalize_osc7, Config, Event, Osc52, Phase, Scanner, ShellKind, Turn};
pub use strip::LogStripper;
pub use utf8::Utf8Stream;
