//! AWS credentials and Signature Version 4 signing over NodeCraft's own [`crate::Http`] (#1163).
//!
//! [`AwsCredentials`] is an access key ID, a secret access key and, for temporary credentials,
//! a session token and its expiry, as the caller read them from its secret store: nothing here
//! reads AWS environment variables, profiles or instance metadata. [`SigV4Http`] is a transport
//! that signs every request with them just before the real transport sends it, so any dialect
//! spoken over [`crate::Http`] (Bedrock's OpenAI-compatible route and Converse) is signed the
//! same way, whole or streamed. The algorithm is in `sigv4`, pure Rust, so it builds for the
//! browser too; it passes AWS's published SigV4 test suite (`fixtures/sigv4`).
//!
//! Nothing secret is ever printed: `Debug` shows no key, errors never quote one, and an error
//! reply that echoes the session token has it removed before anyone reads it.

mod signed;
pub(crate) mod sigv4;

pub use signed::SigV4Http;

use crate::{ErrorKind, ProviderError};

/// Longest access key ID AWS issues (`AccessKey.AccessKeyId`: up to 128 word characters; real
/// ones are 20, AWS's documentation examples shorter).
const ACCESS_KEY_ID_LEN: std::ops::RangeInclusive<usize> = 1..=128;
/// Longest secret or session token accepted (the keychain's own limit).
const MAX_SECRET: usize = crate::secrets::MAX_SECRET_BYTES;
/// Credentials this close to their expiry are treated as expired: a request signed now must
/// still be valid when AWS reads it.
pub const EXPIRY_MARGIN_S: u64 = 30;

/// An AWS access key ID and secret access key, with the session token and expiry of temporary
/// credentials. `Debug` shows none of them.
#[derive(Clone, PartialEq, Eq)]
pub struct AwsCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    /// Unix seconds after which AWS refuses them.
    expires_at: Option<u64>,
}

impl std::fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsCredentials").field("session_token", &self.session_token.is_some()).field("expires_at", &self.expires_at).finish_non_exhaustive()
    }
}

fn visible(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_SECRET && text.bytes().all(|b| b.is_ascii_graphic())
}

impl AwsCredentials {
    /// Checks the shapes AWS issues; errors never echo a value.
    pub fn new(access_key_id: &str, secret_access_key: &str, session_token: Option<&str>) -> Result<Self, ProviderError> {
        let access_key_id = access_key_id.trim();
        if !ACCESS_KEY_ID_LEN.contains(&access_key_id.len()) || !access_key_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(ProviderError::new(ErrorKind::Auth, "the AWS access key ID isn't one AWS issues (at most 128 letters and digits, such as AKIA…)"));
        }
        let secret_access_key = secret_access_key.trim();
        if !visible(secret_access_key) {
            return Err(ProviderError::new(ErrorKind::Auth, "the AWS secret access key is empty or holds characters AWS keys never do"));
        }
        let session_token = match session_token.map(str::trim) {
            None => None,
            Some(token) if visible(token) => Some(token.to_owned()),
            Some(_) => return Err(ProviderError::new(ErrorKind::Auth, "the AWS session token is empty or holds characters AWS tokens never do")),
        };
        Ok(Self { access_key_id: access_key_id.to_owned(), secret_access_key: secret_access_key.to_owned(), session_token, expires_at: None })
    }

    /// From keychain secrets.
    pub fn from_secrets(
        access_key_id: &crate::secrets::Secret,
        secret_access_key: &crate::secrets::Secret,
        session_token: Option<&crate::secrets::Secret>,
    ) -> Result<Self, ProviderError> {
        Self::new(access_key_id.expose(), secret_access_key.expose(), session_token.map(crate::secrets::Secret::expose))
    }

    /// The same credentials, refused once Unix second `expires_at` is near.
    pub fn with_expiry(mut self, expires_at: u64) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn expires_at(&self) -> Option<u64> {
        self.expires_at
    }

    pub fn has_session_token(&self) -> bool {
        self.session_token.is_some()
    }

    /// An Authentication error when they have expired (or will within [`EXPIRY_MARGIN_S`]) at
    /// Unix second `now`.
    pub fn check_fresh(&self, now: u64) -> Result<(), ProviderError> {
        match self.expires_at {
            Some(at) if now.saturating_add(EXPIRY_MARGIN_S) >= at => Err(ProviderError::new(
                ErrorKind::Auth,
                "the temporary AWS credentials have expired; store a fresh session token, access key ID, secret access key and expiry",
            )),
            _ => Ok(()),
        }
    }

    pub(crate) fn access_key_id(&self) -> &str {
        &self.access_key_id
    }

    pub(crate) fn secret_access_key(&self) -> &str {
        &self.secret_access_key
    }

    pub(crate) fn session_token(&self) -> Option<&str> {
        self.session_token.as_deref()
    }
}

/// Seconds since the Unix epoch on the wall clock (the browser's too); `None` before 1970.
pub fn system_seconds() -> Option<u64> {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn number(text: &str, digits: usize) -> Option<i64> {
    (text.len() == digits && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse().ok()).flatten()
}

/// Unix seconds of an RFC 3339 time such as `2026-11-08T20:00:00Z` or
/// `2026-11-08T22:00:00.123+02:00` (the `Expiration` AWS tools print). Years 1970–9999.
pub fn parse_expiry(text: &str) -> Result<u64, ProviderError> {
    let bad = || ProviderError::new(ErrorKind::UnsupportedInput, "the AWS credential expiry isn't an RFC 3339 time such as 2026-11-08T20:00:00Z");
    let text = text.trim();
    let date = text.get(..10).ok_or_else(bad)?;
    let sep = text.get(10..11).ok_or_else(bad)?;
    let time = text.get(11..19).ok_or_else(bad)?;
    let mut rest = text.get(19..).ok_or_else(bad)?;
    if !matches!(sep, "T" | "t" | " ")
        || date.get(4..5) != Some("-")
        || date.get(7..8) != Some("-")
        || time.get(2..3) != Some(":")
        || time.get(5..6) != Some(":")
    {
        return Err(bad());
    }
    let field = |s: &str, from: usize, digits: usize| s.get(from..from.saturating_add(digits)).and_then(|f| number(f, digits)).ok_or_else(bad);
    let (year, month, day) = (field(date, 0, 4)?, field(date, 5, 2)?, field(date, 8, 2)?);
    let (hour, minute, second) = (field(time, 0, 2)?, field(time, 3, 2)?, field(time, 6, 2)?);
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err(bad());
        }
        rest = fraction.get(digits..).ok_or_else(bad)?;
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.get(..1) {
                Some("+") => 1,
                Some("-") => -1,
                _ => return Err(bad()),
            };
            if rest.len() != 6 || rest.get(3..4) != Some(":") {
                return Err(bad());
            }
            let (h, m) = (field(rest, 1, 2)?, field(rest, 4, 2)?);
            if h > 23 || m > 59 {
                return Err(bad());
            }
            sign * (h * 3_600 + m * 60)
        }
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(bad()),
    };
    if !(1970..=9999).contains(&year) || !(1..=month_days).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return Err(bad());
    }
    let days = sigv4::days_from_civil(year, month, day);
    let seconds = days.saturating_mul(86_400).saturating_add(hour * 3_600 + minute * 60 + second).saturating_sub(offset);
    u64::try_from(seconds).map_err(|_| bad())
}

#[cfg(test)]
mod tests;
