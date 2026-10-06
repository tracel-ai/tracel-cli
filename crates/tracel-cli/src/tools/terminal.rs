use std::{fmt::Display, io::IsTerminal};

use cliclack::{ProgressBar, clear_screen};

use colored::CustomColor;

use crate::error::{CliError, ErrorKind};
use crate::output::OutputMode;

#[allow(dead_code)]
pub const BURN_ORANGE: CustomColor = CustomColor {
    r: 254,
    g: 75,
    b: 0,
};

#[derive(Clone)]
pub struct Terminal {
    output: OutputMode,
    no_input: bool,
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new(OutputMode::Human)
    }
}

impl Terminal {
    pub fn new(output: OutputMode) -> Self {
        Self {
            output,
            no_input: false,
        }
    }

    pub fn with_no_input(mut self, no_input: bool) -> Self {
        self.no_input = no_input;
        self
    }

    fn is_human(&self) -> bool {
        self.output != OutputMode::Json
    }

    pub(crate) fn is_styled(&self) -> bool {
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

    pub fn outro_cancel(&self, message: &str) {
        if self.is_styled() {
            cliclack::outro_cancel(message).expect("To be able to print message");
        } else {
            self.print_err(message);
        }
    }

    pub fn spinner(&self) -> TerminalSpinner {
        TerminalSpinner {
            bar: self.is_styled().then(cliclack::spinner),
            terminal: self.clone(),
        }
    }

    #[allow(dead_code)]
    pub fn clear(&self) {
        if self.is_styled() {
            clear_screen().expect("Failed to clear screen");
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
        if !self.is_interactive() {
            let mut message = format!("Input needed: {}", message.trim());
            if !values.is_empty() {
                message.push_str(&format!(" Valid values: {}.", values.join(", ")));
            }
            return Err(CliError::new(ErrorKind::Usage, message)
                .with_hint(format!("Pass --{flag} to answer without a prompt."))
                .into());
        }
        Ok(())
    }

    pub fn confirm(&self, message: &str, flag: &str, initial: bool) -> anyhow::Result<bool> {
        self.require_input(message, flag, &[])?;
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

    pub fn command_title(&self, title: &str) {
        if self.is_styled() {
            let title = format!(" {} {} ", "▶", title);
            cliclack::intro(console::style(title).black().on_green())
                .expect("To be able to print title");
        }
    }

    pub fn finalize(&self, msg: &str) {
        if self.is_styled() {
            cliclack::outro(console::style(format!(" {} ", msg)).black().on_green())
                .expect("To be able to print message");
        } else {
            self.print_success(msg);
        }
    }

    pub fn cancel_finalize(&self, msg: &str) {
        if self.is_styled() {
            cliclack::outro_cancel(console::style(format!(" {} ", msg)).black().on_red())
                .expect("To be able to print message");
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
            self.output,
            ci.as_ref().map(|value| value.to_str().unwrap_or("true")),
            self.no_input,
        )
    }
}

fn allows_input(
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
    output: OutputMode,
    ci: Option<&str>,
    no_input: bool,
) -> bool {
    stdin_is_terminal
        && stderr_is_terminal
        && output != OutputMode::Json
        && matches!(ci, None | Some("" | "0" | "false"))
        && !no_input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactivity_requires_terminals_human_output_and_allowed_environment() {
        for ci in [None, Some(""), Some("0"), Some("false")] {
            for output in [OutputMode::Auto, OutputMode::Human, OutputMode::Json] {
                for stdin in [false, true] {
                    for stderr in [false, true] {
                        for no_input in [false, true] {
                            assert_eq!(
                                allows_input(stdin, stderr, output, ci, no_input),
                                stdin && stderr && output != OutputMode::Json && !no_input
                            );
                        }
                    }
                }
            }
        }
        for ci in ["1", "true", "yes", "FALSE", " "] {
            assert!(!allows_input(
                true,
                true,
                OutputMode::Human,
                Some(ci),
                false
            ));
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

    pub fn cancel(&self, message: impl Display) {
        if let Some(bar) = &self.bar {
            bar.cancel(message);
        } else {
            self.terminal.print_err(&message.to_string());
        }
    }
}
