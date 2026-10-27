use super::*;
use nodecraft_engine::secrets::{Account, MemoryStore, Secret};
use nodecraft_providers::{BoxFuture, HttpRequest, HttpResponse, JobHandle, JobProgress, ProviderError, ProviderJob, Submission};
use std::sync::Mutex;
struct NeverGenerate(String);
impl Provider for NeverGenerate {
    fn id(&self) -> &str {
        &self.0
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        panic!("a provider test must never generate")
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        panic!("a provider test has no paid task")
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}
struct Wire {
    status: u16,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("credential checks cannot follow redirects")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        assert_eq!(request.method, nodecraft_providers::HttpMethod::Get);
        assert_eq!(request.url, "https://api.openai.com/v1/models");
        self.requests.lock().unwrap().push(request);
        let response = HttpResponse { status: self.status, headers: vec![], body: br#"{"data":[],"echo":"cli97-fixture-not-a-key"}"#.to_vec() };
        Box::pin(async { Ok(response) })
    }
}
fn store() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    for name in ["openai", "fal"] {
        store.set(&Account::new(name).unwrap(), &Secret::new("cli97-fixture-not-a-key").unwrap()).unwrap();
    }
    store
}
#[test]
fn cli97_provider_tests_use_the_real_free_key_command_and_deduplicate_shared_accounts() {
    let http = Arc::new(Wire { status: 200, requests: Mutex::default() });
    let report = run(
        None,
        |id| {
            if ["openai", "openai-images", "openai-audio"].contains(&id) {
                Ok(Arc::new(NeverGenerate(id.into())) as Arc<dyn Provider>)
            } else {
                Err(Readiness::NotSetUp("test fixture".into()))
            }
        },
        store(),
        http.clone(),
    )
    .unwrap();
    for name in ["openai", "openai-images", "openai-audio"] {
        assert_eq!(report.as_array().unwrap().iter().find(|r| r["id"] == name).unwrap()["state"], "verified");
    }
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert!(!report.to_string().contains("cli97-fixture-not-a-key"));
}
#[test]
fn cli97_provider_rejection_is_not_verified_and_unsupported_checks_make_no_requests() {
    let http = Arc::new(Wire { status: 401, requests: Mutex::default() });
    let report = run(Some("openai-images"), |id| Ok(Arc::new(NeverGenerate(id.into())) as Arc<dyn Provider>), store(), http.clone()).unwrap();
    assert_eq!(report[0]["state"], "failed");
    assert!(!report.to_string().contains("cli97-fixture-not-a-key"));
    let http = Arc::new(Wire { status: 200, requests: Mutex::default() });
    let report = run(Some("fal"), |id| Ok(Arc::new(NeverGenerate(id.into())) as Arc<dyn Provider>), store(), http.clone()).unwrap();
    assert_eq!(report[0]["state"], "unverifiable");
    assert!(http.requests.lock().unwrap().is_empty());
    assert!(run(Some("missing-provider"), |_| panic!("invalid ids do not read keys"), store(), http).is_err());
}
