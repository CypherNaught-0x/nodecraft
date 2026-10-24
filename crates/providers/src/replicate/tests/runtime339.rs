use super::*;
use crate::{CancelSignal, Clock, RunLimits, RunUpdate};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct Time(AtomicU64);
impl Clock for Time {
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
        self.0.fetch_add(ms, Ordering::SeqCst);
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct NotCancelled;
impl CancelSignal for NotCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
}
fn deadline(request: &HttpRequest) -> Option<&str> {
    request.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("cancel-after")).map(|(_, value)| value.as_str())
}
fn metrics(mut reply: HttpResponse, raw: &str) -> HttpResponse {
    assert_eq!(reply.body.pop(), Some(b'}'));
    reply.body.extend_from_slice(format!(",\"metrics\":{raw}}}").as_bytes());
    reply
}

#[test]
fn runtime339_real_runner_sends_bounded_server_lifetime_only_at_creation() {
    for (milliseconds, header) in [(5000, "5s"), (5001, "6s"), (86_400_000, "86400s")] {
        let (provider, http) = fixture(vec![
            prediction(HELLO, "starting", serde_json::Value::Null),
            metrics(prediction(HELLO, "succeeded", serde_json::json!("result")), r#"{"predict_time":1.234,"total_time":8}"#),
        ]);
        let mut updates = Vec::new();
        let done = block_on(crate::run_to_completion(
            &provider,
            &job(HELLO),
            &NotCancelled,
            &Time::default(),
            RunLimits { deadline_ms: milliseconds, ..Default::default() },
            |update| updates.push(update),
        ))
        .unwrap();
        assert!(matches!(updates.first(), Some(RunUpdate::Submitted(_))));
        assert_eq!(done.usage.execution_ms, Some(1234));
        assert_eq!((done.usage.cost, done.usage.input_tokens, done.usage.output_tokens, done.usage.billable_units), (None, None, None, None));
        let requests = http.seen.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(deadline(&requests[0]), Some(header));
        assert_eq!(deadline(&requests[1]), None, "polling must not restart the server lifetime");
    }
}

#[test]
fn runtime339_unsupported_lifetimes_are_proven_unsent_without_allocating_a_request() {
    for milliseconds in [0, 4999, 86_400_001, u64::MAX] {
        let (provider, http) = fixture(vec![]);
        let failure = block_on(crate::run_to_completion(
            &provider,
            &job(HELLO),
            &NotCancelled,
            &Time::default(),
            RunLimits { deadline_ms: milliseconds, ..Default::default() },
            |_| {},
        ))
        .unwrap_err();
        assert_eq!(failure.error.kind, ErrorKind::UnsupportedInput);
        assert!(!failure.uncertain);
        assert!(http.seen.lock().unwrap().is_empty());
    }
}

#[test]
fn runtime339_direct_submit_uses_default_but_restored_poll_download_and_cancel_never_reset_it() {
    let (provider, http) = fixture(vec![
        prediction(SDXL, "starting", serde_json::Value::Null),
        prediction(SDXL, "succeeded", serde_json::json!(["https://replicate.delivery/result.png"])),
        bytes(png(1, 1)),
        prediction(SDXL, "canceled", serde_json::Value::Null),
    ]);
    let handle = handle(&provider, &job(SDXL));
    let done = block_on(crate::resume(&provider, &handle, &NotCancelled, &Time::default(), RunLimits::default(), |_| {})).unwrap();
    assert_eq!(done.outputs.len(), 1);
    block_on(provider.cancel(&handle)).unwrap();
    let requests = http.seen.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(deadline(&requests[0]), Some("3600s"));
    assert!(requests.iter().skip(1).all(|request| deadline(request).is_none()));
}

#[test]
fn runtime339_optional_execution_metrics_never_become_money_or_discard_valid_outputs() {
    for (raw, expected) in [
        (r#"{"predict_time":0}"#, Some(0)),
        (r#"{"predict_time":0.0006}"#, Some(1)),
        (r#"{"predict_time":86400}"#, Some(86_400_000)),
        (r#"{"predict_time":1.25e1}"#, Some(12_500)),
        (r#"{"total_time":99}"#, None),
        (r#"{"predict_time":null}"#, None),
        (r#"{"predict_time":"1.2"}"#, None),
        (r#"{"predict_time":-1}"#, None),
        (r#"{"predict_time":86401}"#, None),
        (r#"{"predict_time":1e300}"#, None),
        (r#"{"predict_time":1,"predict_time":2}"#, None),
        (r#"[1.234]"#, None),
        (r#"null"#, None),
    ] {
        let (provider, _) =
            fixture(vec![prediction(HELLO, "starting", serde_json::Value::Null), metrics(prediction(HELLO, "succeeded", serde_json::json!("retained")), raw)]);
        let handle = handle(&provider, &job(HELLO));
        let JobProgress::Done(done) = block_on(provider.poll(&handle)).unwrap() else { panic!("completed output") };
        assert_eq!(done.outputs, [Output::Text("retained".into())], "{raw}");
        assert_eq!(done.usage.execution_ms, expected, "{raw}");
        assert!(done.usage.cost.is_none());
    }
}
