use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Serialize;

use super::Format;
use super::human::{Human, Style};
use super::screen::{Outro, Screen};
use crate::error::ErrorReport;

/// A command result: serialized for JSON output, written as text for people.
pub trait Render: Serialize {
    /// The text a person reads on stdout. The default writes nothing, for actions
    /// that report what they did on stderr as they go.
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let _ = out;
        Ok(())
    }

    /// One line that sums the result up: the badge that closes the frame on a
    /// terminal, the last line of the text otherwise.
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

/// Stdout, which carries only command results, in the format the user chose.
#[derive(Clone)]
pub struct Output {
    screen: Arc<Screen>,
}

impl Output {
    pub fn new(screen: Arc<Screen>) -> Self {
        Self { screen }
    }

    /// Writes the result of a finished command, and closes the frame it was drawn in
    /// with its summary.
    pub fn finish(&self, outcome: Outcome) -> anyhow::Result<()> {
        let format = self.screen.format();
        let style = self.screen.text_style();
        let framed = self.screen.frame_open();
        let summary = outcome.0.as_ref().and_then(|report| report.summary());
        let line_start = AtomicBool::new(true);
        self.write_stdout(|out| {
            let summary = if framed { None } else { summary.as_deref() };
            finish(format, style, &line_start, outcome, summary, out)
        })?;
        if framed {
            self.screen.outro(Outro::Done(summary.as_deref()));
        }
        Ok(())
    }

    /// Writes one event of a stream as it happens: an NDJSON line, or its text.
    pub fn event(&self, event: &impl Render) -> anyhow::Result<()> {
        let format = self.screen.format();
        let style = self.screen.text_style();
        let line_start = self.screen.line_start();
        self.write_stdout(|out| {
            write_event(format, style, line_start, event, out)?;
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

/// `summary` is the result's last line, when no frame shows it.
fn finish(
    format: Format,
    style: Style,
    line_start: &AtomicBool,
    outcome: Outcome,
    summary: Option<&str>,
    out: &mut dyn Write,
) -> io::Result<()> {
    let Some(report) = outcome.0 else {
        return Ok(());
    };
    match format {
        Format::Human => {
            let mut out = Human::new(out, style, line_start);
            report.write_text(&mut out)?;
            out.spacer()?;
            match summary {
                Some(summary) => {
                    let summary = out.dim(summary);
                    writeln!(out, "{summary}")
                }
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
    style: Style,
    line_start: &AtomicBool,
    event: &impl Render,
    out: &mut dyn Write,
) -> io::Result<()> {
    match format {
        Format::Human => event.render(&mut Human::new(out, style, line_start)),
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

    fn finished(format: Format, outcome: Outcome, summary: Option<&str>) -> String {
        let line_start = AtomicBool::new(true);
        written(|out| finish(format, Style::plain(), &line_start, outcome, summary, out))
    }

    #[test]
    fn results_are_an_envelope_or_their_text() {
        let greeting = || Outcome::from(Greeting { name: "Ada" });
        assert_eq!(
            finished(Format::Json, greeting(), None),
            "{\"ok\":true,\"data\":{\"name\":\"Ada\"}}\n"
        );
        assert_eq!(finished(Format::Human, greeting(), None), "Hello, Ada.\n");
        assert_eq!(finished(Format::Human, Done {}.into(), None), "");
    }

    #[test]
    fn without_a_frame_the_summary_ends_the_text() {
        let greeting = || Outcome::from(Greeting { name: "Ada" });
        assert_eq!(
            finished(Format::Human, greeting(), Some("Greeted.")),
            "Hello, Ada.\nGreeted.\n"
        );
        assert_eq!(
            finished(Format::Json, greeting(), Some("Greeted.")),
            "{\"ok\":true,\"data\":{\"name\":\"Ada\"}}\n"
        );
    }

    #[test]
    fn streamed_output_is_not_followed_by_an_envelope() {
        for format in [Format::Human, Format::Json] {
            assert_eq!(finished(format, Outcome::streamed(), None), "");
        }
    }

    #[test]
    fn events_are_bare_json_lines_or_their_text() {
        let event = Greeting { name: "Ada" };
        let line_start = AtomicBool::new(true);
        assert_eq!(
            written(|out| write_event(Format::Json, Style::plain(), &line_start, &event, out)),
            "{\"name\":\"Ada\"}\n"
        );
        assert_eq!(
            written(|out| write_event(Format::Human, Style::plain(), &line_start, &event, out)),
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
