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
    use rstest::rstest;

    use super::*;

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
    fn given_a_client_error_under_context_when_classifying_then_the_client_error_decides() {
        let error = anyhow::Error::new(ClientError::NotFound).context("Can't read model 'weights'");

        assert_eq!(classify(&error), ErrorKind::NotFound);
    }

    #[test]
    fn given_a_cli_error_under_context_when_reporting_then_its_kind_and_hint_are_kept() {
        let error = anyhow::Error::new(ClientError::NotFound)
            .context(CliError::new(ErrorKind::LimitReached, "Limit reached.").with_hint("Wait."))
            .context("Can't upload");

        let report = ErrorReport::new(&error);

        assert_eq!(report.code, ErrorKind::LimitReached);
        assert_eq!(report.hint, Some("Wait."));
    }

    #[test]
    fn given_a_limit_reached_error_with_a_forbidden_status_when_classifying_then_the_limit_decides()
    {
        let error = anyhow::Error::new(api_error(403, ApiErrorCode::LimitReached));

        assert_eq!(classify(&error), ErrorKind::LimitReached);
    }

    #[rstest]
    #[case::no_response(ClientError::UnknownError("connection refused".into()))]
    #[case::server_error(ClientError::InternalServerError)]
    #[case::too_many_requests(api_error(429, ApiErrorCode::Unknown))]
    fn given_a_console_that_cannot_answer_when_classifying_then_it_is_unavailable(
        #[case] client_error: ClientError,
    ) {
        let error = anyhow::Error::new(client_error);

        assert_eq!(classify(&error), ErrorKind::Unavailable);
    }

    #[test]
    fn given_a_login_that_failed_on_a_client_error_when_classifying_then_the_client_error_decides()
    {
        let error = anyhow::Error::new(DeviceFlowError::Client(ClientError::InternalServerError));

        assert_eq!(classify(&error), ErrorKind::Unavailable);
    }

    #[test]
    fn given_an_error_nothing_classifies_when_classifying_then_it_is_internal() {
        let error = anyhow::anyhow!("unexpected");

        assert_eq!(classify(&error), ErrorKind::Internal);
    }

    #[rstest]
    #[case::sentence(anyhow::anyhow!("No model 'w'."), "No model 'w'.")]
    #[case::question(anyhow::anyhow!("Ready?"), "Ready?")]
    #[case::chain(
        anyhow::anyhow!("No such file").context("Can't read 'a'"),
        "Can't read 'a': No such file."
    )]
    #[case::trailing_space(anyhow::anyhow!("Can't read 'a'  "), "Can't read 'a'.")]
    fn given_an_error_when_reporting_it_then_its_message_ends_as_a_sentence(
        #[case] error: anyhow::Error,
        #[case] message: &str,
    ) {
        let report = ErrorReport::new(&error);

        assert_eq!(report.message, message);
    }
}
