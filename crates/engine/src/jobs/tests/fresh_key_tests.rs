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
fn a_requeue_or_forget_starts_counting_again() {
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
