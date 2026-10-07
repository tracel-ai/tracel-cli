use std::fmt::Display;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

use cliclack::ProgressBar;

use super::Format;
use super::screen::{Ending, Screen};
use crate::error::{CliError, ErrorKind, ErrorReport};

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

    /// Draws with cliclack once nothing else is drawing.
    fn draw(&self, draw: impl FnOnce() -> io::Result<()>) {
        self.screen.settle(Ending::Done);
        let _ = draw();
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
            self.draw(|| cliclack::log::warning(message));
        } else if self.is_human() {
            self.line(format_args!("warning: {message}"));
        }
    }

    pub fn print(&self, message: &str) {
        if self.is_styled() {
            self.draw(|| cliclack::log::info(message));
        } else if self.is_human() {
            self.line(message);
        }
    }

    pub fn print_err(&self, message: &str) {
        if self.is_styled() {
            self.draw(|| cliclack::log::error(message));
        } else if self.is_human() {
            self.line(format_args!("error: {message}"));
        }
    }

    pub fn print_success(&self, message: &str) {
        if self.is_styled() {
            self.draw(|| cliclack::log::success(message));
        } else if self.is_human() {
            self.line(message);
        }
    }

    /// A note a person must read whatever the format, such as instructions to follow
    /// before the command can go on.
    pub fn instruct(&self, message: &str) {
        if self.is_human() {
            self.print(message);
        } else {
            self.line(message);
        }
    }

    /// Reports a failed command: the message and hint here, the envelope on stdout
    /// with JSON output. An open frame ends with the hint, or with the error itself.
    pub fn error(&self, report: &ErrorReport<'_>) {
        self.screen.settle(Ending::Failed);
        if !self.is_human() {
            self.line(format_args!("error: {}", one_line(&report.message)));
        } else if self.is_styled() && self.screen.frame_open() {
            match report.hint {
                Some(hint) => {
                    self.draw(|| {
                        cliclack::log::error(&report.message)?;
                        cliclack::outro_cancel(hint)
                    });
                    self.screen.set_frame(false);
                }
                None => self.cancel_finalize(&report.message),
            }
        } else {
            let label = console::style("error:").for_stderr().red().bold();
            let _ = writeln!(io::stderr(), "{label} {}", report.message);
            if let Some(hint) = report.hint {
                let _ = writeln!(io::stderr(), "{hint}");
            }
        }
    }

    pub fn spinner(&self) -> TerminalSpinner {
        TerminalSpinner {
            terminal: self.clone(),
            id: self.screen.next_id(),
        }
    }

    /// A bar that counts `total` steps under `title`.
    pub fn progress(&self, title: &str, total: u64) -> TerminalProgress {
        let id = self.screen.next_id();
        if self.is_styled() {
            self.screen.show(id, String::new(), || {
                let group = cliclack::multi_progress(title);
                let bar = group.add(ProgressBar::new(total).with_download_template());
                (bar, Some(group))
            });
        } else {
            self.print(title);
        }
        TerminalProgress {
            terminal: self.clone(),
            id,
        }
    }

    /// A URL styled for stderr, plain when stderr is not a terminal or `NO_COLOR` is set.
    pub fn format_url(&self, url: &url::Url) -> String {
        if self.is_styled() {
            console::style(url).for_stderr().blue().bold().to_string()
        } else {
            url.to_string()
        }
    }

    fn require_input(&self, message: &str, flag: &str, values: &[&str]) -> anyhow::Result<()> {
        self.require_answer(ErrorKind::Usage, message, flag, values)
    }

    /// Fails when nobody can answer, and otherwise clears the screen for a prompt.
    fn require_answer(
        &self,
        kind: ErrorKind,
        message: &str,
        flag: &str,
        values: &[&str],
    ) -> anyhow::Result<()> {
        if !self.is_interactive() {
            let mut message = format!("Input needed: {}", message.trim());
            if !values.is_empty() {
                message.push_str(&format!(" Valid values: {}.", values.join(", ")));
            }
            return Err(CliError::new(kind, message)
                .with_hint(format!("Pass --{flag} to answer without a prompt."))
                .into());
        }
        self.screen.settle(Ending::Done);
        Ok(())
    }

    /// Fails as `confirm` does when nobody can answer, so a command can stop before slow
    /// work that comes ahead of its confirmation.
    pub fn require_confirmation(&self, message: &str, flag: &str) -> anyhow::Result<()> {
        self.require_answer(ErrorKind::ConfirmationRequired, message, flag, &[])
    }

    pub fn confirm(&self, message: &str, flag: &str, initial: bool) -> anyhow::Result<bool> {
        self.require_confirmation(message, flag)?;
        cliclack::confirm(message)
            .initial_value(initial)
            .interact()
            .map_err(anyhow::Error::from)
    }

    pub fn input(&self, message: &str, flag: &str) -> anyhow::Result<String> {
        self.input_validated(message, flag, "", |_| Ok(()))
    }

    pub fn input_validated(
        &self,
        message: &str,
        flag: &str,
        placeholder: &str,
        validate: impl Fn(&String) -> Result<(), String> + 'static,
    ) -> anyhow::Result<String> {
        self.require_input(message, flag, &[])?;
        cliclack::input(message)
            .placeholder(placeholder)
            .required(false)
            .validate(validate)
            .interact()
            .map_err(anyhow::Error::from)
    }

    pub fn select<T: Clone + Eq>(
        &self,
        message: &str,
        flag: &str,
        items: &[(T, impl Display, impl Display)],
        initial: Option<T>,
        values: &[&str],
    ) -> anyhow::Result<T> {
        self.require_input(message, flag, values)?;
        let mut prompt = cliclack::select(message).items(items);
        if let Some(initial) = initial {
            prompt = prompt.initial_value(initial);
        }
        prompt.interact().map_err(anyhow::Error::from)
    }

    pub fn multiselect<T: Clone + Eq>(
        &self,
        message: &str,
        flag: &str,
        items: &[(T, impl Display, impl Display)],
        initial: Vec<T>,
    ) -> anyhow::Result<Vec<T>> {
        self.require_input(message, flag, &[])?;
        cliclack::multiselect(message)
            .items(items)
            .initial_values(initial)
            .required(true)
            .interact()
            .map_err(anyhow::Error::from)
    }

    /// Opens the frame that an action's progress is drawn in.
    pub fn command_title(&self, title: &str) {
        if self.is_styled() {
            let title = console::style(format!(" ▶ {title} ")).black().on_green();
            self.draw(|| cliclack::intro(title));
            self.screen.set_frame(true);
        }
    }

    pub fn finalize(&self, msg: &str) {
        if self.is_styled() {
            let message = console::style(format!(" {msg} ")).black().on_green();
            self.draw(|| cliclack::outro(message));
            self.screen.set_frame(false);
        } else {
            self.print_success(msg);
        }
    }

    pub fn cancel_finalize(&self, msg: &str) {
        if self.is_styled() {
            let message = console::style(format!(" {msg} ")).black().on_red();
            self.draw(|| cliclack::outro_cancel(message));
            self.screen.set_frame(false);
        } else {
            self.print_err(msg);
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

/// A spinner for one step. It is drawn when started, and stops early when something
/// else draws before it ends; its last message then appears as a plain line.
#[derive(Clone)]
pub struct TerminalSpinner {
    terminal: Terminal,
    id: u64,
}

impl TerminalSpinner {
    pub fn start(&self, message: impl Display) {
        if self.terminal.is_styled() {
            self.terminal
                .screen
                .show(self.id, message.to_string(), || (cliclack::spinner(), None));
        } else {
            self.terminal.print(&message.to_string());
        }
    }

    pub fn set_message(&self, message: impl Display) {
        if self.terminal.is_styled() {
            self.terminal
                .screen
                .update(self.id, message.to_string(), None);
        } else {
            self.terminal.print(&message.to_string());
        }
    }

    pub fn stop(&self, message: impl Display) {
        let message = message.to_string();
        if !self.terminal.is_styled()
            || !self.terminal.screen.finish(self.id, Ending::Done, &message)
        {
            self.terminal.print_success(&message);
        }
    }

    pub fn error(&self, message: impl Display) {
        let message = message.to_string();
        if !self.terminal.is_styled()
            || !self
                .terminal
                .screen
                .finish(self.id, Ending::Failed, &message)
        {
            self.terminal.print_err(&message);
        }
    }
}

/// A bar counting the steps of one task, drawn from creation until it ends.
pub struct TerminalProgress {
    terminal: Terminal,
    id: u64,
}

impl TerminalProgress {
    pub fn set(&self, done: u64, message: impl Display) {
        self.terminal
            .screen
            .update(self.id, message.to_string(), Some(done));
    }

    pub fn stop(&self, message: impl Display) {
        let message = message.to_string();
        if !self.terminal.is_styled()
            || !self.terminal.screen.finish(self.id, Ending::Done, &message)
        {
            self.terminal.print_success(&message);
        }
    }

    pub fn error(&self, message: impl Display) {
        let message = message.to_string();
        if !self.terminal.is_styled()
            || !self
                .terminal
                .screen
                .finish(self.id, Ending::Failed, &message)
        {
            self.terminal.print_err(&message);
        }
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
}
