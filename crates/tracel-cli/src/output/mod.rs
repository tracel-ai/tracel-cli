//! Stdout. It carries a command's result and nothing else, so it can be piped or
//! captured. Progress, prompts and diagnostics go to stderr through `Terminal`.

mod format;
mod human;
mod stdout;
mod time;

pub use format::{Format, FormatArg};
pub use human::{Details, Table, json_section};
pub use stdout::{Outcome, Output, Render, StdoutClosed};
pub use time::Timestamp;
