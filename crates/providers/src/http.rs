//! HTTP as providers use it: a small trait so adapters are tested against fixtures, and a
//! `reqwest` implementation (feature `http`) for desktop and browser.

use crate::provider::{BoxFuture, MaybeSendSync};
use crate::{ErrorKind, ProviderError};

/// The User-Agent every request carries: generic, nothing about the user or machine.
pub const USER_AGENT: &str = concat!("NodeCraft/", env!("CARGO_PKG_VERSION"));

/// Default cap on a JSON reply.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Default request timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
}

/// An HTTP request.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    /// Header values may hold keys: they never appear in `Debug` output or errors.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub timeout_ms: u64,
    /// The reply is refused (`ErrorKind::Malformed`) once it grows past this.
    pub max_response_bytes: usize,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(n, _)| n.as_str()).collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &redact_url(&self.url))
            .field("headers", &names)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: HttpMethod::Get,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    pub fn post_json(url: impl Into<String>, json: &serde_json::Value) -> Result<Self, ProviderError> {
        let body = serde_json::to_vec(json).map_err(|e| ProviderError::new(ErrorKind::Malformed, e.to_string()))?;
        Ok(Self { method: HttpMethod::Post, body, ..Self::get(url) }.header("Content-Type", "application/json"))
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    pub fn max_response_bytes(mut self, max: usize) -> Self {
        self.max_response_bytes = max;
        self
    }
}

/// An HTTP reply (any status; mapping statuses to errors is the adapter's call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Fails with the status's error kind unless 2xx, honouring `Retry-After` (seconds).
    pub fn error_for_status(self) -> Result<Self, ProviderError> {
        if self.is_success() {
            return Ok(self);
        }
        let mut e = ProviderError::from_status(self.status, &self.body);
        e.retry_after_ms = self.header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).map(|s| s.saturating_mul(1000));
        Err(e)
    }

    /// Parses the body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, ProviderError> {
        serde_json::from_slice(&self.body).map_err(|e| ProviderError::new(ErrorKind::Malformed, format!("{e}: {}", crate::error::excerpt(&self.body))))
    }
}

/// Sends HTTP requests. Network failures are `ErrorKind::Network`, timeouts `Timeout`, and a
/// reply over the size cap `Malformed`; any HTTP status is returned as a response.
pub trait Http: MaybeSendSync {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>>;
}

/// The URL without its query string, for logs and errors (queries can carry tokens).
pub fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?…"),
        None => url.to_owned(),
    }
}

#[cfg(feature = "http")]
pub use reqwest_impl::ReqwestHttp;

#[cfg(feature = "http")]
mod reqwest_impl {
    use super::*;

    /// [`Http`] over `reqwest`: rustls on desktop, `fetch` in the browser. Redirects are followed
    /// only within the same origin, so auth headers never reach another host.
    #[derive(Debug, Clone)]
    pub struct ReqwestHttp {
        client: reqwest::Client,
    }

    impl ReqwestHttp {
        pub fn new() -> Result<Self, ProviderError> {
            let builder = reqwest::Client::builder().user_agent(USER_AGENT);
            #[cfg(not(target_arch = "wasm32"))]
            let builder = builder.redirect(reqwest::redirect::Policy::custom(|attempt| {
                let same_origin = attempt.previous().last().is_some_and(|prev| prev.origin() == attempt.url().origin());
                if attempt.previous().len() >= 5 || !same_origin { attempt.stop() } else { attempt.follow() }
            }));
            let client = builder.build().map_err(|e| ProviderError::new(ErrorKind::Network, format!("HTTP client: {e}")))?;
            Ok(Self { client })
        }
    }

    fn network(e: &reqwest::Error) -> ProviderError {
        // reqwest errors can include the URL; strip queries, which may hold tokens.
        let kind = if e.is_timeout() { ErrorKind::Timeout } else { ErrorKind::Network };
        #[cfg(not(target_arch = "wasm32"))]
        let connect = e.is_connect();
        #[cfg(target_arch = "wasm32")]
        let connect = false;
        let what = if connect {
            "could not connect"
        } else if e.is_timeout() {
            "the request timed out"
        } else if e.is_body() || e.is_decode() {
            "the connection dropped while reading the reply"
        } else {
            "the request failed"
        };
        let url = e.url().map(|u| redact_url(u.as_str())).unwrap_or_default();
        ProviderError::new(kind, format!("{what} ({url})"))
    }

    impl Http for ReqwestHttp {
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            Box::pin(async move {
                let method = match request.method {
                    HttpMethod::Get => reqwest::Method::GET,
                    HttpMethod::Post => reqwest::Method::POST,
                    HttpMethod::Put => reqwest::Method::PUT,
                    HttpMethod::Delete => reqwest::Method::DELETE,
                };
                let mut builder = self.client.request(method, &request.url);
                for (name, value) in &request.headers {
                    builder = builder.header(name, value);
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    builder = builder.timeout(std::time::Duration::from_millis(request.timeout_ms));
                }
                if !request.body.is_empty() {
                    builder = builder.body(request.body);
                }
                #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
                let mut response = builder.send().await.map_err(|e| network(&e))?;
                let status = response.status().as_u16();
                let headers = response.headers().iter().filter_map(|(n, v)| Some((n.as_str().to_ascii_lowercase(), v.to_str().ok()?.to_owned()))).collect();
                let cap = request.max_response_bytes;
                if response.content_length().is_some_and(|len| len > cap as u64) {
                    return Err(ProviderError::new(ErrorKind::Malformed, format!("the reply is larger than {cap} bytes")));
                }
                let too_large = || ProviderError::new(ErrorKind::Malformed, format!("the reply is larger than {cap} bytes"));
                // Native: read in chunks and stop at the cap. Browser: `fetch` buffers the body
                // anyway; the length was checked above when the server sent one.
                #[cfg(not(target_arch = "wasm32"))]
                let body = {
                    let mut body = Vec::new();
                    while let Some(chunk) = response.chunk().await.map_err(|e| network(&e))? {
                        if body.len().saturating_add(chunk.len()) > cap {
                            return Err(too_large());
                        }
                        body.extend_from_slice(&chunk);
                    }
                    body
                };
                #[cfg(target_arch = "wasm32")]
                let body = {
                    let bytes = response.bytes().await.map_err(|e| network(&e))?;
                    if bytes.len() > cap {
                        return Err(too_large());
                    }
                    bytes.to_vec()
                };
                Ok(HttpResponse { status, headers, body })
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_hide_secrets_in_debug() {
        let r = HttpRequest::get("https://api.example.com/v1/x?key=sk-123").header("Authorization", "Key sk-456");
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("sk-123") && !dbg.contains("sk-456"), "{dbg}");
        assert!(dbg.contains("Authorization"));
        assert_eq!(redact_url("https://h/p"), "https://h/p");
    }

    #[test]
    fn responses() {
        let ok = HttpResponse { status: 200, headers: vec![], body: br#"{"a":1}"#.to_vec() };
        assert_eq!(ok.json::<serde_json::Value>().unwrap()["a"], 1);
        let limited = HttpResponse { status: 429, headers: vec![("retry-after".into(), "7".into())], body: vec![] };
        let e = limited.error_for_status().unwrap_err();
        assert_eq!((e.kind, e.retry_after_ms), (ErrorKind::RateLimited, Some(7000)));
        let html = HttpResponse { status: 200, headers: vec![], body: b"<html>".to_vec() };
        assert_eq!(html.json::<serde_json::Value>().unwrap_err().kind, ErrorKind::Malformed);
        let post = HttpRequest::post_json("http://h", &serde_json::json!({"x": 1})).unwrap();
        assert_eq!(post.method, HttpMethod::Post);
        assert_eq!(post.headers, vec![("Content-Type".to_string(), "application/json".to_string())]);
        assert!(USER_AGENT.starts_with("NodeCraft/"));
    }
}
