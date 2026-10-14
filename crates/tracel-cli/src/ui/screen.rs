use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use cliclack::ProgressBar;

use super::Format;
use super::layout::{Layout, Place};

/// The columns a cliclack box takes around its text: its bar and two spaces on the
/// left, two spaces and its bar on the right.
pub const BOX_PADDING: u16 = 6;

/// Which of stderr and stdout are terminals.
#[derive(Clone, Copy)]
pub struct Terminals {
    pub stderr: bool,
    pub stdout: bool,
}

impl Terminals {
    pub fn detect() -> Self {
        Self {
            stderr: io::stderr().is_terminal(),
            stdout: io::stdout().is_terminal(),
        }
    }
}

/// Where a result goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    /// One box on the terminal, its text fitted to the columns inside the box.
    Box(Option<usize>),
    /// Stdout, which carries nothing else: an envelope, or plain text ending with the
    /// summary.
    Stdout,
}

/// The terminal a command draws on, shared by its stdout and stderr. With human output
/// on a terminal, cliclack draws on stderr: a flow's session with its steps and
/// prompts, and a read's box when stdout is the same terminal. One thing draws at a
/// time, so anything new first settles the live spinner or bar.
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

/// A spinner or bar that redraws itself until it ends.
struct Live {
    id: u64,
    bar: ProgressBar,
    message: String,
}

impl Screen {
    pub fn new(format: Format, layout: Layout, place: Place, terminals: Terminals) -> Self {
        Self {
            format,
            styled: format == Format::Human && terminals.stderr,
            stdout_is_terminal: terminals.stdout,
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

    /// Whether cliclack draws on stderr.
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

    /// Measure stdout's width, when it is a terminal.
    pub fn stdout_width(&self) -> Option<usize> {
        if self.stdout_is_terminal {
            columns(&console::Term::stdout())
        } else {
            None
        }
    }

    /// Build the line under the tracel badge that opens a flow's session: what the flow
    /// does and where. None when no session opens.
    fn header(&self) -> Option<String> {
        let Layout::Flow(what) = &self.layout else {
            return None;
        };
        if !self.styled {
            return None;
        }
        let mut header = what.clone();
        if let Some(project) = &self.place.project {
            let project = console::style(project).for_stderr().bold();
            header.push_str(&format!(" in {project}"));
        }
        if let Some(environment) = &self.place.environment {
            let environment = format!(" · {environment}");
            header.push_str(&console::style(environment).for_stderr().dim().to_string());
        }
        Some(header)
    }

    /// Open a flow's session: the tracel badge, then what the flow does and where.
    pub fn intro(&self) {
        if let Some(header) = self.header() {
            let badge = console::style(" tracel ").for_stderr().black().on_green();
            let _ = cliclack::intro(badge);
            let _ = cliclack::log::remark(header);
            self.session.store(true, Ordering::Relaxed);
        }
    }

    /// Show a read's result: `body` in one box under `summary` and the place, or the
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

    /// End a flow's session on its outcome. Nothing when no session is open.
    pub fn outro(&self, outcome: &str) {
        if self.session.swap(false, Ordering::Relaxed) {
            self.draw(|| cliclack::outro(outcome));
        }
    }

    /// End the command on its `failure`: cliclack's red closing line, in a session or not.
    pub fn fail(&self, failure: &str) {
        self.session.store(false, Ordering::Relaxed);
        self.draw(|| cliclack::outro_cancel(failure));
    }

    /// Settle the live widget, then draw with cliclack.
    pub fn draw(&self, draw: impl FnOnce() -> io::Result<()>) {
        self.settle();
        let _ = draw();
    }

    /// Settle the live widget, then start `bar` as the live one. Returns its id.
    pub fn start(&self, bar: ProgressBar, message: String) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut live = self.lock();
        if let Some(previous) = live.take() {
            previous.stop();
        }
        bar.start(&message);
        *live = Some(Live { id, bar, message });
        id
    }

    /// Change the message of live widget `id`, unless it has ended.
    pub fn set_message(&self, id: u64, message: String) {
        if let Some(live) = self.lock().as_mut().filter(|live| live.id == id) {
            live.bar.set_message(&message);
            live.message = message;
        }
    }

    /// Move the bar of live widget `id` to `position`, unless it has ended.
    pub fn set_position(&self, id: u64, position: u64) {
        if let Some(live) = self.lock().as_ref().filter(|live| live.id == id) {
            live.bar.set_position(position);
        }
    }

    /// End live widget `id` without leaving a line, unless it has ended.
    pub fn clear(&self, id: u64) {
        let mut live = self.lock();
        if live.as_ref().is_some_and(|live| live.id == id) {
            if let Some(widget) = live.take() {
                widget.bar.clear();
            }
        }
    }

    /// End the live widget, if any, with its last message as a line.
    pub fn settle(&self) {
        if let Some(widget) = self.lock().take() {
            widget.stop();
        }
    }

    /// End the live widget, if any, without leaving a line.
    pub fn clear_live(&self) {
        if let Some(widget) = self.lock().take() {
            widget.bar.clear();
        }
    }

    /// Stdout, once nothing else draws where it lands.
    pub fn stdout(&self) -> io::StdoutLock<'static> {
        if self.shares_stdout() {
            self.settle();
        }
        io::stdout().lock()
    }

    fn lock(&self) -> MutexGuard<'_, Option<Live>> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Live {
    fn stop(self) {
        self.bar.stop(&self.message);
    }
}

/// cliclack's theme while a result's box is drawn. cliclack dims a box's text as it
/// dims a prompt's answer, and a result keeps its own styles instead. tracel sets no
/// other theme, so the box goes back to cliclack's own after it.
struct ResultTheme;

impl cliclack::Theme for ResultTheme {
    fn input_style(&self, state: &cliclack::ThemeState) -> console::Style {
        match state {
            cliclack::ThemeState::Cancel => console::Style::new().dim().strikethrough(),
            _ => console::Style::new(),
        }
    }
}

fn columns(terminal: &console::Term) -> Option<usize> {
    terminal
        .size_checked()
        .map(|(_, columns)| usize::from(columns))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn screen(format: Format, layout: Layout, stderr: bool, stdout: bool) -> Screen {
        let place = Place {
            project: Some("test/demo".into()),
            environment: Some("development".into()),
        };
        Screen::new(format, layout, place, Terminals { stderr, stdout })
    }

    #[test]
    fn given_a_read_whose_stdout_shares_the_terminal_when_delivering_then_its_result_is_boxed() {
        let screen = screen(Format::Human, Layout::Read, true, true);

        let delivery = screen.delivery();

        assert!(matches!(delivery, Delivery::Box(_)));
    }

    #[rstest]
    #[case::stdout_piped(Format::Human, Layout::Read, true, false)]
    #[case::stderr_piped(Format::Human, Layout::Read, false, true)]
    #[case::json(Format::Json, Layout::Read, true, true)]
    #[case::flow(Format::Human, Layout::flow("Link"), true, true)]
    fn given_anything_but_a_read_sharing_the_terminal_when_delivering_then_the_result_goes_to_stdout(
        #[case] format: Format,
        #[case] layout: Layout,
        #[case] stderr: bool,
        #[case] stdout: bool,
    ) {
        let screen = screen(format, layout, stderr, stdout);

        let delivery = screen.delivery();

        assert!(matches!(delivery, Delivery::Stdout));
    }

    #[test]
    fn given_a_flow_on_a_terminal_when_it_starts_then_its_header_says_what_it_does_and_where() {
        let screen = screen(
            Format::Human,
            Layout::flow("Link this workspace"),
            true,
            false,
        );

        let header = screen.header().unwrap();

        assert_eq!(
            console::strip_ansi_codes(&header),
            "Link this workspace in test/demo · development"
        );
    }

    #[rstest]
    #[case::stderr_piped(Format::Human, false)]
    #[case::json(Format::Json, true)]
    fn given_a_flow_that_does_not_draw_when_it_starts_then_no_session_opens(
        #[case] format: Format,
        #[case] stderr: bool,
    ) {
        let screen = screen(format, Layout::flow("Link"), stderr, true);

        screen.intro();

        assert!(!screen.session_open());
    }
}
