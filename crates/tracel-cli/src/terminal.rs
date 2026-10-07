use std::fmt::Display;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

use cliclack::{MultiProgress, ProgressBar};

use crate::error::{CliError, ErrorKind, ErrorReport};
use crate::output::Format;

/// Whether `command_title` opened a frame that is still open. Stderr is one per
/// process, and so is the frame drawn on it.
static FRAMED: AtomicBool = AtomicBool::new(false);

/// Stderr, for the person running a command: what it is doing, its progress, and
/// questions for them. With JSON output it carries only errors and the instructions
/// a person must follow, as plain lines.
#[derive(Clone)]
pub struct Terminal {
    format: Format,
    no_input: bool,
}

impl Terminal {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            no_input: false,
        }
    }

    pub fn with_no_input(mut self, no_input: bool) -> Self {
        self.no_input = no_input;
        self
    }

    fn is_human(&self) -> bool {
        self.format == Format::Human
    }

    fn is_styled(&self) -> bool {
        self.is_human() && std::io::stderr().is_terminal()
    }

    pub fn print_warning(&self, message: &str) {
        if self.is_styled() {
            cliclack::log::warning(message).expect("To be able to print remark");
        } else if self.is_human() {
            eprintln!("warning: {}", console::strip_ansi_codes(message));
        }
    }

    pub fn print(&self, message: &str) {
        if self.is_styled() {
            cliclack::log::info(message).expect("To be able to print message");
        } else if self.is_human() {
            eprintln!("{}", console::strip_ansi_codes(message));
        }
    }

    pub fn print_err(&self, message: &str) {
        if self.is_styled() {
            cliclack::log::error(message).expect("To be able to print message");
        } else if self.is_human() {
            eprintln!("error: {}", console::strip_ansi_codes(message));
        }
    }

    pub fn print_success(&self, message: &str) {
        if self.is_styled() {
            cliclack::log::success(message).expect("To be able to print success message");
        } else if self.is_human() {
            eprintln!("{}", console::strip_ansi_codes(message));
        }
    }

    pub fn step(&self, message: &str) {
        if self.is_styled() {
            cliclack::log::step(message).expect("To be able to print message");
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
            eprintln!("{}", console::strip_ansi_codes(message));
        }
    }

    /// Reports a failed command: the message and hint here, the envelope on stdout
    /// with JSON output. An open frame ends with the hint, or with the error itself.
    pub fn error(&self, report: &ErrorReport<'_>) {
        if !self.is_human() {
            eprintln!("error: {}", one_line(&report.message));
        } else if self.is_styled() && FRAMED.load(Ordering::Relaxed) {
            match report.hint {
                Some(hint) => {
                    cliclack::log::error(&report.message).expect("To be able to print message");
                    cliclack::outro_cancel(hint).expect("To be able to print message");
                    FRAMED.store(false, Ordering::Relaxed);
                }
                None => self.cancel_finalize(&report.message),
            }
        } else {
            let label = console::style("error:").for_stderr().red().bold();
            eprintln!("{label} {}", report.message);
            if let Some(hint) = report.hint {
                eprintln!("{hint}");
            }
        }
    }

    pub fn spinner(&self) -> TerminalSpinner {
        TerminalSpinner {
            bar: self.is_styled().then(cliclack::spinner),
            terminal: self.clone(),
        }
    }

    /// A bar that counts `total` steps under `title`.
    pub fn progress(&self, title: &str, total: u64) -> TerminalProgress {
        let bars = self.is_styled().then(|| {
            let group = cliclack::multi_progress(title);
            let bar = group.add(ProgressBar::new(total).with_download_template());
            (group, bar)
        });
        if bars.is_none() {
            self.print(title);
        }
        TerminalProgress {
            bars,
            terminal: self.clone(),
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
        Ok(())
    }

    pub fn confirm(&self, message: &str, flag: &str, initial: bool) -> anyhow::Result<bool> {
        self.require_answer(ErrorKind::ConfirmationRequired, message, flag, &[])?;
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
            let title = format!(" {} {} ", "▶", title);
            cliclack::intro(console::style(title).black().on_green())
                .expect("To be able to print title");
            FRAMED.store(true, Ordering::Relaxed);
        }
    }

    pub fn finalize(&self, msg: &str) {
        if self.is_styled() {
            cliclack::outro(console::style(format!(" {} ", msg)).black().on_green())
                .expect("To be able to print message");
            FRAMED.store(false, Ordering::Relaxed);
        } else {
            self.print_success(msg);
        }
    }

    pub fn cancel_finalize(&self, msg: &str) {
        if self.is_styled() {
            cliclack::outro_cancel(console::style(format!(" {} ", msg)).black().on_red())
                .expect("To be able to print message");
            FRAMED.store(false, Ordering::Relaxed);
        } else {
            self.print_err(msg);
        }
    }

    /// Whether a person can answer prompts: they read from stdin and draw on stderr.
    pub fn is_interactive(&self) -> bool {
        let ci = std::env::var_os("CI");
        allows_input(
            std::io::stdin().is_terminal(),
            std::io::stderr().is_terminal(),
            self.format,
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

#[derive(Clone)]
pub struct TerminalSpinner {
    bar: Option<ProgressBar>,
    terminal: Terminal,
}

impl TerminalSpinner {
    pub fn start(&self, message: impl Display) {
        if let Some(bar) = &self.bar {
            bar.start(message);
        } else {
            self.terminal.print(&message.to_string());
        }
    }

    pub fn set_message(&self, message: impl Display) {
        if let Some(bar) = &self.bar {
            bar.set_message(message);
        } else {
            self.terminal.print(&message.to_string());
        }
    }

    pub fn stop(&self, message: impl Display) {
        if let Some(bar) = &self.bar {
            bar.stop(message);
        } else {
            self.terminal.print_success(&message.to_string());
        }
    }

    pub fn error(&self, message: impl Display) {
        if let Some(bar) = &self.bar {
            bar.error(message);
        } else {
            self.terminal.print_err(&message.to_string());
        }
    }
}

pub struct TerminalProgress {
    bars: Option<(MultiProgress, ProgressBar)>,
    terminal: Terminal,
}

impl TerminalProgress {
    pub fn start(&self, message: impl Display) {
        if let Some((_, bar)) = &self.bars {
            bar.start(message);
        }
    }

    pub fn set(&self, done: u64, message: impl Display) {
        if let Some((_, bar)) = &self.bars {
            bar.set_position(done);
            bar.set_message(message);
        }
    }

    pub fn stop(&self, message: impl Display) {
        match &self.bars {
            Some((group, bar)) => {
                bar.stop(message);
                group.stop();
            }
            None => self.terminal.print_success(&message.to_string()),
        }
    }

    pub fn error(&self, message: impl Display) {
        match &self.bars {
            Some((group, bar)) => {
                bar.error(&message);
                group.error(message);
            }
            None => self.terminal.print_err(&message.to_string()),
        }
    }
}
