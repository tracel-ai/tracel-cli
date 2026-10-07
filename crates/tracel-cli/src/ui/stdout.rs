use std::io::{self, Write};
use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;

use super::Format;
use super::human::{Human, Style};
use super::screen::{Ending, Screen};
use crate::error::ErrorReport;

/// A command result: serialized for JSON output, written as text for people.
pub trait Render: Serialize {
    /// The text a person reads on stdout. The default writes nothing, for actions
    /// that report what they did on stderr as they go.
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let _ = out;
        Ok(())
    }
}

/// What a command leaves for stdout when it finishes.
pub struct Outcome(Option<Box<dyn Report>>);

impl Outcome {
    /// The command wrote to stdout while it ran: streamed events, or a program it ran.
    pub fn streamed() -> Self {
        Self(None)
    }
}

impl<T: Render + 'static> From<T> for Outcome {
    fn from(result: T) -> Self {
        Self(Some(Box::new(result)))
    }
}

trait Report {
    fn write_json(&self, out: &mut dyn Write) -> serde_json::Result<()>;
    fn write_text(&self, out: &mut Human<'_>) -> io::Result<()>;
}

impl<T: Render> Report for T {
    fn write_json(&self, out: &mut dyn Write) -> serde_json::Result<()> {
        serde_json::to_writer(
            out,
            &Success {
                ok: true,
                data: self,
            },
        )
    }

    fn write_text(&self, out: &mut Human<'_>) -> io::Result<()> {
        self.render(out)
    }
}

#[derive(Serialize)]
struct Success<'a, T> {
    ok: bool,
    data: &'a T,
}

#[derive(Serialize)]
struct Failure<'a> {
    ok: bool,
    error: &'a ErrorReport<'a>,
}

/// Stdout was closed by its reader, as in `tracel ... | head`.
#[derive(Debug, thiserror::Error)]
#[error("stdout was closed")]
pub struct StdoutClosed;

/// Stdout, which carries only command results, in the format the user chose.
#[derive(Clone)]
pub struct Output {
    screen: Arc<Screen>,
}

impl Output {
    pub fn new(screen: Arc<Screen>) -> Self {
        Self { screen }
    }

    /// Writes the result of a finished command.
    pub fn finish(&self, outcome: Outcome) -> anyhow::Result<()> {
        let format = self.screen.format();
        let style = self.screen.text_style();
        self.write_stdout(|out| finish(format, style, outcome, out))
    }

    /// Writes one event of a stream as it happens: an NDJSON line, or its text.
    pub fn event(&self, event: &impl Render) -> anyhow::Result<()> {
        let format = self.screen.format();
        let style = self.screen.text_style();
        self.write_stdout(|out| {
            write_event(format, style, event, out)?;
            out.flush()
        })
    }

    /// Where a program the command runs writes its stdout: here with text output, or to
    /// stderr with JSON output, which keeps stdout for the envelope. The program writes
    /// to the screen on its own, so the live widget is settled first, and it must not
    /// run inside a frame.
    pub fn child_stdout(&self) -> Stdio {
        debug_assert!(!self.screen.frame_open(), "a program runs inside a frame");
        self.screen.settle(Ending::Done);
        match self.screen.format() {
            Format::Human => Stdio::inherit(),
            Format::Json => Stdio::from(io::stderr()),
        }
    }

    /// Writes the error envelope of a failed command; text errors go to stderr.
    pub fn error(&self, report: &ErrorReport<'_>) {
        if self.screen.format() == Format::Json {
            let _ = self.write_stdout(|out| write_failure(report, out));
        }
    }

    fn write_stdout(
        &self,
        write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> anyhow::Result<()> {
        match write(&mut self.screen.stdout()) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(StdoutClosed.into()),
            result => Ok(result?),
        }
    }
}

fn finish(format: Format, style: Style, outcome: Outcome, out: &mut dyn Write) -> io::Result<()> {
    let Some(report) = outcome.0 else {
        return Ok(());
    };
    match format {
        Format::Human => report.write_text(&mut Human::new(out, style)),
        Format::Json => {
            report.write_json(out)?;
            writeln!(out)
        }
    }
}

fn write_event(
    format: Format,
    style: Style,
    event: &impl Render,
    out: &mut dyn Write,
) -> io::Result<()> {
    match format {
        Format::Human => event.render(&mut Human::new(out, style)),
        Format::Json => {
            serde_json::to_writer(&mut *out, event)?;
            writeln!(out)
        }
    }
}

fn write_failure(report: &ErrorReport<'_>, out: &mut dyn Write) -> io::Result<()> {
    serde_json::to_writer(
        &mut *out,
        &Failure {
            ok: false,
            error: report,
        },
    )?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CliError, ErrorKind};

    #[derive(Serialize)]
    struct Greeting {
        name: &'static str,
    }

    impl Render for Greeting {
        fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
            writeln!(out, "Hello, {}.", self.name)
        }
    }

    #[derive(Serialize)]
    struct Done {}

    impl Render for Done {}

    fn written(write: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn results_are_an_envelope_or_their_text() {
        let greeting = || Outcome::from(Greeting { name: "Ada" });
        assert_eq!(
            written(|out| finish(Format::Json, Style::plain(), greeting(), out)),
            "{\"ok\":true,\"data\":{\"name\":\"Ada\"}}\n"
        );
        assert_eq!(
            written(|out| finish(Format::Human, Style::plain(), greeting(), out)),
            "Hello, Ada.\n"
        );
        assert_eq!(
            written(|out| finish(Format::Human, Style::plain(), Done {}.into(), out)),
            ""
        );
    }

    #[test]
    fn streamed_output_is_not_followed_by_an_envelope() {
        for format in [Format::Human, Format::Json] {
            assert_eq!(
                written(|out| finish(format, Style::plain(), Outcome::streamed(), out)),
                ""
            );
        }
    }

    #[test]
    fn events_are_bare_json_lines_or_their_text() {
        let event = Greeting { name: "Ada" };
        assert_eq!(
            written(|out| write_event(Format::Json, Style::plain(), &event, out)),
            "{\"name\":\"Ada\"}\n"
        );
        assert_eq!(
            written(|out| write_event(Format::Human, Style::plain(), &event, out)),
            "Hello, Ada.\n"
        );
    }

    #[test]
    fn failures_carry_the_error_report() {
        let error = anyhow::Error::new(
            CliError::new(ErrorKind::NotFound, "No model 'weights'.").with_hint("List models."),
        );
        let report = ErrorReport::new(&error);
        let value: serde_json::Value =
            serde_json::from_str(&written(|out| write_failure(&report, out))).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "ok": false,
                "error": {
                    "code": "NOT_FOUND",
                    "message": "No model 'weights'.",
                    "hint": "List models.",
                    "exit_code": 5,
                },
            })
        );
    }
}
