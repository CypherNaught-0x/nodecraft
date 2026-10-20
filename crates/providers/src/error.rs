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
    /// The provider ran the job and it failed (a workflow error, a model that refused). Not
    /// retried automatically: running it again would usually fail the same way, and may cost.
    JobFailed,
    /// The job ran past its deadline. Not retried: it may still be running (and billing) on the
    /// provider, so the job's handle should be resumed or cancelled, never resubmitted.
    Expired,
}

impl ErrorKind {
    /// Every kind, for lookups.
    pub const ALL: [Self; 14] = [
        Self::BlockedContent,
        Self::UnsupportedInput,
        Self::Busy,
        Self::RateLimited,
        Self::Timeout,
        Self::Budget,
        Self::Auth,
        Self::Network,
        Self::Server,
        Self::Malformed,
        Self::NotFound,
        Self::Cancelled,
        Self::JobFailed,
        Self::Expired,
    ];

    /// What the error's text starts with ([`ProviderError`]'s `Display` is `"{label}: {message}"`).
    pub fn label(self) -> &'static str {
        match self {
            Self::BlockedContent => "blocked by the provider's content filter",
            Self::UnsupportedInput => "input not supported",
            Self::Busy => "provider busy",
            Self::RateLimited => "rate limited",
            Self::Timeout => "timed out",
            Self::Budget => "out of budget or credit",
            Self::Auth => "not authorised (check the API key)",
            Self::Network => "network error",
            Self::Server => "provider error",
            Self::Malformed => "unexpected reply",
            Self::NotFound => "job not found",
            Self::Cancelled => "cancelled",
            Self::Expired => "took too long",
            Self::JobFailed => "the job failed",
        }
    }

    /// The kind of a [`ProviderError`] from its displayed text, for hosts that only kept the
    /// text (a scheduler failure carries a string).
    pub fn from_error_text(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| text.strip_prefix(kind.label()).is_some_and(|rest| rest.starts_with(": ")))
    }

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
            // A redirect that wasn't followed (too many hops, or no usable Location).
            300..=399 => ErrorKind::Malformed,
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
        let what = self.kind.label();
        write!(f, "{what}: {}", self.message)
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayed_errors_name_their_kind() {
        for kind in ErrorKind::ALL {
            assert_eq!(ErrorKind::from_error_text(&ProviderError::new(kind, "x: y").to_string()), Some(kind), "{kind:?}");
        }
        assert_eq!(ErrorKind::from_error_text("cancelledness"), None);
        assert_eq!(ErrorKind::from_error_text(""), None);
    }

    #[test]
    fn status_mapping_and_transience() {
        let k = |s| ProviderError::from_status(s, b"").kind;
        assert_eq!(k(401), ErrorKind::Auth);
        assert_eq!(k(402), ErrorKind::Budget);
        assert_eq!(k(429), ErrorKind::RateLimited);
        assert_eq!(k(503), ErrorKind::Busy);
        assert_eq!(k(500), ErrorKind::Server);
        assert_eq!(k(422), ErrorKind::UnsupportedInput);
        assert_eq!(k(302), ErrorKind::Malformed);
        assert!(!k(302).is_transient() && !ErrorKind::Expired.is_transient());
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
