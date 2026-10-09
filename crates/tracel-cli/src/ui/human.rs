use std::fmt::Display;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::Value;

/// The narrowest a shrinkable table column gets, ellipsis included.
const MIN_COLUMN_WIDTH: usize = 4;

/// How text on stdout may look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
    /// The columns text may use, when stdout is a terminal.
    pub width: Option<usize>,
    /// Whether the text sits inside a cliclack frame, after its bar.
    pub framed: bool,
}

impl Style {
    pub fn plain() -> Self {
        Self {
            color: false,
            width: None,
            framed: false,
        }
    }
}

/// Stdout for a person. Inside a frame each line starts with the frame's bar, or with
/// a symbol as cliclack's own lines do; piped, it is plain text.
pub struct Human<'a> {
    out: &'a mut dyn Write,
    style: Style,
    line_start: &'a AtomicBool,
    symbol: Option<String>,
    line_blank: bool,
    last_blank: bool,
}

impl<'a> Human<'a> {
    /// `line_start` carries over between the writes of one stream.
    pub fn new(out: &'a mut dyn Write, style: Style, line_start: &'a AtomicBool) -> Self {
        Self {
            out,
            style,
            line_start,
            symbol: None,
            line_blank: true,
            // A frame's intro ends with a bar of its own.
            last_blank: true,
        }
    }

    /// Text without color, width or frame, as when stdout is piped.
    #[cfg(test)]
    pub fn plain(out: &'a mut dyn Write) -> Self {
        static LINE_START: AtomicBool = AtomicBool::new(true);
        Self::new(out, Style::plain(), &LINE_START)
    }

    pub fn framed(&self) -> bool {
        self.style.framed
    }

    pub fn width(&self) -> Option<usize> {
        self.style.width
    }

    fn styled(&self, text: impl Display, style: console::Style) -> String {
        style
            .apply_to(text)
            .force_styling(self.style.color)
            .to_string()
    }

    pub fn bold(&self, text: impl Display) -> String {
        self.styled(text, console::Style::new().bold())
    }

    pub fn dim(&self, text: impl Display) -> String {
        self.styled(text, console::Style::new().dim())
    }

    /// One line of a card: in a frame, after a bullet and followed by a bar, as
    /// cliclack logs a message.
    pub fn item(&mut self, text: impl Display) -> io::Result<()> {
        if self.framed() {
            self.symbol = Some(self.styled(console::Emoji("●", "•"), console::Style::new().blue()));
            writeln!(self, "{text}")?;
            self.spacer()
        } else {
            writeln!(self, "{text}")
        }
    }

    /// A line that names what follows: a step in a frame, a title ending in a colon
    /// otherwise.
    pub fn heading(&mut self, text: impl Display) -> io::Result<()> {
        if self.framed() {
            self.symbol =
                Some(self.styled(console::Emoji("◇", "o"), console::Style::new().green()));
            writeln!(self, "{text}")
        } else {
            writeln!(self, "{text}:")
        }
    }

    /// An empty line of the frame's bar, unless the line before was one.
    pub fn spacer(&mut self) -> io::Result<()> {
        if self.framed() && !self.last_blank {
            writeln!(self)?;
        }
        Ok(())
    }

    fn bar(&self) -> String {
        self.styled(
            console::Emoji("│", "|"),
            console::Style::new().bright().black(),
        )
    }
}

impl Write for Human<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if !self.style.framed {
            return self.out.write(buffer);
        }
        for line in buffer.split_inclusive(|&byte| byte == b'\n') {
            if self.line_start.load(Ordering::Relaxed) {
                let lead = self.symbol.take().unwrap_or_else(|| self.bar());
                write!(self.out, "{lead}  ")?;
                self.line_blank = true;
            }
            let ends_line = line.ends_with(b"\n");
            if line.len() > usize::from(ends_line) {
                self.line_blank = false;
            }
            self.out.write_all(line)?;
            if ends_line {
                self.last_blank = self.line_blank;
            }
            self.line_start.store(ends_line, Ordering::Relaxed);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Columns aligned under their headers, one row per line. On a terminal too narrow
/// for it, the columns marked with `shrink` are shortened to fit. Without rows it
/// writes nothing, or its `empty` text: a list says it has none in its summary.
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
                    writeln!(out, "{text}")
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
        self.write_line(out, &header, &widths)?;
        for row in &rows {
            let cells = std::array::from_fn(|column| match row[column].as_str() {
                "-" => out.dim("-"),
                cell => fitted(cell, widths[column]),
            });
            self.write_line(out, &cells, &widths)?;
        }
        Ok(())
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

    fn write_line(
        &self,
        out: &mut Human<'_>,
        cells: &[String; N],
        widths: &[usize; N],
    ) -> io::Result<()> {
        for (column, cell) in cells.iter().enumerate() {
            write!(out, "{cell}")?;
            if column + 1 < N {
                let padding = widths[column].saturating_sub(console::measure_text_width(cell));
                write!(out, "{}  ", " ".repeat(padding))?;
            }
        }
        writeln!(out)
    }
}

/// Labelled values: a card of bullets in a frame, aligned lines otherwise. Sections
/// list the fields of a JSON value under a heading.
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
                    if out.framed() {
                        out.item(format!("{label}: {value}"))?;
                    } else {
                        let padding = " ".repeat(width - label.len());
                        let label = out.dim(format!("{label}:"));
                        writeln!(out, "{label}{padding}  {value}")?;
                    }
                }
                Entry::Section(title, fields) => {
                    out.heading(title)?;
                    let width = fields.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
                    let indent = if out.framed() { "" } else { "  " };
                    for (key, value) in fields {
                        let padding = " ".repeat(width - key.len());
                        let (key, value) = (out.dim(key), shown(out, value));
                        writeln!(out, "{indent}{key}{padding}  {value}")?;
                    }
                    out.spacer()?;
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

    fn text(style: Style, write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        let line_start = AtomicBool::new(true);
        let mut out = Vec::new();
        write(&mut Human::new(&mut out, style, &line_start)).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn plain(write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        text(Style::plain(), write)
    }

    fn framed(width: Option<usize>) -> Style {
        Style {
            color: false,
            width,
            framed: true,
        }
    }

    /// `text` with the frame's glyphs as this terminal draws them.
    fn drawn(text: &str) -> String {
        text.replace('│', &console::Emoji("│", "|").to_string())
            .replace('●', &console::Emoji("●", "•").to_string())
            .replace('◇', &console::Emoji("◇", "o").to_string())
    }

    fn narrow(width: usize) -> Style {
        Style {
            color: false,
            width: Some(width),
            framed: false,
        }
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
    fn a_narrow_terminal_shortens_only_shrinkable_columns() {
        let table = || {
            Table::new(["NAME", "DESCRIPTION", "ID"]).rows([[
                "weights".into(),
                "a rather long description".into(),
                "0123456789".into(),
            ]])
        };
        assert_eq!(
            text(narrow(36), |out| table().shrink("DESCRIPTION").write(out)),
            "NAME     DESCRIPTION      ID\n\
             weights  a rather long…   0123456789\n"
        );
        assert_eq!(
            text(narrow(36), |out| table().write(out)),
            plain(|out| table().write(out))
        );
    }

    #[test]
    fn a_shrinking_column_can_cut_its_header() {
        assert_eq!(
            text(narrow(18), |out| table().shrink("DESCRIPTION").write(out)),
            "NAME     DESC…  ID\n\
             weights  two…   1\n\
             w        -      22\n"
        );
    }

    #[test]
    fn a_framed_table_sits_after_the_bar() {
        assert_eq!(
            text(framed(None), |out| table().write(out)),
            drawn(
                "│  NAME     DESCRIPTION  ID\n\
                 │  weights  two lines    1\n\
                 │  w        -            22\n"
            )
        );
    }

    #[test]
    fn colors_apply_only_when_enabled() {
        let colored = Style {
            color: true,
            width: None,
            framed: false,
        };
        assert!(text(colored, |out| table().write(out)).contains("\u{1b}["));
        assert!(!plain(|out| table().write(out)).contains("\u{1b}["));
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
    fn framed_details_are_a_card() {
        assert_eq!(
            text(framed(None), |out| details().write(out)),
            drawn(
                "●  Name: weights\n\
                 │  \n\
                 ●  Description: -\n\
                 │  \n\
                 ◇  Config\n\
                 │  lr               0.1\n\
                 │  schedule.warmup  2\n\
                 │  tags             [\"a\"]\n\
                 │  \n"
            )
        );
    }

    #[test]
    fn a_stream_keeps_one_bar_per_line_across_writes() {
        let line_start = AtomicBool::new(true);
        let mut out = Vec::new();
        for piece in ["par", "tial\nnext", "\n"] {
            Human::new(&mut out, framed(None), &line_start)
                .write_all(piece.as_bytes())
                .unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            drawn("│  partial\n│  next\n")
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
