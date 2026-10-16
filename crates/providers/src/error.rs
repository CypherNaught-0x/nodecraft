use std::fmt;

/// What went wrong, in the terms every provider shares (CONTRIBUTING.md, "Adding a provider").
/// The kind decides whether the scheduler retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The provider's safety filter refused the prompt or an input.
    BlockedContent,
    /// The model can't take this input (format, size, resolution, duration).
    UnsupportedInput,
    /// The provider or server is overloaded or queue-full; try again later.
    Busy,
    /// Too many requests; try again later.
    RateLimited,
    /// The request or the job took too long.
    Timeout,
    /// The provider account is out of credit, or a NodeCraft budget blocked the call.
    Budget,
    /// The key is missing, wrong or lacks access to this model.
    Auth,
    /// The connection failed or dropped.
    Network,
    /// The provider failed on its side (HTTP 5xx, a failed job).
    Server,
    /// The provider replied with something that can't be read.
    Malformed,
    /// The job no longer exists on the provider.
    NotFound,
    /// The job was cancelled.
    Cancelled,
}

impl ErrorKind {
    /// Worth retrying with backoff (the scheduler does, under the same idempotency key).
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Busy | Self::RateLimited | Self::Timeout | Self::Network | Self::Server)
    }
}

/// A provider failure. The message is safe to show: it never contains keys or auth headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub message: String,
    /// The provider asked to wait this long before retrying (`Retry-After`).
    pub retry_after_ms: Option<u64>,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), retry_after_ms: None }
    }

    pub fn is_transient(&self) -> bool {
        self.kind.is_transient()
    }

    /// The error for an HTTP status from a provider API, with a short excerpt of the body.
    pub fn from_status(status: u16, body: &[u8]) -> Self {
        let kind = match status {
            400 | 413 | 415 | 422 => ErrorKind::UnsupportedInput,
            401 | 403 => ErrorKind::Auth,
            402 => ErrorKind::Budget,
            404 | 410 => ErrorKind::NotFound,
            408 | 504 => ErrorKind::Timeout,
            429 => ErrorKind::RateLimited,
            503 => ErrorKind::Busy,
            _ => ErrorKind::Server,
        };
        Self::new(kind, format!("HTTP {status}: {}", excerpt(body)))
    }
}

/// The start of a response body as text, for error messages.
pub fn excerpt(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body.get(..body.len().min(300)).unwrap_or_default());
    let text: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let text = text.trim();
    if text.is_empty() { "no details".into() } else { text.to_owned() }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            ErrorKind::BlockedContent => "blocked by the provider's content filter",
            ErrorKind::UnsupportedInput => "input not supported",
            ErrorKind::Busy => "provider busy",
            ErrorKind::RateLimited => "rate limited",
            ErrorKind::Timeout => "timed out",
            ErrorKind::Budget => "out of budget or credit",
            ErrorKind::Auth => "not authorised (check the API key)",
            ErrorKind::Network => "network error",
            ErrorKind::Server => "provider error",
            ErrorKind::Malformed => "unexpected reply",
            ErrorKind::NotFound => "job not found",
            ErrorKind::Cancelled => "cancelled",
        };
        write!(f, "{what}: {}", self.message)
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_and_transience() {
        let k = |s| ProviderError::from_status(s, b"").kind;
        assert_eq!(k(401), ErrorKind::Auth);
        assert_eq!(k(402), ErrorKind::Budget);
        assert_eq!(k(429), ErrorKind::RateLimited);
        assert_eq!(k(503), ErrorKind::Busy);
        assert_eq!(k(500), ErrorKind::Server);
        assert_eq!(k(422), ErrorKind::UnsupportedInput);
        assert!(k(429).is_transient() && k(500).is_transient() && k(504).is_transient());
        assert!(!k(401).is_transient() && !k(402).is_transient() && !k(422).is_transient());
        assert!(!ErrorKind::BlockedContent.is_transient());
    }

    #[test]
    fn excerpts_are_short_and_single_line() {
        assert_eq!(excerpt(b""), "no details");
        assert_eq!(excerpt(b"bad\r\nthing\x00"), "bad  thing");
        assert_eq!(excerpt(&[b'x'; 1000]).len(), 300);
        assert_eq!(excerpt(&[0xFF, 0xFE]), "\u{fffd}\u{fffd}");
        let e = ProviderError::from_status(429, br#"{"error":"slow down"}"#);
        assert_eq!(e.to_string(), r#"rate limited: HTTP 429: {"error":"slow down"}"#);
    }
}
