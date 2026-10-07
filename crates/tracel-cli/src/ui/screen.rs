use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cliclack::{MultiProgress, ProgressBar};

use super::Format;
use super::human::Style;

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

    /// How text on stdout may look: colors and width only on a terminal.
    pub fn text_style(&self) -> Style {
        if !self.stdout_is_terminal {
            return Style::plain();
        }
        Style {
            color: console::colors_enabled(),
            width: console::Term::stdout()
                .size_checked()
                .map(|(_, columns)| usize::from(columns)),
        }
    }

    pub fn frame_open(&self) -> bool {
        self.frame.load(Ordering::Relaxed)
    }

    pub fn set_frame(&self, open: bool) {
        self.frame.store(open, Ordering::Relaxed);
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

    /// Stdout for text: inside an open frame, each line starts with the frame's bar.
    pub fn stdout(&self) -> impl Write + '_ {
        if self.shares_stdout() {
            self.settle(Ending::Done);
        }
        Gutter {
            out: io::stdout().lock(),
            bar: (self.shares_stdout() && self.frame_open()).then(gutter),
            line_start: &self.line_start,
        }
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

/// The bar cliclack draws down the left of a frame.
fn gutter() -> String {
    let bar = console::style(console::Emoji("│", "|")).bright().black();
    format!("{}  ", bar.for_stdout())
}

/// Writes through to `out`, starting each line with `bar` when there is one.
/// `line_start` persists across writes, since a stream arrives in pieces.
struct Gutter<'a, W> {
    out: W,
    bar: Option<String>,
    line_start: &'a AtomicBool,
}

impl<W: Write> Write for Gutter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(bar) = &self.bar else {
            return self.out.write(buffer);
        };
        for line in buffer.split_inclusive(|&byte| byte == b'\n') {
            if self.line_start.load(Ordering::Relaxed) {
                self.out.write_all(bar.as_bytes())?;
            }
            self.out.write_all(line)?;
            self.line_start
                .store(line.ends_with(b"\n"), Ordering::Relaxed);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn through_gutter(pieces: &[&str]) -> String {
        let line_start = AtomicBool::new(true);
        let mut written = Vec::new();
        for piece in pieces {
            let mut gutter = Gutter {
                out: &mut written,
                bar: Some("| ".into()),
                line_start: &line_start,
            };
            gutter.write_all(piece.as_bytes()).unwrap();
        }
        String::from_utf8(written).unwrap()
    }

    #[test]
    fn every_line_in_a_frame_starts_with_its_bar() {
        assert_eq!(through_gutter(&["a\nb\n"]), "| a\n| b\n");
        assert_eq!(through_gutter(&["\n"]), "| \n");
    }

    #[test]
    fn lines_split_across_writes_get_one_bar() {
        assert_eq!(
            through_gutter(&["par", "tial\nnext", "\n"]),
            "| partial\n| next\n"
        );
    }

    #[test]
    fn without_a_frame_text_passes_through() {
        let line_start = AtomicBool::new(true);
        let mut written = Vec::new();
        Gutter {
            out: &mut written,
            bar: None,
            line_start: &line_start,
        }
        .write_all(b"a\nb")
        .unwrap();
        assert_eq!(written, b"a\nb");
    }
}
