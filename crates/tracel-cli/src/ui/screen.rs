use std::io::{self, IsTerminal};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cliclack::{MultiProgress, ProgressBar};

use super::Format;

/// The columns cliclack's bar and the space after it take at the start of a line.
const GUTTER_WIDTH: usize = 3;

/// The terminal a command draws on. With human output on a styled stderr, cliclack
/// draws a frame and widgets there, and the result too when stdout is the same
/// terminal. Only one thing draws at a time: anything new settles the live widget
/// first.
pub struct Screen {
    format: Format,
    styled: bool,
    stdout_is_terminal: bool,
    frame: AtomicBool,
    live: Mutex<Option<Live>>,
    next_id: AtomicU64,
}

/// A spinner or progress bar that redraws itself until it is finished.
struct Live {
    id: u64,
    bar: ProgressBar,
    group: Option<MultiProgress>,
    message: String,
}

#[derive(Clone, Copy)]
pub enum Ending {
    Done,
    Failed,
}

/// How a frame ends: a green badge, a bare corner, or a red badge.
pub enum Outro<'a> {
    Done(Option<&'a str>),
    Cancelled(&'a str),
}

impl Screen {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            styled: format == Format::Human && io::stderr().is_terminal(),
            stdout_is_terminal: io::stdout().is_terminal(),
            frame: AtomicBool::new(false),
            live: Mutex::new(None),
            next_id: AtomicU64::new(0),
        }
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Whether stderr draws with cliclack.
    pub fn is_styled(&self) -> bool {
        self.styled
    }

    /// Whether stdout lands among what stderr draws.
    fn shares_stdout(&self) -> bool {
        self.styled && self.stdout_is_terminal
    }

    pub fn frame_open(&self) -> bool {
        self.frame.load(Ordering::Relaxed)
    }

    /// Whether cliclack draws the result in the frame, which stdout shares.
    pub fn frames_results(&self) -> bool {
        self.shares_stdout() && self.frame_open()
    }

    /// The columns inside the frame, after cliclack's bar.
    pub fn frame_width(&self) -> Option<usize> {
        columns(&console::Term::stderr()).map(|columns| columns.saturating_sub(GUTTER_WIDTH))
    }

    /// The columns of the terminal stdout is, if it is one.
    pub fn stdout_width(&self) -> Option<usize> {
        if self.stdout_is_terminal {
            columns(&console::Term::stdout())
        } else {
            None
        }
    }

    /// Opens the frame a command draws in, under a green title badge.
    pub fn intro(&self, title: &str) {
        if !self.styled {
            return;
        }
        self.settle(Ending::Done);
        let badge = console::style(format!(" ▶ {title} ")).black().on_green();
        let _ = cliclack::intro(badge);
        self.frame.store(true, Ordering::Relaxed);
    }

    /// Ends the frame, open or not, so a command always finishes on its badge.
    pub fn outro(&self, outro: Outro<'_>) {
        if !self.styled {
            return;
        }
        self.settle(Ending::Done);
        let _ = match outro {
            Outro::Done(Some(message)) => {
                cliclack::outro(console::style(format!(" {message} ")).black().on_green())
            }
            Outro::Done(None) => cliclack::outro(""),
            Outro::Cancelled(message) => {
                cliclack::outro_cancel(console::style(format!(" {message} ")).black().on_red())
            }
        };
        self.frame.store(false, Ordering::Relaxed);
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Settles the live widget, then draws with cliclack.
    pub fn draw(&self, draw: impl FnOnce() -> io::Result<()>) {
        self.settle(Ending::Done);
        let _ = draw();
    }

    /// Settles the live widget, then draws the one `draw` makes as live widget `id`.
    pub fn show(
        &self,
        id: u64,
        message: String,
        draw: impl FnOnce() -> (ProgressBar, Option<MultiProgress>),
    ) {
        let mut live = self.lock();
        if let Some(previous) = live.take() {
            previous.end(Ending::Done, None);
        }
        let (bar, group) = draw();
        bar.start(&message);
        *live = Some(Live {
            id,
            bar,
            group,
            message,
        });
    }

    /// Changes the live widget `id`; nothing when it was settled.
    pub fn update(&self, id: u64, message: String, position: Option<u64>) {
        if let Some(live) = self.lock().as_mut().filter(|live| live.id == id) {
            if let Some(position) = position {
                live.bar.set_position(position);
            }
            live.bar.set_message(&message);
            live.message = message;
        }
    }

    /// Finishes the live widget `id`. False when something else settled it first.
    pub fn finish(&self, id: u64, ending: Ending, message: &str) -> bool {
        let mut live = self.lock();
        match live.take() {
            Some(widget) if widget.id == id => {
                widget.end(ending, Some(message));
                true
            }
            other => {
                *live = other;
                false
            }
        }
    }

    /// Finishes the live widget, if any, with its last message.
    pub fn settle(&self, ending: Ending) {
        if let Some(widget) = self.lock().take() {
            widget.end(ending, None);
        }
    }

    /// Stdout, once nothing else draws where it lands.
    pub fn stdout(&self) -> io::StdoutLock<'static> {
        if self.shares_stdout() {
            self.settle(Ending::Done);
        }
        io::stdout().lock()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Live>> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn columns(terminal: &console::Term) -> Option<usize> {
    terminal
        .size_checked()
        .map(|(_, columns)| usize::from(columns))
}

impl Live {
    fn end(self, ending: Ending, message: Option<&str>) {
        let message = message.unwrap_or(&self.message);
        match ending {
            Ending::Done => {
                self.bar.stop(message);
                if let Some(group) = &self.group {
                    group.stop();
                }
            }
            Ending::Failed => {
                self.bar.error(message);
                if let Some(group) = &self.group {
                    group.error(message);
                }
            }
        }
    }
}
