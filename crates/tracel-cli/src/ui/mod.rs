//! What a command shows the user. Results go to stdout: a JSON envelope, or the text
//! of their `Render` impl. Progress, prompts and errors go to stderr. On a terminal,
//! cliclack draws them: a read's result in one box, and a flow, a command that does
//! work, in a session that says what it does and ends on its outcome.

mod format;
mod human;
mod layout;
mod screen;
mod stderr;
mod stdout;
mod time;

use std::sync::Arc;

use screen::{Screen, Terminals};

pub use format::{Format, FormatArg};
pub use human::{Details, Human, Table, counted, list_summary, number, short_digest, size};
pub use layout::{Layout, Place};
pub use stderr::{Declined, Terminal, Unanswered};
pub use stdout::{Outcome, Output, Render, StdoutClosed};
pub use time::{Timestamp, log_time, when};

/// Open stdout and stderr for one command, drawing on the same screen. A flow's session
/// opens now, saying what it does and where.
pub fn channels(
    format: Format,
    no_input: bool,
    layout: Layout,
    place: Place,
) -> (Output, Terminal) {
    let screen = Arc::new(Screen::new(format, layout, place, Terminals::detect()));
    screen.intro();
    (
        Output::new(Arc::clone(&screen)),
        Terminal::new(screen, no_input),
    )
}
