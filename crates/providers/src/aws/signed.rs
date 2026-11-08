//! [`SigV4Http`]: a transport that signs each request with AWS credentials, then hands it to the
//! real one.

use std::sync::Arc;

use super::AwsCredentials;
use super::sigv4::{self, Canonical};
use crate::http::{ChunkSink, Http, HttpMethod, HttpRequest, HttpResponse};
use crate::provider::BoxFuture;
use crate::{ErrorKind, ProviderError};

/// Headers this signer sets; any the caller set are replaced.
const SIGNER_HEADERS: &[&str] = &["authorization", "x-amz-date", "x-amz-content-sha256", "x-amz-security-token", "host"];
/// What an error reply that quoted the session token shows in its place.
const REDACTED: &[u8] = b"[redacted]";

/// Signs every request with AWS Signature Version 4 for one service in one Region, and sends it
/// with the transport it wraps. It signs only `https` requests to the one host it was made for:
/// anything else is refused before it is sent, so the credentials never sign a request for
/// another server. The signature covers `host`, `x-amz-date`, `x-amz-content-sha256` (the
/// body's SHA-256), `x-amz-security-token` (with a session token) and `content-type`.
pub struct SigV4Http {
    inner: Arc<dyn Http>,
    credentials: AwsCredentials,
    region: String,
    service: String,
    host: String,
    clock: fn() -> Option<u64>,
}

impl std::fmt::Debug for SigV4Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigV4Http").field("region", &self.region).field("service", &self.service).field("host", &self.host).finish_non_exhaustive()
    }
}

impl SigV4Http {
    /// Signs for `service` (Bedrock's is `bedrock`) in `region`, for requests to `host` only, on
    /// the wall clock.
    pub fn new(inner: Arc<dyn Http>, credentials: AwsCredentials, region: &str, service: &str, host: &str) -> Self {
        Self { inner, credentials, region: region.to_owned(), service: service.to_owned(), host: host.to_ascii_lowercase(), clock: super::system_seconds }
    }

    /// The same signer on another clock (Unix seconds), for tests.
    pub fn with_clock(mut self, clock: fn() -> Option<u64>) -> Self {
        self.clock = clock;
        self
    }

    /// `request` with the signature headers added, or why it can't be signed.
    pub fn sign(&self, mut request: HttpRequest) -> Result<HttpRequest, ProviderError> {
        let url = url::Url::parse(&request.url).map_err(|_| ProviderError::new(ErrorKind::UnsupportedInput, "the request address isn't a URL"))?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if url.scheme() != "https" || host != self.host || url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
            return Err(ProviderError::new(ErrorKind::UnsupportedInput, format!("AWS credentials sign requests to https://{} only", self.host)));
        }
        let now = (self.clock)().ok_or_else(|| ProviderError::new(ErrorKind::Auth, "the system clock is unset, so the AWS request can't be signed"))?;
        self.credentials.check_fresh(now)?;
        let (amz_date, date) = sigv4::amz_date(now);
        let payload_hash = sigv4::sha256_hex(&request.body);
        request.headers.retain(|(name, _)| !SIGNER_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h)));
        request = request.header("x-amz-date", amz_date.clone()).header("x-amz-content-sha256", payload_hash.clone());
        if let Some(token) = self.credentials.session_token() {
            request = request.secret_header("x-amz-security-token", token);
        }
        let mut signed: Vec<(String, String)> = vec![("host".into(), host)];
        signed.extend(
            request
                .headers
                .iter()
                .filter(|(name, _)| {
                    let name = name.to_ascii_lowercase();
                    name == "content-type" || name.starts_with("x-amz-")
                })
                .cloned(),
        );
        let method = match request.method {
            HttpMethod::Get => "GET",
            HttpMethod::Head => "HEAD",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
        };
        let canonical = Canonical { method, path: url.path(), query: url.query().unwrap_or(""), headers: &signed, payload_hash: &payload_hash };
        let (canonical, signed_headers) = canonical.request();
        let scope = sigv4::scope(&date, &self.region, &self.service);
        let to_sign = sigv4::string_to_sign(&amz_date, &scope, &canonical);
        let signature = sigv4::signature(self.credentials.secret_access_key(), &date, &self.region, &self.service, &to_sign)?;
        let authorization =
            format!("{} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}", sigv4::ALGORITHM, self.credentials.access_key_id());
        Ok(request.secret_header("Authorization", authorization))
    }

    /// An error reply with the session token taken out, so no error built from it can show it:
    /// as it is, JSON-escaped (`/` as `\/`) and percent-encoded.
    fn scrubbed(&self, mut response: HttpResponse) -> HttpResponse {
        if let Some(token) = self.credentials.session_token().filter(|_| !response.is_success()) {
            for form in [token.to_owned(), token.replace('/', "\\/"), sigv4::uri_encode(token.as_bytes(), false)] {
                response.body = scrub(&response.body, form.as_bytes());
            }
        }
        response
    }
}

/// `body` with every occurrence of `secret` replaced.
fn scrub(body: &[u8], secret: &[u8]) -> Vec<u8> {
    if secret.is_empty() {
        return body.to_vec();
    }
    let mut out = Vec::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = memchr::memmem::find(rest, secret) {
        out.extend_from_slice(rest.get(..at).unwrap_or_default());
        out.extend_from_slice(REDACTED);
        rest = rest.get(at.saturating_add(secret.len())..).unwrap_or_default();
    }
    out.extend_from_slice(rest);
    out
}

fn refused<'a, T: 'a>(error: ProviderError) -> BoxFuture<'a, Result<T, ProviderError>> {
    Box::pin(async move { Err(error) })
}

impl Http for SigV4Http {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        match self.sign(request) {
            Ok(request) => Box::pin(async move { self.inner.send(request).await.map(|r| self.scrubbed(r)) }),
            Err(error) => refused(error),
        }
    }

    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        match self.sign(request) {
            Ok(request) => Box::pin(async move { self.inner.send_no_redirect(request).await.map(|r| self.scrubbed(r)) }),
            Err(error) => refused(error),
        }
    }

    fn send_streaming_no_redirect<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        match self.sign(request) {
            Ok(request) => Box::pin(async move { self.inner.send_streaming_no_redirect(request, on_chunk).await.map(|r| self.scrubbed(r)) }),
            Err(error) => refused(error),
        }
    }

    fn send_streaming_no_redirect_bodiless_errors<'a>(
        &'a self,
        request: HttpRequest,
        on_chunk: &'a mut ChunkSink<'a>,
    ) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        match self.sign(request) {
            Ok(request) => self.inner.send_streaming_no_redirect_bodiless_errors(request, on_chunk),
            Err(error) => refused(error),
        }
    }

    fn send_with_delivery<'a>(&'a self, request: HttpRequest, context: &'a crate::RequestContext) -> BoxFuture<'a, Result<HttpResponse, crate::HttpFailure>> {
        match self.sign(request) {
            Ok(request) => Box::pin(async move { self.inner.send_with_delivery(request, context).await.map(|r| self.scrubbed(r)) }),
            Err(error) => {
                // Refused before anything was handed to a transport: nothing was sent.
                context.arm();
                let failure = context.failure(error);
                Box::pin(async move { Err(failure) })
            }
        }
    }

    fn send_streaming_with_delivery<'a>(
        &'a self,
        request: HttpRequest,
        on_chunk: &'a mut ChunkSink<'a>,
        context: &'a crate::RequestContext,
    ) -> BoxFuture<'a, Result<HttpResponse, crate::HttpFailure>> {
        match self.sign(request) {
            Ok(request) => Box::pin(async move { self.inner.send_streaming_with_delivery(request, on_chunk, context).await.map(|r| self.scrubbed(r)) }),
            Err(error) => {
                context.arm();
                let failure = context.failure(error);
                Box::pin(async move { Err(failure) })
            }
        }
    }

    fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        match self.sign(request) {
            Ok(request) => Box::pin(async move { self.inner.send_streaming(request, on_chunk).await.map(|r| self.scrubbed(r)) }),
            Err(error) => refused(error),
        }
    }
}
