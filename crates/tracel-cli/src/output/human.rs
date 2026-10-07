use std::fmt::Display;
use std::io::{self, Write};

use serde::Serialize;

/// Columns aligned under their headers, one row per line.
pub struct Table<const N: usize> {
    headers: [&'static str; N],
    rows: Vec<[String; N]>,
    total: Option<u64>,
}

impl<const N: usize> Table<N> {
    pub fn new(headers: [&'static str; N]) -> Self {
        Self {
            headers,
            rows: Vec::new(),
            total: None,
        }
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = [String; N]>) -> Self {
        self.rows.extend(rows);
        self
    }

    /// Ends the table with "Showing N of TOTAL" when it holds fewer rows than exist.
    pub fn total(mut self, total: impl TryInto<u64>) -> Self {
        self.total = total.try_into().ok();
        self
    }

    pub fn write(&self, out: &mut dyn Write) -> io::Result<()> {
        let header = self.headers.map(String::from);
        let lines: Vec<[String; N]> = std::iter::once(&header)
            .chain(&self.rows)
            .map(|row| row.clone().map(|cell| cell_text(&cell)))
            .collect();
        let widths: [usize; N] = std::array::from_fn(|column| {
            lines
                .iter()
                .map(|line| console::measure_text_width(&line[column]))
                .max()
                .unwrap_or(0)
        });
        for line in &lines {
            for (column, cell) in line.iter().enumerate() {
                write!(out, "{cell}")?;
                if column + 1 < N {
                    let padding = widths[column] - console::measure_text_width(cell);
                    write!(out, "{}  ", " ".repeat(padding))?;
                }
            }
            writeln!(out)?;
        }
        let shown = self.rows.len() as u64;
        match self.total {
            Some(total) if shown < total => writeln!(out, "Showing {shown} of {total}"),
            _ => Ok(()),
        }
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

    pub fn write(&self, out: &mut dyn Write) -> io::Result<()> {
        let width = self
            .fields
            .iter()
            .map(|(label, _)| label.len())
            .max()
            .unwrap_or(0);
        for (label, value) in &self.fields {
            let padding = " ".repeat(width - label.len());
            writeln!(out, "{label}:{padding}  {}", cell_text(value))?;
        }
        Ok(())
    }
}

/// A titled JSON value, pretty-printed below its title.
pub fn json_section(out: &mut dyn Write, title: &str, value: &impl Serialize) -> io::Result<()> {
    writeln!(out, "{title}:")?;
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)
}

/// A value on one line, with "-" standing in for nothing.
fn cell_text(value: &str) -> String {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() { "-".into() } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(write: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn table_aligns_columns_and_keeps_cells_on_one_line() {
        let table = Table::new(["NAME", "DESCRIPTION", "ID"]).rows([
            ["weights".into(), "two\nlines".into(), "1".into()],
            ["w".into(), String::new(), "22".into()],
        ]);
        assert_eq!(
            text(|out| table.write(out)),
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
                text(|out| table.write(out)),
                format!("NAME\na\nb\n{footer}")
            );
        }
        assert_eq!(
            text(|out| Table::new(["NAME"]).total(0_usize).write(out)),
            "NAME\n"
        );
    }

    #[test]
    fn details_align_values_and_mark_empty_ones() {
        let details = Details::new()
            .field("Name", "weights")
            .optional("Description", None::<&str>)
            .field("Created at", "today");
        assert_eq!(
            text(|out| details.write(out)),
            "Name:         weights\n\
             Description:  -\n\
             Created at:   today\n"
        );
    }

    #[test]
    fn json_sections_are_pretty_printed() {
        assert_eq!(
            text(|out| json_section(out, "Metadata", &serde_json::json!({"lr": 0.1}))),
            "Metadata:\n{\n  \"lr\": 0.1\n}\n"
        );
    }
}
