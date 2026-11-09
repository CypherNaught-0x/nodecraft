//! A template's `limits.concurrency` (#174): the broker never has more of its requests in flight,
//! across every caller sharing its gate, and a wait for a slot sends and bills nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::executor::block_on;

use super::*;
use crate::broker::RequestGate;
use crate::secrets::Secret;
use crate::{BoxFuture, Http, HttpRequest, HttpResponse, Provider, ProviderError, SubmitContext};

const KEY: &str = "broker-fixture-key-not-real";

/// A service that takes a while to answer each request and notes the most it was sent at once.
#[derive(Default)]
struct Slow {
    in_flight: Mutex<usize>,
    most: Mutex<usize>,
    sent: Mutex<usize>,
}

impl Slow {
    fn most(&self) -> usize {
        *self.most.lock().unwrap()
    }
    fn sent(&self) -> usize {
        *self.sent.lock().unwrap()
    }
}

impl Http for Slow {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("the broker never follows redirects")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        *self.sent.lock().unwrap() += 1;
        {
            let mut now = self.in_flight.lock().unwrap();
            *now += 1;
            let mut most = self.most.lock().unwrap();
            *most = (*most).max(*now);
        }
        let body = if r.url.ends_with("/jobs") { json!({"id": "j-1"}) } else { json!({"status": "running"}) };
        Box::pin(async move {
            // Each caller runs on its own thread, so blocking here only holds this request open.
            std::thread::sleep(Duration::from_millis(60));
            *self.in_flight.lock().unwrap() -= 1;
            Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
        })
    }
}

fn broker(http: Arc<Slow>) -> Broker {
    Broker::new("plugin.acme", template(), Some(Secret::new(KEY).unwrap()), http).unwrap()
}

fn handle() -> JobHandle {
    JobHandle { remote_id: "j-1".into(), data: json!({"model": "acme/v1"}) }
}

#[test]
fn a_run_holds_no_more_jobs_than_the_template_allows() {
    let b = broker(Arc::new(Slow::default()));
    assert_eq!(template().limits.concurrency, 2);
    assert_eq!(b.max_jobs_at_once(), Some(2), "the scheduler lane is capped at the template's limit");
    let one = decode(&with("/limits/concurrency", json!(1))).unwrap();
    assert_eq!(Broker::new("plugin.acme", one, Some(Secret::new(KEY).unwrap()), Arc::new(Slow::default())).unwrap().max_jobs_at_once(), Some(1));
}

/// N + 1 callers at once, submitting and polling through two brokers that share one gate as a
/// rebuilt broker does, never have more than N requests in flight.
#[test]
fn n_plus_one_requests_at_once_never_exceed_the_templates_limit() {
    let limit = usize::try_from(template().limits.concurrency).unwrap();
    let http = Arc::new(Slow::default());
    let first = Arc::new(broker(http.clone()));
    let rebuilt = Arc::new(broker(http.clone()).gated(Arc::clone(first.gate())));
    assert!(Arc::ptr_eq(first.gate(), rebuilt.gate()));
    // N + 1 submits and N + 1 polls, half of them on each broker.
    let callers: Vec<_> = (0..=limit)
        .flat_map(|n| [(n, true), (n, false)])
        .map(|(n, submit)| {
            let b = Arc::clone(if n % 2 == 0 { &first } else { &rebuilt });
            std::thread::spawn(move || if submit { block_on(b.submit(&job())).map(drop) } else { block_on(b.poll(&handle())).map(drop) })
        })
        .collect();
    let count = callers.len();
    for caller in callers {
        caller.join().unwrap().unwrap();
    }
    assert_eq!(http.sent(), count, "every request was sent in the end");
    assert!(count > limit);
    assert!(http.most() <= limit, "{} requests were in flight at once; the template allows {limit}", http.most());
    assert_eq!(first.gate().held(), 0, "every slot was given back");
}

#[test]
fn a_gate_of_another_size_is_not_shared() {
    let b = broker(Arc::new(Slow::default()));
    let other = RequestGate::new(4);
    assert!(!Arc::ptr_eq(b.gated(Arc::clone(&other)).gate(), &other), "a gate sized for another template adds slots");
}

/// A submit waiting for a slot has sent nothing: a wait given up is not marked as possibly
/// billed, and a grant withdrawn while it waits stops it before the paid request.
#[test]
fn a_submit_waiting_for_a_slot_sends_and_bills_nothing() {
    let http = Arc::new(Slow::default());
    let granted = Arc::new(AtomicBool::new(true));
    let still = Arc::clone(&granted);
    let one = decode(&with("/limits/concurrency", json!(1))).unwrap();
    let b = Broker::new("plugin.acme", one, Some(Secret::new(KEY).unwrap()), http.clone()).unwrap().granted(Arc::new(move || still.load(Ordering::SeqCst)));
    let held = b.gate().acquire().now_or_never().expect("the only slot");

    // Given up while waiting.
    let scope = SubmitContext::new();
    assert!(b.submit_with_delivery(&job(), &scope).now_or_never().is_none(), "it waits for the slot");
    assert!(scope.finish_error().is_some(), "nothing was sent, so nothing was billed");

    // Withdrawn while waiting.
    let scope = SubmitContext::new();
    let queued = job();
    let mut waiting = b.submit_with_delivery(&queued, &scope);
    assert!((&mut waiting).now_or_never().is_none());
    granted.store(false, Ordering::SeqCst);
    drop(held);
    let error = block_on(waiting).unwrap_err();
    assert!(error.message.contains("nothing was sent"), "{}", error.message);
    assert!(scope.finish_error().is_some(), "refused before the paid request");
    assert_eq!(http.sent(), 0);
    assert_eq!(b.gate().held(), 0);
}
