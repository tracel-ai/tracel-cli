use std::fmt::Display;
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

use super::screen::GUTTER_WIDTH;

/// The narrowest a shrinkable table column gets, ellipsis included.
const MIN_COLUMN_WIDTH: usize = 4;

/// A result for a person: lines cliclack draws in the command's frame on a terminal,
/// plain text everywhere else. Cards, headings and tables are written through
/// `Details`, `heading` and `Table`; other text with `write!`.
pub struct Human<'a> {
    sink: Sink<'a>,
    width: Option<usize>,
    color: bool,
}

enum Sink<'a> {
    Plain(&'a mut dyn Write),
    Cliclack(Drawing),
}

/// One cliclack log line of a result.
#[derive(Debug, PartialEq, Eq)]
pub enum Draw {
    /// A field of a card.
    Info(String),
    /// A heading, with the lines under it.
    Step(String),
    /// A table, or other text.
    Remark(String),
}

impl Draw {
    /// Draws the line with cliclack, on stderr. A remark's lines that are wider than
    /// the frame wrap inside its gutter.
    pub fn show(&self) -> io::Result<()> {
        match self {
            Self::Info(text) => cliclack::log::info(text),
            Self::Step(text) => cliclack::log::step(text),
            Self::Remark(text) => cliclack::log::remark(in_gutter(text)),
        }
    }
}

/// `text` with each line wider than the frame wrapped to fit after cliclack's bar. The
/// width skips escape sequences, so styled text wraps where it shows.
fn in_gutter(text: &str) -> String {
    cliclack::termwrap(text, GUTTER_WIDTH)
}

/// The lines drawn so far, and the one that text written now goes to.
#[derive(Default)]
struct Drawing {
    draws: Vec<Draw>,
    open: Option<Open>,
}

/// The line being written: a heading and the lines under it, or text of its own.
struct Open {
    heading: bool,
    text: Vec<u8>,
}

impl Drawing {
    fn open(&mut self, heading: bool, text: impl Into<Vec<u8>>) {
        self.close();
        self.open = Some(Open {
            heading,
            text: text.into(),
        });
    }

    /// Adds `text` to the open line, or to a new one.
    fn write(&mut self, text: &[u8]) {
        match &mut self.open {
            Some(open) => open.text.extend_from_slice(text),
            None => self.open(false, text),
        }
    }

    /// Adds `text` to the heading above it, or starts a line for it.
    fn block(&mut self, text: &[u8]) {
        if !self.open.as_ref().is_some_and(|open| open.heading) {
            self.close();
        }
        self.write(text);
    }

    /// Ends the open line. One that is only blank lines draws nothing: cliclack
    /// leaves one after each of its lines already.
    fn close(&mut self) {
        let Some(Open { heading, text }) = self.open.take() else {
            return;
        };
        let text = String::from_utf8_lossy(&text);
        let text = text.trim_start_matches('\n').trim_end();
        if !text.is_empty() {
            let draw = if heading { Draw::Step } else { Draw::Remark };
            self.draws.push(draw(text.into()));
        }
    }
}

impl<'a> Human<'a> {
    /// Plain text on `out`. Tables fit in `width` columns, when there is one.
    pub fn plain(out: &'a mut dyn Write, width: Option<usize>) -> Self {
        Self {
            sink: Sink::Plain(out),
            width,
            color: false,
        }
    }
}

impl Human<'static> {
    /// cliclack's lines, drawn by `Draw::show`. `width` is what fits after its bar.
    pub fn cliclack(width: Option<usize>) -> Self {
        Self {
            sink: Sink::Cliclack(Drawing::default()),
            width,
            color: console::colors_enabled_stderr(),
        }
    }
}

impl Human<'_> {
    pub fn width(&self) -> Option<usize> {
        self.width
    }

    /// The cliclack lines of what was written, in order: none for plain text, which
    /// is already out.
    pub fn into_draws(self) -> Vec<Draw> {
        match self.sink {
            Sink::Plain(_) => Vec::new(),
            Sink::Cliclack(mut drawing) => {
                drawing.close();
                drawing.draws
            }
        }
    }

    /// `text` in `style` on cliclack, when stderr shows colors.
    fn styled(&self, text: impl Display, style: console::Style) -> String {
        if self.color {
            style.force_styling(true).apply_to(text).to_string()
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: impl Display) -> String {
        self.styled(text, console::Style::new().bold())
    }

    pub fn dim(&self, text: impl Display) -> String {
        self.styled(text, console::Style::new().dim())
    }

    /// Names what follows, up to the next heading or card: a step with the lines
    /// under it on cliclack, a title ending in a colon in plain text.
    pub fn heading(&mut self, text: impl Display) -> io::Result<()> {
        match &mut self.sink {
            Sink::Plain(out) => writeln!(out, "{text}:"),
            Sink::Cliclack(drawing) => {
                drawing.open(true, format!("{text}\n"));
                Ok(())
            }
        }
    }

    /// One field of a card: a line of its own on cliclack, its value aligned after
    /// labels `width` wide in plain text.
    fn field(&mut self, label: &str, value: &str, width: usize) -> io::Result<()> {
        match &mut self.sink {
            Sink::Plain(out) => {
                let padding = " ".repeat(width - label.len());
                writeln!(out, "{label}:{padding}  {value}")
            }
            Sink::Cliclack(drawing) => {
                drawing.close();
                drawing.draws.push(Draw::Info(format!("{label}: {value}")));
                Ok(())
            }
        }
    }

    /// Lines under a heading, indented in plain text.
    fn section(&mut self, title: &str, lines: &[String]) -> io::Result<()> {
        self.heading(title)?;
        let indent = match self.sink {
            Sink::Plain(_) => "  ",
            Sink::Cliclack(_) => "",
        };
        lines
            .iter()
            .try_for_each(|line| writeln!(self, "{indent}{line}"))
    }

    /// Lines that belong together, such as a table: a cliclack line of their own,
    /// unless they sit under a heading.
    fn block(&mut self, text: &str) -> io::Result<()> {
        match &mut self.sink {
            Sink::Plain(out) => out.write_all(text.as_bytes()),
            Sink::Cliclack(drawing) => {
                drawing.block(text.as_bytes());
                Ok(())
            }
        }
    }
}

impl Write for Human<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match &mut self.sink {
            Sink::Plain(out) => out.write(buffer),
            Sink::Cliclack(drawing) => {
                drawing.write(buffer);
                Ok(buffer.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.sink {
            Sink::Plain(out) => out.flush(),
            Sink::Cliclack(_) => Ok(()),
        }
    }
}

/// Columns aligned under their headers, one row per line. When the width is too
/// narrow for it, the columns marked with `shrink` are shortened to fit. Without
/// rows it writes nothing, or its `empty` text: a list says it has none in its
/// summary.
pub struct Table<const N: usize> {
    headers: [&'static str; N],
    rows: Vec<[String; N]>,
    shrinkable: [bool; N],
    empty: Option<&'static str>,
}

impl<const N: usize> Table<N> {
    pub fn new(headers: [&'static str; N]) -> Self {
        Self {
            headers,
            rows: Vec::new(),
            shrinkable: [false; N],
            empty: None,
        }
    }

    /// Stands in for the rows when there are none, for a table inside other text.
    pub fn empty(mut self, text: &'static str) -> Self {
        self.empty = Some(text);
        self
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = [String; N]>) -> Self {
        self.rows.extend(rows);
        self
    }

    /// Lets the column with this header be shortened, for free text such as a
    /// description. Columns holding what other commands take, such as names and
    /// ids, stay whole.
    pub fn shrink(mut self, header: &'static str) -> Self {
        let column = self.headers.iter().position(|&name| name == header);
        debug_assert!(column.is_some(), "no column named {header}");
        if let Some(column) = column {
            self.shrinkable[column] = true;
        }
        self
    }

    pub fn write(&self, out: &mut Human<'_>) -> io::Result<()> {
        if self.rows.is_empty() {
            return match self.empty {
                Some(text) => {
                    let text = out.dim(text);
                    out.block(&format!("{text}\n"))
                }
                None => Ok(()),
            };
        }
        let rows: Vec<[String; N]> = self
            .rows
            .iter()
            .map(|row| row.clone().map(|cell| cell_text(&cell)))
            .collect();
        let mut widths: [usize; N] = std::array::from_fn(|column| {
            rows.iter()
                .map(|row| console::measure_text_width(&row[column]))
                .chain([self.headers[column].len()])
                .max()
                .unwrap_or(0)
        });
        if let Some(available) = out.width() {
            self.fit(&mut widths, available);
        }

        let header =
            std::array::from_fn(|column| out.bold(fitted(self.headers[column], widths[column])));
        let mut text = line(&header, &widths);
        for row in &rows {
            let cells = std::array::from_fn(|column| match row[column].as_str() {
                "-" => out.dim("-"),
                cell => fitted(cell, widths[column]),
            });
            text.push_str(&line(&cells, &widths));
        }
        out.block(&text)
    }

    /// Narrows the widest shrinkable columns until the table fits in `available`
    /// columns or nothing can shrink further.
    fn fit(&self, widths: &mut [usize; N], available: usize) {
        let gaps = 2 * N.saturating_sub(1);
        while widths.iter().sum::<usize>() + gaps > available {
            let widest = (0..N)
                .filter(|&column| self.shrinkable[column] && widths[column] > MIN_COLUMN_WIDTH)
                .max_by_key(|&column| widths[column]);
            match widest {
                Some(column) => widths[column] -= 1,
                None => break,
            }
        }
    }
}

/// One row of a table: each cell but the last padded to its column, then a gap.
fn line<const N: usize>(cells: &[String; N], widths: &[usize; N]) -> String {
    let mut line = String::new();
    for (column, cell) in cells.iter().enumerate() {
        line.push_str(cell);
        if column + 1 < N {
            let padding = widths[column].saturating_sub(console::measure_text_width(cell));
            line.push_str(&" ".repeat(padding + 2));
        }
    }
    line.push('\n');
    line
}

/// Labelled values: a card of fields. Sections list the fields of a JSON value
/// under a heading.
#[derive(Default)]
pub struct Details {
    entries: Vec<Entry>,
}

enum Entry {
    Field(&'static str, String),
    Section(&'static str, Vec<(String, String)>),
}

impl Details {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn field(mut self, label: &'static str, value: impl Display) -> Self {
        self.entries.push(Entry::Field(label, value.to_string()));
        self
    }

    pub fn optional(self, label: &'static str, value: Option<impl Display>) -> Self {
        match value {
            Some(value) => self.field(label, value),
            None => self.field(label, ""),
        }
    }

    /// The fields of `value` under `title`, nested keys joined with dots. An empty or
    /// null value adds nothing.
    pub fn section(mut self, title: &'static str, value: &impl Serialize) -> Self {
        let value = serde_json::to_value(value).unwrap_or(Value::Null);
        let mut fields = Vec::new();
        match &value {
            Value::Object(_) => flatten("", &value, &mut fields),
            Value::Null => {}
            other => fields.push(("value".into(), other.to_string())),
        }
        if !fields.is_empty() {
            self.entries.push(Entry::Section(title, fields));
        }
        self
    }

    pub fn write(&self, out: &mut Human<'_>) -> io::Result<()> {
        let width = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Field(label, _) => Some(label.len()),
                Entry::Section(..) => None,
            })
            .max()
            .unwrap_or(0);
        for entry in &self.entries {
            match entry {
                Entry::Field(label, value) => {
                    let value = shown(out, value);
                    out.field(label, &value, width)?;
                }
                Entry::Section(title, fields) => {
                    let width = fields.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
                    let lines: Vec<String> = fields
                        .iter()
                        .map(|(key, value)| {
                            let padding = " ".repeat(width - key.len());
                            format!("{}{padding}  {}", out.dim(key), shown(out, value))
                        })
                        .collect();
                    out.section(title, &lines)?;
                }
            }
        }
        Ok(())
    }
}

/// The summary of a list: "Showing 10 of 16 experiments." when some are left out,
/// "16 experiments." when not, and `none` when there are none.
pub fn counted(shown: usize, total: u64, [one, many]: [&str; 2], none: &str) -> String {
    let shown = shown as u64;
    match total.max(shown) {
        0 => none.to_string(),
        total if shown < total => format!("Showing {shown} of {total} {many}."),
        1 => format!("1 {one}."),
        total => format!("{total} {many}."),
    }
}

fn flatten(prefix: &str, value: &Value, fields: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                let key = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&key, value, fields);
            }
        }
        Value::Object(_) if prefix.is_empty() => {}
        Value::String(text) => fields.push((prefix.into(), text.clone())),
        other => fields.push((prefix.into(), other.to_string())),
    }
}

/// A value on one line, with a dim "-" standing in for nothing.
fn shown(out: &Human<'_>, value: &str) -> String {
    match cell_text(value) {
        text if text == "-" => out.dim(text),
        text => text,
    }
}

/// `text` cut to `width` columns with an ellipsis, when it is wider.
fn fitted(text: &str, width: usize) -> String {
    if console::measure_text_width(text) <= width {
        return text.to_string();
    }
    let kept = console::truncate_str(text, width.saturating_sub(1), "");
    format!("{}…", kept.trim_end())
}

/// A value on one line, with "-" standing in for nothing.
fn cell_text(value: &str) -> String {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() { "-".into() } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(width: Option<usize>, write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut Human::plain(&mut out, width)).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn plain(write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        text(None, write)
    }

    /// The cliclack lines `write` makes, unstyled.
    fn drawn(write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> Vec<Draw> {
        let mut out = Human::cliclack(None);
        out.color = false;
        write(&mut out).unwrap();
        out.into_draws()
    }

    fn table() -> Table<3> {
        Table::new(["NAME", "DESCRIPTION", "ID"]).rows([
            ["weights".into(), "two\nlines".into(), "1".into()],
            ["w".into(), String::new(), "22".into()],
        ])
    }

    const TABLE: &str = "NAME     DESCRIPTION  ID\n\
                         weights  two lines    1\n\
                         w        -            22";

    #[test]
    fn table_aligns_columns_and_keeps_cells_on_one_line() {
        assert_eq!(plain(|out| table().write(out)), format!("{TABLE}\n"));
    }

    #[test]
    fn an_empty_table_writes_its_empty_text_or_nothing() {
        let empty = || Table::new(["NAME"]);
        assert_eq!(plain(|out| empty().write(out)), "");
        assert_eq!(
            plain(|out| empty().empty("No files.").write(out)),
            "No files.\n"
        );
    }

    #[test]
    fn a_narrow_width_shortens_only_shrinkable_columns() {
        let table = || {
            Table::new(["NAME", "DESCRIPTION", "ID"]).rows([[
                "weights".into(),
                "a rather long description".into(),
                "0123456789".into(),
            ]])
        };
        assert_eq!(
            text(Some(36), |out| table().shrink("DESCRIPTION").write(out)),
            "NAME     DESCRIPTION      ID\n\
             weights  a rather long…   0123456789\n"
        );
        assert_eq!(
            text(Some(36), |out| table().write(out)),
            plain(|out| table().write(out))
        );
    }

    #[test]
    fn a_shrinking_column_can_cut_its_header() {
        assert_eq!(
            text(Some(18), |out| table().shrink("DESCRIPTION").write(out)),
            "NAME     DESC…  ID\n\
             weights  two…   1\n\
             w        -      22\n"
        );
    }

    #[test]
    fn plain_text_is_never_styled() {
        let text = plain(|out| {
            details().write(out)?;
            table().write(out)?;
            let (bold, dim) = (out.bold("bold"), out.dim("dim"));
            writeln!(out, "{bold} {dim}")
        });
        assert!(!text.contains('\u{1b}'));
    }

    fn details() -> Details {
        Details::new()
            .field("Name", "weights")
            .optional("Description", None::<&str>)
            .section(
                "Config",
                &serde_json::json!({"lr": 0.1, "schedule": {"warmup": 2}, "tags": ["a"]}),
            )
            .section("Attributes", &serde_json::json!({}))
    }

    #[test]
    fn details_align_values_and_list_json_fields() {
        assert_eq!(
            plain(|out| details().write(out)),
            "Name:         weights\n\
             Description:  -\n\
             Config:\n  \
               lr               0.1\n  \
               schedule.warmup  2\n  \
               tags             [\"a\"]\n"
        );
    }

    #[test]
    fn on_cliclack_details_are_a_line_per_field_and_a_step_per_section() {
        assert_eq!(
            drawn(|out| details().write(out)),
            [
                Draw::Info("Name: weights".into()),
                Draw::Info("Description: -".into()),
                Draw::Step(
                    "Config\n\
                     lr               0.1\n\
                     schedule.warmup  2\n\
                     tags             [\"a\"]"
                        .into()
                ),
            ]
        );
    }

    #[test]
    fn on_cliclack_a_heading_takes_the_table_under_it() {
        let draws = drawn(|out| {
            out.heading("Files")?;
            table().write(out)?;
            Details::new()
                .section("Metadata", &serde_json::json!({"seed": 7}))
                .write(out)
        });
        assert_eq!(
            draws,
            [
                Draw::Step(format!("Files\n{TABLE}")),
                Draw::Step("Metadata\nseed  7".into()),
            ]
        );
    }

    #[test]
    fn on_cliclack_a_table_is_a_line_of_its_own() {
        let draws = drawn(|out| {
            writeln!(out, "Before.")?;
            table().write(out)?;
            writeln!(out)?;
            Table::new(["GROUP"]).empty("No groups.").write(out)
        });
        assert_eq!(
            draws,
            [
                Draw::Remark("Before.".into()),
                Draw::Remark(TABLE.into()),
                Draw::Remark("No groups.".into()),
            ]
        );
    }

    #[test]
    fn on_cliclack_text_collects_until_the_next_field() {
        let draws = drawn(|out| {
            write!(out, "par")?;
            writeln!(out, "tial")?;
            writeln!(out, "next")?;
            Details::new().field("Name", "weights").write(out)?;
            writeln!(out)
        });
        assert_eq!(
            draws,
            [
                Draw::Remark("partial\nnext".into()),
                Draw::Info("Name: weights".into()),
            ]
        );
    }

    /// The columns `in_gutter` wraps to, measured as `cliclack::termwrap` measures them.
    fn gutter_width() -> usize {
        usize::from(
            console::Term::stderr()
                .size()
                .1
                .saturating_sub(GUTTER_WIDTH),
        )
    }

    #[test]
    fn wrapping_in_the_gutter_keeps_a_styled_table_fitted_to_the_frame() {
        let width = gutter_width();
        let mut out = Human::cliclack(Some(width));
        out.color = true;
        Table::new(["NAME", "DESCRIPTION", "ID"])
            .shrink("DESCRIPTION")
            .rows([[
                "weights".into(),
                "a rather long description ".repeat(width / 8),
                String::new(),
            ]])
            .write(&mut out)
            .unwrap();
        let [Draw::Remark(table)] = <[Draw; 1]>::try_from(out.into_draws()).unwrap() else {
            panic!("expected one remark");
        };
        assert!(table.contains('\u{1b}'));
        assert_eq!(in_gutter(&table), table);
    }

    #[test]
    fn text_wider_than_the_frame_wraps_in_the_gutter() {
        let width = gutter_width();
        let wrapped = in_gutter("word ".repeat(width).trim_end());
        assert!(wrapped.lines().count() > 1);
        assert!(
            wrapped
                .lines()
                .all(|line| console::measure_text_width(line) <= width)
        );
    }

    #[test]
    fn counts_mention_what_was_left_out() {
        let count = |shown, total| counted(shown, total, ["model", "models"], "No models.");
        assert_eq!(count(0, 0), "No models.");
        assert_eq!(count(1, 1), "1 model.");
        assert_eq!(count(3, 3), "3 models.");
        assert_eq!(count(10, 16), "Showing 10 of 16 models.");
    }
}
