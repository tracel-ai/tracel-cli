use std::io::IsTerminal;

use cliclack::{ProgressBar, clear_screen, confirm};

use colored::CustomColor;

#[allow(dead_code)]
pub const BURN_ORANGE: CustomColor = CustomColor {
    r: 254,
    g: 75,
    b: 0,
};

#[derive(Clone, Default)]
pub struct Terminal {}

impl Terminal {
    pub fn print_warning(&self, message: &str) {
        cliclack::log::warning(message).expect("To be able to print remark");
    }

    pub fn print(&self, message: &str) {
        cliclack::log::info(message).expect("To be able to print message");
    }

    pub fn print_err(&self, message: &str) {
        cliclack::log::error(message).expect("To be able to print message");
    }

    pub fn print_success(&self, message: &str) {
        cliclack::log::success(message).expect("To be able to print success message");
    }

    pub fn spinner(&self) -> ProgressBar {
        cliclack::spinner()
    }

    #[allow(dead_code)]
    pub fn clear(&self) {
        clear_screen().expect("Failed to clear screen");
    }

    /// A URL styled for stderr, plain when stderr is not a terminal or `NO_COLOR` is set.
    pub fn format_url(&self, url: &url::Url) -> String {
        console::style(url).for_stderr().blue().bold().to_string()
    }

    pub fn confirm(&self, message: &str) -> anyhow::Result<bool> {
        confirm(message).interact().map_err(anyhow::Error::from)
    }

    pub fn command_title(&self, title: &str) {
        let title = format!(" {} {} ", "▶", title);
        cliclack::intro(console::style(title).black().on_green())
            .expect("To be able to print title");
    }

    pub fn finalize(&self, msg: &str) {
        cliclack::outro(console::style(format!(" {} ", msg)).black().on_green())
            .expect("To be able to print message");
    }

    pub fn cancel_finalize(&self, msg: &str) {
        cliclack::outro_cancel(console::style(format!(" {} ", msg)).black().on_red())
            .expect("To be able to print message");
    }

    /// Whether a person can answer prompts: they read from stdin and draw on stderr.
    pub fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }
}
