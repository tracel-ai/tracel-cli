use std::fmt::Display;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

use cliclack::ProgressBar;

use super::Format;
use super::screen::Screen;
use crate::error::{CliError, ErrorKind, ErrorReport};

/// The columns cliclack's closing corner and the spaces after it take.
const CLOSING_INDENT: usize = 3;

/// A failure as its closing line shows it: the message, then the hint dimmed, every
/// line after the first indented under the first.
fn failure(message: &str, hint: Option<&str>) -> String {
    let hint = hint
        .into_iter()
        .flat_map(str::lines)
        .map(|line| console::style(line).for_stderr().dim().to_string());
    let lines: Vec<String> = message.lines().map(str::to_string).chain(hint).collect();
    lines.join(&format!("\n{}", " ".repeat(CLOSING_INDENT)))
}

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

/// What a prompt says when the user can't answer it: a message such as "Unlinking
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

/// Stderr, where a command tells the user what it does, shows its progress and asks
/// its questions. With JSON output it carries only errors and the instructions the
/// user must follow, as plain lines.
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

    /// Write a plain line, for stderr that cliclack doesn't draw on.
    fn line(&self, text: impl Display) {
        let _ = writeln!(
            io::stderr(),
            "{}",
            console::strip_ansi_codes(&text.to_string())
        );
    }

    /// Say something the user should know, as a sentence.
    pub fn info(&self, message: &str) {
        if self.is_styled() {
            self.screen.draw(|| cliclack::log::info(message));
        } else if self.is_human() {
            self.line(message);
        }
    }

    pub fn warning(&self, message: &str) {
        if self.is_styled() {
            self.screen.draw(|| cliclack::log::warning(message));
        } else if self.is_human() {
            self.line(format_args!("warning: {message}"));
        }
    }

    /// Record a step that left a fact the user needs later, such as "Created model
    /// 'weights'".
    pub fn step(&self, message: &str) {
        if self.is_styled() {
            self.screen.draw(|| cliclack::log::step(message));
        } else if self.is_human() {
            self.line(message);
        }
    }

    /// Show instructions the user must follow before the command can go on, in every
    /// format.
    pub fn instruct(&self, message: &str) {
        if self.is_human() {
            self.info(message);
        } else {
            self.line(message);
        }
    }

    /// End the command on its outcome: the corner of a flow's session, or a line.
    pub fn outro(&self, outcome: &str) {
        if self.screen.session_open() {
            self.screen.outro(outcome);
        } else {
            self.step(outcome);
        }
    }

    /// End the line a program the command ran left after the `^C` the terminal echoed,
    /// when it was interrupted, so what comes next starts a line of its own.
    pub fn program_ended(&self, interrupted: bool) {
        if interrupted && io::stderr().is_terminal() {
            let _ = writeln!(io::stderr());
        }
    }

    /// Report a failed command: its message and hint here, and its envelope on stdout
    /// with JSON output. On a terminal the command ends on cliclack's red closing line,
    /// with the hint under the message.
    pub fn error(&self, report: &ErrorReport<'_>) {
        self.screen.clear_live();
        if !self.is_human() {
            self.line(format_args!("error: {}", one_line(&report.message)));
        } else if self.is_styled() {
            let wrap = |text: &str| cliclack::termwrap(text, CLOSING_INDENT as u16);
            let hint = report.hint.map(wrap);
            self.screen
                .fail(&failure(&wrap(&report.message), hint.as_deref()));
        } else {
            self.line(format_args!("error: {}", report.message));
            if let Some(hint) = report.hint {
                self.line(hint);
            }
        }
    }

    /// Start a spinner for one step.
    pub fn spinner(&self, message: impl Display) -> Progress {
        self.start(cliclack::spinner(), message.to_string())
    }

    /// Start a bar that counts the `total` steps of one task.
    pub fn progress(&self, message: impl Display, total: u64) -> Progress {
        self.start(cliclack::progress_bar(total), message.to_string())
    }

    fn start(&self, bar: ProgressBar, message: String) -> Progress {
        let id = if self.is_styled() {
            Some(self.screen.start(bar, message))
        } else {
            self.info(&message);
            None
        };
        Progress {
            screen: Arc::clone(&self.screen),
            id,
        }
    }

    /// A URL as cliclack shows it: styled unless `NO_COLOR` is set, and a hyperlink a
    /// click opens in terminals that render them. Plain when cliclack doesn't draw.
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

    /// Fail with `kind` when the user can't answer a prompt, and otherwise settle the screen
    /// for it.
    fn require_answer(&self, kind: ErrorKind, unanswered: Unanswered) -> anyhow::Result<()> {
        if !self.is_interactive() {
            return Err(unanswered.into_error(kind).into());
        }
        self.screen.settle();
        Ok(())
    }

    /// Fail as `confirm` does when the user can't answer, so a command can stop before
    /// the slow work that comes ahead of its question.
    pub fn require_confirmation(&self, unanswered: Unanswered) -> anyhow::Result<()> {
        self.require_answer(ErrorKind::ConfirmationRequired, unanswered)
    }

    pub fn confirm(
        &self,
        question: &str,
        initial: bool,
        unanswered: Unanswered,
    ) -> anyhow::Result<bool> {
        self.require_confirmation(unanswered)?;
        cliclack::confirm(question)
            .initial_value(initial)
            .interact()
            .map_err(anyhow::Error::from)
    }

    /// Offer a shortcut, yes by default. When the user can't answer, it isn't asked and the
    /// answer is no.
    pub fn offer(&self, question: &str) -> anyhow::Result<bool> {
        if !self.is_interactive() {
            return Ok(false);
        }
        self.screen.settle();
        cliclack::confirm(question)
            .initial_value(true)
            .interact()
            .map_err(anyhow::Error::from)
    }

    /// Ask for optional text.
    pub fn input(&self, question: &str, unanswered: Unanswered) -> anyhow::Result<String> {
        self.input_validated(question, "", unanswered, |_| Ok(()))
    }

    /// Ask for text that `validate` accepts, `default` when the answer is empty.
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

    /// Whether the user can answer prompts: they type on stdin and read stderr.
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

/// `text` as a terminal hyperlink to `url` (OSC 8), which terminals that render them
/// open on a click.
fn hyperlink(url: &url::Url, text: &str) -> String {
    format!("\u{1b}]8;;{url}\u{1b}\\{text}\u{1b}]8;;\u{1b}\\")
}

/// A spinner or bar for one step, drawn until it ends. When something else draws
/// first, it ends early as a line with its last message.
#[derive(Clone)]
pub struct Progress {
    screen: Arc<Screen>,
    /// The live widget on the screen, when cliclack draws.
    id: Option<u64>,
}

impl Progress {
    /// Replace the message with a passing detail, such as what cargo compiles. Only a
    /// terminal shows it.
    pub fn set_message(&self, message: impl Display) {
        if let Some(id) = self.id {
            self.screen.set_message(id, message.to_string());
        }
    }

    /// Set how many of a bar's steps are done.
    pub fn set_position(&self, done: u64) {
        if let Some(id) = self.id {
            self.screen.set_position(id, done);
        }
    }

    /// End the step without a line, for a step whose outcome says what it did.
    pub fn clear(&self) {
        if let Some(id) = self.id {
            self.screen.clear(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::error::classify;
    use crate::test_support::quiet_terminal;

    fn unlinking() -> Unanswered {
        Unanswered::new(
            "Unlinking test/demo needs confirmation.",
            "Pass --yes to unlink without asking.",
        )
    }

    #[test]
    fn given_terminals_and_text_output_when_checking_for_input_then_prompts_may_ask() {
        let allowed = allows_input(true, true, Format::Human, None, false);

        assert!(allowed);
    }

    #[rstest]
    #[case::stdin_piped(false, true, Format::Human, None, false)]
    #[case::stderr_piped(true, false, Format::Human, None, false)]
    #[case::json_output(true, true, Format::Json, None, false)]
    #[case::ci(true, true, Format::Human, Some("true"), false)]
    #[case::no_input(true, true, Format::Human, None, true)]
    fn given_no_one_to_answer_when_checking_for_input_then_prompts_may_not_ask(
        #[case] stdin: bool,
        #[case] stderr: bool,
        #[case] format: Format,
        #[case] ci: Option<&str>,
        #[case] no_input: bool,
    ) {
        let allowed = allows_input(stdin, stderr, format, ci, no_input);

        assert!(!allowed);
    }

    #[rstest]
    #[case::empty("")]
    #[case::zero("0")]
    #[case::false_("false")]
    fn given_ci_set_to_false_when_checking_for_input_then_prompts_may_ask(#[case] ci: &str) {
        let allowed = allows_input(true, true, Format::Human, Some(ci), false);

        assert!(allowed);
    }

    #[test]
    fn given_a_failure_with_a_hint_when_closing_on_it_then_its_lines_and_hint_are_indented_under_it()
     {
        let closing = failure(
            "No job 'nope'.\nJobs: train.",
            Some("See them with `tracel run --list`."),
        );

        assert_eq!(
            closing,
            "No job 'nope'.\n   Jobs: train.\n   See them with `tracel run --list`."
        );
    }

    #[test]
    fn given_a_confirmation_no_one_can_answer_when_asking_then_it_requires_its_flag() {
        let terminal = quiet_terminal();

        let error = terminal
            .confirm("Unlink test/demo?", false, unlinking())
            .unwrap_err();

        assert_eq!(classify(&error), ErrorKind::ConfirmationRequired);
        assert!(ErrorReport::new(&error).hint.unwrap().contains("--yes"));
    }

    #[test]
    fn given_other_input_no_one_can_answer_when_asking_then_it_is_a_usage_error() {
        let terminal = quiet_terminal();

        let error = terminal.input("Name", unlinking()).unwrap_err();

        assert_eq!(classify(&error), ErrorKind::Usage);
    }

    #[test]
    fn given_an_offer_no_one_can_answer_when_offering_then_it_is_declined_without_asking() {
        let terminal = quiet_terminal();

        let accepted = terminal.offer("Open it in your browser?").unwrap();

        assert!(!accepted);
    }
}
