//! What the person running a command sees. On a terminal, a read draws its result in
//! one box, and a flow, a command that does work, draws a session: what it does and
//! where, its progress and prompts through `Terminal`, and its outcome. What a command
//! streams is written as it comes. Otherwise stdout carries the result and nothing
//! else, so it can be piped or captured: an envelope with JSON output, or the plain
//! text of its `Render` impl.

mod format;
mod human;
mod layout;
mod screen;
mod stderr;
mod stdout;
mod time;

use std::sync::Arc;

pub use format::{Format, FormatArg};
pub use human::{Details, Human, Table, counted, list_summary, number, short_digest, size};
pub use layout::{Layout, Place};
pub use stderr::{Declined, Terminal, Unanswered};
pub use stdout::{Outcome, Output, Render, StdoutClosed};
pub use time::{Timestamp, log_time, when};

/// Stdout and stderr for one command, drawing on the same screen. A flow's session
/// opens on the terminal now, saying what it does and where.
pub fn channels(
    format: Format,
    no_input: bool,
    layout: Layout,
    place: Place,
) -> (Output, Terminal) {
    let screen = Arc::new(screen::Screen::new(format, layout, place));
    screen.begin();
    (
        Output::new(Arc::clone(&screen)),
        Terminal::new(screen, no_input),
    )
}
