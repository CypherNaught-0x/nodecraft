use super::*;
use crate::secrets::MemoryStore;
use nodecraft_providers::{BoxFuture, HttpRequest, HttpResponse, JobHandle, JobProgress, ProviderError, ProviderJob, Submission};

use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts requests; answers none.
#[derive(Default)]
struct Wire(AtomicUsize);
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ProviderError::new(nodecraft_providers::ErrorKind::Network, "offline")) })
    }
}

struct Named(&'static str);
impl Provider for Named {
    fn id(&self) -> &str {
        self.0
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(nodecraft_providers::ErrorKind::Network, "test")) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(nodecraft_providers::ErrorKind::Network, "test")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

fn env<'a>(keys: &'a MemoryStore, http: &Arc<Wire>, settings: &'a ProviderSettings) -> HostEnv<'a> {
    HostEnv { keys, http: http.clone(), settings, now_ms: Arc::new(|| 0) }
}

const NODE: NodeId = NodeId::from_bytes([9; 16]);

#[test]
fn without_keys_or_a_server_every_paid_adapter_says_what_to_do_and_nothing_is_sent() {
    let catalog = Catalog::builtin().unwrap();
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    let env = env(&keys, &http, &settings);
    let mut setup = build(&catalog, |adapter| make(adapter, &env)).unwrap();
    let all: Vec<_> = setup.adapters().map(|(a, r)| (a.to_owned(), r.clone())).collect();
    assert!(all.len() >= 4, "every adapter the catalog names: {all:?}");
    assert_eq!(setup.readiness("openai-images"), Some(&Readiness::NeedsKey { account: "openai".into() }));
    assert_eq!(setup.readiness("replicate"), Some(&Readiness::NeedsKey { account: "replicate".into() }));
    assert_eq!(setup.readiness("seedream"), Some(&Readiness::NeedsKey { account: "seedream".into() }));
    assert!(matches!(setup.readiness("comfyui"), Some(Readiness::NotSetUp(why)) if why.contains("comfyui_url")));
    let error = setup.select(NODE, "openai-images").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(error.message, "openai-images needs an API key: add one for “openai” in API keys.");
    assert_eq!(setup.select(NODE, "nobody").unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(http.0.load(Ordering::SeqCst), 0, "zero requests");
}

#[test]
fn with_keys_and_a_server_every_adapter_is_built_bound_and_selectable() {
    let catalog = Catalog::builtin().unwrap();
    let keys = MemoryStore::default();
    for account in ["openai", "replicate", "seedream", "comfyui:gpu-box:8188"] {
        keys.set(&Account::new(account).unwrap(), &Secret::new("sk-test-not-a-real-key").unwrap()).unwrap();
    }
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings { comfyui_url: Some("http://gpu-box:8188".into()), install_salt: "salt".into() };
    let env = env(&keys, &http, &settings);
    let mut setup = build(&catalog, |adapter| make(adapter, &env)).unwrap();
    for (adapter, readiness) in setup.adapters() {
        assert_eq!(readiness, &Readiness::Ready, "{adapter}");
    }
    for (i, adapter) in ["openai-images", "replicate", "seedream", "comfyui"].into_iter().enumerate() {
        setup.select(NodeId::from_bytes([i as u8 + 1; 16]), adapter).unwrap();
    }
    let mut session = crate::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "P").unwrap()).unwrap();
    session.set_provider_routing(setup.into_routing()).unwrap();
    assert_eq!(http.0.load(Ordering::SeqCst), 0, "building sends nothing");
}

#[test]
fn comfyui_needs_an_address_and_a_salt_and_its_key_is_optional() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let mut settings = ProviderSettings { comfyui_url: Some("http://127.0.0.1:8188".into()), install_salt: String::new() };
    assert!(matches!(make("comfyui", &env(&keys, &http, &settings)), Err(Readiness::NotSetUp(why)) if why.contains("salt")));
    settings.install_salt = "s".into();
    assert!(make("comfyui", &env(&keys, &http, &settings)).is_ok(), "a local server needs no key");
    settings.comfyui_url = Some("not a url".into());
    assert!(matches!(make("comfyui", &env(&keys, &http, &settings)), Err(Readiness::NotSetUp(_))));
    assert!(matches!(make("fal", &env(&keys, &http, &settings)), Err(Readiness::NotSetUp(_))));
}

/// A keychain that fails, with the stored key in nothing it says.
struct Locked;
impl SecretStore for Locked {
    fn get(&self, _: &Account) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Denied("unlock the keychain and allow NodeCraft access".into()))
    }
    fn set(&self, _: &Account, _: &Secret) -> Result<(), SecretError> {
        Ok(())
    }
    fn delete(&self, _: &Account) -> Result<bool, SecretError> {
        Ok(false)
    }
}

#[test]
fn a_locked_keychain_is_reported_not_retried_or_mistaken_for_a_missing_key() {
    let keys = Locked;
    let http: Arc<dyn Http> = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    let env = HostEnv { keys: &keys, http, settings: &settings, now_ms: Arc::new(|| 0) };
    let Err(Readiness::Failed(why)) = make("replicate", &env) else { panic!("refused") };
    assert!(why.contains("unlock the keychain"));
}

#[test]
fn the_test_path_binds_whatever_the_maker_gives_through_the_same_routing() {
    let catalog = Catalog::builtin().unwrap();
    let mut asked = Vec::new();
    let mut setup = build(&catalog, |adapter| {
        asked.push(adapter.to_owned());
        match adapter {
            "replicate" => Ok(Arc::new(Named("replicate")) as Arc<dyn Provider>),
            "seedream" => Ok(Arc::new(Named("impostor")) as Arc<dyn Provider>),
            _ => Err(Readiness::NotSetUp("not in this test".into())),
        }
    })
    .unwrap();
    assert_eq!(asked.iter().filter(|a| *a == "replicate").count(), 1, "each adapter is built once");
    setup.select(NODE, "replicate").unwrap();
    assert!(matches!(setup.readiness("seedream"), Some(Readiness::Failed(_))), "an adapter answering to another id isn't bound");
    assert!(setup.select(NODE, "seedream").is_err());
}
