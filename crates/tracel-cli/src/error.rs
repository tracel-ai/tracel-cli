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
            message: format!("{error:#}"),
            hint,
            exit_code: code.exit_code(),
        }
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
                | ClientCreationError::ApiKeyRefused(_) => ErrorKind::NotAuthenticated,
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
    fn exit_codes_and_code_strings_are_stable() {
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
            assert_eq!(kind.exit_code(), exit);
            assert_eq!(serde_json::to_value(kind).unwrap(), code);
        }
    }

    #[test]
    fn client_errors_survive_context() {
        for (error, kind) in [
            (ClientError::Unauthenticated, ErrorKind::NotAuthenticated),
            (ClientError::AppSessionEnded, ErrorKind::NotAuthenticated),
            (ClientError::CredentialNotAllowed, ErrorKind::Forbidden),
            (ClientError::NotFound, ErrorKind::NotFound),
            (
                ClientError::NotFoundWithCode(tracel_client::ApiErrorCode::Unknown),
                ErrorKind::NotFound,
            ),
            (ClientError::InternalServerError, ErrorKind::Unavailable),
            (
                ClientError::UnknownError("connection refused".into()),
                ErrorKind::Unavailable,
            ),
            (
                ClientError::SessionStore("unreadable".into()),
                ErrorKind::Internal,
            ),
        ] {
            assert_eq!(
                classify(&anyhow::Error::new(error).context("request failed")),
                kind
            );
        }
    }

    #[test]
    fn api_status_determines_kind() {
        for (status, kind) in [
            (400, ErrorKind::Internal),
            (401, ErrorKind::NotAuthenticated),
            (403, ErrorKind::Forbidden),
            (404, ErrorKind::NotFound),
            (409, ErrorKind::Conflict),
            (429, ErrorKind::Unavailable),
            (500, ErrorKind::Unavailable),
            (503, ErrorKind::Unavailable),
            (599, ErrorKind::Unavailable),
            (408, ErrorKind::Internal),
        ] {
            let error = ClientError::ApiError {
                status: status.try_into().unwrap(),
                body: Default::default(),
            };
            assert_eq!(
                classify(&anyhow::Error::new(error).context("request failed")),
                kind
            );
        }
    }

    #[test]
    fn server_limit_code_is_limit_reached() {
        let error = ClientError::ApiError {
            status: 403.try_into().unwrap(),
            body: tracel_client::error::ApiErrorBody {
                code: ApiErrorCode::LimitReached,
                message: "limit".into(),
            },
        };
        assert_eq!(
            classify(&anyhow::Error::new(error).context("request failed")),
            ErrorKind::LimitReached
        );
    }

    #[test]
    fn device_flow_errors_separate_outcomes_from_outages() {
        for (error, kind) in [
            (DeviceFlowError::AccessDenied, ErrorKind::NotAuthenticated),
            (DeviceFlowError::ExpiredToken, ErrorKind::NotAuthenticated),
            (
                DeviceFlowError::Client(ClientError::UnknownError("connection refused".into())),
                ErrorKind::Unavailable,
            ),
        ] {
            assert_eq!(
                classify(&anyhow::Error::new(error).context("login failed")),
                kind
            );
        }
    }

    #[test]
    fn creation_errors_keep_their_kind() {
        for (error, kind) in [
            (
                ClientCreationError::NoCredentials,
                ErrorKind::NotAuthenticated,
            ),
            (
                ClientCreationError::InvalidCredentials,
                ErrorKind::NotAuthenticated,
            ),
            (
                ClientCreationError::ApiKeyRefused(ClientError::CredentialNotAllowed),
                ErrorKind::NotAuthenticated,
            ),
            (
                ClientCreationError::ServerConnectionError("offline".into()),
                ErrorKind::Unavailable,
            ),
            (
                ClientCreationError::SessionStore(ClientError::SessionStore("unreadable".into())),
                ErrorKind::Internal,
            ),
        ] {
            assert_eq!(
                classify(&anyhow::Error::new(error).context("client failed")),
                kind
            );
        }
    }

    #[test]
    fn explicit_kind_and_hint_survive_context() {
        let error = anyhow::Error::new(ClientError::NotFound)
            .context(
                CliError::new(ErrorKind::LimitReached, "limit reached")
                    .with_hint("Try again later."),
            )
            .context("operation failed");
        let report = ErrorReport::new(&error);
        assert_eq!(report.code, ErrorKind::LimitReached);
        assert_eq!(report.hint, Some("Try again later."));
        assert_eq!(report.exit_code, 8);
        assert_eq!(
            classify(&anyhow::anyhow!("unclassified")),
            ErrorKind::Internal
        );
    }
}
