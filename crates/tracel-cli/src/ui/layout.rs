use std::fmt::{self, Display};

/// How a command shows on a terminal: a read in a box, a flow in a session.
pub enum Layout {
    /// A command that reads, its result in one box under its summary.
    Read,
    /// A command that does work, in a session that first says what it does, such as
    /// "Upload a version of model 'weights'", and where.
    Flow(String),
    /// A command whose output is a bare value or another program's, with nothing
    /// drawn.
    Bare,
}

impl Layout {
    pub fn flow(what: impl Into<String>) -> Self {
        Self::Flow(what.into())
    }
}

/// Where a command works: its project, and its environment unless that is production.
#[derive(Default)]
pub struct Place {
    pub project: Option<String>,
    pub environment: Option<String>,
}

impl Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<&str> = [&self.project, &self.environment]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        f.write_str(&parts.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_names_what_it_has() {
        let place = |project: Option<&str>, environment: Option<&str>| {
            Place {
                project: project.map(str::to_string),
                environment: environment.map(str::to_string),
            }
            .to_string()
        };
        assert_eq!(
            place(Some("test/demo"), Some("development")),
            "test/demo · development"
        );
        assert_eq!(place(Some("test/demo"), None), "test/demo");
        assert_eq!(place(None, Some("development")), "development");
        assert_eq!(place(None, None), "");
    }
}
