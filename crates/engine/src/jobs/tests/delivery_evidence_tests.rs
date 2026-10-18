use super::*;
use nodecraft_providers::llm::{Dialect, Endpoint, LlmClient};
use nodecraft_providers::{Http, HttpRequest, HttpResponse, JobInput};
struct LegacyHttp(AtomicU64);
impl Http for LegacyHttp {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "fixture lost response")) })
    }
}
fn llm(http: Arc<LegacyHttp>, valid: bool) -> QueuedJob {
    let client = LlmClient::new(Endpoint { id: "fixture".into(), dialect: Dialect::OpenAi, base_url: "http://public-fixture.invalid".into(), key: None }, http);
    let mut q = priced(Arc::new(client), Some(100_000));
    q.job.model = if valid { "model" } else { "" }.into();
    q.job.inputs = vec![JobInput::Text { role: "user".into(), text: "public fixture".into() }];
    q
}
#[test]
fn audited_fresh_local_llm_failure_releases_without_calling_http() {
    let (runner, budget) = budgeted(Some(1_000_000));
    let http = Arc::new(LegacyHttp(AtomicU64::new(0)));
    runner.register(JobId(1), llm(http.clone(), false));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(http.0.load(Ordering::SeqCst), 0);
    assert_eq!(spent(&budget), 0);
    assert_eq!(budget.log.lock().unwrap().entries().count(), 0);
}
#[test]
fn prior_ambiguous_attempt_is_not_released_by_later_proven_local_failure() {
    let (runner, budget) = budgeted(Some(1_000_000));
    let http = Arc::new(LegacyHttp(AtomicU64::new(0)));
    runner.register(JobId(1), llm(http.clone(), true));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(spent(&budget), 100_000);
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    runner.register(JobId(1), llm(http.clone(), false));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    assert_eq!(spent(&budget), 100_000);
    let log = budget.log.lock().unwrap();
    assert_eq!(log.entries().map(|e| (e.cost.micros, e.status)).collect::<Vec<_>>(), [(100_000, Charge::Unresolved)]);
}

#[test]
fn a_proven_local_failure_credits_the_run_ceiling_through_the_runner() {
    let (runner, budget) = budgeted(None);
    let ceiling = crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, 100_000)].into(), 0);
    let with_ceiling = |mut q: QueuedJob| {
        if let Some(price) = q.price.as_mut() {
            price.ceiling = Some(ceiling.clone());
        }
        q
    };
    let http = Arc::new(LegacyHttp(AtomicU64::new(0)));
    // Reserved, then refused locally before any HTTP call: released, and the debit credited.
    runner.register(JobId(1), with_ceiling(llm(http.clone(), false)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(http.0.load(Ordering::SeqCst), 0);
    assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (100_000, 0));
    // So the whole accepted amount is there for the next job, which reaches the provider and
    // keeps its debit once its reply is lost.
    runner.register(JobId(2), with_ceiling(llm(http.clone(), true)));
    assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    runner.settle(JobId(2));
    assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (100_000, 100_000));
    assert_eq!(spent(&budget), 100_000);
}
