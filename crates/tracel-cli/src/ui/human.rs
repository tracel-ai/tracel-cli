use std::fmt::Display;
use std::io::{self, Write};

use serde::Serialize;

/// How text on stdout may look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
    /// The terminal's width in columns, when stdout is a terminal.
    pub width: Option<usize>,
}

impl Style {
    pub fn plain() -> Self {
        Self {
            color: false,
            width: None,
        }
    }
}

/// Stdout for a person: plain text when piped, styled and fitted on a terminal.
pub struct Human<'a> {
    out: &'a mut dyn Write,
    style: Style,
}

impl<'a> Human<'a> {
    pub fn new(out: &'a mut dyn Write, style: Style) -> Self {
        Self { out, style }
    }

    /// Text without color or a width, as when stdout is piped.
    #[cfg(test)]
    pub fn plain(out: &'a mut dyn Write) -> Self {
        Self::new(out, Style::plain())
    }

    pub fn width(&self) -> Option<usize> {
        self.style.width
    }

    /// Whether the text may be colored.
    pub fn color(&self) -> bool {
        self.style.color
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
}

impl Write for Human<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.out.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Columns aligned under their headers, one row per line. On a terminal too narrow
/// for it, the columns marked with `shrink` are shortened to fit.
pub struct Table<const N: usize> {
    headers: [&'static str; N],
    rows: Vec<[String; N]>,
    shrinkable: [bool; N],
    total: Option<u64>,
}

impl<const N: usize> Table<N> {
    pub fn new(headers: [&'static str; N]) -> Self {
        Self {
            headers,
            rows: Vec::new(),
            shrinkable: [false; N],
            total: None,
        }
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

    /// Ends the table with "Showing N of TOTAL" when it holds fewer rows than exist.
    pub fn total(mut self, total: impl TryInto<u64>) -> Self {
        self.total = total.try_into().ok();
        self
    }

    pub fn write(&self, out: &mut Human<'_>) -> io::Result<()> {
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

        let header = self.headers.map(|header| out.bold(header));
        self.write_line(out, &header, &widths)?;
        for row in &rows {
            let cells = std::array::from_fn(|column| {
                let cell = &row[column];
                if cell == "-" {
                    out.dim(cell)
                } else if console::measure_text_width(cell) > widths[column] {
                    shorten(cell, widths[column])
                } else {
                    cell.clone()
                }
            });
            self.write_line(out, &cells, &widths)?;
        }
        let shown = self.rows.len() as u64;
        match self.total {
            Some(total) if shown < total => {
                let footer = out.dim(format!("Showing {shown} of {total}"));
                writeln!(out, "{footer}")
            }
            _ => Ok(()),
        }
    }

    /// Narrows the widest shrinkable columns, never below their header, until the
    /// table fits in `available` columns or nothing can shrink.
    fn fit(&self, widths: &mut [usize; N], available: usize) {
        let gaps = 2 * N.saturating_sub(1);
        while widths.iter().sum::<usize>() + gaps > available {
            let widest = (0..N)
                .filter(|&column| {
                    self.shrinkable[column] && widths[column] > self.headers[column].len()
                })
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

/// Labelled values, one per line, with the values aligned.
#[derive(Default)]
pub struct Details {
    fields: Vec<(&'static str, String)>,
}

impl Details {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn field(mut self, label: &'static str, value: impl Display) -> Self {
        self.fields.push((label, value.to_string()));
        self
    }

    pub fn optional(self, label: &'static str, value: Option<impl Display>) -> Self {
        match value {
            Some(value) => self.field(label, value),
            None => self.field(label, ""),
        }
    }

    pub fn write(&self, out: &mut Human<'_>) -> io::Result<()> {
        let width = self
            .fields
            .iter()
            .map(|(label, _)| label.len())
            .max()
            .unwrap_or(0);
        for (label, value) in &self.fields {
            let padding = " ".repeat(width - label.len());
            let label = out.dim(format!("{label}:"));
            let value = match cell_text(value) {
                text if text == "-" => out.dim(text),
                text => text,
            };
            writeln!(out, "{label}{padding}  {value}")?;
        }
        Ok(())
    }
}

/// A titled JSON value, pretty-printed below its title.
pub fn json_section(out: &mut Human<'_>, title: &str, value: &impl Serialize) -> io::Result<()> {
    let title = out.dim(format!("{title}:"));
    writeln!(out, "{title}")?;
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)
}

/// `text` cut to `width` columns, ending in an ellipsis.
fn shorten(text: &str, width: usize) -> String {
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
        let mut out = Vec::new();
        write(&mut Human::new(&mut out, style)).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn plain(write: impl FnOnce(&mut Human<'_>) -> io::Result<()>) -> String {
        text(Style::plain(), write)
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
    fn table_counts_rows_only_when_some_are_left_out() {
        let rows = || [["a".to_string()], ["b".to_string()]];
        for (total, footer) in [(2, ""), (3, "Showing 2 of 3\n")] {
            let table = Table::new(["NAME"]).rows(rows()).total(total);
            assert_eq!(
                plain(|out| table.write(out)),
                format!("NAME\na\nb\n{footer}")
            );
        }
        assert_eq!(
            plain(|out| Table::new(["NAME"]).total(0_usize).write(out)),
            "NAME\n"
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
        let narrow = Style {
            color: false,
            width: Some(36),
        };
        assert_eq!(
            text(narrow, |out| table().shrink("DESCRIPTION").write(out)),
            "NAME     DESCRIPTION      ID\n\
             weights  a rather long…   0123456789\n"
        );
        assert_eq!(
            text(narrow, |out| table().write(out)),
            plain(|out| table().write(out))
        );
        let wide = Style {
            color: false,
            width: Some(200),
        };
        assert_eq!(
            text(wide, |out| table().shrink("DESCRIPTION").write(out)),
            plain(|out| table().write(out))
        );
    }

    #[test]
    fn colors_apply_only_when_enabled() {
        let colored = Style {
            color: true,
            width: None,
        };
        assert!(text(colored, |out| table().write(out)).contains("\u{1b}["));
        assert!(!plain(|out| table().write(out)).contains("\u{1b}["));
    }

    #[test]
    fn details_align_values_and_mark_empty_ones() {
        let details = Details::new()
            .field("Name", "weights")
            .optional("Description", None::<&str>)
            .field("Created at", "today");
        assert_eq!(
            plain(|out| details.write(out)),
            "Name:         weights\n\
             Description:  -\n\
             Created at:   today\n"
        );
    }

    #[test]
    fn json_sections_are_pretty_printed() {
        assert_eq!(
            plain(|out| json_section(out, "Metadata", &serde_json::json!({"lr": 0.1}))),
            "Metadata:\n{\n  \"lr\": 0.1\n}\n"
        );
    }
}
