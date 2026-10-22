use super::*;
use crate::secrets::MemoryStore;
use nodecraft_providers::{BoxFuture, HttpRequest, HttpResponse, JobHandle, JobProgress, ProviderError, ProviderJob, Submission};

use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts and keeps requests; answers none.
#[derive(Default)]
struct Wire(AtomicUsize, std::sync::Mutex<Vec<HttpRequest>>);
impl Http for Wire {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        self.1.lock().unwrap().push(request);
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

/// A throwaway Google Cloud service account made for tests, in its compact keychain form (the
/// providers crate's Google Cloud tests build the same key from its primes).
const SERVICE_ACCOUNT: &str = "gsa1:fixture@nodecraft-test.iam.gserviceaccount.com:nodecraft-test:0123456789abcdef0123456789abcdef01234567:7eGpEZd-MjEQRmjRJKQk23e1n4UNmiEbwk1_ndJvOvoMVMEbMoujF0rau_cbfj3_tq2ZwDvMDwLJYE535QzffIhyKiXLcL97BCr1-aO2s7ycnuu-nYGmhSH8hWfsLP3DhdTt1f_khooZFdqpYsAc-d68dmkqNyEoeUVgwUYa46c:0Ld7HsEvX0LP4wAadYUD7Qd4JGr80ZVgc_deaETVGTaWYHYuWnA2JxFws7HSvR-znh29y10sAEu1aRf7Xe0-vJiR-GsOFhqpBA96xC6T4gin-aDi48TKnHh0NrnncJVNh-r3jd94Dq5IHqpaIse2oM-Le_TtoX8yaAGQpDXaIu0";

#[test]
fn without_keys_or_a_server_every_paid_adapter_says_what_to_do_and_nothing_is_sent() {
    let catalog = Catalog::builtin().unwrap();
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    let env = env(&keys, &http, &settings);
    let setup = build(&catalog, |adapter| make(adapter, &env));
    let all: Vec<_> = setup.adapters().map(|(a, r)| (a.to_owned(), r.clone())).collect();
    assert!(all.len() >= 5, "every adapter the catalog names: {all:?}");
    assert_eq!(setup.readiness("openai-images"), Some(&Readiness::NeedsKey { account: "openai".into() }));
    assert_eq!(setup.readiness("openai"), Some(&Readiness::NeedsKey { account: "openai".into() }));
    assert_eq!(setup.readiness("anthropic"), Some(&Readiness::NeedsKey { account: "anthropic".into() }));
    assert_eq!(setup.readiness("replicate"), Some(&Readiness::NeedsKey { account: "replicate".into() }));
    assert_eq!(setup.readiness("seedream"), Some(&Readiness::NeedsKey { account: "seedream".into() }));
    assert_eq!(setup.readiness("gemini-images"), Some(&Readiness::NeedsKey { account: "gemini".into() }));
    assert_eq!(setup.readiness("gemini-video"), Some(&Readiness::NeedsKey { account: "gemini".into() }));
    assert_eq!(setup.readiness("seedance"), Some(&Readiness::NeedsKey { account: "seedream".into() }), "the ModelArk key Seedream uses");
    assert_eq!(setup.readiness("google-veo"), Some(&Readiness::NeedsKey { account: "google-cloud".into() }));
    assert_eq!(setup.readiness("google-tts"), Some(&Readiness::NeedsKey { account: "google-cloud".into() }));
    assert_eq!(setup.readiness("fal"), Some(&Readiness::NeedsKey { account: "fal".into() }));
    assert_eq!(setup.readiness("tripo"), Some(&Readiness::NeedsKey { account: "tripo".into() }));
    assert_eq!(setup.readiness("meshy"), Some(&Readiness::NeedsKey { account: "meshy".into() }));
    assert_eq!(setup.readiness("higgsfield"), Some(&Readiness::NeedsKey { account: "higgsfield".into() }));
    assert_eq!(setup.readiness("kling"), Some(&Readiness::NeedsKey { account: "kling".into() }));
    assert!(matches!(setup.readiness("comfyui"), Some(Readiness::NotSetUp(why)) if why.contains("comfyui_url")));
    let error = setup.routing([(NODE, "openai-images")]).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(error.message, "openai-images needs an API key: add one for “openai” in API keys.");
    assert_eq!(setup.routing([(NODE, "nobody")]).err().unwrap().kind, ErrorKind::NotFound);
    assert_eq!(http.0.load(Ordering::SeqCst), 0, "zero requests");
}

#[test]
fn with_keys_and_a_server_every_adapter_is_built_bound_and_selectable() {
    let catalog = Catalog::builtin().unwrap();
    let keys = MemoryStore::default();
    for account in
        ["openai", "anthropic", "replicate", "seedream", "gemini", "fal", "tripo", "meshy", "higgsfield", "kling", "elevenlabs", "comfyui:gpu-box:8188"]
    {
        keys.set(&Account::new(account).unwrap(), &Secret::new("sk-test-not-a-real-key").unwrap()).unwrap();
    }
    keys.set(&Account::new("google-cloud").unwrap(), &Secret::new(SERVICE_ACCOUNT).unwrap()).unwrap();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings { comfyui_url: Some("http://gpu-box:8188".into()), install_salt: "salt".into() };
    let env = env(&keys, &http, &settings);
    let setup = build(&catalog, |adapter| make(adapter, &env));
    for (adapter, readiness) in setup.adapters() {
        assert_eq!(readiness, &Readiness::Ready, "{adapter}");
    }
    let selections: Vec<_> = [
        "openai-images",
        "openai",
        "anthropic",
        "replicate",
        "seedream",
        "seedance",
        "gemini-images",
        "gemini-video",
        "google-veo",
        "google-tts",
        "fal",
        "tripo",
        "meshy",
        "higgsfield",
        "kling",
        "elevenlabs",
        "comfyui",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, a)| (NodeId::from_bytes([i as u8 + 0x10; 16]), a))
    .collect();
    let mut session = crate::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "P").unwrap()).unwrap();
    session.set_provider_routing(setup.routing(selections.iter().copied()).unwrap()).unwrap();
    // Each run gets a fresh routing from the same adapters, without reading keys again.
    session.set_provider_routing(setup.routing(selections.iter().copied().chain([(NODE, "replicate"), (NODE, "replicate")])).unwrap()).unwrap();
    assert_eq!(setup.routing([(NODE, "replicate"), (NODE, "seedream")]).err().unwrap().kind, ErrorKind::InvalidInput, "one node, two providers");
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
    assert!(matches!(make("not-a-provider", &env(&keys, &http, &settings)), Err(Readiness::NotSetUp(_))));
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
    // ComfyUI's token is optional: a refused keychain means no token, not no ComfyUI.
    let settings = ProviderSettings { comfyui_url: Some("http://127.0.0.1:8188".into()), install_salt: "s".into() };
    let env = HostEnv { keys: &keys, http: Arc::new(Wire::default()), settings: &settings, now_ms: Arc::new(|| 0) };
    assert!(make("comfyui", &env).is_ok());
}

#[test]
fn comfyui_sends_its_stored_token_as_a_bearer_header_and_never_shows_it() {
    const KEY: &str = "sk-comfy-token-not-real-0123";
    let keys = MemoryStore::default();
    keys.set(&Account::new("comfyui:gpu-box:8188").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings { comfyui_url: Some("http://gpu-box:8188".into()), install_salt: "s".into() };
    let provider = make("comfyui", &env(&keys, &http, &settings)).unwrap();
    let job = ProviderJob { model: "w".into(), params: Default::default(), inputs: vec![], idempotency_key: "k".into() };
    let _ = futures::executor::block_on(provider.submit(&job));
    let requests = http.1.lock().unwrap().clone();
    assert!(!requests.is_empty(), "the submit reached the server");
    for request in &requests {
        assert!(request.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {KEY}")), "{request:?}");
        assert!(!format!("{request:?}").contains(KEY), "never in Debug output");
    }
    // Credentials in the address are refused before the keychain is read.
    let with_user = ProviderSettings { comfyui_url: Some("http://me:pw@gpu-box:8188".into()), install_salt: "s".into() };
    assert!(matches!(make("comfyui", &env(&keys, &http, &with_user)), Err(Readiness::NotSetUp(why)) if !why.contains("pw")));
}

#[test]
fn the_setup_can_move_to_a_worker_thread() {
    fn send<T: Send>() {}
    send::<ProviderSetup>();
}

#[test]
fn the_test_path_binds_whatever_the_maker_gives_through_the_same_routing() {
    let catalog = Catalog::builtin().unwrap();
    let mut asked = Vec::new();
    let setup = build(&catalog, |adapter| {
        asked.push(adapter.to_owned());
        match adapter {
            "replicate" => Ok(Arc::new(Named("replicate")) as Arc<dyn Provider>),
            "seedream" => Ok(Arc::new(Named("impostor")) as Arc<dyn Provider>),
            _ => Err(Readiness::NotSetUp("not in this test".into())),
        }
    });
    assert_eq!(asked.iter().filter(|a| *a == "replicate").count(), 1, "each adapter is built once");
    setup.routing([(NODE, "replicate")]).unwrap();
    assert!(matches!(setup.readiness("seedream"), Some(Readiness::Failed(_))), "an adapter answering to another id isn't bound");
    assert!(setup.routing([(NODE, "seedream")]).is_err());
    // Lazily: only what is ensured is built, once, until forgotten.
    let mut lazy = ProviderSetup::default();
    let mut built = 0;
    for _ in 0..3 {
        lazy.ensure("replicate", |_| {
            built += 1;
            Ok(Arc::new(Named("replicate")) as Arc<dyn Provider>)
        });
    }
    assert_eq!(built, 1);
    assert!(lazy.readiness("seedream").is_none(), "nothing else was read");
    lazy.forget("replicate");
    lazy.ensure("replicate", |_| {
        built += 1;
        Ok(Arc::new(Named("replicate")) as Arc<dyn Provider>)
    });
    assert_eq!(built, 2);
}

#[test]
fn a_damaged_google_cloud_key_fails_veo_and_tts_without_quoting_it_or_sending_anything() {
    let catalog = Catalog::builtin().unwrap();
    let keys = MemoryStore::default();
    keys.set(&Account::new("google-cloud").unwrap(), &Secret::new("gsa1:not-a-service-account-zzz").unwrap()).unwrap();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    let env = env(&keys, &http, &settings);
    let setup = build(&catalog, |adapter| make(adapter, &env));
    for adapter in ["google-veo", "google-tts"] {
        let Some(Readiness::Failed(why)) = setup.readiness(adapter) else { panic!("{adapter}: {:?}", setup.readiness(adapter)) };
        assert!(why.contains("paste the service-account key file again") && !why.contains("zzz"), "{adapter}: {why}");
    }
    assert_eq!(http.0.load(Ordering::SeqCst), 0);
}
