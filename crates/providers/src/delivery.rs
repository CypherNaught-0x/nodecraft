//! Audited delivery evidence. Opaque contexts are supplied by the runner, never callers.
//! This is an audited adapter contract, not a sandbox for arbitrary Rust side effects.
use crate::http::{ChunkSink, Http, HttpRequest, HttpResponse};
use crate::{BoxFuture, ProviderError};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Default)]
struct Operation {
    armed: bool,
    unknown: bool,
    /// A paid request was sent through the adapter's own transport (#631).
    paid: bool,
    /// The provider answered that paid request with a documented refusal.
    refused: bool,
    children: usize,
    closed: bool,
}
/// An invocation-local scope. Only audited in-crate adapters can arm positive evidence.
/// ```compile_fail
/// let scope = nodecraft_providers::SubmitContext::new();
/// ```
/// ```compile_fail
/// fn copy(scope: &nodecraft_providers::SubmitContext) -> nodecraft_providers::SubmitContext {
///     Clone::clone(scope)
/// }
/// ```
pub struct SubmitContext {
    state: Arc<Mutex<Operation>>,
    /// Text streamed so far and not yet reported, when the runner asked for it (#504).
    text: Option<Mutex<Streamed>>,
    /// Where a paid request is saved before it is sent, when the runner keeps them (#664).
    stager: Option<Arc<crate::Stager>>,
    /// The latest preview of a result still arriving and not yet reported, when the runner asked
    /// for them (#671). A newer one replaces it.
    preview: Option<Mutex<Option<crate::Output>>>,
}

/// Most bytes of streamed text one submit reports; the rest of a longer answer arrives only with
/// the result.
pub const MAX_STREAMED_TEXT: usize = 256 * 1024;
/// Most bytes of text one report carries.
pub const MAX_TEXT_DELTA: usize = 64 * 1024;

#[derive(Default)]
struct Streamed {
    pending: String,
    /// Bytes taken so far, pending included.
    total: usize,
}
/// A request-local scope; it cannot be promoted into a whole-operation assertion by callers.
pub struct RequestContext {
    root: Arc<Mutex<Operation>>,
    id: Arc<()>,
    state: Mutex<RequestState>,
}
#[derive(Default)]
struct RequestState {
    armed: bool,
    unknown: bool,
    closed: bool,
    issued: bool,
}
struct NotSent {
    root: Arc<Mutex<Operation>>,
    child: Arc<()>,
}
/// A diagnostic with opaque, request-local evidence. External implementations can report only
/// unknown delivery. Positive evidence is neither Clone nor publicly constructible.
/// ```compile_fail
/// let failure = nodecraft_providers::HttpFailure::unknown(
///     nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::Network, "fixture"));
/// let reused = failure.clone();
/// ```
pub struct HttpFailure {
    error: ProviderError,
    proof: Option<NotSent>,
}
impl std::fmt::Debug for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFailure").field("error", &self.error).field("not_sent", &self.proof.is_some()).finish()
    }
}
impl HttpFailure {
    pub fn unknown(error: ProviderError) -> Self {
        Self { error, proof: None }
    }
    pub fn error(&self) -> &ProviderError {
        &self.error
    }
    pub fn into_error(self) -> ProviderError {
        self.error
    }
}
/// Only the runner consumes this one-shot result; there is no public whole-submit constructor.
pub(crate) struct SubmitNotAccepted;

impl SubmitContext {
    pub(crate) fn new() -> Self {
        Self { state: Arc::default(), text: None, stager: None, preview: None }
    }
    /// A context that collects a text answer as it streams, for [`Self::take_text`].
    pub(crate) fn streaming_text() -> Self {
        Self { state: Arc::default(), text: Some(Mutex::default()), stager: None, preview: None }
    }
    /// Saves staged requests with `stager` (see [`Self::stage`]).
    pub(crate) fn with_stager(mut self, stager: Option<Arc<crate::Stager>>) -> Self {
        self.stager = stager;
        self
    }
    /// Saves the paid request about to be sent, before [`Self::paid`], so it can be sent again
    /// unchanged after a restart. Without a stager nothing is saved. An error means it couldn't
    /// be saved: the adapter must return it without sending anything.
    pub(crate) fn stage(&self, staged: &crate::StagedSubmit) -> Result<(), ProviderError> {
        let Some(stager) = &self.stager else { return Ok(()) };
        if staged.body.len() > crate::MAX_STAGED_BODY {
            return Err(ProviderError::new(crate::ErrorKind::UnsupportedInput, "the request is too large to save before sending it, so it wasn't sent"));
        }
        stager(staged).map_err(|reason| ProviderError::new(crate::ErrorKind::UnsupportedInput, reason))
    }
    /// Collects previews of a result while it arrives, for [`Self::take_preview`], when `on`.
    pub(crate) fn with_previews(mut self, on: bool) -> Self {
        self.preview = on.then(Mutex::default);
        self
    }
    /// Whether the runner wants previews of a result while it arrives.
    pub(crate) fn wants_previews(&self) -> bool {
        self.preview.is_some()
    }
    /// Keeps `preview` as the latest one, replacing any not yet reported.
    pub(crate) fn push_preview(&self, preview: crate::Output) {
        let Some(slot) = &self.preview else { return };
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(preview);
    }
    /// The latest preview since the last call, if there is a newer one.
    pub(crate) fn take_preview(&self) -> Option<crate::Output> {
        self.preview.as_ref()?.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
    /// Whether the runner wants text as it streams.
    pub(crate) fn wants_text(&self) -> bool {
        self.text.is_some()
    }
    /// Keeps a piece of streamed text, up to [`MAX_STREAMED_TEXT`] in all.
    pub(crate) fn push_text(&self, piece: &str) {
        let Some(text) = &self.text else { return };
        let mut text = text.lock().unwrap_or_else(PoisonError::into_inner);
        let room = MAX_STREAMED_TEXT.saturating_sub(text.total);
        let piece = piece.get(..floor_char_boundary(piece, room)).unwrap_or_default();
        text.total = text.total.saturating_add(piece.len());
        text.pending.push_str(piece);
    }
    /// The text streamed since the last call, in pieces of at most [`MAX_TEXT_DELTA`] bytes.
    pub(crate) fn take_text(&self) -> Vec<String> {
        let Some(text) = &self.text else { return Vec::new() };
        let mut pending = std::mem::take(&mut text.lock().unwrap_or_else(PoisonError::into_inner).pending);
        let mut pieces = Vec::new();
        while !pending.is_empty() {
            let mut end = floor_char_boundary(&pending, MAX_TEXT_DELTA);
            if end == 0 {
                // A char wider than the delta can't happen (chars are at most 4 bytes); take one.
                end = pending.chars().next().map_or(pending.len(), char::len_utf8);
            }
            let rest = pending.split_off(end);
            pieces.push(pending);
            pending = rest;
        }
        pieces
    }
    pub(crate) fn arm(&self) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !s.closed {
            s.armed = true;
        }
    }
    pub(crate) fn unknown(&self) {
        taint(&self.state);
    }
    /// Marks the paid request an audited adapter is about to send itself. Until
    /// [`Self::refused`] reports the provider's documented refusal of it, a failure may hide an
    /// accepted, billed job. Unlike [`Self::unknown`] this mark can be cleared, so only an adapter
    /// that sends exactly one paid request after it may use it: any later request it sends itself
    /// must call [`Self::unknown`] first, or a refusal of the earlier one would certify it.
    pub(crate) fn paid(&self) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !s.closed {
            s.paid = true;
            s.refused = false;
        }
    }
    /// The paid request came back with a parsed refusal from the provider's documented list of
    /// replies that create no job (#631). Call it only after matching such a reply: a transport
    /// error, a 5xx, an oversized or unparsed body stays possibly billed. It clears only the
    /// [`Self::paid`] mark; anything [`Self::unknown`] or a request child recorded stays.
    pub(crate) fn refused(&self) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !s.closed && s.paid {
            s.refused = true;
        }
    }
    pub(crate) fn finish_error(self) -> Option<SubmitNotAccepted> {
        let mut s = match self.state.lock() {
            Ok(s) => s,
            Err(e) => {
                let mut s = e.into_inner();
                s.unknown = true;
                s
            }
        };
        let proven = !s.closed && s.armed && !s.unknown && (!s.paid || s.refused) && s.children == 0;
        s.closed = true;
        proven.then_some(SubmitNotAccepted)
    }
    // Register before even constructing the transport future. Unpolled/dropped children count.
    pub(crate) fn send<'a>(&'a self, http: &'a dyn Http, request: HttpRequest) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        let child = Child::new(Arc::clone(&self.state));
        let Some(child) = child else {
            return Box::pin(async { Err(crate::ProviderError::new(crate::ErrorKind::Network, "submission evidence scope is closed")) });
        };
        Box::pin(async move {
            let result = http.send_with_delivery(request, &child.context).await;
            child.finish(result)
        })
    }
}
impl SubmitContext {
    /// [`Self::send`] for a streamed reply: the body goes to `on_chunk` as it arrives.
    pub(crate) fn send_streaming<'a>(
        &'a self,
        http: &'a dyn Http,
        request: HttpRequest,
        on_chunk: &'a mut ChunkSink<'a>,
    ) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        let child = Child::new(Arc::clone(&self.state));
        let Some(child) = child else {
            return Box::pin(async { Err(crate::ProviderError::new(crate::ErrorKind::Network, "submission evidence scope is closed")) });
        };
        Box::pin(async move {
            let result = http.send_streaming_with_delivery(request, on_chunk, &child.context).await;
            child.finish(result)
        })
    }
}

/// The largest char boundary in `text` at or below `at`.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    (0..=at).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0)
}

fn taint(root: &Arc<Mutex<Operation>>) {
    let mut s = root.lock().unwrap_or_else(PoisonError::into_inner);
    if !s.closed {
        s.unknown = true;
    }
}
impl RequestContext {
    pub(crate) fn arm(&self) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !s.closed {
            s.armed = true;
        }
    }
    pub(crate) fn unknown(&self) {
        taint(&self.root);
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !s.closed {
            s.unknown = true;
        }
    }
    pub(crate) fn failure(&self, error: ProviderError) -> HttpFailure {
        let mut s = match self.state.lock() {
            Ok(s) => s,
            Err(e) => {
                let mut s = e.into_inner();
                s.unknown = true;
                drop(s);
                self.unknown();
                return HttpFailure::unknown(error);
            }
        };
        let proof = if s.armed && !s.unknown && !s.closed && !s.issued {
            s.issued = true;
            Some(NotSent { root: Arc::clone(&self.root), child: Arc::clone(&self.id) })
        } else {
            None
        };
        HttpFailure { error, proof }
    }
}
struct Child {
    context: RequestContext,
    finished: bool,
}
impl Child {
    fn new(root: Arc<Mutex<Operation>>) -> Option<Self> {
        {
            let mut s = match root.lock() {
                Ok(s) => s,
                Err(e) => {
                    let mut s = e.into_inner();
                    s.unknown = true;
                    s
                }
            };
            if s.closed {
                return None;
            }
            let Some(children) = s.children.checked_add(1) else {
                s.unknown = true;
                return None;
            };
            s.children = children;
        }
        Some(Self { context: RequestContext { root, id: Arc::new(()), state: Mutex::default() }, finished: false })
    }
    fn finish(mut self, result: Result<HttpResponse, HttpFailure>) -> Result<HttpResponse, ProviderError> {
        let not_sent = match &result {
            Err(f) => f.proof.as_ref().is_some_and(|p| Arc::ptr_eq(&p.root, &self.context.root) && Arc::ptr_eq(&p.child, &self.context.id)),
            Ok(_) => false,
        };
        let clean = match self.context.state.lock() {
            Ok(mut s) => {
                let clean = s.armed && !s.unknown && !s.closed;
                s.closed = true;
                clean
            }
            Err(e) => {
                e.into_inner().closed = true;
                false
            }
        };
        {
            let mut s = match self.context.root.lock() {
                Ok(s) => s,
                Err(e) => {
                    let mut s = e.into_inner();
                    s.unknown = true;
                    s
                }
            };
            if !s.closed {
                s.unknown |= !not_sent || !clean;
                match s.children.checked_sub(1) {
                    Some(n) => s.children = n,
                    None => s.unknown = true,
                }
            }
        }
        self.finished = true;
        result.map_err(HttpFailure::into_error)
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut s = self.context.root.lock().unwrap_or_else(PoisonError::into_inner);
        if !s.closed {
            s.unknown = true;
            match s.children.checked_sub(1) {
                Some(n) => s.children = n,
                None => s.unknown = true,
            }
        }
    }
}

#[cfg(test)]
mod tests;
