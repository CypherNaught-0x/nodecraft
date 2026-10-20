//! A text job's answer streams through the runner as it arrives (#504).
use super::*;
use crate::http::{ChunkSink, HttpResponse};
use crate::provider::{CancelSignal, Clock};
use crate::{RunLimits, RunUpdate, run_to_completion};
use futures::executor::block_on;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

/// Streams `chunks` one at a time, letting the caller run between them; raises `cancel` once
/// `cancel_after` chunks have gone, and then stalls.
struct Trickle {
    chunks: Vec<&'static str>,
    cancel_after: Option<usize>,
    cancel: Arc<Flag>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Http for Trickle {
    fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "only streamed requests are expected")) })
    }
    fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        Box::pin(async move {
            for (sent, chunk) in self.chunks.iter().enumerate() {
                if self.cancel_after == Some(sent) {
                    // Nothing more arrives, so only the cancel can end the race.
                    self.cancel.0.store(true, Ordering::SeqCst);
                    futures::future::pending::<()>().await;
                }
                on_chunk(chunk.as_bytes())?;
                yield_once().await;
            }
            Ok(HttpResponse { status: 200, headers: vec![], body: Vec::new() })
        })
    }
}

/// Pending once, so whoever polls gets to run.
async fn yield_once() {
    let mut yielded = false;
    futures::future::poll_fn(|cx| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
}

#[derive(Default)]
struct Flag(AtomicBool);

impl CancelSignal for Flag {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(futures::future::poll_fn(|cx| {
            if self.is_cancelled() {
                return Poll::Ready(());
            }
            cx.waker().wake_by_ref();
            Poll::Pending
        }))
    }
}

struct Still;

impl Clock for Still {
    fn sleep(&self, _ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

const ANSWER: [&str; 2] = [
    "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
];

fn job() -> ProviderJob {
    ProviderJob {
        model: "m".into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "hi".into() }],
        idempotency_key: "k".into(),
    }
}

fn run(http: Arc<Trickle>, stream_text: bool) -> (Result<Completed, crate::RunFailure>, Vec<RunUpdate>) {
    let client = LlmClient::new(Endpoint::openai(ApiKey::new("k")), http.clone());
    let mut updates = Vec::new();
    let limits = RunLimits { stream_text, ..RunLimits::default() };
    let result = block_on(run_to_completion(&client, &job(), http.cancel.as_ref(), &Still, limits, |u| updates.push(u)));
    (result, updates)
}

fn trickle(cancel_after: Option<usize>) -> Arc<Trickle> {
    Arc::new(Trickle { chunks: ANSWER.to_vec(), cancel_after, cancel: Arc::default(), seen: Mutex::default() })
}

#[test]
fn a_streamed_answer_is_reported_piece_by_piece_and_arrives_whole() {
    let http = trickle(None);
    let (result, updates) = run(http.clone(), true);
    // Each piece is reported when it arrives, not gathered up at the end.
    assert_eq!(updates, [RunUpdate::Text("Hel".into()), RunUpdate::Text("lo".into())]);
    let done = result.unwrap();
    assert!(matches!(done.outputs.as_slice(), [Output::Text(text)] if text == "Hello"));
    let body: Value = serde_json::from_slice(&http.seen.lock().unwrap()[0].body).unwrap();
    assert_eq!(body["stream"], true);
}

#[test]
fn a_cancel_mid_stream_keeps_what_streamed_and_may_be_billed() {
    let (result, updates) = run(trickle(Some(1)), true);
    assert_eq!(updates, [RunUpdate::Text("Hel".into())]);
    let failure = result.unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Cancelled);
    assert!(failure.uncertain, "the request was out, so it may be billed");
}

#[test]
fn without_streaming_asked_for_nothing_is_streamed() {
    let canned = tests::Canned::new(200, r#"{"choices":[{"message":{"role":"assistant","content":"Hello"},"finish_reason":"stop"}]}"#);
    let client = LlmClient::new(Endpoint::openai(ApiKey::new("k")), canned.clone());
    let mut updates = Vec::new();
    let done = block_on(run_to_completion(&client, &job(), &Flag::default(), &Still, RunLimits::default(), |u| updates.push(u))).unwrap();
    assert!(updates.is_empty());
    assert!(matches!(done.outputs.as_slice(), [Output::Text(text)] if text == "Hello"));
    assert_ne!(canned.body()["stream"], true);
}
