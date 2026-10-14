use std::fmt::Display;
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

/// The narrowest a shrinkable table column gets, ellipsis included.
const MIN_COLUMN_WIDTH: usize = 4;

/// A writer of a result's text: plain on stdout, or styled for its box on a terminal.
/// Fields, headings and tables go through `Details`, `heading` and `Table`, and other
/// text through `write!`.
pub struct Human<'a> {
    out: &'a mut dyn Write,
    width: Option<usize>,
    styled: bool,
}

impl<'a> Human<'a> {
    /// Write text on `out`, with tables fitted to `width` columns when given, and
    /// styled for a terminal when `styled`.
    pub fn new(out: &'a mut dyn Write, width: Option<usize>, styled: bool) -> Self {
        Self { out, width, styled }
    }

    /// Write plain text on `out`, with tables fitted to `width` columns when given.
    pub fn plain(out: &'a mut dyn Write, width: Option<usize>) -> Self {
        Self::new(out, width, false)
    }
}

impl Human<'_> {
    pub fn width(&self) -> Option<usize> {
        self.width
    }

    fn styled(&self, text: impl Display, style: console::Style) -> String {
        if self.styled {
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

    /// Write a title for what follows, ending in a colon.
    pub fn heading(&mut self, text: impl Display) -> io::Result<()> {
        writeln!(self, "{text}:")
    }

    /// Write one field, its value aligned after labels `width` wide.
    fn field(&mut self, label: &str, value: &str, width: usize) -> io::Result<()> {
        let padding = " ".repeat(width - label.len());
        let label = self.dim(format!("{label}:"));
        writeln!(self, "{label}{padding}  {value}")
    }

    /// Write lines indented under a heading.
    fn section(&mut self, title: &str, lines: &[String]) -> io::Result<()> {
        self.heading(title)?;
        lines.iter().try_for_each(|line| writeln!(self, "  {line}"))
    }

    /// Write lines that belong together, such as a table.
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

/// Columns aligned under their headers, one row per line. Columns marked with `shrink`
/// shorten to fit a narrow width. Without rows it writes its `empty` text, or nothing
/// when a list's summary says it has none.
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

    /// Show `text` when there are no rows, for a table inside other text.
    pub fn empty(mut self, text: &'static str) -> Self {
        self.empty = Some(text);
        self
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = [String; N]>) -> Self {
        self.rows.extend(rows);
        self
    }

    /// Let the column with this header shorten, for free text such as a description.
    /// Columns of values that other commands take, such as names, stay whole.
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
            std::array::from_fn(|column| out.bold(truncate(self.headers[column], widths[column])));
        let mut text = line(&header, &widths);
        for row in &rows {
            let cells = std::array::from_fn(|column| match row[column].as_str() {
                "-" => out.dim("-"),
                cell => truncate(cell, widths[column]),
            });
            text.push_str(&line(&cells, &widths));
        }
        out.block(&text)
    }

    /// Narrow the widest shrinkable columns until the table fits in `available`
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

/// A row of a table: each cell but the last padded to its column, then a gap.
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

/// Labelled values, aligned, and sections that list the fields of a JSON value under a
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

    /// Add the fields of `value` under `title`, nested keys joined with dots. An empty
    /// or null value adds nothing.
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
                    let value = styled_cell(out, value);
                    out.field(label, &value, width)?;
                }
                Entry::Section(title, fields) => {
                    let width = fields.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
                    let lines: Vec<String> = fields
                        .iter()
                        .map(|(key, value)| {
                            let padding = " ".repeat(width - key.len());
                            format!("{}{padding}  {}", out.dim(key), styled_cell(out, value))
                        })
                        .collect();
                    out.section(title, &lines)?;
                }
            }
        }
        Ok(())
    }
}

/// Count things in words: "1 file" or "3 files".
pub fn counted(count: u64, [one, many]: [&str; 2]) -> String {
    match count {
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    }
}

/// Sum up a list: "Showing 10 of 16 experiments." when some are left out,
/// "16 experiments." when not, and `none` when there are none.
pub fn list_summary(shown: usize, total: u64, forms: [&str; 2], none: &str) -> String {
    let shown = shown as u64;
    match total.max(shown) {
        0 => none.to_string(),
        total if shown < total => format!("Showing {shown} of {total} {}.", forms[1]),
        total => format!("{}.", counted(total, forms)),
    }
}

/// Shorten a digest to 12 characters, enough to tell versions apart.
pub fn short_digest(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

/// Write a size in bytes as "512 B", "6.9 KB" or "713.7 MB".
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{} {}", number(value), UNITS[unit])
}

/// Round a number to about 4 significant digits, as "0.75" for 0.7499999999999999,
/// keeping all the integer digits of a large one.
pub fn number(value: f64) -> String {
    if value == 0.0 || !value.is_finite() {
        return value.to_string();
    }
    let magnitude = value.abs().log10().floor() as i32;
    let decimals = (3 - magnitude).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
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
        Value::Number(value) if value.is_f64() => {
            fields.push((prefix.into(), number(value.as_f64().unwrap_or_default())))
        }
        other => fields.push((prefix.into(), other.to_string())),
    }
}

/// Put a value on one line, with a dim "-" for nothing.
fn styled_cell(out: &Human<'_>, value: &str) -> String {
    match cell_text(value) {
        text if text == "-" => out.dim(text),
        text => text,
    }
}

/// Cut `text` to `width` columns with an ellipsis, when it is wider.
fn truncate(text: &str, width: usize) -> String {
    if console::measure_text_width(text) <= width {
        return text.to_string();
    }
    let kept = console::truncate_str(text, width.saturating_sub(1), "");
    format!("{}…", kept.trim_end())
}

/// Put a value on one line, with "-" for nothing.
fn cell_text(value: &str) -> String {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() { "-".into() } else { text }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn text(width: Option<usize>, write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut Human::plain(&mut out, width)).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn files() -> Table<3> {
        Table::new(["NAME", "DESCRIPTION", "ID"]).rows([
            ["weights".into(), "best\nepoch".into(), "1".into()],
            ["w".into(), String::new(), "22".into()],
        ])
    }

    #[test]
    fn given_rows_when_writing_a_table_then_its_columns_align_with_one_line_per_row() {
        let written = text(None, |out| files().write(out));

        assert_eq!(
            written,
            "NAME     DESCRIPTION  ID\n\
             weights  best epoch   1\n\
             w        -            22\n"
        );
    }

    #[test]
    fn given_no_rows_when_writing_a_table_then_only_its_empty_text_is_written() {
        let written = text(None, |out| {
            Table::new(["NAME"]).empty("No files.").write(out)
        });

        assert_eq!(written, "No files.\n");
    }

    #[test]
    fn given_a_narrow_width_when_writing_a_table_then_only_the_shrinkable_column_is_cut() {
        let table = Table::new(["NAME", "DESCRIPTION", "ID"])
            .shrink("DESCRIPTION")
            .rows([[
                "weights".into(),
                "a rather long description".into(),
                "0123456789".into(),
            ]]);

        let written = text(Some(36), |out| table.write(out));

        assert_eq!(
            written,
            "NAME     DESCRIPTION      ID\n\
             weights  a rather long…   0123456789\n"
        );
    }

    #[test]
    fn given_fields_and_a_section_when_writing_details_then_fields_align_above_dotted_keys() {
        let details = Details::new()
            .field("Name", "weights")
            .optional("Description", None::<&str>)
            .section(
                "Config",
                &serde_json::json!({"lr": 0.1, "schedule": {"warmup": 2}, "tags": ["a"]}),
            );

        let written = text(None, |out| details.write(out));

        assert_eq!(
            written,
            "Name:         weights\n\
             Description:  -\n\
             Config:\n  \
               lr               0.1\n  \
               schedule.warmup  2\n  \
               tags             [\"a\"]\n"
        );
    }

    #[test]
    fn given_styles_when_writing_then_removing_them_leaves_the_plain_text() {
        let write = |out: &mut Human<'_>| {
            let name = out.bold("NAME");
            writeln!(out, "{name}")?;
            files().write(out)
        };
        let mut styled = Vec::new();

        write(&mut Human::new(&mut styled, None, true)).unwrap();

        let styled = String::from_utf8(styled).unwrap();
        assert_ne!(styled, text(None, write));
        assert_eq!(console::strip_ansi_codes(&styled), text(None, write));
    }

    #[rstest]
    #[case::none(0, 0, "No models.")]
    #[case::all(3, 3, "3 models.")]
    #[case::some(10, 16, "Showing 10 of 16 models.")]
    fn given_a_list_when_summing_it_up_then_it_says_how_many_were_shown(
        #[case] shown: usize,
        #[case] total: u64,
        #[case] expected: &str,
    ) {
        let summary = list_summary(shown, total, ["model", "models"], "No models.");

        assert_eq!(summary, expected);
    }

    #[rstest]
    #[case::float_noise(0.7499999999999999, "0.75")]
    #[case::long_fraction(0.923076923076923, "0.9231")]
    #[case::large(1234.5678, "1235")]
    #[case::whole(2.0, "2")]
    fn given_a_metric_when_showing_it_then_it_keeps_about_four_significant_digits(
        #[case] value: f64,
        #[case] expected: &str,
    ) {
        let shown = number(value);

        assert_eq!(shown, expected);
    }

    #[rstest]
    #[case::bytes(999, "999 B")]
    #[case::kilobytes(6900, "6.9 KB")]
    #[case::megabytes(713_713_674, "713.7 MB")]
    #[case::gigabytes(2_500_000_000, "2.5 GB")]
    fn given_a_byte_count_when_showing_it_then_it_is_in_its_largest_unit(
        #[case] bytes: u64,
        #[case] expected: &str,
    ) {
        let shown = size(bytes);

        assert_eq!(shown, expected);
    }
}
