//! HTTP as providers use it: a small trait so adapters are tested against fixtures, and a
//! `reqwest` implementation (feature `http`) for desktop and browser.

use crate::provider::{BoxFuture, MaybeSendSync};
use crate::{ErrorKind, ProviderError};

mod public_only;
pub use public_only::public_address;

/// The User-Agent every request carries: generic, nothing about the user or machine.
pub const USER_AGENT: &str = concat!("NodeCraft/", env!("CARGO_PKG_VERSION"));

/// Default cap on a JSON reply.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Default request timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Redirect hops followed before giving up.
pub const MAX_REDIRECTS: usize = 5;

/// Headers that always carry credentials and are dropped when a redirect leaves the origin.
const ALWAYS_SECRET: &[&str] = &["authorization", "proxy-authorization", "cookie", "x-api-key", "api-key", "x-goog-api-key", "xi-api-key"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

/// An HTTP request.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    /// Header values may hold keys: they never appear in `Debug` output or errors.
    pub headers: Vec<(String, String)>,
    /// Names (lowercase) of headers that hold credentials beyond the well-known ones: they are
    /// sent only to the request's own origin, never across a redirect to another host.
    pub secret_headers: Vec<String>,
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
            secret_headers: Vec::new(),
            body: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    pub fn post_json(url: impl Into<String>, json: &serde_json::Value) -> Result<Self, ProviderError> {
        let body = serde_json::to_vec(json).map_err(|e| ProviderError::new(ErrorKind::Malformed, e.to_string()))?;
        Ok(Self { method: HttpMethod::Post, body, ..Self::get(url) }.header("Content-Type", "application/json"))
    }

    /// A `multipart/form-data` POST with text `fields` and `files` (field name, file name, MIME
    /// type, bytes), for uploading inputs.
    pub fn post_multipart(url: impl Into<String>, fields: &[(&str, &str)], files: &[(&str, &str, &str, &[u8])]) -> Result<Self, ProviderError> {
        let clean = |s: &str| -> String { s.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').collect() };
        let total: usize = files.iter().map(|f| f.3.len()).fold(0, usize::saturating_add);
        // A boundary that doesn't occur in any part, derived from the content.
        let mut seed =
            files.iter().flat_map(|f| f.3.iter()).take(4096).fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
        let boundary = (0..8)
            .map(|_| {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                format!("----NodeCraft{seed:016x}")
            })
            .find(|b| !files.iter().any(|f| f.3.windows(b.len()).any(|w| w == b.as_bytes())))
            .ok_or_else(|| ProviderError::new(ErrorKind::UnsupportedInput, "could not encode the upload"))?;
        let mut body = Vec::with_capacity(total.saturating_add(1024));
        for (name, value) in fields {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{value}\r\n", clean(name)).as_bytes());
        }
        for (name, filename, mime, bytes) in files {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                    clean(name),
                    clean(filename),
                    clean(mime)
                )
                .as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        Ok(Self { method: HttpMethod::Post, body, ..Self::get(url) }.header("Content-Type", format!("multipart/form-data; boundary={boundary}")))
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// A header holding a credential: sent to this origin only.
    pub fn secret_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        self.secret_headers.push(name.to_ascii_lowercase());
        self.headers.push((name, value.into()));
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

    fn is_secret(&self, header: &str) -> bool {
        let h = header.to_ascii_lowercase();
        ALWAYS_SECRET.contains(&h.as_str()) || self.secret_headers.contains(&h)
    }

    /// The request to send after a redirect to `location`, or `None` when it can't be followed.
    /// Credentials stay behind when the redirect leaves the origin; 301–303 turn into a bodiless
    /// GET as browsers do, 307/308 keep the method and body.
    pub fn redirected(&self, status: u16, location: &str) -> Option<Self> {
        let from = url::Url::parse(&self.url).ok()?;
        let to = from.join(location).ok()?;
        if !matches!(to.scheme(), "http" | "https") || (from.scheme() == "https" && to.scheme() == "http") {
            return None; // no scheme change, and never downgrade
        }
        let mut next = self.clone();
        next.url = to.to_string();
        if from.origin() != to.origin() {
            next.headers.retain(|(n, _)| !self.is_secret(n));
        }
        if matches!(status, 301..=303) && !matches!(self.method, HttpMethod::Get | HttpMethod::Head) {
            next.method = HttpMethod::Get;
            next.body.clear();
            next.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-type"));
        }
        Some(next)
    }
}

/// An HTTP reply (any status; mapping statuses to errors is the adapter's call).
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Bodies and headers can carry signed URLs and cookies.
        let names: Vec<&str> = self.headers.iter().map(|(n, _)| n.as_str()).collect();
        f.debug_struct("HttpResponse").field("status", &self.status).field("headers", &names).field("body_len", &self.body.len()).finish()
    }
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Fails with the status's error kind unless 2xx, honouring `Retry-After` in seconds (the
    /// HTTP-date form is treated as absent).
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

/// Receives a streamed reply body piece by piece; an error stops the stream.
#[cfg(not(target_arch = "wasm32"))]
pub type ChunkSink<'a> = dyn FnMut(&[u8]) -> Result<(), ProviderError> + Send + 'a;
#[cfg(target_arch = "wasm32")]
pub type ChunkSink<'a> = dyn FnMut(&[u8]) -> Result<(), ProviderError> + 'a;

/// Sends HTTP requests. Network failures are `ErrorKind::Network`, timeouts `Timeout`, and a
/// reply over the size cap `Malformed`; any HTTP status is returned as a response.
pub trait Http: MaybeSendSync {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>>;

    /// One bounded request with no redirect following, including same-origin redirects.
    /// Implementations opt in to this operation; refusing it must not construct a legacy send.
    /// This trusted transport contract supplies no affirmative nonbilling evidence.
    fn send_no_redirect(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::UnsupportedInput, "this transport cannot send without following redirects")) })
    }

    /// Request-local evidence. Legacy implementations are unknown before future construction.
    fn send_with_delivery<'a>(&'a self, request: HttpRequest, context: &'a crate::RequestContext) -> BoxFuture<'a, Result<HttpResponse, crate::HttpFailure>> {
        context.unknown();
        let work = self.send(request);
        Box::pin(async move { work.await.map_err(crate::HttpFailure::unknown) })
    }

    /// Streaming counterpart with the same conservative legacy construction boundary.
    fn send_streaming_with_delivery<'a>(
        &'a self,
        request: HttpRequest,
        on_chunk: &'a mut ChunkSink<'a>,
        context: &'a crate::RequestContext,
    ) -> BoxFuture<'a, Result<HttpResponse, crate::HttpFailure>> {
        context.unknown();
        let work = self.send_streaming(request, on_chunk);
        Box::pin(async move { work.await.map_err(crate::HttpFailure::unknown) })
    }

    /// Like [`Http::send`], but a successful (2xx) body goes to `on_chunk` as it arrives and the
    /// returned response has an empty body; an error status's body is returned whole. The default
    /// waits for the whole body and hands it over at once.
    fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        Box::pin(async move {
            let mut response = self.send(request).await?;
            if response.is_success() {
                on_chunk(&response.body)?;
                response.body.clear();
            }
            Ok(response)
        })
    }
}

/// The URL without credentials, query or fragment, for logs and errors (they can carry tokens).
pub fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let had_query = u.query().is_some();
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            if had_query { format!("{u}?…") } else { u.to_string() }
        }
        Err(_) => url.split(['?', '#', '@']).next().unwrap_or_default().to_owned(),
    }
}

#[cfg(feature = "http")]
pub use reqwest_impl::ReqwestHttp;

#[cfg(feature = "http")]
mod reqwest_impl {
    use super::*;

    /// [`Http`] over `reqwest`. Redirects are followed here, not by reqwest, so credentials are
    /// dropped when one leaves the origin ([`HttpRequest::redirected`]). In the browser, `fetch`
    /// follows redirects itself and only drops `Authorization` across origins, so browser-build
    /// adapters must not rely on custom key headers being dropped.
    #[derive(Debug, Clone)]
    pub struct ReqwestHttp {
        client: reqwest::Client,
        /// Built by [`Self::public_only`]: a URL naming a non-public address is refused too,
        /// since the resolver never sees an address literal.
        public_only: bool,
    }

    impl ReqwestHttp {
        pub fn new() -> Result<Self, ProviderError> {
            let builder = reqwest::Client::builder().user_agent(USER_AGENT);
            #[cfg(not(target_arch = "wasm32"))]
            let builder = builder.redirect(reqwest::redirect::Policy::none());
            let client = builder.build().map_err(|e| ProviderError::new(ErrorKind::Network, format!("HTTP client: {e}")))?;
            Ok(Self { client, public_only: false })
        }

        /// A transport for requests a package shapes (#174): no redirects, no proxy (a proxy
        /// would resolve names itself), and names resolved only to public addresses
        /// ([`public_address`]). Native only; the browser resolves names itself.
        #[cfg(not(target_arch = "wasm32"))]
        pub fn public_only() -> Result<Self, ProviderError> {
            let client = reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .dns_resolver(super::public_only::PublicOnly)
                .build()
                .map_err(|e| ProviderError::new(ErrorKind::Network, format!("HTTP client: {e}")))?;
            Ok(Self { client, public_only: true })
        }

        /// Sends the request (following redirects on native) and returns the final response with
        /// its status and lowercased headers.
        async fn open(
            &self,
            mut request: HttpRequest,
            context: Option<&crate::RequestContext>,
        ) -> Result<(reqwest::Response, u16, Vec<(String, String)>), ProviderError> {
            for _ in 0..=MAX_REDIRECTS {
                let response = self.send_once(&request, context).await?;
                let status = response.status().as_u16();
                let headers: Vec<(String, String)> =
                    response.headers().iter().filter_map(|(n, v)| Some((n.as_str().to_ascii_lowercase(), v.to_str().ok()?.to_owned()))).collect();
                let location = headers.iter().find(|(n, _)| n == "location").map(|(_, v)| v.clone());
                let next = match (status, location) {
                    (301 | 302 | 303 | 307 | 308, Some(location)) => request.redirected(status, &location),
                    _ => None,
                };
                match next {
                    Some(next) => request = next,
                    None => return Ok((response, status, headers)),
                }
            }
            Err(ProviderError::new(ErrorKind::Malformed, format!("more than {MAX_REDIRECTS} redirects")))
        }

        async fn send_once(&self, request: &HttpRequest, context: Option<&crate::RequestContext>) -> Result<reqwest::Response, ProviderError> {
            if self.public_only {
                let host = url::Url::parse(&request.url).ok().and_then(|u| u.host().map(|h| h.to_owned()));
                let literal = match host {
                    Some(url::Host::Ipv4(ip)) => Some(std::net::IpAddr::V4(ip)),
                    Some(url::Host::Ipv6(ip)) => Some(std::net::IpAddr::V6(ip)),
                    _ => None,
                };
                if literal.is_some_and(|ip| !super::public_address(ip)) {
                    return Err(ProviderError::new(ErrorKind::Network, "refused to connect to a private or local address"));
                }
            }
            let method = match request.method {
                HttpMethod::Get => reqwest::Method::GET,
                HttpMethod::Head => reqwest::Method::HEAD,
                HttpMethod::Post => reqwest::Method::POST,
                HttpMethod::Put => reqwest::Method::PUT,
                HttpMethod::Patch => reqwest::Method::PATCH,
                HttpMethod::Delete => reqwest::Method::DELETE,
            };
            let mut builder = self.client.request(method, &request.url).timeout(std::time::Duration::from_millis(request.timeout_ms));
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            if !request.body.is_empty() {
                builder = builder.body(request.body.clone());
            }
            let request = builder.build().map_err(|_| ProviderError::new(ErrorKind::UnsupportedInput, "HTTP request could not be constructed"))?;
            // Delivery becomes possible when execute is INVOKED, before its future is polled.
            if let Some(context) = context {
                context.unknown();
            }
            self.client.execute(request).await.map_err(|e| network(&e))
        }
    }

    fn network(e: &reqwest::Error) -> ProviderError {
        // reqwest's own Display can include the URL; build our own message with a redacted one.
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
        } else if cfg!(target_arch = "wasm32") {
            // The browser reports a refused cross-origin request (CORS) the same as any other
            // failure, and refusing it is what most providers do to a web page (#118).
            "the browser couldn't reach the provider; it may not accept requests from web pages"
        } else {
            "the request failed"
        };
        let url = e.url().map(|u| redact_url(u.as_str())).unwrap_or_default();
        ProviderError::new(kind, format!("{what} ({url})"))
    }

    fn too_large(cap: usize) -> ProviderError {
        ProviderError::new(ErrorKind::Malformed, format!("the reply is larger than {cap} bytes"))
    }

    /// Reads a body into memory, refusing it past `cap`. Native reads in chunks and stops at the
    /// cap; the browser's `fetch` buffers a body without a length before the cap can be checked.
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    async fn read_body(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, ProviderError> {
        if response.content_length().is_some_and(|len| len > cap as u64) {
            return Err(too_large(cap));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| network(&e))? {
                if body.len().saturating_add(chunk.len()) > cap {
                    return Err(too_large(cap));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let bytes = response.bytes().await.map_err(|e| network(&e))?;
            if bytes.len() > cap {
                return Err(too_large(cap));
            }
            Ok(bytes.to_vec())
        }
    }

    impl ReqwestHttp {
        async fn send_inner(&self, request: HttpRequest, context: Option<&crate::RequestContext>) -> Result<HttpResponse, ProviderError> {
            let cap = request.max_response_bytes;
            let (response, status, headers) = self.open(request, context).await?;
            let body = read_body(response, cap).await?;
            Ok(HttpResponse { status, headers, body })
        }
        async fn streaming_inner<'a>(
            &'a self,
            request: HttpRequest,
            on_chunk: &'a mut ChunkSink<'a>,
            context: Option<&'a crate::RequestContext>,
        ) -> Result<HttpResponse, ProviderError> {
            let cap = request.max_response_bytes;
            let (response, status, headers) = self.open(request, context).await?;
            if !(200..300).contains(&status) {
                let body = read_body(response, cap).await?;
                return Ok(HttpResponse { status, headers, body });
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let mut response = response;
                let mut total = 0usize;
                while let Some(chunk) = response.chunk().await.map_err(|e| network(&e))? {
                    total = total.saturating_add(chunk.len());
                    if total > cap {
                        return Err(too_large(cap));
                    }
                    on_chunk(&chunk)?;
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                let body = read_body(response, cap).await?;
                on_chunk(&body)?;
            }
            Ok(HttpResponse { status, headers, body: Vec::new() })
        }
    }
    impl Http for ReqwestHttp {
        fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            #[cfg(not(target_arch = "wasm32"))]
            return Box::pin(async move {
                let cap = request.max_response_bytes;
                let response = self.send_once(&request, None).await?;
                let status = response.status().as_u16();
                let headers = response.headers().iter().filter_map(|(n, v)| Some((n.as_str().to_ascii_lowercase(), v.to_str().ok()?.to_owned()))).collect();
                // The reqwest request has consumed its body clone; release our original before decoding.
                drop(request);
                let body = read_body(response, cap).await?;
                Ok(HttpResponse { status, headers, body })
            });
            #[cfg(target_arch = "wasm32")]
            {
                let _ = request;
                Box::pin(async { Err(ProviderError::new(ErrorKind::UnsupportedInput, "no-redirect image requests need a native or audited host transport")) })
            }
        }
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            Box::pin(self.send_inner(request, None))
        }
        fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
            Box::pin(self.streaming_inner(request, on_chunk, None))
        }
        fn send_with_delivery<'a>(
            &'a self,
            request: HttpRequest,
            context: &'a crate::RequestContext,
        ) -> BoxFuture<'a, Result<HttpResponse, crate::HttpFailure>> {
            context.arm();
            // Browser fetch owns redirects/staging: no positive browser delivery claim.
            #[cfg(target_arch = "wasm32")]
            context.unknown();
            Box::pin(async move { self.send_inner(request, Some(context)).await.map_err(|error| context.failure(error)) })
        }
        fn send_streaming_with_delivery<'a>(
            &'a self,
            request: HttpRequest,
            on_chunk: &'a mut ChunkSink<'a>,
            context: &'a crate::RequestContext,
        ) -> BoxFuture<'a, Result<HttpResponse, crate::HttpFailure>> {
            context.arm();
            #[cfg(target_arch = "wasm32")]
            context.unknown();
            Box::pin(async move { self.streaming_inner(request, on_chunk, Some(context)).await.map_err(|error| context.failure(error)) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_responses_hide_secrets_in_debug() {
        let r = HttpRequest::get("https://u:p@api.example.com/v1/x?key=sk-123#frag").header("Authorization", "Key sk-456");
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("sk-123") && !dbg.contains("sk-456") && !dbg.contains("u:p") && !dbg.contains("frag"), "{dbg}");
        assert!(dbg.contains("Authorization"));
        assert_eq!(redact_url("https://h/p"), "https://h/p");
        let resp = HttpResponse { status: 200, headers: vec![("set-cookie".into(), "s=secret".into())], body: b"https://signed?sig=secret".to_vec() };
        assert!(!format!("{resp:?}").contains("secret"));
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

    #[test]
    fn redirects_keep_secrets_at_home() {
        let r = HttpRequest::post_json("https://api.example.com/v1/run", &serde_json::json!({}))
            .unwrap()
            .secret_header("X-Custom-Key", "k")
            .header("Authorization", "Bearer t")
            .header("Accept", "*/*");
        let same = r.redirected(307, "/v2/run").unwrap();
        assert_eq!(same.url, "https://api.example.com/v2/run");
        assert_eq!(same.headers.len(), 4, "same origin keeps everything");
        assert_eq!(same.method, HttpMethod::Post);
        let away = r.redirected(302, "https://cdn.example.net/file.png").unwrap();
        let names: Vec<&str> = away.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Accept"], "credentials and the body's content type stay behind");
        assert_eq!((away.method, away.body.len()), (HttpMethod::Get, 0));
        assert!(r.redirected(302, "http://api.example.com/x").is_none(), "no downgrade");
        assert!(r.redirected(302, "file:///etc/passwd").is_none());
        assert!(HttpRequest::get("not a url").redirected(302, "/x").is_none());
    }

    #[test]
    fn multipart_bodies() {
        let r = HttpRequest::post_multipart("https://h/upload", &[("type", "input")], &[("image", "a\"b.png", "image/png", b"PNGDATA")]).unwrap();
        let ct = r.headers.iter().find(|(n, _)| n == "Content-Type").unwrap().1.clone();
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let body = String::from_utf8_lossy(&r.body);
        assert!(body.contains("name=\"image\"; filename=\"ab.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n"), "{body}");
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
    }
}
