//! The machinery queue-style providers share: submit, poll until done, cancel, and download
//! outputs with size caps, type checks and content hashes.

use std::fmt;

use futures::FutureExt;

use crate::http::{Http, HttpRequest};
use crate::provider::{BoxFuture, CancelSignal, Clock, Completed, JobProgress, Provider, ProviderJob, Submission};
use crate::{ErrorKind, JobHandle, ProviderError};

/// How long a job may take overall before it counts as expired.
pub const DEFAULT_JOB_DEADLINE_MS: u64 = 60 * 60 * 1000;
/// How long asking the provider to cancel may take before we stop waiting for it.
const CANCEL_TIMEOUT_MS: u64 = 15_000;

/// Limits for [`run_to_completion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    /// Give up (as `ErrorKind::Expired`) once this much real time has passed since polling began.
    pub deadline_ms: u64,
    /// Cap on the poll interval the provider may ask for.
    pub max_poll_interval_ms: u64,
    /// Consecutive transient poll failures tolerated (each retried after backoff).
    pub max_poll_errors: u32,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self { deadline_ms: DEFAULT_JOB_DEADLINE_MS, max_poll_interval_ms: 30_000, max_poll_errors: 5 }
    }
}

/// What [`run_to_completion`] reports while it waits.
#[derive(Debug, Clone, PartialEq)]
pub enum RunUpdate {
    /// The job is with the provider: persist the handle, so a restart or retry resumes it.
    Submitted(JobHandle),
    Progress(JobProgress),
    /// A poll failed and will be retried (shown as "connection lost, retrying").
    PollRetry(ProviderError),
}

/// A failed run. When `handle` is set the job reached the provider (and may be billed, or still
/// running): a retry must [`resume`] that handle rather than submit again, or it pays twice.
#[derive(Debug, Clone, PartialEq)]
pub struct RunFailure {
    pub error: ProviderError,
    pub handle: Option<JobHandle>,
    /// The provider itself reported the job as failed: it is over, and resuming it would only
    /// read the same failure again, so a retry must submit afresh.
    pub finished: bool,
    /// The submit was interrupted with no handle back (cancelled midway, timed out, cut off, a
    /// server error, a garbled reply, or one saying the job ran): the provider may have accepted,
    /// and billed, the job anyway. Never set for a run cancelled before its submit, or proven whole-submit nonacceptance.
    /// `false` alone does not mean unbilled: a returned `handle` independently proves acceptance
    /// and possible billing, even when submission itself is no longer uncertain.
    pub uncertain: bool,
}

impl RunFailure {
    fn before_submit(error: ProviderError) -> Self {
        Self { error, handle: None, finished: false, uncertain: false }
    }

    fn submit_failed(error: ProviderError, proven: bool) -> Self {
        Self { error, handle: None, finished: false, uncertain: !proven }
    }

    /// Worth retrying: transient, and either never submitted or resumable by handle.
    pub fn is_transient(&self) -> bool {
        self.error.is_transient()
    }
}

impl fmt::Display for RunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for RunFailure {}

/// Submits `job` and, for queue-style providers, polls until it finishes, fails, is cancelled or
/// runs past the deadline. Cancellation interrupts waiting and polling alike; the provider is then
/// asked to stop (time-limited), and the error says if it couldn't be.
pub async fn run_to_completion(
    provider: &dyn Provider,
    job: &ProviderJob,
    cancel: &dyn CancelSignal,
    clock: &dyn Clock,
    limits: RunLimits,
    mut on_update: impl FnMut(RunUpdate),
) -> Result<Completed, RunFailure> {
    if cancel.is_cancelled() {
        return Err(RunFailure::before_submit(ProviderError::new(ErrorKind::Cancelled, "cancelled before it started")));
    }
    let context = crate::SubmitContext::new();
    let submitted = race(provider.submit_with_delivery(job, &context), cancel).await;
    let handle = match submitted {
        // Cancelled while submitting: we can't know whether the provider accepted it.
        None => return Err(RunFailure::submit_failed(ProviderError::new(ErrorKind::Cancelled, "cancelled while submitting"), false)),
        Some(Err(e)) => return Err(RunFailure::submit_failed(e, context.finish_error().is_some())),
        Some(Ok(Submission::Finished(done))) => return Ok(done),
        Some(Ok(Submission::Queued(handle))) => handle,
    };
    on_update(RunUpdate::Submitted(handle.clone()));
    resume(provider, &handle, cancel, clock, limits, on_update).await
}

/// Polls an already submitted job to the end (after a restart, or a retry of a failed run that
/// carries a handle). See [`run_to_completion`].
pub async fn resume(
    provider: &dyn Provider,
    handle: &JobHandle,
    cancel: &dyn CancelSignal,
    clock: &dyn Clock,
    limits: RunLimits,
    mut on_update: impl FnMut(RunUpdate),
) -> Result<Completed, RunFailure> {
    let fail = |error: ProviderError| RunFailure { error, handle: Some(handle.clone()), finished: false, uncertain: false };
    let started = clock.now_ms();
    let mut poll_errors: u32 = 0;
    let max_interval = limits.max_poll_interval_ms.max(1);
    let mut interval = provider.poll_interval_ms(&JobProgress::Queued { position: None }).clamp(1, max_interval);
    loop {
        let elapsed = clock.now_ms().saturating_sub(started);
        if elapsed >= limits.deadline_ms {
            let note = stop(provider, handle, clock).await;
            return Err(fail(ProviderError::new(ErrorKind::Expired, format!("no result after {} s{note}", elapsed / 1000))));
        }
        // Wait for the next poll, then poll; cancellation interrupts either.
        let wait = interval.min(limits.deadline_ms.saturating_sub(elapsed)).max(1);
        let polled = race(
            async {
                clock.sleep(wait).await;
                provider.poll(handle).await
            },
            cancel,
        )
        .await;
        let Some(polled) = polled else {
            let note = stop(provider, handle, clock).await;
            return Err(fail(ProviderError::new(ErrorKind::Cancelled, format!("cancelled{note}"))));
        };
        match polled {
            Ok(JobProgress::Done(done)) => return Ok(done),
            Ok(JobProgress::Failed(e)) => return Err(RunFailure { error: e, handle: Some(handle.clone()), finished: true, uncertain: false }),
            Ok(progress) => {
                poll_errors = 0;
                interval = provider.poll_interval_ms(&progress).clamp(1, max_interval);
                on_update(RunUpdate::Progress(progress));
            }
            Err(e) if e.is_transient() && poll_errors < limits.max_poll_errors => {
                poll_errors += 1;
                let backoff = e.retry_after_ms.unwrap_or(1000u64.saturating_mul(1 << poll_errors.min(5)));
                interval = backoff.clamp(1, max_interval);
                on_update(RunUpdate::PollRetry(e));
            }
            Err(e) => return Err(fail(e)),
        }
    }
}

/// Runs `work` unless `cancel` fires first (`None`).
async fn race<T>(work: impl std::future::Future<Output = T>, cancel: &dyn CancelSignal) -> Option<T> {
    if cancel.is_cancelled() {
        return None;
    }
    let mut work = std::pin::pin!(work.fuse());
    let mut stop = cancel.cancelled().fuse();
    futures::select! {
        out = work => Some(out),
        () = stop => None,
    }
}

/// Asks the provider to stop the job, for at most [`CANCEL_TIMEOUT_MS`]. Returns a note for the
/// error message when it couldn't confirm.
async fn stop(provider: &dyn Provider, handle: &JobHandle, clock: &dyn Clock) -> String {
    let mut cancel = provider.cancel(handle).fuse();
    let mut timeout: futures::future::Fuse<BoxFuture<'static, ()>> = clock.sleep(CANCEL_TIMEOUT_MS).fuse();
    let result = futures::select! {
        r = cancel => Some(r),
        () = timeout => None,
    };
    match result {
        Some(Ok(())) => String::new(),
        Some(Err(e)) => format!("; the provider could not be asked to stop the job ({e}), so it may still run and be billed"),
        None => "; the provider didn't confirm stopping the job, so it may still run and be billed".into(),
    }
}

/// A downloaded output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// The MIME type sniffed from the bytes (the provider's claim is not trusted).
    pub mime: &'static str,
    /// BLAKE3 of the bytes: the asset store's key.
    pub hash: nodecraft_media::ContentHash,
    pub bytes: Vec<u8>,
}

/// Downloads a file output, capped at `max_bytes`, and checks it: the length matches what the
/// server announced, and it is media of the `expected` kind (an HTML error page served with 200
/// is refused rather than stored as an "image"). The result carries its content hash.
pub async fn fetch_output(http: &dyn Http, url: &str, expected: Option<nodecraft_media::MediaKind>, max_bytes: usize) -> Result<Downloaded, ProviderError> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(ProviderError::new(ErrorKind::Malformed, "the output link is not an http(s) URL"));
    }
    let response = http.send(HttpRequest::get(url).max_response_bytes(max_bytes).timeout_ms(10 * 60 * 1000)).await?.error_for_status()?;
    // A compressed body is decoded (by the browser's fetch) while Content-Length stays compressed.
    if response.header("content-encoding").is_none()
        && let Some(announced) = response.header("content-length").and_then(|v| v.trim().parse::<usize>().ok())
        && announced != response.body.len()
    {
        return Err(ProviderError::new(ErrorKind::Network, format!("the download was cut short ({} of {announced} bytes)", response.body.len())));
    }
    check_media(&response.body, expected)
}

/// Checks inline output bytes the same way as downloads ([`Output::Bytes`] mime types are the
/// provider's claim and are not trusted either).
pub fn check_media(bytes: &[u8], expected: Option<nodecraft_media::MediaKind>) -> Result<Downloaded, ProviderError> {
    let sniffed = nodecraft_media::sniff(bytes)
        .ok_or_else(|| ProviderError::new(ErrorKind::Malformed, format!("the output is not a known media type: {}", crate::error::excerpt(bytes))))?;
    if let Some(kind) = expected
        && sniffed.kind != kind
    {
        return Err(ProviderError::new(ErrorKind::Malformed, format!("expected {kind:?} but the output is {}", sniffed.mime)));
    }
    Ok(Downloaded { mime: sniffed.mime, hash: nodecraft_media::ContentHash::of(bytes), bytes: bytes.to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Output;
    use crate::http::HttpResponse;
    use futures::executor::block_on;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// A provider whose poll results follow a script; each poll takes `poll_ms` of clock time.
    struct Scripted {
        polls: Mutex<Vec<Result<JobProgress, ProviderError>>>,
        cancelled: AtomicBool,
        cancel_result: Result<(), ProviderError>,
        submits: AtomicU64,
        clock: std::sync::Arc<Clocked>,
        poll_ms: u64,
        sync: bool,
    }

    impl Scripted {
        fn new(mut polls: Vec<Result<JobProgress, ProviderError>>) -> Self {
            polls.reverse();
            Self {
                polls: Mutex::new(polls),
                cancelled: AtomicBool::new(false),
                cancel_result: Ok(()),
                submits: AtomicU64::new(0),
                clock: std::sync::Arc::new(Clocked::default()),
                poll_ms: 0,
                sync: false,
            }
        }
    }

    impl Provider for Scripted {
        fn id(&self) -> &str {
            "scripted"
        }
        fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            let id = job.idempotency_key.clone();
            let sync = self.sync;
            Box::pin(async move {
                if sync {
                    Ok(Submission::Finished(Completed::new(vec![Output::Text("now".into())])))
                } else {
                    Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::Value::Null }))
                }
            })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            self.clock.0.fetch_add(self.poll_ms, Ordering::SeqCst);
            let next = self.polls.lock().unwrap().pop().unwrap_or(Ok(JobProgress::Queued { position: None }));
            Box::pin(async move { next })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            self.cancelled.store(true, Ordering::SeqCst);
            let r = self.cancel_result.clone();
            Box::pin(async move { r })
        }
    }

    /// A cancel signal; `cancelled()` really resolves once `fire` is called.
    #[derive(Default)]
    struct Flag(AtomicBool);
    impl CancelSignal for Flag {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(futures::future::poll_fn(move |cx| {
                if self.0.load(Ordering::SeqCst) {
                    std::task::Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            }))
        }
    }

    /// Virtual time: `sleep` advances it.
    #[derive(Default)]
    struct Clocked(AtomicU64);
    impl Clock for Clocked {
        fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
            self.0.fetch_add(ms, Ordering::SeqCst);
            Box::pin(async {})
        }
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl Clock for std::sync::Arc<Clocked> {
        fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
            (**self).sleep(ms)
        }
        fn now_ms(&self) -> u64 {
            (**self).now_ms()
        }
    }

    fn job() -> ProviderJob {
        ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: "k1".into() }
    }

    fn running(f: f32) -> Result<JobProgress, ProviderError> {
        Ok(JobProgress::Running { fraction: Some(f), message: None, partial: None })
    }

    fn run(p: &Scripted, flag: &Flag, limits: RunLimits, updates: &mut Vec<RunUpdate>) -> Result<Completed, RunFailure> {
        block_on(run_to_completion(p, &job(), flag, &p.clock, limits, |u| updates.push(u)))
    }

    #[test]
    fn polls_until_done_and_reports_progress() {
        let p = Scripted::new(vec![
            Ok(JobProgress::Queued { position: Some(2) }),
            running(0.5),
            Ok(JobProgress::Done(Completed::new(vec![Output::Text("hi".into())]))),
        ]);
        let mut updates = Vec::new();
        let out = run(&p, &Flag::default(), RunLimits::default(), &mut updates).unwrap();
        assert_eq!(out.outputs, vec![Output::Text("hi".into())]);
        assert!(matches!(&updates[0], RunUpdate::Submitted(h) if h.remote_id == "k1"), "the idempotency key reaches the provider");
        assert_eq!(updates.len(), 3);
    }

    #[test]
    fn synchronous_providers_finish_at_submit() {
        let p = Scripted { sync: true, ..Scripted::new(vec![]) };
        let mut updates = Vec::new();
        assert_eq!(run(&p, &Flag::default(), RunLimits::default(), &mut updates).unwrap().outputs, vec![Output::Text("now".into())]);
        assert!(updates.is_empty());
    }

    #[test]
    fn transient_poll_errors_are_retried_with_retry_after_then_give_up_with_the_handle() {
        let mut limited = ProviderError::new(ErrorKind::RateLimited, "slow down");
        limited.retry_after_ms = Some(7_000);
        let p = Scripted::new(vec![Err(limited), Ok(JobProgress::Done(Completed::default()))]);
        let mut updates = Vec::new();
        assert!(run(&p, &Flag::default(), RunLimits::default(), &mut updates).is_ok());
        assert!(matches!(updates.get(1), Some(RunUpdate::PollRetry(_))));
        assert!(p.clock.now_ms() >= 7_000, "Retry-After was honoured");

        let net = || Err(ProviderError::new(ErrorKind::Network, "reset"));
        let p = Scripted::new((0..10).map(|_| net()).collect());
        let f = run(&p, &Flag::default(), RunLimits::default(), &mut Vec::new()).unwrap_err();
        assert_eq!(f.error.kind, ErrorKind::Network);
        assert_eq!(f.handle.map(|h| h.remote_id).as_deref(), Some("k1"), "a retry must resume this job, not submit again");
        assert_eq!(p.submits.load(Ordering::SeqCst), 1, "polling never resubmits");

        let p = Scripted::new(vec![Err(ProviderError::new(ErrorKind::BlockedContent, "nope"))]);
        assert_eq!(run(&p, &Flag::default(), RunLimits::default(), &mut Vec::new()).unwrap_err().error.kind, ErrorKind::BlockedContent);
    }

    /// Fails every submit with `0`.
    struct Refuses(ErrorKind);
    impl Provider for Refuses {
        fn id(&self) -> &str {
            "refuses"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            let kind = self.0;
            Box::pin(async move { Err(ProviderError::new(kind, "no")) })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            Box::pin(async { Ok(JobProgress::Queued { position: None }) })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn legacy_submit_errors_are_uncertain_for_every_kind() {
        for (kind, uncertain) in [
            (ErrorKind::Cancelled, true),
            (ErrorKind::Network, true),
            (ErrorKind::Timeout, true),
            (ErrorKind::Server, true),
            (ErrorKind::Malformed, true),
            (ErrorKind::JobFailed, true),
            (ErrorKind::Expired, true),
            (ErrorKind::Auth, true),
            (ErrorKind::RateLimited, true),
            (ErrorKind::Busy, true),
            (ErrorKind::UnsupportedInput, true),
            (ErrorKind::BlockedContent, true),
            (ErrorKind::Budget, true),
            (ErrorKind::NotFound, true),
        ] {
            let f = block_on(run_to_completion(&Refuses(kind), &job(), &Flag::default(), &Clocked::default(), RunLimits::default(), |_| {})).unwrap_err();
            assert_eq!((f.handle.is_none(), f.uncertain), (true, uncertain), "{kind:?}");
        }
    }

    /// Count invocation separately from polling: constructing a provider future may have effects.
    #[derive(Default)]
    struct PendingSubmit {
        invoked: AtomicU64,
        polled: AtomicU64,
        other_calls: AtomicU64,
    }
    impl Provider for PendingSubmit {
        fn id(&self) -> &str {
            "pending"
        }
        fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            self.invoked.fetch_add(1, Ordering::SeqCst);
            Box::pin(futures::future::poll_fn(|_| {
                self.polled.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Pending
            }))
        }
        fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            self.other_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "no handle")) })
        }
        fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            self.other_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn cancellation_of_a_polled_pending_submit_is_uncertain_without_a_handle() {
        let provider = PendingSubmit::default();
        let flag = Flag::default();
        let clock = Clocked::default();
        let job = job();
        let mut updates = Vec::new();
        let mut run = Box::pin(run_to_completion(&provider, &job, &flag, &clock, RunLimits::default(), |u| updates.push(u)));
        let waker = futures::task::noop_waker();
        assert!(run.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
        assert_eq!(provider.invoked.load(Ordering::SeqCst), 1);
        assert_eq!(provider.polled.load(Ordering::SeqCst), 1, "submit really reached its first pending poll");
        flag.0.store(true, Ordering::SeqCst);
        let failure = block_on(run).unwrap_err();
        assert_eq!(failure.error.kind, ErrorKind::Cancelled);
        assert!(failure.uncertain && failure.handle.is_none() && !failure.finished);
        assert!(!failure.is_transient());
        assert!(updates.is_empty());
        assert_eq!(provider.other_calls.load(Ordering::SeqCst), 0, "no handle exists to poll or cancel");
        assert_eq!(provider.invoked.load(Ordering::SeqCst), 1);
        let before = PendingSubmit::default();
        let failure = block_on(run_to_completion(&before, &job, &flag, &clock, RunLimits::default(), |_| {})).unwrap_err();
        assert!(!failure.uncertain && failure.handle.is_none());
        assert_eq!(before.invoked.load(Ordering::SeqCst), 0, "pre-invocation cancel never calls submit");
    }

    #[test]
    fn job_failures_surface_their_kind() {
        for kind in [ErrorKind::BlockedContent, ErrorKind::Budget, ErrorKind::Server] {
            let p = Scripted::new(vec![Ok(JobProgress::Failed(ProviderError::new(kind, "x")))]);
            let f = run(&p, &Flag::default(), RunLimits::default(), &mut Vec::new()).unwrap_err();
            assert_eq!(f.error.kind, kind);
            assert!(f.handle.is_some() && f.finished, "the provider ended it: nothing to resume");
        }
    }

    #[test]
    fn cancellation_interrupts_and_reports_failed_provider_cancels() {
        let p = Scripted::new(vec![]);
        let flag = Flag::default();
        flag.0.store(true, Ordering::SeqCst);
        let f = run(&p, &flag, RunLimits::default(), &mut Vec::new()).unwrap_err();
        assert_eq!((f.error.kind, f.handle), (ErrorKind::Cancelled, None), "cancelled before submit: nothing to resume");
        assert!(!f.uncertain, "it never reached the provider");

        // Cancelled while polling: resume() races the poll against the signal.
        let p = Scripted { cancel_result: Err(ProviderError::new(ErrorKind::Network, "down")), ..Scripted::new(vec![]) };
        let flag = Flag::default();
        let handle = JobHandle { remote_id: "x".into(), data: serde_json::Value::Null };
        let mut polls = 0;
        let f = block_on(resume(&p, &handle, &flag, &p.clock, RunLimits::default(), |_| {
            polls += 1;
            if polls == 2 {
                flag.0.store(true, Ordering::SeqCst);
            }
        }))
        .unwrap_err();
        assert_eq!(f.error.kind, ErrorKind::Cancelled);
        assert!(p.cancelled.load(Ordering::SeqCst), "the provider is asked to cancel");
        assert!(f.error.message.contains("may still run and be billed"), "{}", f.error);
    }

    #[test]
    fn the_deadline_counts_real_time_and_is_not_retried() {
        // Each poll takes 4 s of clock time: wall time, not the sum of sleeps, ends the run.
        let p = Scripted { poll_ms: 4_000, ..Scripted::new(vec![]) };
        let limits = RunLimits { deadline_ms: 10_000, max_poll_interval_ms: 1_000, ..RunLimits::default() };
        let f = run(&p, &Flag::default(), limits, &mut Vec::new()).unwrap_err();
        assert_eq!(f.error.kind, ErrorKind::Expired);
        assert!(!f.is_transient(), "an expired job may still be running: never resubmit it");
        assert!(p.cancelled.load(Ordering::SeqCst));
        assert!(p.clock.now_ms() < 30_000, "real time ended it (sum-of-sleeps would poll for ~50 s): {}", p.clock.now_ms());
    }

    struct FakeHttp(HttpResponse);
    impl Http for FakeHttp {
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            let r = self.0.clone();
            Box::pin(async move {
                if r.body.len() > request.max_response_bytes {
                    return Err(ProviderError::new(ErrorKind::Malformed, "too large"));
                }
                Ok(r)
            })
        }
    }

    #[test]
    fn downloads_are_sniffed_hashed_and_length_checked() {
        let png = FakeHttp(HttpResponse { status: 200, headers: vec![], body: b"\x89PNG\r\n\x1a\n rest".to_vec() });
        let d = block_on(fetch_output(&png, "https://cdn/x", Some(nodecraft_media::MediaKind::Image), 1024)).unwrap();
        assert_eq!(d.mime, "image/png");
        assert_eq!(d.hash, nodecraft_media::ContentHash::of(b"\x89PNG\r\n\x1a\n rest"));
        assert_eq!(block_on(fetch_output(&png, "https://cdn/x", Some(nodecraft_media::MediaKind::Video), 1024)).unwrap_err().kind, ErrorKind::Malformed);
        assert!(block_on(fetch_output(&png, "https://cdn/x", None, 4)).is_err(), "size cap");
        let short = FakeHttp(HttpResponse { status: 200, headers: vec![("content-length".into(), "999".into())], body: b"\x89PNG\r\n\x1a\n".to_vec() });
        let e = block_on(fetch_output(&short, "https://cdn/x", None, 1024)).unwrap_err();
        assert!(e.is_transient(), "a truncated download is worth retrying");
        let html = FakeHttp(HttpResponse { status: 200, headers: vec![], body: b"<html><body>\x00error</body></html>".to_vec() });
        assert!(block_on(fetch_output(&html, "https://cdn/x", Some(nodecraft_media::MediaKind::Image), 1024)).is_err());
        let gone = FakeHttp(HttpResponse { status: 404, headers: vec![], body: vec![] });
        assert_eq!(block_on(fetch_output(&gone, "https://cdn/x", None, 1024)).unwrap_err().kind, ErrorKind::NotFound);
        assert!(block_on(fetch_output(&png, "file:///etc/passwd", None, 1024)).is_err());
        assert!(check_media(b"GIF89a", Some(nodecraft_media::MediaKind::Image)).is_ok());
    }
}
