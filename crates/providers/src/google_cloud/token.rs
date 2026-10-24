//! Access tokens for a service account: a signed JWT exchanged at Google's token endpoint, kept
//! until five minutes before it expires. The exchange is free; a refused one is an `Auth` error.
use super::credential::{ServiceAccount, TOKEN_URI};
use crate::{ErrorKind, Http, HttpMethod, HttpRequest, ProviderError};
use serde::Deserialize;
use std::sync::{Arc, Mutex, PoisonError};

/// Seconds since the Unix epoch, or `None` when the clock can't be read.
pub type Clock = Arc<dyn Fn() -> Option<u64> + Send + Sync>;
/// A token is replaced this long before it expires, so a request never starts with one about to
/// lapse.
const MARGIN_S: u64 = 300;
/// Google's access tokens are well under this.
const MAX_TOKEN_BYTES: usize = 4096;

/// The system's wall clock (the browser's in the web build).
pub fn system_clock() -> Clock {
    Arc::new(|| web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).ok().map(|d| d.as_secs()))
}

#[derive(Deserialize)]
struct Granted {
    access_token: String,
    expires_in: u64,
    token_type: String,
}
#[derive(Deserialize)]
struct Refused {
    error: Option<String>,
}

/// A token and when (Unix seconds) it stops being used.
struct Held {
    token: String,
    until_s: u64,
}

pub struct Tokens {
    account: ServiceAccount,
    http: Arc<dyn Http>,
    clock: Clock,
    held: Mutex<Option<Held>>,
}

impl Tokens {
    pub fn new(account: ServiceAccount, http: Arc<dyn Http>, clock: Clock) -> Self {
        Self { account, http, clock, held: Mutex::new(None) }
    }
    pub fn account(&self) -> &ServiceAccount {
        &self.account
    }

    /// An access token for the `Authorization: Bearer` header.
    pub async fn bearer(&self) -> Result<String, ProviderError> {
        let now =
            (self.clock)().ok_or_else(|| ProviderError::new(ErrorKind::Auth, "the system clock can't be read, so a Google Cloud token can't be requested"))?;
        if let Some(held) = self.held.lock().unwrap_or_else(PoisonError::into_inner).as_ref().filter(|h| now < h.until_s) {
            return Ok(held.token.clone());
        }
        let assertion = self.account.assertion(now)?;
        // The assertion is base64url and dots: nothing in it needs escaping in a form body.
        let body = format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}");
        let request = HttpRequest { method: HttpMethod::Post, body: body.into_bytes(), ..HttpRequest::get(TOKEN_URI) }
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", crate::USER_AGENT)
            .timeout_ms(30_000)
            .max_response_bytes(64 * 1024);
        let reply =
            self.http.send_no_redirect(request).await.map_err(|e| ProviderError { message: "Google's token service couldn't be reached".into(), ..e })?;
        if !reply.is_success() {
            return Err(refused(reply.status, &reply.body));
        }
        let granted: Granted =
            serde_json::from_slice(&reply.body).map_err(|_| ProviderError::new(ErrorKind::Malformed, "Google's token reply can't be read"))?;
        let token = granted.access_token;
        if !granted.token_type.eq_ignore_ascii_case("bearer")
            || token.is_empty()
            || token.len() > MAX_TOKEN_BYTES
            || !token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(ProviderError::new(ErrorKind::Malformed, "Google's token reply holds no usable token"));
        }
        // Held for its lifetime less the margin; one too short to be worth keeping serves this
        // request only.
        let until_s = now.saturating_add(granted.expires_in.min(86_400)).saturating_sub(MARGIN_S);
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(Held { token: token.clone(), until_s });
        Ok(token)
    }

    /// Drops the held token, after the API refused it.
    pub fn forget(&self) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// What a refused exchange means, without quoting the reply (it may echo the request).
fn refused(status: u16, body: &[u8]) -> ProviderError {
    let error = serde_json::from_slice::<Refused>(body).ok().and_then(|r| r.error);
    let message = match (status, error.as_deref()) {
        (_, Some("invalid_grant")) => {
            "Google refused the service account's token request: check that its key hasn't been deleted or disabled and that this computer's clock is right"
        }
        (_, Some("invalid_client" | "unauthorized_client")) => "Google refused the service account: check that it still exists and is enabled",
        (429, _) => "Google's token service is rate limiting; try again shortly",
        (500..=599, _) => "Google's token service failed; try again shortly",
        _ => "Google refused the service account's token request",
    };
    let kind = match status {
        429 => ErrorKind::RateLimited,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::Auth,
    };
    ProviderError::new(kind, format!("{message} (HTTP {status})"))
}
