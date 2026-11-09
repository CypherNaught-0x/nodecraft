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
    HostEnv { keys, http: http.clone(), settings, now_ms: Arc::new(|| 0), progress: None }
}

const NODE: NodeId = NodeId::from_bytes([9; 16]);

/// A throwaway Google Cloud service account made for tests, in its compact keychain form (the
/// providers crate's Google Cloud tests build the same key from its primes).
const SERVICE_ACCOUNT: &str = "gsa1:fixture@nodecraft-test.iam.gserviceaccount.com:nodecraft-test:0123456789abcdef0123456789abcdef01234567:7eGpEZd-MjEQRmjRJKQk23e1n4UNmiEbwk1_ndJvOvoMVMEbMoujF0rau_cbfj3_tq2ZwDvMDwLJYE535QzffIhyKiXLcL97BCr1-aO2s7ycnuu-nYGmhSH8hWfsLP3DhdTt1f_khooZFdqpYsAc-d68dmkqNyEoeUVgwUYa46c:0Ld7HsEvX0LP4wAadYUD7Qd4JGr80ZVgc_deaETVGTaWYHYuWnA2JxFws7HSvR-znh29y10sAEu1aRf7Xe0-vJiR-GsOFhqpBA96xC6T4gin-aDi48TKnHh0NrnncJVNh-r3jd94Dq5IHqpaIse2oM-Le_TtoX8yaAGQpDXaIu0";

#[test]
fn luma26_catalog_models_reuse_the_fal_key_and_shipping_factory() {
    let catalog = Catalog::builtin().unwrap();
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    let missing = env(&keys, &http, &settings);
    assert_eq!(build(&catalog, |adapter| make(adapter, &missing)).readiness("fal"), Some(&Readiness::NeedsKey { account: "fal".into() }));
    keys.set(&Account::new("fal").unwrap(), &Secret::new("luma26-public-fixture-not-a-key").unwrap()).unwrap();
    let ready = env(&keys, &http, &settings);
    let setup = build(&catalog, |adapter| make(adapter, &ready));
    for model in ["fal.luma-ray32", "fal.luma-ray32-i2v", "fal.luma-photon"] {
        let descriptor = catalog.get(model).unwrap();
        assert_eq!(descriptor.provider, "fal");
        assert_eq!(setup.readiness(&descriptor.provider), Some(&Readiness::Ready));
        assert!(setup.routing([(NODE, descriptor.provider.as_str())]).is_ok());
    }
    assert_eq!(http.0.load(Ordering::SeqCst), 0, "choosing a catalog model performs no paid request");
}

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
    assert_eq!(setup.readiness("openai-audio"), Some(&Readiness::NeedsKey { account: "openai".into() }));
    assert_eq!(setup.readiness("openai-images"), Some(&Readiness::NeedsKey { account: "openai".into() }));
    assert_eq!(setup.readiness("openai"), Some(&Readiness::NeedsKey { account: "openai".into() }));
    assert_eq!(setup.readiness("anthropic"), Some(&Readiness::NeedsKey { account: "anthropic".into() }));
    assert_eq!(setup.readiness("replicate"), Some(&Readiness::NeedsKey { account: "replicate".into() }));
    assert_eq!(setup.readiness("runway"), Some(&Readiness::NeedsKey { account: "runway".into() }));
    assert_eq!(setup.readiness("seedream"), Some(&Readiness::NeedsKey { account: "seedream".into() }));
    assert_eq!(setup.readiness("gemini-images"), Some(&Readiness::NeedsKey { account: "gemini".into() }));
    assert_eq!(setup.readiness("gemini-video"), Some(&Readiness::NeedsKey { account: "gemini".into() }));
    assert_eq!(setup.readiness("seedance"), Some(&Readiness::NeedsKey { account: "seedream".into() }), "the ModelArk key Seedream uses");
    assert_eq!(setup.readiness("google-veo"), Some(&Readiness::NeedsKey { account: "google-cloud".into() }));
    assert_eq!(setup.readiness("google-tts"), Some(&Readiness::NeedsKey { account: "google-cloud".into() }));
    assert_eq!(setup.readiness("google-gemini-images"), Some(&Readiness::NeedsKey { account: "google-cloud".into() }));
    assert_eq!(setup.readiness("fal"), Some(&Readiness::NeedsKey { account: "fal".into() }));
    assert_eq!(setup.readiness("tripo"), Some(&Readiness::NeedsKey { account: "tripo".into() }));
    assert_eq!(setup.readiness("meshy"), Some(&Readiness::NeedsKey { account: "meshy".into() }));
    assert_eq!(setup.readiness("higgsfield"), Some(&Readiness::NeedsKey { account: "higgsfield".into() }));
    assert_eq!(setup.readiness("kling"), Some(&Readiness::NeedsKey { account: "kling".into() }));
    assert_eq!(setup.readiness("magnific"), Some(&Readiness::NeedsKey { account: "magnific".into() }));
    assert!(matches!(setup.readiness("comfyui"), Some(Readiness::NotSetUp(why)) if why.contains("[[providers.comfyui]]")));
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
    for account in [
        "openai",
        "azure-openai",
        "bedrock",
        "anthropic",
        "deepseek",
        "mistral",
        "groq",
        "xai",
        "replicate",
        "runway",
        "bfl",
        "seedream",
        "gemini",
        "fal",
        "tripo",
        "meshy",
        "higgsfield",
        "huggingface",
        "kling",
        "elevenlabs",
        "deepgram",
        "stability",
        "magnific",
        "comfyui:gpu-box:8188",
    ] {
        keys.set(&Account::new(account).unwrap(), &Secret::new("sk-test-not-a-real-key").unwrap()).unwrap();
    }
    keys.set(&Account::new("google-cloud").unwrap(), &Secret::new(SERVICE_ACCOUNT).unwrap()).unwrap();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings {
        azure_openai_url: Some("https://demo.openai.azure.com".into()),
        bedrock_region: Some("us-east-1".into()),
        ..ProviderSettings::with_comfyui("http://gpu-box:8188", "salt")
    };
    let env = env(&keys, &http, &settings);
    let setup = build(&catalog, |adapter| make(adapter, &env));
    for (adapter, readiness) in setup.adapters() {
        assert_eq!(readiness, &Readiness::Ready, "{adapter}");
    }
    let selections: Vec<_> = [
        "openai-audio",
        "openai-images",
        "openai",
        "anthropic",
        "deepseek",
        "mistral",
        "groq",
        "xai",
        "replicate",
        "runway",
        "seedream",
        "seedance",
        "gemini-images",
        "gemini-video",
        "google-veo",
        "google-tts",
        "google-gemini-images",
        "fal",
        "tripo",
        "meshy",
        "higgsfield",
        "huggingface",
        "kling",
        "elevenlabs",
        "deepgram",
        "stability",
        "magnific",
        "bfl",
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
    let mut settings = ProviderSettings::with_comfyui("http://127.0.0.1:8188", "");
    assert!(matches!(make("comfyui", &env(&keys, &http, &settings)), Err(Readiness::NotSetUp(why)) if why.contains("salt")));
    settings.install_salt = "s".into();
    assert!(make("comfyui", &env(&keys, &http, &settings)).is_ok(), "a local server needs no key");
    settings.comfyui[0].url = "not a url".into();
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
    let env = HostEnv { keys: &keys, http, settings: &settings, now_ms: Arc::new(|| 0), progress: None };
    let Err(Readiness::Failed(why)) = make("replicate", &env) else { panic!("refused") };
    assert!(why.contains("unlock the keychain"));
    // ComfyUI's token is optional: a refused keychain means no token, not no ComfyUI.
    let settings = ProviderSettings::with_comfyui("http://127.0.0.1:8188", "s");
    let env = HostEnv { keys: &keys, http: Arc::new(Wire::default()), settings: &settings, now_ms: Arc::new(|| 0), progress: None };
    assert!(make("comfyui", &env).is_ok());
}

#[test]
fn comfyui_sends_its_stored_token_as_a_bearer_header_and_never_shows_it() {
    const KEY: &str = "sk-comfy-token-not-real-0123";
    let keys = MemoryStore::default();
    keys.set(&Account::new("comfyui:gpu-box:8188").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::with_comfyui("http://gpu-box:8188", "s");
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
    let with_user = ProviderSettings::with_comfyui("http://me:pw@gpu-box:8188", "s");
    assert!(matches!(make("comfyui", &env(&keys, &http, &with_user)), Err(Readiness::NotSetUp(why)) if !why.contains("pw")));
}

#[test]
fn the_setup_can_move_to_a_worker_thread() {
    fn send<T: Send>() {}
    send::<ProviderSetup>();
}

#[test]
fn litellm_uses_the_configured_server_and_keeps_its_virtual_key_out_of_urls() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    assert!(matches!(make("litellm", &env(&keys, &http, &ProviderSettings::default())), Err(Readiness::NotSetUp(_))));
    let settings = ProviderSettings { litellm_url: Some("https://proxy.example".into()), ..Default::default() };
    assert_eq!(make("litellm", &env(&keys, &http, &settings)).unwrap().id(), "litellm", "local proxies may omit a key");
    assert!(make("litellm", &HostEnv { keys: &Locked, http: http.clone(), settings: &settings, now_ms: Arc::new(|| 0), progress: None }).is_ok());
    const KEY: &str = "sk-litellm-public-fixture-721";
    keys.set(&Account::new("litellm").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    let provider = make("litellm", &env(&keys, &http, &settings)).unwrap();
    let job = ProviderJob {
        model: "team-model".into(),
        params: Default::default(),
        inputs: vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "A lake".into() }],
        idempotency_key: "public-fixture-721".into(),
    };
    let _ = futures::executor::block_on(provider.submit(&job));
    let requests = http.1.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://proxy.example/v1/chat/completions");
    assert!(requests[0].headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("authorization") && value == &format!("Bearer {KEY}")));
    assert!(!format!("{:?}", requests[0]).contains(KEY));
    let invalid = ProviderSettings { litellm_url: Some(format!("https://user:{KEY}@proxy.example")), ..Default::default() };
    let error = make("litellm", &env(&keys, &http, &invalid)).err().unwrap();
    assert!(!error.message("litellm").unwrap().contains(KEY));
}

#[test]
fn azure_uses_its_own_keychain_account_and_preserves_the_catalog_deployment() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    assert_eq!(key_account("azure-openai"), Some("azure-openai"));
    assert!(matches!(make("azure-openai", &env(&keys, &http, &ProviderSettings::default())), Err(Readiness::NotSetUp(_))));
    let settings = ProviderSettings { azure_openai_url: Some("https://demo.openai.azure.com".into()), ..Default::default() };
    assert_eq!(make("azure-openai", &env(&keys, &http, &settings)).err(), Some(Readiness::NeedsKey { account: "azure-openai".into() }));
    const KEY: &str = "azure-public-fixture-45";
    keys.set(&Account::new("azure-openai").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    let provider = make("azure-openai", &env(&keys, &http, &settings)).unwrap();
    assert_eq!(provider.id(), "azure-openai");
    let job = ProviderJob {
        model: "my-deployment".into(),
        params: Default::default(),
        inputs: vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "A lake".into() }],
        idempotency_key: "public-fixture-45".into(),
    };
    let _ = futures::executor::block_on(provider.submit(&job));
    let requests = http.1.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://demo.openai.azure.com/openai/v1/chat/completions");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "my-deployment");
    assert!(requests[0].headers.iter().any(|(name, value)| name == "Authorization" && value == &format!("Bearer {KEY}")));
    assert!(!format!("{:?}", requests[0]).contains(KEY));
    let invalid = ProviderSettings { azure_openai_url: Some(format!("https://user:{KEY}@demo.openai.azure.com")), ..Default::default() };
    assert!(!make("azure-openai", &env(&keys, &http, &invalid)).err().unwrap().message("azure-openai").unwrap().contains(KEY));
}

#[test]
fn bedrock_uses_its_own_keychain_account_region_and_catalog_model() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    assert_eq!(key_account("bedrock"), Some("bedrock"));
    for region in [None, Some(""), Some("  ")] {
        let settings = ProviderSettings { bedrock_region: region.map(Into::into), ..Default::default() };
        assert!(matches!(make("bedrock", &env(&keys, &http, &settings)), Err(Readiness::NotSetUp(_))), "{region:?}");
    }
    for region in ["US-EAST-1", "eu-central", "useast1", "us-east-1/../x", "cn-north-1", "us-east-1.evil.example"] {
        let settings = ProviderSettings { bedrock_region: Some(region.into()), ..Default::default() };
        let Err(Readiness::NotSetUp(why)) = make("bedrock", &env(&keys, &http, &settings)) else { panic!("{region} must refuse") };
        assert!(!why.contains(region), "{why}");
    }
    let settings = ProviderSettings { bedrock_region: Some(" us-west-2 ".into()), ..Default::default() };
    assert_eq!(make("bedrock", &env(&keys, &http, &settings)).err(), Some(Readiness::NeedsKey { account: "bedrock".into() }));
    const KEY: &str = "bedrock-public-fixture-46";
    keys.set(&Account::new("bedrock").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    let provider = make("bedrock", &env(&keys, &http, &settings)).unwrap();
    assert_eq!(provider.id(), "bedrock");
    assert!(!provider.retry_ambiguous_submit(), "a lost Bedrock reply may already be billed");
    let job = ProviderJob {
        model: "openai.gpt-oss-120b-1:0".into(),
        params: Default::default(),
        inputs: vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "hello".into() }],
        idempotency_key: "bedrock-46".into(),
    };
    let _ = futures::executor::block_on(provider.submit(&job));
    let requests = http.1.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1/chat/completions");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "openai.gpt-oss-120b-1:0");
    assert!(requests[0].headers.iter().any(|(name, value)| name == "Authorization" && value == &format!("Bearer {KEY}")));
    assert!(requests[0].secret_headers.iter().any(|name| name == "authorization"));
    assert!(!format!("{:?}", requests[0]).contains(KEY));
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
fn a_damaged_google_cloud_key_fails_every_google_cloud_adapter_without_quoting_it_or_sending_anything() {
    let catalog = Catalog::builtin().unwrap();
    let keys = MemoryStore::default();
    keys.set(&Account::new("google-cloud").unwrap(), &Secret::new("gsa1:not-a-service-account-zzz").unwrap()).unwrap();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    let env = env(&keys, &http, &settings);
    let setup = build(&catalog, |adapter| make(adapter, &env));
    for adapter in ["google-veo", "google-tts", "google-gemini-images"] {
        let Some(Readiness::Failed(why)) = setup.readiness(adapter) else { panic!("{adapter}: {:?}", setup.readiness(adapter)) };
        assert!(why.contains("paste the service-account key file again") && !why.contains("zzz"), "{adapter}: {why}");
    }
    assert_eq!(http.0.load(Ordering::SeqCst), 0);
}

/// Two servers, `studio` (two jobs at once) the default and `laptop` with no cap.
fn two_servers() -> ProviderSettings {
    let mut settings = ProviderSettings::with_comfyui("http://10.0.0.2:8188", "salt");
    settings.comfyui[0].name = "studio".into();
    settings.comfyui[0].max_jobs = Some(2);
    settings.comfyui_default = Some("studio".into());
    settings.comfyui.push(crate::settings::ComfyServer::new("laptop", "http://127.0.0.1:8188"));
    settings
}

#[test]
fn a_card_runs_on_the_server_it_names_else_the_default() {
    let settings = two_servers();
    let kinds = crate::generation_catalog::default_registry().unwrap().0;
    let values = [("model".into(), nodecraft_types::Value::Text("comfyui.sdxl".into()))].into();
    let mut card = kinds.get(nodecraft_nodes::generation::KIND).unwrap().instantiate_instance(NODE, Default::default(), &values).unwrap();
    assert_eq!(slot_for("comfyui", &card, &settings), "comfyui@studio", "no choice follows the default");
    assert_eq!(slot_for("replicate", &card, &settings), "replicate", "other adapters are their own key");
    card.config.insert(COMFY_SERVER_CONFIG.into(), nodecraft_types::Value::Text("laptop".into()));
    assert_eq!(slot_for("comfyui", &card, &settings), "comfyui@laptop");
    card.config.insert(COMFY_SERVER_CONFIG.into(), nodecraft_types::Value::Text("gone".into()));
    assert_eq!(slot_for("comfyui", &card, &settings), "comfyui@studio", "a removed server falls back to the default");
    assert_eq!(slot_for("comfyui", &card, &ProviderSettings::default()), "comfyui", "no server at all");
    assert_eq!(slot_adapter("comfyui@laptop"), "comfyui");
    assert_eq!(comfy_slot_server("comfyui@laptop"), Some("laptop"));
    assert_eq!(comfy_slot_server("comfyui"), None);
    assert_ne!(comfy_instance("studio"), comfy_instance("laptop"));
    assert_eq!(comfy_instance("studio"), comfy_instance("studio"), "stable, so restored jobs find their server");
}

#[test]
fn each_server_is_its_own_instance_with_its_own_cap() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = two_servers();
    let mut setup = ProviderSetup::default();
    for slot in ["comfyui@studio", "comfyui@laptop"] {
        assert_eq!(setup.ensure(slot, |slot| make(slot, &env(&keys, &http, &settings))), &Readiness::Ready);
    }
    const OTHER: NodeId = NodeId::from_bytes([10; 16]);
    let routing = setup.routing_for([(NODE, "comfyui@studio"), (OTHER, "comfyui@laptop")], &settings).unwrap();
    let studio = routing.targets().get(NODE, "comfyui").unwrap();
    let laptop = routing.targets().get(OTHER, "comfyui").unwrap();
    assert_eq!(studio.instance(), comfy_instance("studio"));
    assert_eq!(laptop.instance(), comfy_instance("laptop"));
    assert_eq!(routing.find_parts("comfyui", studio.instance()).unwrap().limit(), Some(2));
    assert_eq!(routing.find_parts("comfyui", laptop.instance()).unwrap().limit(), None);
    assert!(setup.routing_for([(NODE, "comfyui@elsewhere")], &settings).is_err(), "a slot not set up isn't routed");
    assert_eq!(http.0.load(Ordering::SeqCst), 0, "building sends nothing");
}

#[test]
fn bfl33_host_uses_its_keychain_account() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    assert!(matches!(make("bfl",&env(&keys,&http,&settings)),Err(Readiness::NeedsKey {account}) if account=="bfl"));
    keys.set(&Account::new("bfl").unwrap(), &Secret::new("public-bfl-fixture").unwrap()).unwrap();
    let provider = make("bfl", &env(&keys, &http, &settings)).unwrap();
    assert_eq!(provider.id(), "bfl");
    assert_eq!(key_account("bfl"), Some("bfl"));
    assert_eq!(http.0.load(Ordering::SeqCst), 0);
}

#[test]
fn huggingface48_host_uses_huggingface_key_and_reports_missing_setup() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    assert!(matches!(make("huggingface",&env(&keys,&http,&settings)),Err(Readiness::NeedsKey{account}) if account=="huggingface"));
    assert_eq!(key_account("huggingface"), Some("huggingface"));
    keys.set(&Account::new("huggingface").unwrap(), &Secret::new("public-huggingface-key").unwrap()).unwrap();
    let provider = make("huggingface", &env(&keys, &http, &settings)).unwrap();
    assert_eq!(provider.id(), "huggingface");
    assert!(!provider.retry_ambiguous_submit());
    let job = ProviderJob {
        model: nodecraft_providers::huggingface::MODEL.into(),
        params: Default::default(),
        inputs: vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "A public harbour".into() }],
        idempotency_key: "fixture".into(),
    };
    assert!(futures::executor::block_on(provider.submit(&job)).is_err());
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    let requests = http.1.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request.url, "https://router.huggingface.co/v1/chat/completions");
    assert!(request.headers.iter().any(|(name, value)| name == "Authorization" && value == "Bearer public-huggingface-key"));
}

#[test]
fn gemini44_host_uses_shared_gemini_key_and_reports_missing_setup() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    assert!(matches!(make("gemini-llm",&env(&keys,&http,&settings)),Err(Readiness::NeedsKey{account}) if account=="gemini"));
    assert_eq!(key_account("gemini-llm"), Some("gemini"));
    keys.set(&Account::new("gemini").unwrap(), &Secret::new("public-gemini-key").unwrap()).unwrap();
    let provider = make("gemini-llm", &env(&keys, &http, &settings)).unwrap();
    assert_eq!(provider.id(), "gemini-llm");
    assert!(!provider.retry_ambiguous_submit());
    let job = ProviderJob {
        model: nodecraft_providers::gemini_llm::MODEL.into(),
        params: Default::default(),
        inputs: vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "A public harbour".into() }],
        idempotency_key: "fixture".into(),
    };
    assert!(futures::executor::block_on(provider.submit(&job)).is_err());
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    let requests = http.1.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request.url, "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions");
    assert!(request.headers.iter().any(|(name, value)| name == "Authorization" && value == "Bearer public-gemini-key"));
}

#[test]
fn deepgram40_host_uses_its_keychain_account() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    assert!(matches!(make("deepgram",&env(&keys,&http,&settings)),Err(Readiness::NeedsKey {account}) if account=="deepgram"));
    keys.set(&Account::new("deepgram").unwrap(), &Secret::new("public-deepgram-fixture").unwrap()).unwrap();
    let provider = make("deepgram", &env(&keys, &http, &settings)).unwrap();
    assert_eq!(provider.id(), "deepgram");
    assert_eq!(key_account("deepgram"), Some("deepgram"));
    assert!(!provider.retry_ambiguous_submit());
    assert_eq!(http.0.load(Ordering::SeqCst), 0);
}

#[test]
fn magnific38_host_uses_a_separate_key_account_and_construction_sends_nothing() {
    let keys = MemoryStore::default();
    let http = Arc::new(Wire::default());
    let settings = ProviderSettings::default();
    assert_eq!(key_account("magnific"), Some("magnific"));
    assert!(matches!(make("magnific", &env(&keys, &http, &settings)), Err(Readiness::NeedsKey { account }) if account == "magnific"));
    keys.set(&Account::new("fal").unwrap(), &Secret::new("public-other-account-fixture").unwrap()).unwrap();
    assert!(matches!(make("magnific", &env(&keys, &http, &settings)), Err(Readiness::NeedsKey { .. })));
    keys.set(&Account::new("magnific").unwrap(), &Secret::new("public-magnific-fixture-38").unwrap()).unwrap();
    let provider = make("magnific", &env(&keys, &http, &settings)).unwrap();
    assert_eq!(provider.id(), "magnific");
    assert!(!provider.retry_ambiguous_submit());
    assert_eq!(http.0.load(Ordering::SeqCst), 0, "creating an adapter never validates a key with a paid request");
}

#[test]
fn custom_base_urls_route_catalog_providers_to_the_configured_server_only() {
    const KEY: &str = "sk-openai-public-fixture-960";
    let keys = MemoryStore::default();
    keys.set(&Account::new("openai").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    keys.set(&Account::new("anthropic").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    let http = Arc::new(Wire::default());
    let job = ProviderJob {
        model: "gpt-4.1-mini".into(),
        params: Default::default(),
        inputs: vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "A lake".into() }],
        idempotency_key: "public-fixture-960".into(),
    };
    // Nothing set: every listed provider keeps its own API.
    for entry in crate::custom_endpoints::PROVIDERS {
        let provider = make(entry.provider, &env(&keys, &http, &ProviderSettings::default())).unwrap();
        assert_eq!(provider.custom_base_url(), None, "{}", entry.provider);
    }
    let openai = make("openai", &env(&keys, &http, &ProviderSettings::default())).unwrap();
    let _ = futures::executor::block_on(openai.submit(&job));
    assert_eq!(http.1.lock().unwrap().pop().unwrap().url, "https://api.openai.com/v1/chat/completions");
    // Set: the request and the key go to that server, under the catalog provider's id.
    let mut settings = ProviderSettings::default();
    settings.base_urls.insert("openai".into(), "http://127.0.0.1:4000/v1/".into());
    let openai = make("openai", &env(&keys, &http, &settings)).unwrap();
    assert_eq!(openai.id(), "openai");
    assert_eq!(openai.custom_base_url(), Some("http://127.0.0.1:4000/v1"));
    assert!(!openai.retry_ambiguous_submit());
    let _ = futures::executor::block_on(openai.submit(&job));
    let sent = http.1.lock().unwrap().pop().unwrap();
    assert_eq!(sent.url, "http://127.0.0.1:4000/v1/chat/completions");
    assert!(sent.headers.iter().any(|(name, value)| name == "Authorization" && value == &format!("Bearer {KEY}")));
    // Each provider has its own entry: anthropic is unaffected by openai's.
    assert_eq!(make("anthropic", &env(&keys, &http, &settings)).unwrap().custom_base_url(), None);
    // Unusable: refused before the keychain is read, without echoing the address, and never
    // sent to the default host instead.
    let empty = MemoryStore::default();
    for bad in ["ftp://proxy.example/v1", "http://proxy.example/v1", "https://user:sk-secret-960@proxy.example/v1", "proxy.example"] {
        for provider in ["openai", "anthropic", "openai-images"] {
            let mut invalid = ProviderSettings::default();
            invalid.base_urls.insert(provider.into(), bad.into());
            let Err(Readiness::NotSetUp(why)) = make(provider, &env(&empty, &http, &invalid)) else { panic!("{provider} {bad} must refuse") };
            assert!(why.contains("base URL") && !why.contains("proxy.example") && !why.contains("sk-secret"), "{why}");
            assert!(make(provider, &env(&keys, &http, &invalid)).is_err());
        }
    }
    assert!(http.1.lock().unwrap().is_empty(), "nothing is sent for an unusable base URL");
}

#[test]
fn a_key_set_up_for_a_custom_base_url_never_reaches_another_adapters_own_api() {
    const KEY: &str = "sk-gateway-public-fixture-960";
    let keys = MemoryStore::default();
    keys.set(&Account::new("openai").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    let http = Arc::new(Wire::default());
    let mut settings = ProviderSettings::default();
    settings.base_urls.insert("openai".into(), "https://gateway.example/v1".into());
    // openai-images and openai-audio read the same `openai` key and have no base URL of their own.
    for adapter in ["openai-images", "openai-audio"] {
        let Err(Readiness::NotSetUp(why)) = make(adapter, &env(&keys, &http, &settings)) else { panic!("{adapter} must refuse") };
        assert!(why.contains("openai key") && why.contains("custom base URL") && !why.contains(KEY), "{why}");
    }
    // The other way round too: an images base URL keeps chat from sending the key to OpenAI.
    let mut images_only = ProviderSettings::default();
    images_only.base_urls.insert("openai-images".into(), "https://gateway.example/v1".into());
    assert!(matches!(make("openai", &env(&keys, &http, &images_only)), Err(Readiness::NotSetUp(_))));
    // With its own base URL, it is built and sends there.
    settings.base_urls.insert("openai-images".into(), "https://gateway.example/v1".into());
    assert_eq!(make("openai-images", &env(&keys, &http, &settings)).unwrap().custom_base_url(), Some("https://gateway.example/v1"));
    assert!(make("openai-audio", &env(&keys, &http, &settings)).is_err(), "openai-audio takes no base URL, so it stays refused");
    // Unrelated accounts are unaffected.
    keys.set(&Account::new("anthropic").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    assert!(make("anthropic", &env(&keys, &http, &settings)).is_ok());
    // An unreadable entry refuses too: never the default host.
    let mut unreadable = ProviderSettings::default();
    unreadable.base_urls.insert("openai".into(), crate::custom_endpoints::BaseUrlEntry::Unreadable(serde_json::json!({"url": "x"})));
    for adapter in ["openai", "openai-images", "openai-audio"] {
        assert!(matches!(make(adapter, &env(&keys, &http, &unreadable)), Err(Readiness::NotSetUp(_))), "{adapter}");
    }
    assert!(http.1.lock().unwrap().is_empty());
}
