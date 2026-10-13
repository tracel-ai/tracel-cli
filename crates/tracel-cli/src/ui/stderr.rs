use std::fmt::Display;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

use cliclack::ProgressBar;

use super::Format;
use super::screen::{Ending, Screen};
use crate::error::{CliError, ErrorKind, ErrorReport};

/// A prompt the user answered no. The command stops there and ends on what it didn't
/// do, such as "Nothing was linked.", as a normal outcome.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Declined(String);

impl Declined {
    pub fn new(outcome: impl Into<String>) -> Self {
        Self(outcome.into())
    }

    pub fn outcome(&self) -> &str {
        &self.0
    }
}

/// What a prompt says when nobody can answer it: a message such as "Unlinking
/// test/demo needs confirmation." and a hint naming the flag that answers it, such as
/// "Pass --yes to unlink without asking."
pub struct Unanswered {
    message: String,
    hint: String,
}

impl Unanswered {
    pub fn new(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: hint.into(),
        }
    }

    fn into_error(self, kind: ErrorKind) -> CliError {
        CliError::new(kind, self.message).with_hint(self.hint)
    }
}

/// Stderr, for the person running a command: what it is doing, its progress, and
/// questions for them. With JSON output it carries only errors and the instructions
/// a person must follow, as plain lines.
#[derive(Clone)]
pub struct Terminal {
    screen: Arc<Screen>,
    no_input: bool,
}

impl Terminal {
    pub fn new(screen: Arc<Screen>, no_input: bool) -> Self {
        Self { screen, no_input }
    }

    fn is_human(&self) -> bool {
        self.screen.format() == Format::Human
    }

    fn is_styled(&self) -> bool {
        self.screen.is_styled()
    }

    /// A plain line, for stderr that is not a styled terminal.
    fn line(&self, text: impl Display) {
        let _ = writeln!(
            io::stderr(),
            "{}",
            console::strip_ansi_codes(&text.to_string())
        );
    }

    pub fn print_warning(&self, message: &str) {
        if self.is_styled() {
            self.screen.draw(|| cliclack::log::warning(message));
        } else if self.is_human() {
            self.line(format_args!("warning: {message}"));
        }
    }

    pub fn print(&self, message: &str) {
        if self.is_styled() {
            self.screen.draw(|| cliclack::log::info(message));
        } else if self.is_human() {
            self.line(message);
        }
    }

    pub fn print_success(&self, message: &str) {
        if self.is_styled() {
            self.screen.draw(|| cliclack::log::success(message));
        } else if self.is_human() {
            self.line(message);
        }
    }

    pub fn step(&self, message: &str) {
        if self.is_styled() {
            self.screen.draw(|| cliclack::log::step(message));
        } else {
            self.print(message);
        }
    }

    /// Instructions a person must follow before the command can go on, shown in
    /// every format.
    pub fn instruct(&self, message: &str) {
        if self.is_human() {
            self.print(message);
        } else {
            self.line(message);
        }
    }

    /// Reports a failed command: the message and hint here, the envelope on stdout
    /// with JSON output. On a terminal both are one error, then a bare corner closes
    /// the frame.
    pub fn error(&self, report: &ErrorReport<'_>) {
        self.screen.settle(Ending::Cleared);
        if !self.is_human() {
            self.line(format_args!("error: {}", one_line(&report.message)));
        } else if self.is_styled() {
            let text = match report.hint {
                Some(hint) => format!("{}\n{hint}", report.message),
                None => report.message.clone(),
            };
            self.screen.draw(|| cliclack::log::error(text));
            self.screen.close_failed();
        } else {
            self.line(format_args!("error: {}", report.message));
            if let Some(hint) = report.hint {
                self.line(hint);
            }
        }
    }

    /// A spinner for one step, drawn from now until it ends.
    pub fn spinner(&self, message: impl Display) -> Progress {
        self.live(message.to_string(), cliclack::spinner())
    }

    /// A bar that counts `total` steps of one task, drawn from now until it ends.
    pub fn progress(&self, message: impl Display, total: u64) -> Progress {
        self.live(message.to_string(), cliclack::progress_bar(total))
    }

    fn live(&self, message: String, bar: ProgressBar) -> Progress {
        let id = self.screen.next_id();
        if self.is_styled() {
            self.screen.show(id, bar, message);
        } else {
            self.print(&message);
        }
        Progress {
            terminal: self.clone(),
            id,
        }
    }

    /// A URL as cliclack shows it: styled unless `NO_COLOR` is set and, in terminals that
    /// render hyperlinks, one, so a click opens it. Plain when stderr is not a terminal.
    pub fn format_url(&self, url: &url::Url) -> String {
        if !self.is_styled() {
            return url.to_string();
        }
        let text = console::style(url).for_stderr().blue().bold().to_string();
        if supports_hyperlinks::on(supports_hyperlinks::Stream::Stderr) {
            hyperlink(url, &text)
        } else {
            text
        }
    }

    /// Fails with `kind` when nobody can answer a prompt, and otherwise clears the
    /// screen for it.
    fn require_answer(&self, kind: ErrorKind, unanswered: Unanswered) -> anyhow::Result<()> {
        if !self.is_interactive() {
            return Err(unanswered.into_error(kind).into());
        }
        self.screen.settle(Ending::Done);
        Ok(())
    }

    pub fn confirm(
        &self,
        question: &str,
        initial: bool,
        unanswered: Unanswered,
    ) -> anyhow::Result<bool> {
        self.require_answer(ErrorKind::ConfirmationRequired, unanswered)?;
        cliclack::confirm(question)
            .initial_value(initial)
            .interact()
            .map_err(anyhow::Error::from)
    }

    /// Offers a shortcut with a question answered yes by default. When nobody can answer, the
    /// question is not asked and the answer is no.
    pub fn offer(&self, question: &str) -> anyhow::Result<bool> {
        if !self.is_interactive() {
            return Ok(false);
        }
        self.screen.settle(Ending::Done);
        cliclack::confirm(question)
            .initial_value(true)
            .interact()
            .map_err(anyhow::Error::from)
    }

    /// Asks for optional text.
    pub fn input(&self, question: &str, unanswered: Unanswered) -> anyhow::Result<String> {
        self.input_validated(question, "", unanswered, |_| Ok(()))
    }

    /// Asks for text that `validate` accepts, `default` when the answer is empty.
    pub fn input_validated(
        &self,
        question: &str,
        default: &str,
        unanswered: Unanswered,
        validate: impl Fn(&String) -> Result<(), String> + 'static,
    ) -> anyhow::Result<String> {
        self.require_answer(ErrorKind::Usage, unanswered)?;
        cliclack::input(question)
            .default_input(default)
            .placeholder(default)
            .required(false)
            .validate(validate)
            .interact()
            .map_err(anyhow::Error::from)
    }

    pub fn select<T: Clone + Eq>(
        &self,
        question: &str,
        items: &[(T, impl Display, impl Display)],
        initial: Option<T>,
        unanswered: Unanswered,
    ) -> anyhow::Result<T> {
        self.require_answer(ErrorKind::Usage, unanswered)?;
        let mut prompt = cliclack::select(question).items(items);
        if let Some(initial) = initial {
            prompt = prompt.initial_value(initial);
        }
        prompt.interact().map_err(anyhow::Error::from)
    }

    pub fn multiselect<T: Clone + Eq>(
        &self,
        question: &str,
        items: &[(T, impl Display, impl Display)],
        initial: Vec<T>,
        unanswered: Unanswered,
    ) -> anyhow::Result<Vec<T>> {
        self.require_answer(ErrorKind::Usage, unanswered)?;
        cliclack::multiselect(question)
            .items(items)
            .initial_values(initial)
            .required(true)
            .interact()
            .map_err(anyhow::Error::from)
    }

    /// Ends a flow's session with what it did.
    pub fn outro(&self, message: &str) {
        if self.screen.session_open() {
            self.screen.close(message);
        } else {
            self.print_success(message);
        }
    }

    /// Whether a person can answer prompts: they read from stdin and draw on stderr.
    pub fn is_interactive(&self) -> bool {
        let ci = std::env::var_os("CI");
        allows_input(
            io::stdin().is_terminal(),
            io::stderr().is_terminal(),
            self.screen.format(),
            ci.as_ref().map(|value| value.to_str().unwrap_or("true")),
            self.no_input,
        )
    }
}

fn allows_input(
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
    format: Format,
    ci: Option<&str>,
    no_input: bool,
) -> bool {
    stdin_is_terminal
        && stderr_is_terminal
        && format == Format::Human
        && matches!(ci, None | Some("" | "0" | "false"))
        && !no_input
}

fn one_line(message: &str) -> String {
    console::strip_ansi_codes(message)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` as a terminal hyperlink to `url` (OSC 8): terminals that render hyperlinks open
/// `url` on a click on `text`.
fn hyperlink(url: &url::Url, text: &str) -> String {
    format!("\u{1b}]8;;{url}\u{1b}\\{text}\u{1b}]8;;\u{1b}\\")
}

/// A spinner or bar for one step, drawn from its start until it ends. When something
/// else draws before it ends, it stops early with its last message.
#[derive(Clone)]
pub struct Progress {
    terminal: Terminal,
    id: u64,
}

impl Progress {
    /// A passing detail of the step, such as what cargo compiles: shown on a terminal
    /// only, where it replaces the message.
    pub fn set_message(&self, message: impl Display) {
        self.terminal.screen.update(self.id, message.to_string());
    }

    /// How many of a bar's steps are done.
    pub fn set_position(&self, done: u64) {
        self.terminal.screen.update_position(self.id, done);
    }

    /// Ends the step without a line, for a step whose outcome says what it did.
    pub fn clear(&self) {
        self.terminal.screen.clear(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactivity_requires_terminals_human_output_and_allowed_environment() {
        for ci in [None, Some(""), Some("0"), Some("false")] {
            for format in [Format::Human, Format::Json] {
                for stdin in [false, true] {
                    for stderr in [false, true] {
                        for no_input in [false, true] {
                            assert_eq!(
                                allows_input(stdin, stderr, format, ci, no_input),
                                stdin && stderr && format == Format::Human && !no_input
                            );
                        }
                    }
                }
            }
        }
        for ci in ["1", "true", "yes", "FALSE", " "] {
            assert!(!allows_input(true, true, Format::Human, Some(ci), false));
        }
    }

    #[test]
    fn a_hyperlink_shows_its_text_and_opens_its_url() {
        let url = url::Url::parse("http://localhost:9001/verify?code=BCDF-GHJK").unwrap();

        assert_eq!(
            hyperlink(&url, "the approval page"),
            "\u{1b}]8;;http://localhost:9001/verify?code=BCDF-GHJK\u{1b}\\the approval page\u{1b}]8;;\u{1b}\\"
        );
    }
}
