//! What the person running a command sees. Stdout carries the command's result and
//! nothing else, so it can be piped or captured: an envelope with JSON output, or
//! the text of its `Render` impl. Stderr carries progress, prompts and diagnostics
//! through `Terminal`. On a terminal both draw in one cliclack frame per command.

mod format;
mod human;
mod screen;
mod stderr;
mod stdout;
mod time;

use std::sync::Arc;

pub use format::{Format, FormatArg};
pub use human::{Details, Human, Table, counted};
pub use stderr::Terminal;
pub use stdout::{Outcome, Output, Render, StdoutClosed};
pub use time::{Timestamp, when};

/// Stdout and stderr for one command, drawing on the same screen. With a `title`, a
/// terminal shows the command in a frame under it, from now until the command ends.
pub fn channels(format: Format, no_input: bool, title: Option<&str>) -> (Output, Terminal) {
    let screen = Arc::new(screen::Screen::new(format));
    if let Some(title) = title {
        screen.intro(title);
    }
    (
        Output::new(Arc::clone(&screen)),
        Terminal::new(screen, no_input),
    )
}
