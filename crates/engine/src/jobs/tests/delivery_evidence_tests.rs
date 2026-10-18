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
    // Authorized jobs (bound price and ceiling) that are reserved and then refused locally,
    // before any HTTP call.
    let authorized = |q: QueuedJob, ceiling: &crate::run_ceiling::RunCeiling| {
        let mut q = q;
        let mut price = q.price.clone().unwrap();
        price.ceiling = Some(ceiling.clone());
        q.bound = Some(Some(price));
        q.price = None;
        q
    };
    let http = Arc::new(LegacyHttp(AtomicU64::new(0)));
    // With no room, the job is refused at its reservation: the debit happens before the local
    // failure, not after.
    let (runner, _) = budgeted(None);
    let empty = crate::run_ceiling::RunCeiling::new(std::collections::BTreeMap::new(), 0);
    runner.register(JobId(1), authorized(llm(http.clone(), false), &empty));
    let refused = block_on(runner.run(job(1), CancelToken::default()));
    assert!(matches!(&refused, Outcome::Failed { error, .. } if is_budget_refusal(error) && error.contains("nothing in USD")), "{refused:?}");
    // With exactly one job's room, three such jobs in a row all reserve and are credited back.
    let (runner, budget) = budgeted(None);
    let ceiling = crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, 100_000)].into(), 0);
    for n in 1..=3 {
        runner.register(JobId(n), authorized(llm(http.clone(), false), &ceiling));
        let outcome = block_on(runner.run(job(n), CancelToken::default()));
        assert!(matches!(&outcome, Outcome::Failed { transient: false, error } if !is_budget_refusal(error)), "job {n}: {outcome:?}");
        assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (100_000, 0), "job {n} was credited back");
    }
    assert_eq!(http.0.load(Ordering::SeqCst), 0);
    // The room is still whole for a job that reaches the provider and keeps its debit once its
    // reply is lost.
    runner.register(JobId(4), authorized(llm(http.clone(), true), &ceiling));
    assert!(matches!(block_on(runner.run(job(4), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    runner.settle(JobId(4));
    assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (100_000, 100_000));
    assert_eq!(spent(&budget), 100_000);
}
