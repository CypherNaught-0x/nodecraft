//! `keys.test` (#237): checks a stored key with a request that costs nothing.
//!
//! Each provider NodeCraft knows a free check for gets one GET on its own API: a model list, or an
//! account or balance endpoint. The answer is classified here, from the HTTP status alone, so it
//! doesn't depend on any adapter's or client library's error mapping:
//! - **verified**: the endpoint needs a valid key, and it answered;
//! - **unverifiable**: the provider has no free check (or, like Ollama and LM Studio, takes no key
//!   at all), and nothing is sent;
//! - **rejected**: 401 (or Google's `API_KEY_INVALID`), an error of kind `Authentication`; a 403
//!   is too, worded as rejected or not allowed, since a valid key can lack access.
//!
//! The key goes to the endpoint's origin only: it is a secret header ([`HttpRequest::secret_header`])
//! and redirects are never followed ([`Http::send_no_redirect`]). It comes from the key store alone,
//! never the environment, and is never logged or put in an error. Reply bodies are never quoted,
//! since some providers echo part of a rejected key.

use std::sync::Arc;

use nodecraft_exec::driver::Runtime;
use nodecraft_providers::http::{Http, HttpRequest, HttpResponse};
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

use crate::secrets::{Account, Secret};

/// How long one check may take.
pub const TEST_TIMEOUT_MS: u64 = 20_000;
/// The largest reply read (OpenRouter's whole model list fits).
const MAX_REPLY_BYTES: usize = 8 * 1024 * 1024;

/// What a check found out about a key that wasn't rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    /// The provider accepted the key.
    Verified,
    /// The key can't be confirmed this way; why, in words for a person.
    Unverifiable(&'static str),
}

impl KeyCheck {
    /// The `keys.test` result: `{result: "verified"}` or `{result: "unverifiable", reason}`.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Verified => json!({"result": "verified"}),
            Self::Unverifiable(reason) => json!({"result": "unverifiable", "reason": reason}),
        }
    }
}

/// Where a host runs checks: an async runtime and the HTTP client runs use. The desktop app hands
/// its run host's to the keychain worker ([`crate::secrets::KeyWorker::spawn_with_tests`]).
#[derive(Clone)]
pub struct KeyTestHost {
    pub runtime: Arc<dyn Runtime>,
    pub http: Arc<dyn Http>,
}

/// A free request that needs the key.
struct Probe {
    /// The provider's name, for messages.
    provider: &'static str,
    url: &'static str,
    /// The header carrying the key, and what comes before the key in it.
    header: &'static str,
    scheme: &'static str,
    extra: &'static [(&'static str, &'static str)],
    /// What a 2xx answer means.
    success: KeyCheck,
}

const LOCAL: &str = "this local server takes requests without a key, so there is nothing to check";
const NO_CHECK: &str = "NodeCraft knows no free request that checks this provider's key";

const fn probe(provider: &'static str, url: &'static str, header: &'static str, scheme: &'static str) -> Probe {
    Probe { provider, url, header, scheme, extra: &[], success: KeyCheck::Verified }
}

/// The check for `account`, or why there is none.
fn plan(account: &Account) -> Result<Probe, &'static str> {
    Ok(match account.as_str() {
        "openai" => probe("OpenAI", "https://api.openai.com/v1/models", "Authorization", "Bearer "),
        "anthropic" => {
            Probe { extra: &[("anthropic-version", "2023-06-01")], ..probe("Anthropic", "https://api.anthropic.com/v1/models?limit=1", "x-api-key", "") }
        }
        "gemini" => probe("Google AI", "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1", "x-goog-api-key", ""),
        "replicate" => probe("Replicate", "https://api.replicate.com/v1/account", "Authorization", "Bearer "),
        "stability" => probe("Stability AI", "https://api.stability.ai/v1/user/balance", "Authorization", "Bearer "),
        // The voice list the app already reads (#670).
        "elevenlabs" => probe("ElevenLabs", "https://api.elevenlabs.io/v2/voices?page_size=1&include_total_count=false", "xi-api-key", ""),
        "tripo" => probe("Tripo", "https://api.tripo3d.ai/v2/openapi/user/balance", "Authorization", "Bearer "),
        "meshy" => probe("Meshy", "https://api.meshy.ai/openapi/v1/balance", "Authorization", "Bearer "),
        // The Soul ID list the adapter already reads (#180).
        "higgsfield" => probe("Higgsfield", "https://api.higgsfield.ai/v1/custom-references/list?page=1&page_size=1", "Authorization", "Key "),
        // The key's own details (its limits and usage); the model list answers without a key.
        "openrouter" => probe("OpenRouter", "https://openrouter.ai/api/v1/key", "Authorization", "Bearer "),
        "ollama" | "lmstudio" => return Err(LOCAL),
        _ => return Err(NO_CHECK),
    })
}

/// Checks `key`, stored for `account`, over `http`. Costs nothing: a provider with no free check
/// is answered unverifiable without a request.
pub async fn check(account: &Account, key: &Secret, http: &dyn Http) -> Result<KeyCheck, Error> {
    let probe = match plan(account) {
        Ok(probe) => probe,
        Err(why) => return Ok(KeyCheck::Unverifiable(why)),
    };
    let mut request = HttpRequest::get(probe.url)
        .timeout_ms(TEST_TIMEOUT_MS)
        .max_response_bytes(MAX_REPLY_BYTES)
        .secret_header(probe.header, format!("{}{}", probe.scheme, key.expose()));
    for (name, value) in probe.extra {
        request = request.header(*name, *value);
    }
    let response = http.send_no_redirect(request).await.map_err(|e| {
        let kind = if e.kind == nodecraft_providers::ErrorKind::Timeout { ErrorKind::Timeout } else { ErrorKind::Network };
        // A transport's message names the URL, never a header; scrub it anyway.
        let why: String = e.message.replace(key.expose(), "…").chars().take(200).collect();
        Error::new(kind, format!("couldn't reach {}: {why}", probe.provider))
    })?;
    classify(&probe, &response)
}

/// What an answer says about the key, from its status (and Google's reason for a 400).
fn classify(probe: &Probe, response: &HttpResponse) -> Result<KeyCheck, Error> {
    let (provider, status) = (probe.provider, response.status);
    match status {
        200..=299 => match serde_json::from_slice::<serde::de::IgnoredAny>(&response.body) {
            Ok(_) => Ok(probe.success.clone()),
            Err(_) => Err(Error::new(ErrorKind::Network, format!("{provider} answered with something that isn't its API; check your network"))),
        },
        401 => Err(rejected(provider, status)),
        // A valid key can be refused here too: a region, a disabled API, scoped permissions.
        403 => Err(Error::new(ErrorKind::Authentication, format!("rejected or not allowed: {provider} refused this key or what it may do (HTTP 403)"))),
        // Google answers a key it doesn't know with 400 and this reason.
        400 if contains(&response.body, b"API_KEY_INVALID") => Err(rejected(provider, status)),
        300..=399 => Err(Error::new(ErrorKind::Network, format!("{provider} redirected the check (HTTP {status}); the key wasn't sent on"))),
        429 => Err(Error::new(ErrorKind::Busy, format!("{provider} is rate limiting requests; try again shortly"))),
        500..=599 => Err(Error::new(ErrorKind::Network, format!("{provider} couldn't check the key (HTTP {status}); try again later"))),
        _ => Err(Error::new(ErrorKind::Network, format!("{provider} couldn't check the key (HTTP {status})"))),
    }
}

fn rejected(provider: &str, status: u16) -> Error {
    Error::new(ErrorKind::Authentication, format!("rejected: {provider} refused this key (HTTP {status})"))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_providers::BoxFuture;
    use nodecraft_providers::ProviderError;
    use std::sync::Mutex;

    /// Answers every request with one canned reply and keeps what was sent.
    struct Fixture {
        reply: Result<HttpResponse, ProviderError>,
        sent: Mutex<Vec<HttpRequest>>,
    }

    impl Fixture {
        fn status(status: u16, body: &str) -> Self {
            Self { reply: Ok(HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() }), sent: Mutex::new(vec![]) }
        }
    }

    impl Http for Fixture {
        fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            panic!("a key check must never follow redirects")
        }
        fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            self.sent.lock().unwrap().push(request);
            let reply = self.reply.clone();
            Box::pin(async move { reply })
        }
    }

    const KEY: &str = "sk-test-abcdefghij1234";

    fn run(account: &str, http: &Fixture) -> Result<KeyCheck, Error> {
        futures::executor::block_on(check(&Account::new(account).unwrap(), &Secret::new(KEY).unwrap(), http))
    }

    #[test]
    fn an_accepted_key_is_verified_and_goes_only_to_the_providers_origin() {
        for (account, url, header, value) in [
            ("openai", "https://api.openai.com/v1/models", "authorization", format!("Bearer {KEY}")),
            ("anthropic", "https://api.anthropic.com/v1/models?limit=1", "x-api-key", KEY.to_owned()),
            ("gemini", "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1", "x-goog-api-key", KEY.to_owned()),
            ("replicate", "https://api.replicate.com/v1/account", "authorization", format!("Bearer {KEY}")),
            ("stability", "https://api.stability.ai/v1/user/balance", "authorization", format!("Bearer {KEY}")),
            ("elevenlabs", "https://api.elevenlabs.io/v2/voices?page_size=1&include_total_count=false", "xi-api-key", KEY.to_owned()),
            ("tripo", "https://api.tripo3d.ai/v2/openapi/user/balance", "authorization", format!("Bearer {KEY}")),
            ("meshy", "https://api.meshy.ai/openapi/v1/balance", "authorization", format!("Bearer {KEY}")),
            ("higgsfield", "https://api.higgsfield.ai/v1/custom-references/list?page=1&page_size=1", "authorization", format!("Key {KEY}")),
            ("openrouter", "https://openrouter.ai/api/v1/key", "authorization", format!("Bearer {KEY}")),
        ] {
            let http = Fixture::status(200, r#"{"data": []}"#);
            assert_eq!(run(account, &http), Ok(KeyCheck::Verified), "{account}");
            let sent = http.sent.lock().unwrap();
            let [request] = sent.as_slice() else { panic!("{account}: one request, got {}", sent.len()) };
            assert_eq!(request.method, nodecraft_providers::http::HttpMethod::Get, "{account}");
            assert_eq!(request.url, url);
            // The key travels in exactly one header, marked secret so it never crosses origins.
            let carrying: Vec<_> = request.headers.iter().filter(|(_, v)| v.contains(KEY)).collect();
            assert_eq!(carrying.len(), 1, "{account}");
            assert!(carrying[0].0.eq_ignore_ascii_case(header) && carrying[0].1 == value, "{account}");
            assert!(request.secret_headers.contains(&header.to_owned()), "{account}");
            assert!(!request.url.contains(KEY) && request.body.is_empty());
        }
        let http = Fixture::status(200, "{}");
        run("anthropic", &http).unwrap();
        assert!(http.sent.lock().unwrap()[0].headers.contains(&("anthropic-version".into(), "2023-06-01".into())));
    }

    #[test]
    fn a_provider_without_a_free_check_is_unverifiable() {
        // Nothing is sent where there is nothing free to ask.
        for (account, why) in
            [("ollama", LOCAL), ("lmstudio", LOCAL), ("fal", NO_CHECK), ("kling", NO_CHECK), ("google-cloud", NO_CHECK), ("comfyui:gpu-box:8188", NO_CHECK)]
        {
            let http = Fixture::status(500, "");
            assert_eq!(run(account, &http), Ok(KeyCheck::Unverifiable(why)), "{account}");
            assert!(http.sent.lock().unwrap().is_empty(), "{account}");
        }
        assert_eq!(KeyCheck::Verified.to_json(), json!({"result": "verified"}));
        assert_eq!(KeyCheck::Unverifiable(NO_CHECK).to_json()["result"], "unverifiable");
    }

    #[test]
    fn a_rejected_key_is_an_auth_error_that_never_quotes_the_reply() {
        // OpenAI's 401 echoes part of the key; the error must not.
        let echo = format!(r#"{{"error": {{"message": "Incorrect API key provided: {KEY}"}}}}"#);
        for (account, status, body) in
            [("openai", 401, echo.as_str()), ("anthropic", 403, "{}"), ("gemini", 400, r#"{"error":{"details":[{"reason":"API_KEY_INVALID"}]}}"#)]
        {
            let err = run(account, &Fixture::status(status, body)).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Authentication, "{account}");
            assert!(err.message.starts_with("rejected") && !err.message.contains("abcdefghij"), "{}", err.message);
        }
        // A 403 may be a valid key without access, and says so.
        let err = run("anthropic", &Fixture::status(403, "{}")).unwrap_err();
        assert!(err.message.starts_with("rejected or not allowed:"), "{}", err.message);
    }

    #[test]
    fn other_answers_are_errors_that_dont_claim_a_verdict() {
        for (status, body, kind) in [
            (400, "{}", ErrorKind::Network),
            (404, "{}", ErrorKind::Network),
            (429, "{}", ErrorKind::Busy),
            (503, "{}", ErrorKind::Network),
            (302, "", ErrorKind::Network),
            // A captive portal or proxy page is not the provider saying yes.
            (200, "<html>sign in</html>", ErrorKind::Network),
        ] {
            let err = run("openai", &Fixture::status(status, body)).unwrap_err();
            assert_eq!(err.kind, kind, "{status}");
            // Only a server error is worth retrying later.
            assert_eq!(err.message.contains("try again later"), status == 503, "{status}: {}", err.message);
        }
        let fails = |kind, message: &str| Fixture { reply: Err(ProviderError::new(kind, message)), sent: Mutex::new(vec![]) };
        let err = run("openai", &fails(nodecraft_providers::ErrorKind::Timeout, "timed out")).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Timeout);
        let err = run("openai", &fails(nodecraft_providers::ErrorKind::Network, &format!("refused near {KEY}"))).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Network);
        assert!(!err.message.contains("abcdefghij"), "{}", err.message);
    }
}
