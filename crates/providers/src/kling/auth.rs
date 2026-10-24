//! The two kinds of Kling credential: an API key, sent as is, or an access key and secret key
//! that sign an HS256 token (issuer the access key, valid from five seconds ago for 30 minutes).
use super::*;
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// How long a signed token is valid, in seconds.
const LIFETIME: u64 = 1_800;
/// How far back a token's start is set, for clocks a little ahead of Kling's.
const SKEW: u64 = 5;

pub(super) enum Key<'k> {
    Api(&'k str),
    Signed { access: &'k str, secret: &'k str },
}
impl<'k> Key<'k> {
    pub(super) fn parse(key: &'k Secret) -> Result<Self, ProviderError> {
        let key = key.expose();
        match key.split_once(':') {
            None => Ok(Self::Api(key)),
            Some((access, secret)) if !access.is_empty() && !secret.is_empty() && !secret.contains(':') => Ok(Self::Signed { access, secret }),
            Some(_) => Err(ProviderError::new(ErrorKind::Auth, "a Kling key is an API key, or the access key and secret key joined by a colon")),
        }
    }
    /// The bearer token for one request.
    pub(super) fn bearer(&self, clock: fn() -> Option<u64>) -> Result<String, ProviderError> {
        match self {
            Self::Api(key) => Ok((*key).to_owned()),
            Self::Signed { access, secret } => {
                let now = clock().ok_or_else(|| ProviderError::new(ErrorKind::Auth, "the system clock is unset, so a Kling token can't be signed"))?;
                token(access, secret, now)
            }
        }
    }
    /// Whether `text` holds the key or any long enough part of it, so it is never shown.
    pub(super) fn appears_in(&self, text: &str) -> bool {
        let parts: &[&str] = match self {
            Self::Api(key) => &[key],
            Self::Signed { access, secret } => &[access, secret],
        };
        parts.iter().any(|part| part.len() >= 8 && text.contains(part))
    }
}

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
pub(super) fn token(access: &str, secret: &str, now: u64) -> Result<String, ProviderError> {
    let header = encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = serde_json::json!({"iss": access, "exp": now.saturating_add(LIFETIME), "nbf": now.saturating_sub(SKEW)});
    let signed = format!("{header}.{}", encode(claims.to_string().as_bytes()));
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| ProviderError::new(ErrorKind::Auth, "the Kling secret key can't sign a token"))?;
    mac.update(signed.as_bytes());
    Ok(format!("{signed}.{}", encode(&mac.finalize().into_bytes())))
}
