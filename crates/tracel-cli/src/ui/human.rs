use std::fmt::Display;
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

/// The narrowest a shrinkable table column gets, ellipsis included.
const MIN_COLUMN_WIDTH: usize = 4;

/// A box's text is dim, and a styled span ends with a reset, so each span turns dim
/// back on after it.
const BOX_TEXT: &str = "\u{1b}[2m";

/// A result as text for a person: plain, as stdout carries it, or styled for the box a
/// terminal shows it in. Cards, headings and tables are written through `Details`,
/// `heading` and `Table`; other text with `write!`.
pub struct Human<'a> {
    out: &'a mut dyn Write,
    width: Option<usize>,
    styled: bool,
}

impl<'a> Human<'a> {
    /// Plain text on `out`. Tables fit in `width` columns, when there is one.
    pub fn plain(out: &'a mut dyn Write, width: Option<usize>) -> Self {
        Self {
            out,
            width,
            styled: false,
        }
    }

    /// Text for a box on the terminal, styled unless stderr shows no colors.
    pub fn boxed(out: &'a mut dyn Write, width: Option<usize>) -> Self {
        Self {
            out,
            width,
            styled: console::colors_enabled_stderr(),
        }
    }
}

impl Human<'_> {
    pub fn width(&self) -> Option<usize> {
        self.width
    }

    fn styled(&self, text: impl Display, style: console::Style) -> String {
        if self.styled {
            format!("{}{BOX_TEXT}", style.force_styling(true).apply_to(text))
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

    /// Names what follows: a title ending in a colon.
    pub fn heading(&mut self, text: impl Display) -> io::Result<()> {
        writeln!(self, "{text}:")
    }

    /// One field of a card, its value aligned after labels `width` wide.
    fn field(&mut self, label: &str, value: &str, width: usize) -> io::Result<()> {
        let padding = " ".repeat(width - label.len());
        writeln!(self, "{label}:{padding}  {value}")
    }

    /// Lines under a heading, indented.
    fn section(&mut self, title: &str, lines: &[String]) -> io::Result<()> {
        self.heading(title)?;
        lines.iter().try_for_each(|line| writeln!(self, "  {line}"))
    }

    /// Lines that belong together, such as a table.
    fn block(&mut self, text: &str) -> io::Result<()> {
        self.write_all(text.as_bytes())
    }
}

impl Write for Human<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.out.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
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

/// Labelled values, aligned. Sections list the fields of a JSON value under a
/// heading.
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
    use crate::ui::screen::BOX_PADDING;

    fn text(width: Option<usize>, write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut Human::plain(&mut out, width)).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn plain(write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        text(None, write)
    }

    /// Text for a box, styled.
    fn boxed(width: Option<usize>, write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        let mut human = Human::boxed(&mut out, width);
        human.styled = true;
        write(&mut human).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn table() -> Table<3> {
        Table::new(["NAME", "DESCRIPTION", "ID"]).rows([
            ["weights".into(), "two\nlines".into(), "1".into()],
            ["w".into(), String::new(), "22".into()],
        ])
    }

    #[test]
    fn table_aligns_columns_and_keeps_cells_on_one_line() {
        assert_eq!(
            plain(|out| table().write(out)),
            "NAME     DESCRIPTION  ID\n\
             weights  two lines    1\n\
             w        -            22\n"
        );
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
    fn plain_text_is_never_styled_and_a_box_is_the_same_text_styled() {
        let write = |out: &mut Human<'_>| {
            details().write(out)?;
            table().write(out)
        };
        let plain = plain(write);
        let boxed = boxed(None, write);
        assert!(!plain.contains('\u{1b}'));
        assert!(boxed.contains('\u{1b}'));
        assert_eq!(console::strip_ansi_codes(&boxed), plain);
    }

    #[test]
    fn a_styled_span_turns_the_box_dim_back_on() {
        assert_eq!(
            boxed(None, |out| {
                let (bold, dim) = (out.bold("NAME"), out.dim("-"));
                writeln!(out, "{bold} {dim} x")
            }),
            "\u{1b}[1mNAME\u{1b}[0m\u{1b}[2m \u{1b}[2m-\u{1b}[0m\u{1b}[2m x\n"
        );
    }

    /// The columns inside a box, as cliclack measures them to wrap its text.
    fn box_width() -> usize {
        usize::from(console::Term::stderr().size().1.saturating_sub(BOX_PADDING))
    }

    #[test]
    fn a_table_fitted_to_the_box_is_not_wrapped_again() {
        let width = box_width();
        let fitted = boxed(Some(width), |out| {
            Table::new(["NAME", "DESCRIPTION", "ID"])
                .shrink("DESCRIPTION")
                .rows([[
                    "weights".into(),
                    "a rather long description ".repeat(width / 8),
                    String::new(),
                ]])
                .write(out)
        });
        let fitted = fitted.trim_end();
        assert!(
            fitted
                .lines()
                .any(|line| console::measure_text_width(line) == width)
        );
        assert_eq!(cliclack::termwrap(fitted, BOX_PADDING), fitted);
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
