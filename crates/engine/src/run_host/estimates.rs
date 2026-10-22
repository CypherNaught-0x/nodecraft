//! The [`ESTIMATE_SERVICE`] (#663): `run.estimate` requests wait here until the next pump builds
//! their jobs and asks each provider for a quote on the runtime. Quotes that come back are kept
//! as the session's [`crate::provider_quotes::ProviderQuotes`]; nothing is sent to be run and no
//! idempotency key is used.
use super::{RunNotice, lock};
use crate::Session;
use crate::commands::Services;
use crate::provider_quotes::{ESTIMATE_SERVICE, QUOTE_TTL_MS, QuoteKey};
use nodecraft_exec::driver::Runtime;
use nodecraft_types::{CanvasId, Error, ErrorKind, Money};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Most `run.estimate` requests waiting for a pump, or being answered.
pub const MAX_ESTIMATES: usize = 4;
/// How long the providers have to answer one request; what hasn't answered by then is unquoted.
pub const ESTIMATE_TIMEOUT_MS: u64 = 60_000;

/// One request being answered.
struct Asking {
    request: u64,
    canvas: CanvasId,
    routing: Option<[u8; 32]>,
    started_ms: u64,
    /// Jobs not yet answered.
    remaining: usize,
    quoted: Vec<(QuoteKey, Money)>,
    /// Jobs the provider couldn't quote, or that weren't asked about.
    unquoted: usize,
}

#[derive(Default)]
struct Shared {
    /// With the id the command answered.
    waiting: VecDeque<(u64, Value)>,
    asking: BTreeMap<u64, Asking>,
    next_request: u64,
}

/// See the module docs.
#[derive(Default)]
pub(super) struct Estimates {
    shared: Arc<Mutex<Shared>>,
    next: u64,
}

impl Estimates {
    pub(super) fn register(&self, services: &mut Services) -> Result<(), Error> {
        let shared = Arc::clone(&self.shared);
        services.register(ESTIMATE_SERVICE, move |request| {
            let mut shared = lock(&shared);
            if shared.waiting.len().saturating_add(shared.asking.len()) >= MAX_ESTIMATES {
                return Err(Error::new(ErrorKind::Busy, "too many price requests are in progress; try again when one is answered"));
            }
            let id = shared.next_request;
            shared.next_request = id.wrapping_add(1);
            shared.waiting.push_back((id, request.clone()));
            Ok(json!({"accepted": true, "request": id}))
        })
    }

    /// Whether a request is waiting or being answered.
    pub(super) fn is_busy(&self) -> bool {
        let shared = lock(&self.shared);
        !shared.waiting.is_empty() || !shared.asking.is_empty()
    }

    /// Drops the session's stale quotes, asks about the requests that arrived, and keeps the
    /// answers to those that are done.
    pub(super) fn pump(&mut self, session: &mut Session, runtime: &Arc<dyn Runtime>, notices: &mut Vec<RunNotice>) {
        let now = runtime.now_ms();
        session.expire_provider_quotes(now.saturating_sub(QUOTE_TTL_MS));
        let waiting = std::mem::take(&mut lock(&self.shared).waiting);
        for (request_id, request) in waiting {
            match session.provider_estimates(&request) {
                Ok(estimating) => {
                    let id = self.next;
                    self.next = self.next.wrapping_add(1);
                    let asking = Asking {
                        request: request_id,
                        canvas: estimating.canvas,
                        routing: estimating.routing,
                        started_ms: now,
                        remaining: estimating.jobs.len(),
                        quoted: Vec::new(),
                        unquoted: estimating.skipped,
                    };
                    lock(&self.shared).asking.insert(id, asking);
                    for job in estimating.jobs {
                        let shared = Arc::clone(&self.shared);
                        runtime.spawn(Box::pin(async move {
                            let price = job.provider.quote(&job.job).await.ok();
                            let mut shared = lock(&shared);
                            // Gone when it timed out.
                            let Some(asking) = shared.asking.get_mut(&id) else { return };
                            asking.remaining = asking.remaining.saturating_sub(1);
                            match price {
                                Some(price) => asking.quoted.push((job.key, price)),
                                None => asking.unquoted = asking.unquoted.saturating_add(1),
                            }
                        }));
                    }
                }
                Err(error) => notices.push(RunNotice::NotEstimated { request: request_id, reason: error.message }),
            }
        }
        let done: Vec<Asking> = {
            let mut shared = lock(&self.shared);
            let ids: Vec<u64> = shared
                .asking
                .iter()
                .filter(|(_, asking)| asking.remaining == 0 || now.saturating_sub(asking.started_ms) >= ESTIMATE_TIMEOUT_MS)
                .map(|(id, _)| *id)
                .collect();
            ids.iter().filter_map(|id| shared.asking.remove(id)).collect()
        };
        for asking in done {
            let quoted = asking.quoted.len();
            if quoted > 0 {
                session.keep_provider_quotes(asking.routing, now, asking.quoted);
            }
            let unquoted = asking.unquoted.saturating_add(asking.remaining);
            notices.push(RunNotice::Estimated { request: asking.request, canvas: asking.canvas, quoted, unquoted });
        }
    }
}
