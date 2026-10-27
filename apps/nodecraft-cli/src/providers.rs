//! `providers`: every provider the model catalog names, and whether it can run here. Building an
//! adapter reads its key from the keychain (which can show an unlock prompt) and the app's
//! provider settings, read without changing them; no request is sent.
use std::collections::BTreeMap;
use std::sync::Arc;

use nodecraft_engine::Session;
use nodecraft_engine::provider_setup::{self, HostEnv, Readiness};
use nodecraft_engine::secrets::KeyringStore;
use nodecraft_engine::settings::ProviderSettings;
use nodecraft_providers::Provider;
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};
mod checks;

pub fn command(args: &[String]) -> std::process::ExitCode {
    match args.first().map(String::as_str) {
        Some("test") => checks::command(args.get(1..).unwrap_or_default()),
        Some("list") => crate::read_only("providers", args.get(1..).unwrap_or_default()),
        _ => crate::read_only("providers", args),
    }
}

/// The host's maker over the keychain and `settings`, as the app builds adapters.
pub fn keychain_maker(settings: ProviderSettings) -> Result<impl FnMut(&str) -> Result<Arc<dyn Provider>, Readiness>, Error> {
    let http: Arc<dyn nodecraft_providers::http::Http> =
        Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
    let start = std::time::Instant::now();
    let now_ms: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
    let progress: Arc<dyn nodecraft_providers::comfyui::progress::Feeds> = Arc::new(nodecraft_providers::comfyui::progress::native::NativeFeeds);
    Ok(move |adapter: &str| {
        let env = HostEnv { keys: &KeyringStore, http: http.clone(), settings: &settings, now_ms: now_ms.clone(), progress: Some(progress.clone()) };
        provider_setup::make(adapter, &env)
    })
}

/// Each provider with its model count and readiness.
pub fn providers(session: &Session, maker: impl FnMut(&str) -> Result<Arc<dyn Provider>, Readiness>) -> Result<Value, Error> {
    let catalog = session.generation_catalog()?.run_catalog();
    let mut models: BTreeMap<&str, usize> = BTreeMap::new();
    for model in catalog.catalog().models() {
        let count = models.entry(model.provider.as_str()).or_default();
        *count = count.saturating_add(1);
    }
    let setup = provider_setup::build(catalog.catalog(), maker);
    let listed = setup
        .adapters()
        .map(|(adapter, readiness)| {
            let (state, account) = match readiness {
                Readiness::Ready => ("ready", None),
                Readiness::NeedsKey { account } => ("needs_key", Some(account.as_str())),
                Readiness::NotSetUp(_) => ("not_set_up", None),
                Readiness::Failed(_) => ("failed", None),
            };
            json!({"id": adapter, "models": models.get(adapter).copied().unwrap_or(0), "state": state,
                "key_account": account.or(provider_setup::key_account(adapter)), "message": readiness.message(adapter)})
        })
        .collect();
    Ok(Value::Array(listed))
}

pub fn providers_text(listed: &Value) -> String {
    let mut out = String::new();
    for entry in listed.as_array().into_iter().flatten() {
        let id = entry["id"].as_str().unwrap_or_default();
        let models = entry["models"].as_u64().unwrap_or(0);
        let models = if models == 1 { "1 model".to_owned() } else { format!("{models} models") };
        let state = match entry["message"].as_str() {
            None => "ready".to_owned(),
            Some(message) => message.to_owned(),
        };
        out.push_str(&format!("{id:<16} {models:<10} {state}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_provider_is_listed_with_why_it_cant_run() {
        let session =
            nodecraft_automation::blank_session(nodecraft_types::ProjectId::from_bytes([1; 16]), nodecraft_types::CanvasId::from_bytes([2; 16])).unwrap();
        let listed = providers(&session, |adapter| match adapter {
            "comfyui" => Err(Readiness::NotSetUp("add the server".into())),
            _ => Err(Readiness::NeedsKey { account: provider_setup::key_account(adapter).unwrap_or("x").into() }),
        })
        .unwrap();
        let rows = listed.as_array().unwrap();
        assert!(!rows.is_empty());
        let openai = rows.iter().find(|r| r["id"] == "openai-images").unwrap();
        assert_eq!(openai["state"], "needs_key");
        assert_eq!(openai["key_account"], "openai");
        assert!(openai["models"].as_u64().unwrap() > 0);
        let text = providers_text(&listed);
        assert!(text.contains("openai-images") && text.contains("needs an API key"), "{text}");
    }
}
