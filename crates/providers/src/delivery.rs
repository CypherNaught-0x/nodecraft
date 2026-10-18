//! Audited delivery evidence. Opaque contexts are supplied by the runner, never callers.
//! This is an audited adapter contract, not a sandbox for arbitrary Rust side effects.
use crate::http::{Http, HttpRequest, HttpResponse};
use crate::{BoxFuture, ProviderError};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Default)]
struct Operation {
    armed: bool,
    unknown: bool,
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
        Self { state: Arc::default() }
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
    pub(crate) fn finish_error(self) -> Option<SubmitNotAccepted> {
        let mut s = match self.state.lock() {
            Ok(s) => s,
            Err(e) => {
                let mut s = e.into_inner();
                s.unknown = true;
                s
            }
        };
        let proven = !s.closed && s.armed && !s.unknown && s.children == 0;
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
