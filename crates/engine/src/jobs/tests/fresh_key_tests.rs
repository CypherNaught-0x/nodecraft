//! A retry after a provider-reported failure is a new generation, sent under a derived key (#223).

use super::*;
use std::sync::Mutex as StdMutex;

/// Records every submitted key. Polls follow a script: `Failed` (the provider ran and failed the
/// generation), `Lost` (a network error: the reply never arrived), else `Done`.
#[derive(Clone, Copy)]
enum Poll {
    Failed,
    Lost,
    Done,
}

struct Dedupes {
    keys: StdMutex<Vec<String>>,
    polls: StdMutex<Vec<Poll>>,
}

impl Dedupes {
    fn new(script: Vec<Poll>) -> Arc<Self> {
        Arc::new(Self { keys: StdMutex::default(), polls: StdMutex::new(script.into_iter().rev().collect()) })
    }
    fn keys(&self) -> Vec<String> {
        self.keys.lock().unwrap().clone()
    }
}

impl Provider for Dedupes {
    fn id(&self) -> &str {
        "dedupes"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        let key = job.idempotency_key.clone();
        self.keys.lock().unwrap().push(key.clone());
        Box::pin(async move { Ok(Submission::Queued(JobHandle { remote_id: key, data: serde_json::Value::Null })) })
    }
    fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let next = self.polls.lock().unwrap().pop().unwrap_or(Poll::Done);
        Box::pin(async move {
            match next {
                Poll::Failed => Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Server, "out of memory"))),
                Poll::Lost => Err(ProviderError::new(ErrorKind::Network, "reset")),
                Poll::Done => Ok(JobProgress::Done(Completed::new(vec![Output::Text("ok".into())]))),
            }
        })
    }
    fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn a_retry_after_a_provider_failure_is_sent_under_a_fresh_key() {
    let provider = Dedupes::new(vec![Poll::Failed, Poll::Done]);
    let (runner, budget) = budgeted(Some(1_000_000));
    runner.register(JobId(1), priced(provider.clone(), Some(100_000)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(block_on(runner.run(Start { attempt: 2, ..job(1) }, CancelToken::default())), Outcome::Done);
    let keys = provider.keys();
    assert_eq!(keys, ["nodecraft:r:1", "nodecraft:r:1#1"], "two generations, two keys");
    assert!(keys.iter().all(|k| k.is_ascii() && k.len() <= MAX_SENT_KEY));
    // The usage log charges each generation under the key its provider saw.
    runner.forget(JobId(1));
    let log = budget.log.lock().unwrap();
    let charged: Vec<&str> = log.entries().map(|e| e.idempotency_key.as_str()).collect();
    assert_eq!(charged, ["nodecraft:r:1", "nodecraft:r:1#1"]);
}

#[test]
fn a_lost_reply_on_the_fresh_generation_resumes_it_under_the_same_key() {
    // Failed generation; the new one loses its replies past the poll-error limit, then resumes.
    let provider = Dedupes::new(vec![Poll::Failed, Poll::Lost, Poll::Lost, Poll::Done]);
    let (runner, _budget) = budgeted(Some(1_000_000));
    runner.register(JobId(1), priced(provider.clone(), Some(100_000)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { .. }));
    assert!(matches!(block_on(runner.run(Start { attempt: 2, ..job(1) }, CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(block_on(runner.run(Start { attempt: 3, ..job(1) }, CancelToken::default())), Outcome::Done);
    assert_eq!(provider.keys(), ["nodecraft:r:1", "nodecraft:r:1#1"], "resumed, not submitted a third time");
}

#[test]
fn a_requeue_starts_counting_again() {
    let provider = Dedupes::new(vec![Poll::Failed, Poll::Done, Poll::Done]);
    let (runner, _budget) = budgeted(Some(1_000_000));
    runner.register(JobId(1), priced(provider.clone(), Some(10)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { .. }));
    // Requeued under a fresh scheduler key: that key starts at no failures.
    let fresh = Start { idempotency_key: "nodecraft:r2:1".into(), ..job(1) };
    assert_eq!(block_on(runner.run(fresh, CancelToken::default())), Outcome::Done);
    assert_eq!(provider.keys(), ["nodecraft:r:1", "nodecraft:r2:1"]);
    assert!(runner.lock().failures.is_empty() && runner.lock().handles.is_empty(), "nothing left behind");
}

#[test]
fn derived_keys_are_deterministic_ascii_and_bounded() {
    assert_eq!(sent_key("nodecraft:r:1", 0), "nodecraft:r:1");
    assert_eq!(sent_key("nodecraft:r:1", 3), "nodecraft:r:1#3");
    let long = format!("nodecraft:{}:{}", "a".repeat(40), u64::MAX);
    let first = sent_key(&long, 1);
    assert_eq!(first, sent_key(&long, 1), "deterministic");
    assert_ne!(first, sent_key(&long, 2));
    assert_ne!(sent_key(&long, 0), sent_key(&format!("{long}x"), 0), "different bases stay apart");
    for key in [first, sent_key(&long, u32::MAX), sent_key("nodecraft:ü:ß", 1), sent_key(&"é".repeat(100), 0)] {
        assert!(key.is_ascii() && key.len() <= MAX_SENT_KEY, "{key}");
    }
}

/// Submits wait until `open`; every poll reports the generation failed, except under a derived key.
struct Gated {
    open: std::sync::atomic::AtomicBool,
    keys: StdMutex<Vec<String>>,
}

impl Provider for Gated {
    fn id(&self) -> &str {
        "gated"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        let key = job.idempotency_key.clone();
        self.keys.lock().unwrap().push(key.clone());
        Box::pin(futures::future::poll_fn(move |_| {
            if self.open.load(Ordering::SeqCst) {
                std::task::Poll::Ready(Ok(Submission::Queued(JobHandle { remote_id: key.clone(), data: serde_json::Value::Null })))
            } else {
                std::task::Poll::Pending
            }
        }))
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let fresh = h.remote_id.contains('#');
        Box::pin(async move {
            if fresh {
                Ok(JobProgress::Done(Completed::new(vec![Output::Text("ok".into())])))
            } else {
                Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Server, "out of memory")))
            }
        })
    }
    fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn shared_active_key_is_busy_and_late_older_failure_cannot_regress_restored_generation() {
    let provider = Arc::new(Gated { open: Default::default(), keys: StdMutex::default() });
    let (runner, budget) = budgeted(Some(1_000_000));
    let shared = |id| Start { idempotency_key: "shared".into(), ..job(id) };
    for id in [1, 2] {
        runner.register(JobId(id), priced(provider.clone(), Some(10)));
    }
    let mut old = runner.run(shared(1), CancelToken::default());
    let waker = futures::task::noop_waker();
    assert!(old.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
    assert!(matches!(block_on(runner.run(shared(2), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(*provider.keys.lock().unwrap(), ["shared"], "duplicate must not invoke provider");
    // Explicit host restore ordering, not two normally admitted concurrent same-key runs:
    // restoring generation3 while generation0 is pending must survive that older failure.
    runner.restore_generation("gated", "shared".into(), 3, JobHandle { remote_id: "shared#3".into(), data: serde_json::Value::Null });
    provider.open.store(true, Ordering::SeqCst);
    assert!(matches!(block_on(old), Outcome::Failed { .. }));
    assert_eq!(runner.lock().failures.get(&("gated".into(), "shared".into())), Some(&3), "max(existing, captured+1), not regression or skipped generation");
    assert_eq!(block_on(runner.run(shared(2), CancelToken::default())), Outcome::Done);
    assert_eq!(*provider.keys.lock().unwrap(), ["shared"], "restored generation resumes without submit");
    assert_eq!(budget.log.lock().unwrap().charge("gated", "shared#3").unwrap().status, Charge::Actual);
}

#[test]
fn a_restored_generation_resumes_under_its_derived_key_after_a_restart() {
    // Generation 0 fails; generation 1 loses its replies, so its handle is what the host saved.
    let provider = Dedupes::new(vec![Poll::Failed, Poll::Lost, Poll::Lost, Poll::Done]);
    type Saved = Arc<StdMutex<Vec<(String, u32, JobHandle)>>>;
    let saved: Saved = Arc::default();
    let sink = Arc::clone(&saved);
    let before = ProviderRunner::new(
        Arc::new(InstantRuntime(AtomicU64::new(0))),
        RunLimits { max_poll_errors: 1, ..RunLimits::default() },
        Arc::new(|_, _| {}),
        Arc::new(move |_, sent: Sent<'_>, update| {
            if let RunUpdate::Submitted(handle) = update {
                sink.lock().unwrap().push((sent.key.to_owned(), sent.generation, handle));
            }
        }),
    );
    before.register(JobId(1), priced(provider.clone(), None));
    assert!(matches!(block_on(before.run(job(1), CancelToken::default())), Outcome::Failed { .. }));
    assert!(matches!(block_on(before.run(Start { attempt: 2, ..job(1) }, CancelToken::default())), Outcome::Failed { transient: true, .. }));
    let (key, generation, handle) = saved.lock().unwrap().last().cloned().unwrap();
    assert_eq!((key.as_str(), generation), ("nodecraft:r:1#1", 1));
    // A new runner after a restart: the host restores what it saved.
    let (after, budget) = budgeted(Some(1_000_000));
    after.register(JobId(1), priced(provider.clone(), Some(100_000)));
    after.restore_generation("dedupes", "nodecraft:r:1".into(), generation, handle);
    assert_eq!(block_on(after.run(Start { attempt: 3, ..job(1) }, CancelToken::default())), Outcome::Done);
    assert_eq!(provider.keys(), ["nodecraft:r:1", "nodecraft:r:1#1"], "resumed, not submitted again");
    let log = budget.log.lock().unwrap();
    assert_eq!(log.entries().map(|e| e.idempotency_key.as_str()).collect::<Vec<_>>(), ["nodecraft:r:1#1"], "charged under the key it was sent under");
}
