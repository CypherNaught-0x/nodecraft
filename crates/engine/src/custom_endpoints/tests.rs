use super::*;
use nodecraft_exec::cost::StepQuote;
use nodecraft_providers::{BoxFuture, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::{Currency, NodeId};
use std::sync::Arc;

struct Fixture {
    id: &'static str,
    base: Option<&'static str>,
}
impl Provider for Fixture {
    fn id(&self) -> &str {
        self.id
    }
    fn custom_base_url(&self) -> Option<&str> {
        self.base
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async { panic!("quote tests never submit") })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { panic!("quote tests never poll") })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

const FREE: NodeId = NodeId::from_bytes([1; 16]);
const ZERO: NodeId = NodeId::from_bytes([2; 16]);
const PAID: NodeId = NodeId::from_bytes([3; 16]);
const UNKNOWN: NodeId = NodeId::from_bytes([4; 16]);
const DEFAULT: NodeId = NodeId::from_bytes([5; 16]);

fn step(node: NodeId, per_job: Result<Option<Money>, Unpriced>) -> StepQuote {
    StepQuote { node, models: vec!["openai.fixture".into()], per_job, jobs: 1, jobs_known: true }
}

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

fn quote() -> Quote {
    let steps = vec![
        step(FREE, Ok(None)),
        step(ZERO, Ok(Some(usd(0)))),
        step(PAID, Ok(Some(usd(1_500)))),
        step(UNKNOWN, Err(Unpriced::ByUsage("openai.fixture".into()))),
        step(DEFAULT, Ok(None)),
    ];
    Quote { steps, totals: [(Currency::USD, usd(1_500))].into() }
}

fn routing(base: Option<&'static str>) -> ProviderRouting {
    let mut routing = ProviderRouting::new();
    let custom = routing.bind_default(Arc::new(Fixture { id: "openai", base })).unwrap();
    let other = routing.bind_default(Arc::new(Fixture { id: "ollama-fixture", base: None })).unwrap();
    for node in [FREE, ZERO, PAID, UNKNOWN] {
        routing.select(node, "openai", custom.target()).unwrap();
    }
    routing.select(DEFAULT, "ollama-fixture", other.target()).unwrap();
    routing
}

#[test]
fn a_custom_endpoint_is_never_free_and_catalog_prices_are_kept() {
    let mut priced = quote();
    apply(&routing(Some("http://127.0.0.1:4000/v1")), &mut priced).unwrap();
    let per_job = |node| priced.steps.iter().find(|step| step.node == node).unwrap().per_job.clone();
    assert_eq!(per_job(FREE), Err(Unpriced::Invalid(UNPRICED.into())));
    assert_eq!(per_job(ZERO), Err(Unpriced::Invalid(UNPRICED.into())));
    assert_eq!(per_job(PAID), Ok(Some(usd(1_500))), "the catalog price is kept");
    assert_eq!(per_job(UNKNOWN), Err(Unpriced::ByUsage("openai.fixture".into())), "an unknown price stays as it was");
    assert_eq!(per_job(DEFAULT), Ok(None), "an adapter at its own API keeps a free price");
    assert_eq!(priced.totals, [(Currency::USD, usd(1_500))].into());
    assert!(!priced.is_complete());
    assert_eq!(priced.unpriced().count(), 3);
}

#[test]
fn without_a_custom_endpoint_the_quote_is_unchanged() {
    let mut priced = quote();
    apply(&routing(None), &mut priced).unwrap();
    assert_eq!(priced, quote());
}

#[test]
fn the_quote_binding_follows_the_base_url() {
    let digest = |routing: &ProviderRouting| {
        let mut hash = blake3::Hasher::new();
        bind(routing, routing.targets(), &mut hash);
        *hash.finalize().as_bytes()
    };
    let none = routing(None);
    let a = routing(Some("http://127.0.0.1:4000/v1"));
    let b = routing(Some("http://127.0.0.1:4001/v1"));
    assert_eq!(digest(&none), digest(&routing(None)));
    assert_eq!(digest(&a), digest(&routing(Some("http://127.0.0.1:4000/v1"))));
    assert_ne!(digest(&none), digest(&a));
    assert_ne!(digest(&a), digest(&b));
}

#[test]
fn settings_are_checked_by_provider_without_echoing_input() {
    let mut settings = ProviderSettings::default();
    assert_eq!(base_url(&settings, "openai").unwrap(), None, "nothing set: the default");
    settings.base_urls.insert("openai".into(), "  ".into());
    assert_eq!(base_url(&settings, "openai").unwrap(), None, "blank: the default");
    settings.base_urls.insert("openai".into(), "http://127.0.0.1:4000/v1/".into());
    assert_eq!(base_url(&settings, "openai").unwrap().unwrap().as_str(), "http://127.0.0.1:4000/v1");
    settings.base_urls.insert("replicate".into(), "https://x.example".into());
    assert_eq!(base_url(&settings, "replicate").unwrap(), None, "a provider not listed keeps its API");
    for bad in ["ftp://proxy.example/v1", "http://proxy.example/v1", "https://user:sk-secret@proxy.example/v1", "not a url"] {
        settings.base_urls.insert("anthropic".into(), bad.into());
        let error = base_url(&settings, "anthropic").unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(!error.message.contains("proxy.example") && !error.message.contains("sk-secret"), "{}", error.message);
        assert!(check(Some(bad)).is_err());
    }
    assert_eq!(check(None).unwrap(), None);
    assert_eq!(check(Some(" ")).unwrap(), None);
    let all = check_all(&[("openai".to_owned(), Some("https://proxy.example/v1/".to_owned())), ("anthropic".to_owned(), None)].into()).unwrap();
    assert_eq!(all, [("openai".to_owned(), "https://proxy.example/v1".to_owned())].into());
    let error = check_all(&[("sk-secret-provider".to_owned(), None)].into()).unwrap_err();
    assert!(!error.message.contains("sk-secret"), "{}", error.message);
    for entry in PROVIDERS {
        assert_eq!(endpoint(entry.provider), Some(entry));
        assert!(BaseUrl::parse(entry.default).is_ok(), "{} default", entry.provider);
    }
}
