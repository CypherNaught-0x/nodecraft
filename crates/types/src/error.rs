use serde::{Deserialize, Serialize};

/// Stable categories shared by commands, storage and provider adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidInput,
    Unsupported,
    NotFound,
    Conflict,
    Overflow,
    Underflow,
    CurrencyMismatch,
    Authentication,
    Network,
    Timeout,
    Busy,
    Budget,
    ContentBlocked,
    /// The caller's role doesn't allow it (a view-only or comment-only share, #121).
    Forbidden,
    Internal,
    /// A newer peer supplied a category this build does not recognize; retain its message.
    #[serde(other)]
    Unknown,
}

/// An actionable error. Callers must use public-safe messages, never keys or raw responses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_error_kinds_preserve_the_actionable_message() {
        let error: Error = serde_json::from_str(r#"{"kind":"new_provider_error","message":"Retry after updating the provider configuration"}"#).unwrap();
        assert_eq!(error.kind, ErrorKind::Unknown);
        assert_eq!(error.to_string(), "Retry after updating the provider configuration");
        assert_eq!(serde_json::from_str::<Error>(&serde_json::to_string(&error).unwrap()).unwrap(), error);
    }
}
