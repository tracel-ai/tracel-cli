use std::io::{self, Write};
use std::sync::Arc;

use serde::Serialize;

use super::Format;
use super::human::Human;
use super::screen::{Delivery, Screen};
use crate::error::ErrorReport;

/// A command result: serialized for JSON output, written for people.
pub trait Render: Serialize {
    /// What a person reads. The default writes nothing, for actions that report
    /// what they did on stderr as they go.
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let _ = out;
        Ok(())
    }

    /// One line that sums the result up: the title of its box on a terminal, the
    /// outcome that ends a session, and the last line of plain text.
    fn summary(&self) -> Option<String> {
        None
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
    fn summary(&self) -> Option<String>;
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

    fn summary(&self) -> Option<String> {
        Render::summary(self)
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

/// Where command results go, in the format the user chose: a read's in one box on a
/// terminal, and otherwise on stdout, which carries nothing else.
#[derive(Clone)]
pub struct Output {
    screen: Arc<Screen>,
}

impl Output {
    pub fn new(screen: Arc<Screen>) -> Self {
        Self { screen }
    }

    /// Writes the result of a finished command, and ends the session it ran in with
    /// its summary.
    pub fn finish(&self, outcome: Outcome) -> anyhow::Result<()> {
        let summary = outcome.0.as_ref().and_then(|report| report.summary());
        if let Some(report) = outcome.0 {
            let (format, width) = (self.screen.format(), self.screen.stdout_width());
            match self.screen.delivery() {
                Delivery::Box(width) => {
                    let body = boxed(report.as_ref(), width)?;
                    self.screen.show_result(summary.as_deref(), &body);
                }
                Delivery::Stdout => self.write_stdout(|out| {
                    write_result(format, report.as_ref(), summary.as_deref(), width, out)
                })?,
            }
        }
        self.screen.close(summary.as_deref().unwrap_or_default());
        Ok(())
    }

    /// Writes one event of a stream as it happens: an NDJSON line, or its plain text,
    /// which stays the program's own output on a terminal too.
    pub fn event(&self, event: &impl Render) -> anyhow::Result<()> {
        let (format, width) = (self.screen.format(), self.screen.stdout_width());
        self.write_stdout(|out| {
            write_event(format, width, event, out)?;
            out.flush()
        })
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

/// The text of a result for its box, fitted to `width`.
fn boxed(report: &dyn Report, width: Option<usize>) -> io::Result<String> {
    let mut text = Vec::new();
    report.write_text(&mut Human::boxed(&mut text, width))?;
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// A result on stdout: its envelope, or its plain text ending with `summary`.
fn write_result(
    format: Format,
    report: &dyn Report,
    summary: Option<&str>,
    width: Option<usize>,
    out: &mut dyn Write,
) -> io::Result<()> {
    match format {
        Format::Human => {
            let mut out = Human::plain(out, width);
            report.write_text(&mut out)?;
            match summary {
                Some(summary) => writeln!(out, "{summary}"),
                None => Ok(()),
            }
        }
        Format::Json => {
            report.write_json(out)?;
            writeln!(out)
        }
    }
}

fn write_event(
    format: Format,
    width: Option<usize>,
    event: &impl Render,
    out: &mut dyn Write,
) -> io::Result<()> {
    match format {
        Format::Human => event.render(&mut Human::plain(out, width)),
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

        fn summary(&self) -> Option<String> {
            Some("Greeted.".into())
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

    fn result(format: Format, report: &dyn Report, summary: Option<&str>) -> String {
        written(|out| write_result(format, report, summary, None, out))
    }

    #[test]
    fn results_are_an_envelope_or_their_text() {
        let greeting = Greeting { name: "Ada" };
        assert_eq!(
            result(Format::Json, &greeting, None),
            "{\"ok\":true,\"data\":{\"name\":\"Ada\"}}\n"
        );
        assert_eq!(result(Format::Human, &greeting, None), "Hello, Ada.\n");
        assert_eq!(result(Format::Human, &Done {}, None), "");
    }

    #[test]
    fn plain_text_ends_with_the_summary() {
        let greeting = Greeting { name: "Ada" };
        assert_eq!(
            result(Format::Human, &greeting, Some("Greeted.")),
            "Hello, Ada.\nGreeted.\n"
        );
        assert_eq!(
            result(Format::Json, &greeting, Some("Greeted.")),
            "{\"ok\":true,\"data\":{\"name\":\"Ada\"}}\n"
        );
    }

    #[test]
    fn a_box_holds_the_text_of_a_result() {
        assert_eq!(
            boxed(&Greeting { name: "Ada" }, None).unwrap(),
            "Hello, Ada.\n"
        );
        assert_eq!(boxed(&Done {}, None).unwrap(), "");
    }

    #[test]
    fn events_are_bare_json_lines_or_their_text() {
        let event = Greeting { name: "Ada" };
        assert_eq!(
            written(|out| write_event(Format::Json, None, &event, out)),
            "{\"name\":\"Ada\"}\n"
        );
        assert_eq!(
            written(|out| write_event(Format::Human, None, &event, out)),
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
