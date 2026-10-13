use std::io::{self, IsTerminal};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cliclack::{MultiProgress, ProgressBar};

use super::Format;
use super::layout::{Layout, Place};

/// The columns a cliclack box takes around its text: its bar and two spaces on the
/// left, two spaces and its bar on the right.
pub const BOX_PADDING: u16 = 6;

/// The terminal a command draws on. With human output on a styled stderr, cliclack
/// draws there: a flow's session and widgets, and a read's result when stdout is the
/// same terminal. Only one thing draws at a time: anything new settles the live widget
/// first.
pub struct Screen {
    format: Format,
    styled: bool,
    stdout_is_terminal: bool,
    layout: Layout,
    place: Place,
    session: AtomicBool,
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

/// How a result reaches the person.
pub enum Delivery {
    /// In one box on the terminal, its text fitted to the columns inside the box.
    Box(Option<usize>),
    /// On stdout, which carries nothing else: an envelope, or plain text ending with
    /// its summary.
    Stdout,
}

impl Screen {
    pub fn new(format: Format, layout: Layout, place: Place) -> Self {
        Self {
            format,
            styled: format == Format::Human && io::stderr().is_terminal(),
            stdout_is_terminal: io::stdout().is_terminal(),
            layout,
            place,
            session: AtomicBool::new(false),
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

    pub fn session_open(&self) -> bool {
        self.session.load(Ordering::Relaxed)
    }

    pub fn delivery(&self) -> Delivery {
        match self.layout {
            Layout::Read if self.shares_stdout() => Delivery::Box(
                columns(&console::Term::stderr())
                    .map(|columns| columns.saturating_sub(usize::from(BOX_PADDING))),
            ),
            _ => Delivery::Stdout,
        }
    }

    /// The columns of the terminal stdout is, if it is one.
    pub fn stdout_width(&self) -> Option<usize> {
        if self.stdout_is_terminal {
            columns(&console::Term::stdout())
        } else {
            None
        }
    }

    /// Opens a flow's session: the tracel badge, then what the flow does and where.
    pub fn begin(&self) {
        let Layout::Flow(flow) = &self.layout else {
            return;
        };
        if !self.styled {
            return;
        }
        let mut what = flow.what.clone();
        if let Some(project) = &self.place.project {
            what.push_str(&format!(
                " in {}",
                console::style(project).for_stderr().bold()
            ));
        }
        if let Some(environment) = &self.place.environment {
            let environment = format!(" · {environment}");
            what.push_str(&console::style(environment).for_stderr().dim().to_string());
        }
        let badge = console::style(" tracel ").for_stderr().black().on_green();
        let _ = cliclack::intro(badge);
        let _ = cliclack::log::remark(what);
        self.session.store(true, Ordering::Relaxed);
    }

    /// Shows a read's result: `body` in one box under `summary` and the place, or the
    /// summary alone when there is no body.
    pub fn show_result(&self, summary: Option<&str>, body: &str) {
        let place = self.place.to_string();
        let dim = |text: &str| console::style(text).for_stderr().dim().to_string();
        let summary = summary.unwrap_or_default();
        self.draw(|| {
            if body.trim().is_empty() {
                let line = [summary, place.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("  ");
                return cliclack::outro(dim(&line));
            }
            let summary = summary.strip_suffix('.').unwrap_or(summary);
            let title = match (summary.is_empty(), place.is_empty()) {
                (false, false) => format!(
                    "{}  {}",
                    console::style(summary).for_stderr().bold(),
                    dim(&place)
                ),
                (false, true) => console::style(summary).for_stderr().bold().to_string(),
                (true, _) => dim(&place),
            };
            cliclack::set_theme(ResultTheme);
            let drawn = cliclack::outro_note(title, body.trim_end());
            cliclack::reset_theme();
            drawn
        });
    }

    /// Ends a flow's session with its outcome.
    pub fn close(&self, outcome: &str) {
        if self.session.swap(false, Ordering::Relaxed) {
            self.draw(|| cliclack::outro(outcome));
        }
    }

    /// Ends a flow's session on an outcome that stops short of what it set out to do.
    pub fn close_short(&self, outcome: &str) {
        if self.session.swap(false, Ordering::Relaxed) {
            self.draw(|| cliclack::outro_cancel(outcome));
        }
    }

    /// Ends a flow's session with the commands to run next, one per line.
    pub fn close_with_next_steps(&self, steps: &str) {
        if self.session.swap(false, Ordering::Relaxed) {
            self.draw(|| cliclack::outro_note("Next steps", steps));
        }
    }

    /// Closes what a failure was just reported in: a flow's session with its failure, or
    /// a bare corner.
    pub fn close_failed(&self) {
        let outcome = match &self.layout {
            Layout::Flow(flow) if self.session.swap(false, Ordering::Relaxed) => {
                flow.failure.as_str()
            }
            _ => "",
        };
        self.draw(|| cliclack::outro_cancel(outcome));
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

/// cliclack's theme while a result's box is drawn. cliclack dims the text of a box, as
/// it dims a prompt's answer; a result keeps its own styles instead. tracel sets no
/// other theme, so drawing the box goes back to cliclack's own after it.
struct ResultTheme;

impl cliclack::Theme for ResultTheme {
    fn input_style(&self, state: &cliclack::ThemeState) -> console::Style {
        match state {
            cliclack::ThemeState::Cancel => console::Style::new().dim().strikethrough(),
            cliclack::ThemeState::Submit => console::Style::new(),
            _ => console::Style::new(),
        }
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
