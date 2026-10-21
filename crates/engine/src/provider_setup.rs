//! Building provider adapters from the catalog (#409): the one place catalog provider ids meet
//! adapter constructors.
//!
//! A [`ProviderSetup`] builds each adapter at most once, when it is first needed
//! ([`ProviderSetup::ensure`]; [`build`] ensures every adapter a catalog names), and remembers
//! why the ones it couldn't build can't run ([`Readiness`]). The host's maker is [`make`]: it reads
//! the provider's key from the keychain (never logged, never in an error) and constructs the
//! adapter over the shared HTTP client. Tests pass their own maker (mock providers), so they
//! exercise the same binding and selection as the app.
//!
//! For each run, [`ProviderSetup::routing`] mints a fresh `ProviderRouting` from the built
//! adapters, routing each generation node to its adapter. An adapter that isn't ready is refused
//! there, with what to do about it, so no request is ever sent for it. Hosts show that error: a
//! node left unrouted fails later, at admission, with a generic one.
//!
//! Reading keys can show a keychain prompt: hosts call [`ProviderSetup::ensure`] (and [`build`])
//! off the UI thread, and only for the adapters a run needs.
use crate::provider_routing::ProviderRouting;
use crate::secrets::{Account, Secret, SecretError, SecretStore};
use crate::settings::ProviderSettings;
use nodecraft_catalog::Catalog;
use nodecraft_providers::Provider;
use nodecraft_providers::http::Http;
use nodecraft_providers::llm::{ApiKey, Endpoint, LlmClient};
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
        "openai" | "openai-images" => Some("openai"),
        "anthropic" => Some("anthropic"),
        "replicate" => Some("replicate"),
        "seedream" => Some("seedream"),
        "gemini-images" => Some("gemini"),
        "fal" => Some("fal"),
        "tripo" => Some("tripo"),
        "meshy" => Some("meshy"),
        "higgsfield" => Some("higgsfield"),
        "comfy-cloud" => Some("comfy-cloud"),
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

fn read_key_error(error: SecretError) -> Readiness {
    Readiness::Failed(match error {
        SecretError::Unavailable(why) | SecretError::Denied(why) | SecretError::Failed(why) | SecretError::Invalid(why) => {
            format!("the keychain refused: {why}")
        }
        _ => "the keychain refused".into(),
    })
}

fn read_key(keys: &dyn SecretStore, account: &Account) -> Result<Option<Secret>, Readiness> {
    keys.get(account).map_err(read_key_error)
}

fn needs_key(keys: &dyn SecretStore, name: &str) -> Result<Secret, Readiness> {
    let account = Account::new(name).map_err(|_| Readiness::Failed("its keychain account name is invalid".into()))?;
    read_key(keys, &account)?.ok_or(Readiness::NeedsKey { account: name.to_owned() })
}

fn adapter_error(error: nodecraft_providers::ProviderError) -> Readiness {
    Readiness::Failed(error.message)
}

/// The ComfyUI adapter from `[providers] comfyui_url`, the install salt and the server's optional
/// bearer token. Runs and option fetches (#529) both build it here, so they read the settings alike.
pub fn comfyui(env: &HostEnv<'_>) -> Result<nodecraft_providers::comfyui::ComfyProvider, Readiness> {
    let url = env.settings.comfyui_url.as_deref().map(str::trim).filter(|u| !u.is_empty());
    let Some(url) = url else {
        return Err(Readiness::NotSetUp("add the ComfyUI server's address in settings ([providers] comfyui_url)".into()));
    };
    if env.settings.install_salt.trim().is_empty() {
        return Err(Readiness::NotSetUp("the install salt isn't set ([providers] install_salt), and the settings can't be saved to make one".into()));
    }
    let authority = url.split_once("://").map_or(url, |(_, rest)| rest).split('/').next().unwrap_or("");
    if authority.contains('@') {
        return Err(Readiness::NotSetUp("remove the user name and password from the ComfyUI address; store a token in API keys instead".into()));
    }
    // A key stored for the server is its bearer token. It's optional (a local server needs
    // none), so a keychain that can't be opened or was refused means no token, not no
    // ComfyUI.
    let account = Account::server("comfyui", url).map_err(|_| Readiness::NotSetUp("the ComfyUI address isn't a server URL".into()))?;
    let stored = match env.keys.get(&account) {
        Ok(key) => key,
        Err(SecretError::Unavailable(_) | SecretError::Denied(_)) => None,
        Err(error) => return Err(read_key_error(error)),
    };
    let auth = stored.map(|key| ("Authorization".to_owned(), format!("Bearer {}", key.expose())));
    nodecraft_providers::comfyui::ComfyProvider::new(url, auth, &env.settings.install_salt, env.http.clone(), env.now_ms.clone()).map_err(adapter_error)
}

/// The Higgsfield adapter from its stored key. Runs and the restyle preset fetch (#179) both
/// build it here.
pub fn higgsfield(env: &HostEnv<'_>) -> Result<nodecraft_providers::higgsfield::Higgsfield, Readiness> {
    Ok(nodecraft_providers::higgsfield::Higgsfield::new(needs_key(env.keys, "higgsfield")?, env.http.clone()))
}

fn llm(endpoint: Endpoint, env: &HostEnv<'_>) -> LlmClient {
    LlmClient::new(endpoint, env.http.clone())
}

/// The host's maker: the adapter for `adapter`, built from its key and settings.
pub fn make(adapter: &str, env: &HostEnv<'_>) -> Result<Arc<dyn Provider>, Readiness> {
    let provider: Arc<dyn Provider> = match adapter {
        // Chat models (#503): the LLM client, keyed by its endpoint id.
        "openai" => Arc::new(llm(Endpoint::openai(ApiKey::from(&needs_key(env.keys, "openai")?)), env)),
        "anthropic" => Arc::new(llm(Endpoint::anthropic(ApiKey::from(&needs_key(env.keys, "anthropic")?)), env)),
        "openai-images" => Arc::new(nodecraft_providers::openai_images::OpenAiImages::new(needs_key(env.keys, "openai")?, env.http.clone())),
        "replicate" => Arc::new(nodecraft_providers::replicate::Replicate::new(needs_key(env.keys, "replicate")?, env.http.clone())),
        "seedream" => Arc::new(nodecraft_providers::seedream::Seedream::new(needs_key(env.keys, "seedream")?, env.http.clone())),
        "gemini-images" => Arc::new(nodecraft_providers::gemini_images::GeminiImages::new(needs_key(env.keys, "gemini")?, env.http.clone())),
        "fal" => Arc::new(nodecraft_providers::fal::Fal::new(needs_key(env.keys, "fal")?, env.http.clone())),
        "tripo" => Arc::new(nodecraft_providers::tripo::Tripo::new(needs_key(env.keys, "tripo")?, env.http.clone())),
        "meshy" => Arc::new(nodecraft_providers::meshy::Meshy::new(needs_key(env.keys, "meshy")?, env.http.clone())),
        "higgsfield" => Arc::new(higgsfield(env)?),
        "comfy-cloud" => Arc::new(nodecraft_providers::comfy_cloud::ComfyCloud::new(needs_key(env.keys, "comfy-cloud")?, env.http.clone(), env.now_ms.clone())),
        "comfyui" => Arc::new(comfyui(env)?),
        _ => return Err(Readiness::NotSetUp("NodeCraft can't run this provider's models yet".into())),
    };
    if provider.id() != adapter {
        return Err(Readiness::Failed("the adapter reports a different id".into()));
    }
    Ok(provider)
}

/// The adapters built so far, and why the others can't run.
#[derive(Default)]
pub struct ProviderSetup {
    adapters: BTreeMap<String, Arc<dyn Provider>>,
    readiness: BTreeMap<String, Readiness>,
}

/// A setup with every adapter `catalog` names built by `maker` (the host's is [`make`]). This
/// reads every key; hosts prefer [`ProviderSetup::ensure`] for the adapters a run uses.
pub fn build(catalog: &Catalog, mut maker: impl FnMut(&str) -> Result<Arc<dyn Provider>, Readiness>) -> ProviderSetup {
    let mut setup = ProviderSetup::default();
    let adapters: std::collections::BTreeSet<&str> = catalog.models().map(|m| m.provider.as_str()).collect();
    for adapter in adapters {
        setup.ensure(adapter, &mut maker);
    }
    setup
}

impl ProviderSetup {
    /// Builds `adapter` with `maker` unless it was tried before; how ready it is.
    pub fn ensure(&mut self, adapter: &str, maker: impl FnOnce(&str) -> Result<Arc<dyn Provider>, Readiness>) -> &Readiness {
        if !self.readiness.contains_key(adapter) {
            let ready = match maker(adapter) {
                Ok(provider) if provider.id() == adapter => {
                    self.adapters.insert(adapter.to_owned(), provider);
                    Readiness::Ready
                }
                Ok(_) => Readiness::Failed("the adapter reports a different id".into()),
                Err(why) => why,
            };
            self.readiness.insert(adapter.to_owned(), ready);
        }
        self.readiness.get(adapter).unwrap_or(&Readiness::Ready)
    }

    /// Forgets `adapter`, so the next [`Self::ensure`] builds it again (after its key or server
    /// changed).
    pub fn forget(&mut self, adapter: &str) {
        self.adapters.remove(adapter);
        self.readiness.remove(adapter);
    }

    /// The built `adapter`, when it is ready.
    pub fn provider(&self, adapter: &str) -> Option<&Arc<dyn Provider>> {
        self.adapters.get(adapter)
    }

    /// Whether `adapter` can run, and if not why; `None` before it was tried.
    pub fn readiness(&self, adapter: &str) -> Option<&Readiness> {
        self.readiness.get(adapter)
    }

    /// Every adapter tried, with its readiness.
    pub fn adapters(&self) -> impl Iterator<Item = (&str, &Readiness)> {
        self.readiness.iter().map(|(a, r)| (a.as_str(), r))
    }

    /// A fresh routing (to install with `Session::set_provider_routing`) sending each generation
    /// node to its adapter. An adapter that isn't ready refuses the whole routing, saying what to
    /// do, so no request is sent for it; one not tried yet is refused too (ensure it first).
    pub fn routing<'a>(&self, selections: impl IntoIterator<Item = (NodeId, &'a str)>) -> Result<ProviderRouting, Error> {
        let mut chosen: BTreeMap<NodeId, &str> = BTreeMap::new();
        for (node, adapter) in selections {
            if chosen.insert(node, adapter).is_some_and(|before| before != adapter) {
                return Err(Error::new(ErrorKind::InvalidInput, "a node is routed to two providers"));
            }
        }
        let mut routing = ProviderRouting::new();
        let mut bound = BTreeMap::new();
        for (node, adapter) in chosen {
            if !bound.contains_key(adapter) {
                let provider = match self.readiness.get(adapter) {
                    Some(Readiness::Ready) => self.adapters.get(adapter).ok_or_else(|| Error::new(ErrorKind::Internal, "a ready adapter is missing"))?,
                    Some(other) => return Err(Error::new(ErrorKind::Unsupported, other.message(adapter).unwrap_or_default())),
                    None => return Err(Error::new(ErrorKind::NotFound, format!("{adapter} hasn't been set up in this session"))),
                };
                bound.insert(adapter, routing.bind_default(provider.clone())?);
            }
            if let Some(binding) = bound.get(adapter) {
                routing.select(node, adapter, binding.target())?;
            }
        }
        Ok(routing)
    }
}

#[cfg(test)]
mod tests;
