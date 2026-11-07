//! `rig-core`'s HTTP seam over NodeCraft's own [`Http`] (docs/decisions/2026-11-06-llm-layer-rig.md).
//!
//! The chat client builds and reads requests; every byte still goes through the [`Http`] the
//! session configured, so a request keeps what that transport guarantees: the generic
//! `NodeCraft/<version>` User-Agent (set by the transport; the crate's own headers are dropped,
//! bar the few named in [`RigHttp::forward`]), the redirect policy, the response size cap and
//! timeout set here, the browser's `fetch` on wasm, and, for a runner's submit, its delivery
//! evidence ([`DeliveryLink`]). Keys never pass through the crate: they are added here as secret
//! headers, from the key NodeCraft already holds.
//!
//! [`RigHttp`] is built per request and records how the exchange went ([`Exchange`]), so the
//! caller maps errors with NodeCraft's own rules rather than the crate's: a transport failure
//! comes back as the [`ProviderError`] the transport raised, an error status with its own
//! status, headers and body.
//!
//! No `rig-core` type leaves this crate (`cargo xtask layers` checks it).

use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;

use futures::{FutureExt as _, StreamExt as _};
use rig_core::http_client::{self, Bytes, HeaderMap, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, Response, StatusCode};
use rig_core::wasm_compat::WasmCompatSend;

use crate::delivery::DeliveryLink;
use crate::http::{Http, HttpMethod, HttpRequest, HttpResponse};
use crate::provider::BoxFuture;
use crate::{ErrorKind, ProviderError};

/// Rewrites a request's JSON body before it is sent, for what the crate can't express.
pub(crate) type BodyFix = Box<dyn Fn(&mut serde_json::Value) + Send + Sync>;

/// How the exchange went, as far as the transport saw it.
#[derive(Debug, Default, Clone)]
pub(crate) enum Exchange {
    /// Nothing was sent.
    #[default]
    Unsent,
    /// The transport failed with this error: before, during or after sending.
    Failed(ProviderError),
    /// The server answered: its status, its headers, and the body of a reply that isn't 2xx.
    Answered { status: u16, headers: Vec<(String, String)>, body: Vec<u8> },
}

/// One request's transport. Cheap to clone; clones share the record.
#[derive(Clone)]
pub(crate) struct RigHttp {
    inner: Arc<Inner>,
}

struct Inner {
    /// Where requests go. Its own `Arc`, so a send kept in [`Inner::unfinished`] holds this and
    /// never the `Inner` that keeps it: no cycle, and dropping the transport drops the send.
    route: Arc<Route>,
    /// Headers holding credentials, sent to the request's own origin only.
    secrets: Vec<(String, String)>,
    /// Plain headers this transport adds itself.
    extra: Vec<(String, String)>,
    /// Lowercase names of the crate's headers that are sent on; everything else it sets (its
    /// User-Agent, any identity or session header) is dropped.
    forward: &'static [&'static str],
    timeout_ms: u64,
    max_response_bytes: usize,
    fix: Option<BodyFix>,
    /// Rewrites a whole 2xx JSON reply before the crate reads it.
    reply_fix: Option<fn(&mut serde_json::Value)>,
    /// Where the request goes instead of the crate's URL, when the dialect's own path differs.
    url: Option<String>,
    exchange: Mutex<Exchange>,
    /// A streamed send the crate stopped reading before it ended (it stops at `[DONE]`), with
    /// its body still draining: [`RigHttp::finish`] runs it to its end.
    unfinished: Mutex<Option<BoxFuture<'static, Result<HttpResponse, ProviderError>>>>,
}

/// The transport a request goes through, and the runner's delivery scope when there is one.
struct Route {
    http: Arc<dyn Http>,
    delivery: Option<DeliveryLink>,
}

impl Route {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        match &self.delivery {
            Some(link) => link.send(self.http.as_ref(), request).await,
            None => self.http.send(request).await,
        }
    }

    async fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut crate::http::ChunkSink<'a>) -> Result<HttpResponse, ProviderError> {
        match &self.delivery {
            Some(link) => link.send_streaming(self.http.as_ref(), request, on_chunk).await,
            None => self.http.send_streaming(request, on_chunk).await,
        }
    }
}

impl std::fmt::Debug for RigHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Header values hold keys.
        f.debug_struct("RigHttp").field("timeout_ms", &self.inner.timeout_ms).finish_non_exhaustive()
    }
}

impl RigHttp {
    /// The crate's headers a chat request needs: the body's type and, for a stream, `Accept`.
    pub(crate) const FORWARD: &'static [&'static str] = &["content-type", "accept"];

    pub(crate) fn new(http: Arc<dyn Http>, timeout_ms: u64, max_response_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                route: Arc::new(Route { http, delivery: None }),
                secrets: Vec::new(),
                extra: Vec::new(),
                forward: Self::FORWARD,
                timeout_ms,
                max_response_bytes,
                fix: None,
                reply_fix: None,
                url: None,
                exchange: Mutex::new(Exchange::Unsent),
                unfinished: Mutex::new(None),
            }),
        }
    }

    fn edit(mut self, edit: impl FnOnce(&mut Inner)) -> Self {
        // Only called while building, before any clone exists.
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            edit(inner);
        }
        self
    }

    /// Sends through the runner's delivery scope, so the submit's evidence stays exact.
    pub(crate) fn with_delivery(self, delivery: Option<DeliveryLink>) -> Self {
        self.edit(|inner| {
            if let Some(route) = Arc::get_mut(&mut inner.route) {
                route.delivery = delivery;
            }
        })
    }

    /// A header holding a credential.
    pub(crate) fn with_secret(self, name: &str, value: String) -> Self {
        self.edit(|inner| inner.secrets.push((name.to_owned(), value)))
    }

    /// A plain header of NodeCraft's own.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_header(self, name: &str, value: &str) -> Self {
        self.edit(|inner| inner.extra.push((name.to_owned(), value.to_owned())))
    }

    /// Lowercase names of the crate's headers to send on, in place of [`Self::FORWARD`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn forwarding(self, forward: &'static [&'static str]) -> Self {
        self.edit(|inner| inner.forward = forward)
    }

    /// Rewrites a whole (not streamed) 2xx JSON reply before the crate reads it, for what the
    /// crate needs a server to say that the dialect's servers may leave out.
    pub(crate) fn with_reply_fix(self, fix: fn(&mut serde_json::Value)) -> Self {
        self.edit(|inner| inner.reply_fix = Some(fix))
    }

    /// Sends the request to `url` rather than where the crate addressed it.
    pub(crate) fn with_url(self, url: String) -> Self {
        self.edit(|inner| inner.url = Some(url))
    }

    /// Rewrites the JSON body before it is sent.
    pub(crate) fn with_fix(self, fix: BodyFix) -> Self {
        self.edit(|inner| inner.fix = Some(fix))
    }

    /// Drops a streamed send the crate stopped reading, unfinished: an exit that won't wait for
    /// it (an error, a cancel). The send's delivery scope then counts it as possibly sent.
    pub(crate) fn discard(&self) {
        drop(self.inner.unfinished.lock().unwrap_or_else(PoisonError::into_inner).take());
    }

    /// How the exchange went.
    pub(crate) fn exchange(&self) -> Exchange {
        self.inner.exchange.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Runs a streamed send the crate stopped reading to its end, so its head (and the spend it
    /// reports) is recorded and its delivery scope closes as the transport finished it. Bounded by
    /// the request's own timeout and size cap. An error is the transport's, as it raised it.
    pub(crate) async fn finish(&self) -> Result<(), ProviderError> {
        let unfinished = self.inner.unfinished.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(work) = unfinished else { return Ok(()) };
        match work.await {
            Ok(reply) => {
                self.inner.record(Exchange::Answered { status: reply.status, headers: reply.headers, body: Vec::new() });
                Ok(())
            }
            Err(e) => {
                self.inner.record(Exchange::Failed(e.clone()));
                Err(e)
            }
        }
    }
}

impl Inner {
    fn record(&self, exchange: Exchange) {
        *self.exchange.lock().unwrap_or_else(PoisonError::into_inner) = exchange;
    }

    /// Records `error` and hands the crate an opaque error in its place; the caller reads the
    /// recorded one.
    fn fail(&self, error: ProviderError) -> http_client::Error {
        self.record(Exchange::Failed(error.clone()));
        http_client::Error::instance(error)
    }

    /// The crate's request as one of ours: its method, URL and body, the headers it may set,
    /// then ours.
    fn request(&self, request: Request<Bytes>) -> Result<HttpRequest, ProviderError> {
        let (parts, body) = request.into_parts();
        let method = match parts.method.as_str() {
            "GET" => HttpMethod::Get,
            "HEAD" => HttpMethod::Head,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            other => return Err(ProviderError::new(ErrorKind::UnsupportedInput, format!("the chat client asked for an HTTP {other} request"))),
        };
        let url = self.url.clone().unwrap_or_else(|| parts.uri.to_string());
        let mut out = HttpRequest::get(url).timeout_ms(self.timeout_ms).max_response_bytes(self.max_response_bytes);
        out.method = method;
        for (name, value) in &parts.headers {
            if self.forward.contains(&name.as_str()) {
                let value = value.to_str().map_err(|_| ProviderError::new(ErrorKind::UnsupportedInput, "the chat client set a header that isn't text"))?;
                out = out.header(name.as_str(), value);
            }
        }
        for (name, value) in &self.extra {
            out = out.header(name.as_str(), value.as_str());
        }
        for (name, value) in &self.secrets {
            out = out.secret_header(name.as_str(), value.as_str());
        }
        out.body = match &self.fix {
            Some(fix) if !body.is_empty() => {
                let mut json: serde_json::Value =
                    serde_json::from_slice(&body).map_err(|e| ProviderError::new(ErrorKind::UnsupportedInput, format!("the chat request isn't JSON: {e}")))?;
                fix(&mut json);
                serde_json::to_vec(&json).map_err(|e| ProviderError::new(ErrorKind::UnsupportedInput, e.to_string()))?
            }
            _ => body.to_vec(),
        };
        Ok(out)
    }

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.route.send(request).await
    }

    /// The reply's head for the crate; an error status is recorded with its body.
    fn head(&self, status: u16, headers: &[(String, String)], body: &[u8]) -> Result<(StatusCode, HeaderMap), ProviderError> {
        let code =
            StatusCode::from_u16(status).map_err(|_| ProviderError::new(ErrorKind::Malformed, format!("the server answered with HTTP status {status}")))?;
        let map = header_map(headers);
        let failed = !(200..300).contains(&status);
        self.record(Exchange::Answered { status, headers: headers.to_vec(), body: if failed { body.to_vec() } else { Vec::new() } });
        Ok((code, map))
    }
}

/// The streamed reply's first step.
enum First {
    Done(Result<HttpResponse, ProviderError>),
    Chunk(Bytes),
}

/// Headers the crate can read; a header that isn't valid HTTP is left out.
fn header_map(headers: &[(String, String)]) -> HeaderMap {
    // An RFC 9110 token: the crate's header names take nothing else.
    let token = |name: &str| !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    let mut builder = Response::builder();
    for (name, value) in headers {
        if token(name) && HeaderValue::from_str(value).is_ok() {
            builder = builder.header(name.as_str(), value.as_str());
        }
    }
    builder.body(()).map(|r| r.into_parts().0.headers).unwrap_or_default()
}

/// `headers`, saying the body is an event stream unless they name its type.
fn with_event_stream(mut headers: HeaderMap) -> HeaderMap {
    headers.entry("content-type").or_insert(HeaderValue::from_static("text/event-stream"));
    headers
}

fn response<U: From<Bytes> + WasmCompatSend + 'static>(status: StatusCode, headers: HeaderMap, body: Vec<u8>) -> Response<LazyBody<U>> {
    let body: LazyBody<U> = Box::pin(async move { Ok(U::from(Bytes::from(body))) });
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

impl HttpClientExt for RigHttp {
    fn send<T, U>(&self, req: Request<T>) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let request = inner.request(req.map(Into::into));
        async move {
            let request = request.map_err(|e| inner.fail(e))?;
            let mut reply = inner.send(request).await.map_err(|e| inner.fail(e))?;
            let (status, headers) = inner.head(reply.status, &reply.headers, &reply.body).map_err(|e| inner.fail(e))?;
            if let Some(fix) = inner.reply_fix.filter(|_| reply.is_success())
                && let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&reply.body)
            {
                fix(&mut json);
                if let Ok(body) = serde_json::to_vec(&json) {
                    reply.body = body;
                }
            }
            Ok(response(status, headers, reply.body))
        }
    }

    fn send_multipart<U>(&self, _req: Request<MultipartForm>) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let inner = Arc::clone(&self.inner);
        async move { Err(inner.fail(ProviderError::new(ErrorKind::UnsupportedInput, "the chat client sends no multipart requests"))) }
    }

    fn send_streaming<T>(&self, req: Request<T>) -> impl Future<Output = http_client::Result<http_client::StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        let inner = Arc::clone(&self.inner);
        let request = inner.request(req.map(Into::into));
        async move {
            let request = request.map_err(|e| inner.fail(e))?;
            // Our transport pushes a 2xx body to a sink and returns the head at the end; the crate
            // pulls a stream after the head. The sink feeds a channel, and the stream drives the
            // send while it drains it. The channel holds at most what the size cap lets through.
            let (chunks, mut received) = futures::channel::mpsc::unbounded::<Bytes>();
            // The send holds the route only, never `inner`: it may outlive the reader in
            // `inner.unfinished`.
            let route = Arc::clone(&inner.route);
            let mut sending: BoxFuture<'static, Result<HttpResponse, ProviderError>> = Box::pin(async move {
                let mut sink = move |chunk: &[u8]| -> Result<(), ProviderError> {
                    // The receiver is gone only once the reader stopped; nothing more is wanted.
                    chunks.unbounded_send(Bytes::copy_from_slice(chunk)).map_err(|_| ProviderError::new(ErrorKind::Cancelled, "the reply is no longer read"))
                };
                route.send_streaming(request, &mut sink).await
            });
            // Wait for the body's first piece (the reply is 2xx and streams) or the end.
            let first = match futures::future::select(sending.as_mut(), received.next()).await {
                futures::future::Either::Left((done, _)) => Some(First::Done(done)),
                futures::future::Either::Right((Some(chunk), _)) => Some(First::Chunk(chunk)),
                // The sender lives in the send, so this can't come first; wait for the send.
                futures::future::Either::Right((None, _)) => None,
            };
            let first = match first {
                Some(first) => first,
                None => First::Done(sending.as_mut().await),
            };
            let mut body = Body { first: None, received, sending: None, failure: None, closed: false, inner: Arc::clone(&inner) };
            let headers = match first {
                First::Done(Err(e)) => {
                    // The send is over: its sink, and with it the channel's sender, goes now.
                    drop(sending);
                    let error = inner.fail(e);
                    // A body that broke after some of it arrived: what arrived is read first.
                    let Ok(chunk) = body.received.try_recv() else { return Err(error) };
                    body.first = Some(chunk);
                    body.failure = Some(error);
                    with_event_stream(HeaderMap::new())
                }
                First::Done(Ok(reply)) => {
                    drop(sending);
                    let (status, headers) = inner.head(reply.status, &reply.headers, &reply.body).map_err(|e| inner.fail(e))?;
                    if !reply.is_success() {
                        return Err(http_client::Error::non_success_with_details(status, headers, String::from_utf8_lossy(&reply.body).into_owned()));
                    }
                    // A whole 2xx reply already went into the channel; it reads as a stream.
                    with_event_stream(headers)
                }
                // A sink is only fed a 2xx body, whose real head arrives with the end. Until then
                // it reads as the event stream it is.
                First::Chunk(chunk) => {
                    body.first = Some(chunk);
                    body.sending = Some(sending);
                    with_event_stream(HeaderMap::new())
                }
            };
            let mut response = Response::new(Box::pin(futures::stream::poll_fn(move |cx| body.poll(cx))) as http_client::BoxedStream);
            *response.headers_mut() = headers;
            Ok(response)
        }
    }
}

/// A streamed reply's body as the crate reads it.
struct Body {
    /// A piece taken from the channel before the body was handed over.
    first: Option<Bytes>,
    received: futures::channel::mpsc::UnboundedReceiver<Bytes>,
    /// The send, while it runs.
    sending: Option<BoxFuture<'static, Result<HttpResponse, ProviderError>>>,
    /// How the send failed, read after everything that arrived before.
    failure: Option<http_client::Error>,
    /// Whether the closing blank line went out.
    closed: bool,
    inner: Arc<Inner>,
}

impl Body {
    fn poll(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Option<http_client::Result<Bytes>>> {
        if let Some(chunk) = self.first.take() {
            return Poll::Ready(Some(Ok(chunk)));
        }
        loop {
            match self.received.poll_next_unpin(cx) {
                Poll::Ready(Some(chunk)) => return Poll::Ready(Some(Ok(chunk))),
                // The channel closes when the send ends.
                Poll::Ready(None) => {
                    if let Some(error) = self.failure.take() {
                        return Poll::Ready(Some(Err(error)));
                    }
                    if !self.closed {
                        // A last event the server left without its blank line still counts, as
                        // it always did here; after a whole event this adds nothing.
                        self.closed = true;
                        return Poll::Ready(Some(Ok(Bytes::from_static(b"\n\n"))));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => {}
            }
            let Some(work) = self.sending.as_mut() else { return Poll::Pending };
            match work.poll_unpin(cx) {
                Poll::Ready(done) => {
                    self.sending = None;
                    match done {
                        Ok(reply) => self.inner.record(Exchange::Answered { status: reply.status, headers: reply.headers, body: Vec::new() }),
                        Err(e) => self.failure = Some(self.inner.fail(e)),
                    }
                }
                // A piece the send delivered just now is read on the next poll, so whoever drives
                // the reader gets a turn between pieces, as when the transport handed each over
                // itself. The channel wakes this task for it.
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Drop for Body {
    /// The crate stops reading at the end of the reply's last event, usually before the
    /// transport has ended the send. The send is kept, with the channel it still writes into, for
    /// [`RigHttp::finish`]; dropped unfinished, it would lose the reply's head and leave its
    /// delivery scope open.
    fn drop(&mut self) {
        let Some(sending) = self.sending.take() else { return };
        let received = std::mem::replace(&mut self.received, futures::channel::mpsc::unbounded().1);
        let work: BoxFuture<'static, Result<HttpResponse, ProviderError>> = Box::pin(async move {
            // Held so the sink can still deliver; what arrives now is no longer read.
            let _received = received;
            sending.await
        });
        *self.inner.unfinished.lock().unwrap_or_else(PoisonError::into_inner) = Some(work);
    }
}

#[cfg(test)]
mod tests;
