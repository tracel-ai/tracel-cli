use std::{fmt::Display, io::IsTerminal};

use cliclack::{ProgressBar, clear_screen, confirm};

use colored::CustomColor;

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
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new(OutputMode::Human)
    }
}

impl Terminal {
    pub fn new(output: OutputMode) -> Self {
        Self { output }
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

    pub fn confirm(&self, message: &str) -> anyhow::Result<bool> {
        confirm(message).interact().map_err(anyhow::Error::from)
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
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
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
