use std::io::{self, IsTerminal};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cliclack::{MultiProgress, ProgressBar};

use super::Format;
use super::human::Style;

/// The width cliclack's bar and the space after it take at the start of a line.
const GUTTER_WIDTH: usize = 3;

/// The terminal a command draws on. Stderr draws cliclack frames and widgets on it
/// when it is a styled terminal, and stdout shares it when stdout is a terminal too.
/// Only one thing draws at a time: anything new settles the live widget first.
pub struct Screen {
    format: Format,
    styled: bool,
    stdout_is_terminal: bool,
    frame: AtomicBool,
    line_start: AtomicBool,
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
            line_start: AtomicBool::new(true),
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
    pub fn shares_stdout(&self) -> bool {
        self.styled && self.stdout_is_terminal
    }

    /// How text on stdout may look: colors and width only on a terminal, and drawn
    /// inside the frame when one is open there.
    pub fn text_style(&self) -> Style {
        if !self.stdout_is_terminal {
            return Style::plain();
        }
        let framed = self.shares_stdout() && self.frame_open();
        Style {
            color: console::colors_enabled(),
            width: console::Term::stdout()
                .size_checked()
                .map(|(_, columns)| usize::from(columns))
                .map(|columns| {
                    if framed {
                        columns.saturating_sub(GUTTER_WIDTH)
                    } else {
                        columns
                    }
                }),
            framed,
        }
    }

    pub fn frame_open(&self) -> bool {
        self.frame.load(Ordering::Relaxed)
    }

    /// Whether the next text on stdout starts a line, across the writes of a stream.
    pub fn line_start(&self) -> &AtomicBool {
        &self.line_start
    }

    /// Opens the frame a command draws in, under a green title badge.
    pub fn intro(&self, title: &str) {
        if !self.styled {
            return;
        }
        self.settle(Ending::Done);
        let badge = console::style(format!(" ▶ {title} ")).black().on_green();
        let _ = cliclack::intro(badge);
        self.line_start.store(true, Ordering::Relaxed);
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
        self.line_start.store(true, Ordering::Relaxed);
        self.frame.store(false, Ordering::Relaxed);
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
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
