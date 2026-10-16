//! The machinery queue-style providers share: submit, poll until done, cancel, and download
//! outputs with size caps and type checks.

use futures::FutureExt;

use crate::http::{Http, HttpRequest};
use crate::provider::{CancelSignal, JobProgress, Output, Provider, ProviderJob, Sleep};
use crate::{ErrorKind, JobHandle, ProviderError};

/// How long a job may take overall before it counts as timed out.
pub const DEFAULT_JOB_DEADLINE_MS: u64 = 60 * 60 * 1000;
/// Consecutive poll failures tolerated before the job fails (each retried after backoff).
const MAX_POLL_ERRORS: u32 = 5;

/// Limits for [`run_to_completion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    /// Give up (as `Timeout`) after waiting this long in total.
    pub deadline_ms: u64,
    /// Cap on the poll interval the provider may ask for.
    pub max_poll_interval_ms: u64,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self { deadline_ms: DEFAULT_JOB_DEADLINE_MS, max_poll_interval_ms: 30_000 }
    }
}

/// What [`run_to_completion`] reports while it waits.
#[derive(Debug, Clone, PartialEq)]
pub enum RunUpdate {
    Submitted(JobHandle),
    Progress(JobProgress),
}

/// Submits `job` and polls until it finishes, fails, is cancelled or runs past the deadline.
///
/// - Cancellation is checked while waiting; the provider is asked to cancel (best effort) and the
///   result is `ErrorKind::Cancelled`.
/// - A failed poll is retried with backoff up to a few times, so a dropped connection while a
///   long video renders doesn't lose a job that is already paid for.
/// - `on_update` sees the handle (persist it to resume after a restart) and every progress report.
pub async fn run_to_completion(
    provider: &dyn Provider,
    job: &ProviderJob,
    cancel: &dyn CancelSignal,
    sleep: &dyn Sleep,
    limits: RunLimits,
    mut on_update: impl FnMut(RunUpdate),
) -> Result<Vec<Output>, ProviderError> {
    if cancel.is_cancelled() {
        return Err(ProviderError::new(ErrorKind::Cancelled, "cancelled before it started"));
    }
    let handle = provider.submit(job).await?;
    on_update(RunUpdate::Submitted(handle.clone()));
    resume(provider, &handle, cancel, sleep, limits, on_update).await
}

/// Polls an already submitted job to the end (after a restart, or a retry whose submit already
/// went through). See [`run_to_completion`].
pub async fn resume(
    provider: &dyn Provider,
    handle: &JobHandle,
    cancel: &dyn CancelSignal,
    sleep: &dyn Sleep,
    limits: RunLimits,
    mut on_update: impl FnMut(RunUpdate),
) -> Result<Vec<Output>, ProviderError> {
    let mut waited: u64 = 0;
    let mut poll_errors: u32 = 0;
    let mut interval = provider.poll_interval_ms(&JobProgress::Queued { position: None }).min(limits.max_poll_interval_ms);
    loop {
        if waited >= limits.deadline_ms {
            let _ = provider.cancel(handle).await;
            return Err(ProviderError::new(ErrorKind::Timeout, format!("no result after {} s", waited / 1000)));
        }
        // Wait for the next poll, or for cancellation, whichever comes first.
        let cancelled = {
            let mut tick = sleep.sleep(interval).fuse();
            let mut stop = cancel.cancelled().fuse();
            futures::select! {
                () = tick => false,
                () = stop => true,
            }
        };
        if cancelled || cancel.is_cancelled() {
            let _ = provider.cancel(handle).await;
            return Err(ProviderError::new(ErrorKind::Cancelled, "cancelled"));
        }
        waited = waited.saturating_add(interval);
        match provider.poll(handle).await {
            Ok(JobProgress::Done(outputs)) => return Ok(outputs),
            Ok(JobProgress::Failed(e)) => return Err(e),
            Ok(progress) => {
                poll_errors = 0;
                interval = provider.poll_interval_ms(&progress).clamp(1, limits.max_poll_interval_ms.max(1));
                on_update(RunUpdate::Progress(progress));
            }
            Err(e) if e.is_transient() && poll_errors < MAX_POLL_ERRORS => {
                poll_errors += 1;
                let backoff = e.retry_after_ms.unwrap_or(1000u64.saturating_mul(1 << poll_errors.min(5)));
                interval = backoff.clamp(1, limits.max_poll_interval_ms.max(1));
            }
            Err(e) => return Err(e),
        }
    }
}

/// A downloaded output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// The MIME type sniffed from the bytes (the provider's claim is not trusted).
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

/// Downloads a file output, capped at `max_bytes`, and checks it is media of the expected
/// `kind` (an HTML error page served with status 200 is refused, not stored as an "image").
pub async fn fetch_output(http: &dyn Http, url: &str, expected: Option<nodecraft_media::MediaKind>, max_bytes: usize) -> Result<Downloaded, ProviderError> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(ProviderError::new(ErrorKind::Malformed, "the output link is not an http(s) URL"));
    }
    let response = http.send(HttpRequest::get(url).max_response_bytes(max_bytes).timeout_ms(10 * 60 * 1000)).await?.error_for_status()?;
    let sniffed = nodecraft_media::sniff(&response.body).ok_or_else(|| {
        ProviderError::new(ErrorKind::Malformed, format!("the downloaded file is not a known media type: {}", crate::error::excerpt(&response.body)))
    })?;
    if let Some(kind) = expected
        && sniffed.kind != kind
    {
        return Err(ProviderError::new(ErrorKind::Malformed, format!("expected {kind:?} but the download is {}", sniffed.mime)));
    }
    Ok(Downloaded { mime: sniffed.mime, bytes: response.body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use crate::provider::BoxFuture;
    use futures::executor::block_on;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// A provider whose poll results follow a script.
    struct Scripted {
        polls: Mutex<Vec<Result<JobProgress, ProviderError>>>,
        cancelled: AtomicBool,
        submits: AtomicU64,
    }

    impl Scripted {
        fn new(mut polls: Vec<Result<JobProgress, ProviderError>>) -> Self {
            polls.reverse();
            Self { polls: Mutex::new(polls), cancelled: AtomicBool::new(false), submits: AtomicU64::new(0) }
        }
    }

    impl Provider for Scripted {
        fn id(&self) -> &str {
            "scripted"
        }
        fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<JobHandle, ProviderError>> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            let id = job.idempotency_key.clone();
            Box::pin(async move { Ok(JobHandle { remote_id: id, data: serde_json::Value::Null }) })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            let next = self.polls.lock().unwrap().pop().unwrap_or(Ok(JobProgress::Queued { position: None }));
            Box::pin(async move { next })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            self.cancelled.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Default)]
    struct Flag(AtomicBool);
    impl CancelSignal for Flag {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(futures::future::pending())
        }
    }

    /// Virtual time: records the total slept.
    #[derive(Default)]
    struct Clock(AtomicU64);
    impl Sleep for Clock {
        fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
            self.0.fetch_add(ms, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    fn job() -> ProviderJob {
        ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: "k1".into() }
    }

    fn running(f: f32) -> Result<JobProgress, ProviderError> {
        Ok(JobProgress::Running { fraction: Some(f), message: None, partial: None })
    }

    #[test]
    fn polls_until_done_and_reports_progress() {
        let p = Scripted::new(vec![Ok(JobProgress::Queued { position: Some(2) }), running(0.5), Ok(JobProgress::Done(vec![Output::Text("hi".into())]))]);
        let mut updates = Vec::new();
        let out = block_on(run_to_completion(&p, &job(), &Flag::default(), &Clock::default(), RunLimits::default(), |u| updates.push(u))).unwrap();
        assert_eq!(out, vec![Output::Text("hi".into())]);
        assert!(matches!(&updates[0], RunUpdate::Submitted(h) if h.remote_id == "k1"), "the idempotency key reaches the provider");
        assert_eq!(updates.len(), 3);
    }

    #[test]
    fn transient_poll_errors_are_retried_then_give_up() {
        let net = || Err(ProviderError::new(ErrorKind::Network, "reset"));
        let p = Scripted::new(vec![net(), net(), Ok(JobProgress::Done(vec![]))]);
        assert!(block_on(run_to_completion(&p, &job(), &Flag::default(), &Clock::default(), RunLimits::default(), |_| {})).is_ok());
        let p = Scripted::new((0..10).map(|_| net()).collect());
        let e = block_on(run_to_completion(&p, &job(), &Flag::default(), &Clock::default(), RunLimits::default(), |_| {})).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Network);
        let p = Scripted::new(vec![Err(ProviderError::new(ErrorKind::BlockedContent, "nope"))]);
        let e = block_on(run_to_completion(&p, &job(), &Flag::default(), &Clock::default(), RunLimits::default(), |_| {})).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BlockedContent, "permanent errors end at once");
        assert_eq!(p.submits.load(Ordering::SeqCst), 1, "polling never resubmits");
    }

    #[test]
    fn failed_jobs_cancellation_and_deadline() {
        let p = Scripted::new(vec![Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Server, "OOM")))]);
        assert_eq!(
            block_on(run_to_completion(&p, &job(), &Flag::default(), &Clock::default(), RunLimits::default(), |_| {})).unwrap_err().kind,
            ErrorKind::Server
        );

        let p = Scripted::new(vec![]);
        let flag = Flag::default();
        flag.0.store(true, Ordering::SeqCst);
        let e =
            block_on(resume(&p, &JobHandle { remote_id: "x".into(), data: serde_json::Value::Null }, &flag, &Clock::default(), RunLimits::default(), |_| {}))
                .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Cancelled);
        assert!(p.cancelled.load(Ordering::SeqCst), "the provider is asked to cancel");

        let p = Scripted::new(vec![]); // queued forever
        let clock = Clock::default();
        let limits = RunLimits { deadline_ms: 10_000, max_poll_interval_ms: 1_000 };
        let e = block_on(run_to_completion(&p, &job(), &Flag::default(), &clock, limits, |_| {})).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Timeout);
        assert!(p.cancelled.load(Ordering::SeqCst));
        assert!(clock.0.load(Ordering::SeqCst) >= 10_000);
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
    fn downloads_are_sniffed_and_capped() {
        let png = FakeHttp(HttpResponse { status: 200, headers: vec![], body: b"\x89PNG\r\n\x1a\n rest".to_vec() });
        let d = block_on(fetch_output(&png, "https://cdn/x", Some(nodecraft_media::MediaKind::Image), 1024)).unwrap();
        assert_eq!(d.mime, "image/png");
        let e = block_on(fetch_output(&png, "https://cdn/x", Some(nodecraft_media::MediaKind::Video), 1024)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Malformed);
        assert!(block_on(fetch_output(&png, "https://cdn/x", None, 4)).is_err(), "size cap");
        let html = FakeHttp(HttpResponse { status: 200, headers: vec![], body: b"<html><body>\x00error</body></html>".to_vec() });
        assert!(block_on(fetch_output(&html, "https://cdn/x", Some(nodecraft_media::MediaKind::Image), 1024)).is_err());
        let gone = FakeHttp(HttpResponse { status: 404, headers: vec![], body: vec![] });
        assert_eq!(block_on(fetch_output(&gone, "https://cdn/x", None, 1024)).unwrap_err().kind, ErrorKind::NotFound);
        assert!(block_on(fetch_output(&png, "file:///etc/passwd", None, 1024)).is_err());
    }
}
