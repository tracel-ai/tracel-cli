use serde::Serialize;
use tracel_client::console::auth::DeviceFlowError;
use tracel_client::{ApiErrorCode, ClientError};

use crate::context::ClientCreationError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorKind {
    Internal,
    Usage,
    NotAuthenticated,
    Forbidden,
    NotFound,
    Conflict,
    ConfirmationRequired,
    LimitReached,
    Timeout,
    Unavailable,
}

impl ErrorKind {
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Internal => 1,
            Self::Usage => 2,
            Self::NotAuthenticated => 3,
            Self::Forbidden => 4,
            Self::NotFound => 5,
            Self::Conflict => 6,
            Self::ConfirmationRequired => 7,
            Self::LimitReached => 8,
            Self::Timeout => 10,
            Self::Unavailable => 11,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CliError {
    pub kind: ErrorKind,
    message: String,
    hint: Option<String>,
}

impl CliError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[derive(Serialize)]
pub struct ErrorReport<'a> {
    code: ErrorKind,
    pub message: String,
    pub hint: Option<&'a str>,
    pub exit_code: i32,
}

impl<'a> ErrorReport<'a> {
    pub fn new(error: &'a anyhow::Error) -> Self {
        let code = classify(error);
        let hint = explicit_error(error).and_then(|error| error.hint.as_deref());
        Self {
            code,
            message: sentence(format!("{error:#}")),
            hint,
            exit_code: code.exit_code(),
        }
    }
}

/// End `text` with a period unless it already ends a sentence. An error chain such as
/// "Can't read 'a': No such file or directory (os error 2)" ends without one.
fn sentence(text: String) -> String {
    let text = text.trim_end();
    if text.ends_with(['.', '!', '?']) {
        text.to_string()
    } else {
        format!("{text}.")
    }
}

pub fn classify(error: &anyhow::Error) -> ErrorKind {
    if let Some(error) = explicit_error(error) {
        return error.kind;
    }

    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<ClientCreationError>() {
            return match error {
                ClientCreationError::NoCredentials
                | ClientCreationError::InvalidCredentials
                | ClientCreationError::ApiKeyRefused => ErrorKind::NotAuthenticated,
                ClientCreationError::ServerConnectionError(_) => ErrorKind::Unavailable,
                ClientCreationError::SessionStore(error) => classify_client(error),
            };
        }
        if let Some(error) = cause.downcast_ref::<ClientError>() {
            return classify_client(error);
        }
        if let Some(error) = cause.downcast_ref::<DeviceFlowError>() {
            return match error {
                DeviceFlowError::Client(error) => classify_client(error),
                _ => ErrorKind::NotAuthenticated,
            };
        }
    }
    ErrorKind::Internal
}

fn explicit_error(error: &anyhow::Error) -> Option<&CliError> {
    error.downcast_ref::<CliError>().or_else(|| {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<CliError>())
    })
}

fn classify_client(error: &ClientError) -> ErrorKind {
    match error {
        ClientError::Unauthenticated | ClientError::AppSessionEnded => ErrorKind::NotAuthenticated,
        ClientError::CredentialNotAllowed => ErrorKind::Forbidden,
        ClientError::NotFound | ClientError::NotFoundWithCode(_) => ErrorKind::NotFound,
        ClientError::InternalServerError => ErrorKind::Unavailable,
        ClientError::ApiError { body, .. } if matches!(body.code, ApiErrorCode::LimitReached) => {
            ErrorKind::LimitReached
        }
        ClientError::ApiError { status, .. } => match status.as_u16() {
            400 | 422 => ErrorKind::Usage,
            401 => ErrorKind::NotAuthenticated,
            403 => ErrorKind::Forbidden,
            404 => ErrorKind::NotFound,
            409 => ErrorKind::Conflict,
            429 | 500..=599 => ErrorKind::Unavailable,
            _ => ErrorKind::Internal,
        },
        // A request that never got a response, such as a refused connection.
        ClientError::UnknownError(_) => ErrorKind::Unavailable,
        _ => ErrorKind::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_keeps_its_documented_code_and_exit_code() {
        for (kind, code, exit) in [
            (ErrorKind::Internal, "INTERNAL", 1),
            (ErrorKind::Usage, "USAGE", 2),
            (ErrorKind::NotAuthenticated, "NOT_AUTHENTICATED", 3),
            (ErrorKind::Forbidden, "FORBIDDEN", 4),
            (ErrorKind::NotFound, "NOT_FOUND", 5),
            (ErrorKind::Conflict, "CONFLICT", 6),
            (ErrorKind::ConfirmationRequired, "CONFIRMATION_REQUIRED", 7),
            (ErrorKind::LimitReached, "LIMIT_REACHED", 8),
            (ErrorKind::Timeout, "TIMEOUT", 10),
            (ErrorKind::Unavailable, "UNAVAILABLE", 11),
        ] {
            assert_eq!(kind.exit_code(), exit, "{kind:?}");
            assert_eq!(serde_json::to_value(kind).unwrap(), code, "{kind:?}");
        }
    }

    fn api_error(status: u16, code: ApiErrorCode) -> ClientError {
        ClientError::ApiError {
            status: status.try_into().unwrap(),
            body: tracel_client::error::ApiErrorBody {
                code,
                message: "from the server".into(),
            },
        }
    }

    #[test]
    fn an_error_takes_the_kind_of_its_cause_under_any_context() {
        let console = url::Url::parse("http://localhost:9001/").unwrap();
        let unreachable = || ClientError::UnknownError("connection refused".into());
        for (error, kind) in [
            (
                ClientError::Unauthenticated.into(),
                ErrorKind::NotAuthenticated,
            ),
            (
                ClientError::AppSessionEnded.into(),
                ErrorKind::NotAuthenticated,
            ),
            (
                ClientError::CredentialNotAllowed.into(),
                ErrorKind::Forbidden,
            ),
            (ClientError::NotFound.into(), ErrorKind::NotFound),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Unknown).into(),
                ErrorKind::NotFound,
            ),
            (
                ClientError::InternalServerError.into(),
                ErrorKind::Unavailable,
            ),
            (unreachable().into(), ErrorKind::Unavailable),
            (
                ClientError::SessionStore("unreadable".into()).into(),
                ErrorKind::Internal,
            ),
            (
                api_error(400, ApiErrorCode::Unknown).into(),
                ErrorKind::Usage,
            ),
            (
                api_error(422, ApiErrorCode::Unknown).into(),
                ErrorKind::Usage,
            ),
            (
                api_error(401, ApiErrorCode::Unknown).into(),
                ErrorKind::NotAuthenticated,
            ),
            (
                api_error(403, ApiErrorCode::Unknown).into(),
                ErrorKind::Forbidden,
            ),
            (
                api_error(404, ApiErrorCode::Unknown).into(),
                ErrorKind::NotFound,
            ),
            (
                api_error(409, ApiErrorCode::Unknown).into(),
                ErrorKind::Conflict,
            ),
            (
                api_error(429, ApiErrorCode::Unknown).into(),
                ErrorKind::Unavailable,
            ),
            (
                api_error(503, ApiErrorCode::Unknown).into(),
                ErrorKind::Unavailable,
            ),
            (
                api_error(408, ApiErrorCode::Unknown).into(),
                ErrorKind::Internal,
            ),
            (
                api_error(400, ApiErrorCode::LimitReached).into(),
                ErrorKind::LimitReached,
            ),
            (
                api_error(403, ApiErrorCode::LimitReached).into(),
                ErrorKind::LimitReached,
            ),
            (
                DeviceFlowError::AccessDenied.into(),
                ErrorKind::NotAuthenticated,
            ),
            (
                DeviceFlowError::ExpiredToken.into(),
                ErrorKind::NotAuthenticated,
            ),
            (
                DeviceFlowError::Client(unreachable()).into(),
                ErrorKind::Unavailable,
            ),
            (
                ClientCreationError::NoCredentials.into(),
                ErrorKind::NotAuthenticated,
            ),
            (
                ClientCreationError::InvalidCredentials.into(),
                ErrorKind::NotAuthenticated,
            ),
            (
                ClientCreationError::ApiKeyRefused.into(),
                ErrorKind::NotAuthenticated,
            ),
            (
                ClientCreationError::ServerConnectionError(console.clone()).into(),
                ErrorKind::Unavailable,
            ),
            (
                ClientCreationError::SessionStore(ClientError::SessionStore("x".into())).into(),
                ErrorKind::Internal,
            ),
            (anyhow::anyhow!("unclassified"), ErrorKind::Internal),
        ] {
            let error: anyhow::Error = error;
            let shown = format!("{error:?}");
            assert_eq!(classify(&error.context("Can't do it")), kind, "{shown}");
        }
    }

    #[test]
    fn a_cli_error_keeps_its_kind_and_hint_under_context() {
        let error = anyhow::Error::new(ClientError::NotFound)
            .context(CliError::new(ErrorKind::LimitReached, "Limit reached.").with_hint("Wait."))
            .context("Can't do it");

        let report = ErrorReport::new(&error);

        assert_eq!(report.code, ErrorKind::LimitReached);
        assert_eq!(report.hint, Some("Wait."));
        assert_eq!(report.exit_code, 8);
    }

    #[test]
    fn a_reported_message_ends_as_a_sentence() {
        for (error, expected) in [
            (anyhow::anyhow!("No model 'w'."), "No model 'w'."),
            (
                anyhow::anyhow!("No such file").context("Can't read 'a'"),
                "Can't read 'a': No such file.",
            ),
            (anyhow::anyhow!("Ready?"), "Ready?"),
            (anyhow::anyhow!("trailing space  "), "trailing space."),
        ] {
            assert_eq!(ErrorReport::new(&error).message, expected);
        }
    }
}
