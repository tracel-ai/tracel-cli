use std::io::{self, Write};
use std::sync::Arc;

use serde::Serialize;

use super::Format;
use super::human::Human;
use super::screen::{Delivery, Screen};
use crate::error::ErrorReport;

/// A command's result: serialized for JSON output, written as text otherwise.
pub trait Render: Serialize {
    /// Write the result as text for the user.
    fn render(&self, out: &mut Human<'_>) -> io::Result<()>;

    /// One line that sums the result up: the title of its box on a terminal, the
    /// outcome that ends a session, and the last line of plain text.
    fn summary(&self) -> Option<String> {
        None
    }
}

/// What a command leaves for stdout when it finishes.
pub struct Outcome(Option<Box<dyn Report>>);

impl Outcome {
    /// Nothing more: the command wrote to stdout as it ran, such as streamed events or
    /// the output of a program it ran.
    pub fn streamed() -> Self {
        Self(None)
    }

    /// A result for JSON output only, from a command that says what it did on stderr.
    pub fn data(value: impl Serialize + 'static) -> Self {
        Self(Some(Box::new(Data(value))))
    }
}

impl<T: Render + 'static> From<T> for Outcome {
    fn from(result: T) -> Self {
        Self(Some(Box::new(result)))
    }
}

#[derive(Serialize)]
#[serde(transparent)]
struct Data<T>(T);

impl<T: Serialize> Render for Data<T> {
    fn render(&self, _: &mut Human<'_>) -> io::Result<()> {
        Ok(())
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

/// Stdout, where results go in the format the user chose. On a terminal a read's
/// result goes in one box on stderr instead.
#[derive(Clone)]
pub struct Output {
    screen: Arc<Screen>,
}

impl Output {
    pub fn new(screen: Arc<Screen>) -> Self {
        Self { screen }
    }

    /// Write the result of a finished command, then end its session on the summary.
    pub fn finish(&self, outcome: Outcome) -> anyhow::Result<()> {
        let summary = outcome.0.as_ref().and_then(|report| report.summary());
        if let Some(report) = outcome.0 {
            match self.screen.delivery() {
                Delivery::Box(width) => {
                    let body = box_text(report.as_ref(), width)?;
                    self.screen.show_result(summary.as_deref(), &body);
                }
                Delivery::Stdout => {
                    let (format, width) = (self.screen.format(), self.screen.stdout_width());
                    self.write_stdout(|out| {
                        write_result(format, report.as_ref(), summary.as_deref(), width, out)
                    })?
                }
            }
        }
        self.screen.outro(summary.as_deref().unwrap_or_default());
        Ok(())
    }

    /// Write one event of a stream as it happens: an NDJSON line, or its plain text,
    /// which a terminal shows as it is.
    pub fn event(&self, event: &impl Render) -> anyhow::Result<()> {
        let (format, width) = (self.screen.format(), self.screen.stdout_width());
        self.write_stdout(|out| {
            write_event(format, width, event, out)?;
            out.flush()
        })
    }

    /// Write the error envelope of a failed command with JSON output. Text errors go
    /// to stderr.
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

/// Write a result's text for its box, fitted to `width`.
fn box_text(report: &dyn Report, width: Option<usize>) -> io::Result<String> {
    let mut text = Vec::new();
    report.write_text(&mut Human::for_box(&mut text, width))?;
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// Write a result on stdout: its envelope, or its plain text ending with `summary`.
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

    impl Render for Done {
        fn render(&self, _: &mut Human<'_>) -> io::Result<()> {
            Ok(())
        }
    }

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
            box_text(&Greeting { name: "Ada" }, None).unwrap(),
            "Hello, Ada.\n"
        );
        assert_eq!(box_text(&Done {}, None).unwrap(), "");
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
