use super::*;
use crate::secrets::SecretError;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct Origin {
    values: Mutex<BTreeMap<String, String>>,
    refuse: AtomicBool,
}

impl OriginStorage for Origin {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        Ok(self.values.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        if self.refuse.load(Ordering::Relaxed) {
            return Err(SecretError::Failed("storage refused".into()));
        }
        self.values.lock().unwrap().insert(key.to_owned(), value.to_owned());
        Ok(())
    }
    fn remove(&self, key: &str) -> Result<(), SecretError> {
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
    fn keys(&self) -> Result<Vec<String>, SecretError> {
        Ok(self.values.lock().unwrap().keys().cloned().collect())
    }
}

fn session(store: &OriginProviders) -> crate::Session {
    let mut services = Services::default();
    store.register(&mut services).unwrap();
    crate::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([72; 16]), "Provider settings fixture").unwrap())
        .unwrap()
        .with_services(services)
}

fn addresses() -> Value {
    json!({"comfyui_url": " http://127.0.0.1:8188/ ", "litellm_url": "https://proxy.example/"})
}

#[test]
fn endpoint_commands_persist_per_origin_without_touching_the_document_or_keys() {
    let origin = Arc::new(Origin::default());
    origin.set("nodecraft.key.litellm", "not-a-real-key-721").unwrap();
    let (store, notice) = OriginProviders::load(origin.clone());
    assert!(notice.is_none());
    let mut session = session(&store);
    let document = session.document().clone();
    let saved = session.dispatch("providers.settings_set", &addresses()).unwrap();
    assert_eq!(saved, json!({"comfyui_url": "http://127.0.0.1:8188", "litellm_url": "https://proxy.example"}));
    assert_eq!(session.dispatch("providers.settings_get", &json!({})).unwrap(), saved);
    assert_eq!(session.document(), &document);
    assert_eq!(store.settings().unwrap().install_salt.len(), 64);
    assert!(!saved.to_string().contains(&store.settings().unwrap().install_salt));
    let (reloaded, notice) = OriginProviders::load(origin.clone());
    assert!(notice.is_none());
    assert_eq!(reloaded.settings().unwrap(), store.settings().unwrap(), "the installation salt survives reload too");
    assert!(!origin.get(STORAGE_KEY).unwrap().unwrap().contains("not-a-real-key"));
    assert_eq!(origin.get("nodecraft.key.litellm").unwrap().as_deref(), Some("not-a-real-key-721"));
    assert_eq!(OriginProviders::load(Arc::new(Origin::default())).0.settings().unwrap(), ProviderSettings::default(), "another origin has no addresses");
    session.dispatch("providers.settings_set", &json!({})).unwrap();
    assert!(store.settings().unwrap().comfyui.is_empty() && store.settings().unwrap().litellm_url.is_none());
    assert_eq!(store.settings().unwrap().install_salt, reloaded.settings().unwrap().install_salt);
}

#[test]
fn the_comfyui_address_is_the_one_default_server_and_an_older_record_still_loads() {
    let origin = Arc::new(Origin::default());
    let (store, _) = OriginProviders::load(origin.clone());
    session(&store).dispatch("providers.settings_set", &addresses()).unwrap();
    let saved = store.settings().unwrap();
    assert_eq!(saved.comfy_servers().len(), 1);
    assert_eq!(saved.default_comfy().map(|server| (server.name.as_str(), server.url.as_str())), Some(("127.0.0.1:8188", "http://127.0.0.1:8188")));
    // A record written before servers became a list (#864) keeps its address.
    let older = Arc::new(Origin::default());
    older.set(STORAGE_KEY, r#"{"comfyui_url":"http://gpu-box:8188/","litellm_url":"https://proxy.example","install_salt":"s"}"#).unwrap();
    let (store, notice) = OriginProviders::load(older);
    assert!(notice.is_none());
    let settings = store.settings().unwrap();
    assert_eq!(comfyui_url(&settings).as_deref(), Some("http://gpu-box:8188"));
    assert_eq!(settings.litellm_url.as_deref(), Some("https://proxy.example"));
    assert_eq!(settings.install_salt, "s");
}

#[test]
fn a_failed_save_preserves_both_the_saved_and_live_configuration() {
    let origin = Arc::new(Origin::default());
    let (store, _) = OriginProviders::load(origin.clone());
    let mut session = session(&store);
    session.dispatch("providers.settings_set", &addresses()).unwrap();
    let (before, bytes) = (store.settings().unwrap(), origin.get(STORAGE_KEY).unwrap());
    origin.refuse.store(true, Ordering::Relaxed);
    assert!(session.dispatch("providers.settings_set", &json!({"comfyui_url": "https://new.example"})).is_err());
    assert_eq!(store.settings().unwrap(), before);
    assert_eq!(origin.get(STORAGE_KEY).unwrap(), bytes);
}

#[test]
fn hostile_addresses_and_unknown_fields_refuse_without_echoing_credentials() {
    let origin = Arc::new(Origin::default());
    let (store, _) = OriginProviders::load(origin.clone());
    let mut session = session(&store);
    for address in [
        "file:///tmp/secret721",
        "https://user:secret721@proxy.example",
        "https://proxy.example?key=secret721",
        "https://proxy.example/#secret721",
        "not a server",
    ] {
        let error = session.dispatch("providers.settings_set", &json!({"comfyui_url": address})).unwrap_err();
        assert!(!error.message.contains("secret721"));
    }
    assert!(session.dispatch("providers.settings_set", &json!({"key": "secret721"})).is_err());
    assert!(session.dispatch("providers.settings_set", &json!({"litellm_url": "x".repeat(MAX_URL_BYTES + 1)})).is_err());
    assert!(origin.get(STORAGE_KEY).unwrap().is_none());
    assert_eq!(store.settings().unwrap(), ProviderSettings::default());
}

#[test]
fn damaged_storage_is_reported_without_overwrite_until_a_successful_save() {
    let origin = Arc::new(Origin::default());
    origin.set(STORAGE_KEY, "{secret721: damaged}").unwrap();
    let (store, notice) = OriginProviders::load(origin.clone());
    assert!(notice.unwrap().contains("damaged"));
    assert_eq!(store.settings().unwrap(), ProviderSettings::default());
    assert_eq!(origin.get(STORAGE_KEY).unwrap().as_deref(), Some("{secret721: damaged}"));
    session(&store).dispatch("providers.settings_set", &addresses()).unwrap();
    assert!(OriginProviders::load(origin).1.is_none(), "an explicit Save repairs the record");
}

#[test]
fn entropy_failure_and_headless_calls_report_errors_instead_of_saving() {
    let origin = Arc::new(Origin::default());
    let (store, _) = OriginProviders::load(origin.clone());
    let mut session = session(&store);
    crate::execution_identity::without_entropy(|| assert!(session.dispatch("providers.settings_set", &addresses()).is_err()));
    assert!(origin.get(STORAGE_KEY).unwrap().is_none());
    let mut headless = crate::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([73; 16]), "Headless").unwrap()).unwrap();
    assert_eq!(headless.dispatch("providers.settings_get", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
}
