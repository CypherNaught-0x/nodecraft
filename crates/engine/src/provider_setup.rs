//! Building provider adapters from the catalog (#409): the one place catalog provider ids meet
//! adapter constructors.
//!
//! For every adapter the catalog names, [`build`] asks a maker for it and binds what it gets
//! into a [`ProviderRouting`], recording for the rest why they can't run ([`Readiness`]). The
//! host's maker is [`make`]: it reads the provider's key from the keychain (never logged, never in
//! an error) and constructs the adapter over the shared HTTP client. Tests pass their own maker
//! (mock providers), so they exercise the same binding and selection as the app.
//!
//! [`ProviderSetup::select`] then points a generation node at its adapter. An adapter that isn't
//! ready refuses with what to do about it, before planning, so no request is ever sent for it.
//!
//! Reading keys can show a keychain prompt: hosts call [`make`] off the UI thread.
use crate::provider_routing::ProviderRouting;
use crate::secrets::{Account, Secret, SecretError, SecretStore};
use crate::settings::ProviderSettings;
use nodecraft_catalog::Catalog;
use nodecraft_providers::Provider;
use nodecraft_providers::http::Http;
use nodecraft_types::{Error, ErrorKind, NodeId};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Why an adapter can't run, in words a person can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    /// No key is stored for this keychain account.
    NeedsKey {
        account: String,
    },
    /// Something other than a key is missing (a server address), or this build has no such
    /// adapter.
    NotSetUp(String),
    /// The keychain or the adapter refused (the message never holds the key).
    Failed(String),
}

impl Readiness {
    /// What to tell the person about `adapter`, or `None` when it is ready.
    pub fn message(&self, adapter: &str) -> Option<String> {
        match self {
            Readiness::Ready => None,
            Readiness::NeedsKey { account } => Some(format!("{adapter} needs an API key: add one for “{account}” in API keys.")),
            Readiness::NotSetUp(why) => Some(format!("{adapter} isn't set up: {why}")),
            Readiness::Failed(why) => Some(format!("{adapter} can't be used: {why}")),
        }
    }
}

/// The keychain account holding the key for an adapter that needs one.
pub fn key_account(adapter: &str) -> Option<&'static str> {
    match adapter {
        "openai-images" => Some("openai"),
        "replicate" => Some("replicate"),
        "seedream" => Some("seedream"),
        _ => None,
    }
}

/// What the host's maker needs.
pub struct HostEnv<'a> {
    pub keys: &'a dyn SecretStore,
    pub http: Arc<dyn Http>,
    pub settings: &'a ProviderSettings,
    /// Milliseconds on a monotonic clock (ComfyUI's prompt bookkeeping).
    pub now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}

fn read_key(keys: &dyn SecretStore, account: &Account) -> Result<Option<Secret>, Readiness> {
    keys.get(account).map_err(|error| {
        Readiness::Failed(match error {
            SecretError::Unavailable(why) | SecretError::Denied(why) | SecretError::Failed(why) | SecretError::Invalid(why) => {
                format!("the keychain refused: {why}")
            }
            _ => "the keychain refused".into(),
        })
    })
}

fn needs_key(keys: &dyn SecretStore, name: &str) -> Result<Secret, Readiness> {
    let account = Account::new(name).map_err(|_| Readiness::Failed("its keychain account name is invalid".into()))?;
    read_key(keys, &account)?.ok_or(Readiness::NeedsKey { account: name.to_owned() })
}

fn adapter_error(error: nodecraft_providers::ProviderError) -> Readiness {
    Readiness::Failed(error.message)
}

/// The host's maker: the adapter for `adapter`, built from its key and settings.
pub fn make(adapter: &str, env: &HostEnv<'_>) -> Result<Arc<dyn Provider>, Readiness> {
    let provider: Arc<dyn Provider> = match adapter {
        "openai-images" => Arc::new(nodecraft_providers::openai_images::OpenAiImages::new(needs_key(env.keys, "openai")?, env.http.clone())),
        "replicate" => Arc::new(nodecraft_providers::replicate::Replicate::new(needs_key(env.keys, "replicate")?, env.http.clone())),
        "seedream" => Arc::new(nodecraft_providers::seedream::Seedream::new(needs_key(env.keys, "seedream")?, env.http.clone())),
        "comfyui" => {
            let url = env.settings.comfyui_url.as_deref().map(str::trim).filter(|u| !u.is_empty());
            let Some(url) = url else {
                return Err(Readiness::NotSetUp("add the ComfyUI server's address in settings ([providers] comfyui_url)".into()));
            };
            if env.settings.install_salt.trim().is_empty() {
                return Err(Readiness::NotSetUp("the install salt hasn't been created yet; restart NodeCraft".into()));
            }
            // A key stored for the server is its bearer token; a local server needs none.
            let account = Account::server("comfyui", url).map_err(|_| Readiness::NotSetUp("the ComfyUI address isn't a server URL".into()))?;
            let auth = read_key(env.keys, &account)?.map(|key| ("Authorization".to_owned(), format!("Bearer {}", key.expose())));
            Arc::new(
                nodecraft_providers::comfyui::ComfyProvider::new(url, auth, &env.settings.install_salt, env.http.clone(), env.now_ms.clone())
                    .map_err(adapter_error)?,
            )
        }
        _ => return Err(Readiness::NotSetUp("this build has no adapter for it".into())),
    };
    if provider.id() != adapter {
        return Err(Readiness::Failed("the adapter reports a different id".into()));
    }
    Ok(provider)
}

/// The adapters built for a catalog, and why the others can't run.
pub struct ProviderSetup {
    routing: ProviderRouting,
    readiness: BTreeMap<String, Readiness>,
}

/// Builds every adapter `catalog` names with `maker` (the host's is [`make`]).
pub fn build(catalog: &Catalog, mut maker: impl FnMut(&str) -> Result<Arc<dyn Provider>, Readiness>) -> Result<ProviderSetup, Error> {
    let mut routing = ProviderRouting::new();
    let mut readiness = BTreeMap::new();
    let adapters: std::collections::BTreeSet<&str> = catalog.models().map(|m| m.provider.as_str()).collect();
    for adapter in adapters {
        let ready = match maker(adapter) {
            Ok(provider) if provider.id() == adapter => {
                routing.bind_default(provider)?;
                Readiness::Ready
            }
            Ok(_) => Readiness::Failed("the adapter reports a different id".into()),
            Err(why) => why,
        };
        readiness.insert(adapter.to_owned(), ready);
    }
    Ok(ProviderSetup { routing, readiness })
}

impl ProviderSetup {
    /// Whether `adapter` can run, and if not why; `None` for one the catalog doesn't name.
    pub fn readiness(&self, adapter: &str) -> Option<&Readiness> {
        self.readiness.get(adapter)
    }

    /// Every adapter the catalog names, with its readiness.
    pub fn adapters(&self) -> impl Iterator<Item = (&str, &Readiness)> {
        self.readiness.iter().map(|(a, r)| (a.as_str(), r))
    }

    /// Routes generation node `node` to `adapter`. An adapter that isn't ready is refused here,
    /// with what to do about it, so no request is ever sent for it.
    pub fn select(&mut self, node: NodeId, adapter: &str) -> Result<(), Error> {
        match self.readiness.get(adapter) {
            Some(Readiness::Ready) => {}
            Some(other) => return Err(Error::new(ErrorKind::Unsupported, other.message(adapter).unwrap_or_default())),
            None => return Err(Error::new(ErrorKind::NotFound, "no model in the catalog uses that provider")),
        }
        let target = nodecraft_types::ProviderTarget::default_instance(adapter)?;
        self.routing.select(node, adapter, &target)
    }

    /// The routing to install with `Session::set_provider_routing`.
    pub fn into_routing(self) -> ProviderRouting {
        self.routing
    }
}

#[cfg(test)]
mod tests;
