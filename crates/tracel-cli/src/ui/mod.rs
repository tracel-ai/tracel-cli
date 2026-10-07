//! What the person running a command sees. Stdout carries the command's result and
//! nothing else, so it can be piped or captured: an envelope with JSON output, or
//! the text of its `Render` impl. Stderr carries progress, prompts and diagnostics
//! through `Terminal`. Both draw on one screen when they share a terminal.

mod format;
mod human;
mod screen;
mod stderr;
mod stdout;
mod time;

use std::sync::Arc;

pub use format::{Format, FormatArg};
pub use human::{Details, Human, Table, json_section};
pub use stderr::Terminal;
pub use stdout::{Outcome, Output, Render, StdoutClosed};
pub use time::Timestamp;

/// Stdout and stderr for one command, drawing on the same screen.
pub fn channels(format: Format, no_input: bool) -> (Output, Terminal) {
    let screen = Arc::new(screen::Screen::new(format));
    (
        Output::new(Arc::clone(&screen)),
        Terminal::new(screen, no_input),
    )
}
