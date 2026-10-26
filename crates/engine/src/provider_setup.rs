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
        "huggingface" => Some("huggingface"),
        "litellm" => Some("litellm"),
        "replicate" => Some("replicate"),
        "runway" => Some("runway"),
        "seedream" => Some("seedream"),
        "bfl" => Some("bfl"),
        // Seedance runs on the same BytePlus ModelArk key (#18).
        "seedance" => Some("seedream"),
        "gemini-images" => Some("gemini"),
        "gemini-video" => Some("gemini"),
        "google-veo" | "google-tts" | "google-gemini-images" => Some(nodecraft_providers::google_cloud::ACCOUNT),
        "fal" => Some("fal"),
        "tripo" => Some("tripo"),
        "meshy" => Some("meshy"),
        "higgsfield" => Some("higgsfield"),
        "kling" => Some("kling"),
        "elevenlabs" => Some("elevenlabs"),
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
    /// Opens ComfyUI's progress socket; `None` where runs report progress by polling only (the
    /// browser, tests, hosts that never run).
    pub progress: Option<Arc<dyn nodecraft_providers::comfyui::progress::Feeds>>,
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

pub(crate) fn needs_key(keys: &dyn SecretStore, name: &str) -> Result<Secret, Readiness> {
    let account = Account::new(name).map_err(|_| Readiness::Failed("its keychain account name is invalid".into()))?;
    read_key(keys, &account)?.ok_or(Readiness::NeedsKey { account: name.to_owned() })
}

fn adapter_error(error: nodecraft_providers::ProviderError) -> Readiness {
    Readiness::Failed(error.message)
}

/// Where adapters for one ComfyUI server are kept (#864): `comfyui@<name>`. A setup holds one
/// adapter per server under these keys; each is routed as its own provider instance.
pub fn comfy_slot(name: &str) -> String {
    format!("{COMFY_SLOT}{name}")
}

const COMFY_SLOT: &str = "comfyui@";

/// The server a [`comfy_slot`] key names, or `None` for any other key.
pub fn comfy_slot_server(slot: &str) -> Option<&str> {
    slot.strip_prefix(COMFY_SLOT)
}

/// The adapter a setup key builds: `comfyui` for a server's slot, else the key itself.
pub fn slot_adapter(slot: &str) -> &str {
    if comfy_slot_server(slot).is_some() { "comfyui" } else { slot }
}

/// The node config key naming the ComfyUI server a card runs on (#864); without it the card
/// follows the default server.
pub const COMFY_SERVER_CONFIG: &str = "comfyui.server";

/// The setup key a card whose model runs on `adapter` uses: for ComfyUI, the slot of the server
/// it names (when that server exists) or of the default server, else plain `comfyui`, whose
/// readiness says to add one. Other adapters are their own key.
pub fn slot_for(adapter: &str, node: &nodecraft_graph::Node, settings: &crate::settings::ProviderSettings) -> String {
    if adapter != "comfyui" {
        return adapter.to_owned();
    }
    let named = match node.config.get(COMFY_SERVER_CONFIG) {
        Some(nodecraft_types::Value::Text(name)) => settings.comfy_server(name),
        _ => None,
    };
    named.or_else(|| settings.default_comfy()).map_or_else(|| adapter.to_owned(), |server| comfy_slot(&server.name))
}

/// The provider instance a ComfyUI server's jobs run as (#864), from its name: each server gets
/// its own lane, and a job restored after a restart goes back to the server it was sent to.
pub fn comfy_instance(name: &str) -> Option<nodecraft_types::ProviderInstanceId> {
    let hash = blake3::Hasher::new().update(b"nodecraft.comfyui.server\0").update(name.as_bytes()).finalize();
    hash.as_bytes().first_chunk::<16>().map(|bytes| nodecraft_types::ProviderInstanceId::from_bytes(*bytes))
}

/// The ComfyUI adapter for the default server (`[[providers.comfyui]]`, #864), with the install
/// salt and the server's optional bearer token. Option fetches (#529) and the node list read it.
pub fn comfyui(env: &HostEnv<'_>) -> Result<nodecraft_providers::comfyui::ComfyProvider, Readiness> {
    let Some(server) = env.settings.default_comfy() else {
        let why = if cfg!(target_arch = "wasm32") {
            "open Providers and save the ComfyUI server address; allow this page's origin in the server's CORS settings"
        } else {
            "add a ComfyUI server with the toolbar's ComfyUI button, or in settings ([[providers.comfyui]])"
        };
        return Err(Readiness::NotSetUp(why.into()));
    };
    comfyui_server(env, server)
}

/// The ComfyUI adapter for the server called `name`; runs build theirs here.
pub fn comfyui_named(env: &HostEnv<'_>, name: &str) -> Result<nodecraft_providers::comfyui::ComfyProvider, Readiness> {
    let Some(server) = env.settings.comfy_server(name) else {
        return Err(Readiness::NotSetUp(format!("there's no ComfyUI server called “{name}”; add it with the toolbar's ComfyUI button, or pick another")));
    };
    comfyui_server(env, server)
}

fn comfyui_server(env: &HostEnv<'_>, server: &crate::settings::ComfyServer) -> Result<nodecraft_providers::comfyui::ComfyProvider, Readiness> {
    let url = server.url.trim();
    if url.is_empty() {
        return Err(Readiness::NotSetUp("the ComfyUI server has no address".into()));
    }
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
    let provider =
        nodecraft_providers::comfyui::ComfyProvider::new(url, auth, &env.settings.install_salt, env.http.clone(), env.now_ms.clone()).map_err(adapter_error)?;
    Ok(match &env.progress {
        Some(feeds) => provider.with_progress(feeds.clone()),
        None => provider,
    })
}

/// The Higgsfield adapter from its stored key. Runs and the restyle preset fetch (#179) both
/// build it here.
pub fn higgsfield(env: &HostEnv<'_>) -> Result<nodecraft_providers::higgsfield::Higgsfield, Readiness> {
    Ok(nodecraft_providers::higgsfield::Higgsfield::new(needs_key(env.keys, "higgsfield")?, env.http.clone()))
}

/// ElevenLabs, from its stored key; runs and the voice list (#670) both build it here.
pub fn elevenlabs(env: &HostEnv<'_>) -> Result<nodecraft_providers::elevenlabs::ElevenLabs, Readiness> {
    Ok(nodecraft_providers::elevenlabs::ElevenLabs::new(needs_key(env.keys, "elevenlabs")?, env.http.clone()))
}

/// Veo on Vertex AI (#742), signed in as the stored Google Cloud service account.
fn google_veo(env: &HostEnv<'_>) -> Result<nodecraft_providers::google_cloud::veo::Veo, Readiness> {
    let secret = needs_key(env.keys, nodecraft_providers::google_cloud::ACCOUNT)?;
    nodecraft_providers::google_cloud::veo::Veo::new(&secret, env.http.clone(), nodecraft_providers::google_cloud::system_clock()).map_err(adapter_error)
}

fn google_tts(env: &HostEnv<'_>) -> Result<nodecraft_providers::google_cloud::tts::GoogleTts, Readiness> {
    let secret = needs_key(env.keys, nodecraft_providers::google_cloud::ACCOUNT)?;
    nodecraft_providers::google_cloud::tts::GoogleTts::new(&secret, env.http.clone(), nodecraft_providers::google_cloud::system_clock()).map_err(adapter_error)
}

/// Gemini image models on Vertex AI (#47), signed in as the same Google Cloud service account.
fn google_gemini_images(env: &HostEnv<'_>) -> Result<nodecraft_providers::google_cloud::gemini_images::GoogleGeminiImages, Readiness> {
    let secret = needs_key(env.keys, nodecraft_providers::google_cloud::ACCOUNT)?;
    nodecraft_providers::google_cloud::gemini_images::GoogleGeminiImages::new(&secret, env.http.clone(), nodecraft_providers::google_cloud::system_clock())
        .map_err(adapter_error)
}

fn llm(endpoint: Endpoint, env: &HostEnv<'_>) -> LlmClient {
    LlmClient::new(endpoint, env.http.clone())
}

/// The host's maker: the adapter for setup key `slot` (an adapter id, or a ComfyUI server's
/// [`comfy_slot`]), built from its key and settings.
pub fn make(slot: &str, env: &HostEnv<'_>) -> Result<Arc<dyn Provider>, Readiness> {
    if let Some(name) = comfy_slot_server(slot) {
        return Ok(Arc::new(comfyui_named(env, name)?));
    }
    let adapter = slot;
    let provider: Arc<dyn Provider> = match adapter {
        // Chat models (#503): the LLM client, keyed by its endpoint id.
        "openai" => Arc::new(llm(Endpoint::openai(ApiKey::from(&needs_key(env.keys, "openai")?)), env)),
        "anthropic" => Arc::new(llm(Endpoint::anthropic(ApiKey::from(&needs_key(env.keys, "anthropic")?)), env)),
        "huggingface" => Arc::new(nodecraft_providers::huggingface::HuggingFace::new(&needs_key(env.keys, "huggingface")?, env.http.clone())),
        "litellm" => {
            let url = crate::browser_providers::server_url(env.settings.litellm_url.as_deref())
                .map_err(|_| Readiness::NotSetUp("set a valid LiteLLM server address without credentials; keep its virtual key in API keys".into()))?
                .ok_or_else(|| {
                    let why = if cfg!(target_arch = "wasm32") {
                        "open Providers and save the LiteLLM server address; allow this page's origin in the server's CORS settings"
                    } else {
                        "add the LiteLLM server's address in provider settings (litellm_url)"
                    };
                    Readiness::NotSetUp(why.into())
                })?;
            let account = Account::new("litellm").map_err(|_| Readiness::Failed("invalid LiteLLM key account".into()))?;
            let key = match env.keys.get(&account) {
                Ok(key) => key,
                Err(SecretError::Unavailable(_) | SecretError::Denied(_)) => None,
                Err(error) => return Err(read_key_error(error)),
            };
            Arc::new(llm(Endpoint::litellm(&url, key.as_ref().map(ApiKey::from)), env))
        }
        "openai-images" => Arc::new(nodecraft_providers::openai_images::OpenAiImages::new(needs_key(env.keys, "openai")?, env.http.clone())),
        "replicate" => Arc::new(nodecraft_providers::replicate::Replicate::new(needs_key(env.keys, "replicate")?, env.http.clone())),
        "seedream" => Arc::new(nodecraft_providers::seedream::Seedream::new(needs_key(env.keys, "seedream")?, env.http.clone())),
        "bfl" => Arc::new(nodecraft_providers::bfl::Bfl::new(needs_key(env.keys, "bfl")?, env.http.clone())),
        "runway" => Arc::new(nodecraft_providers::runway::Runway::new(needs_key(env.keys, "runway")?, env.http.clone())),
        "seedance" => Arc::new(nodecraft_providers::seedance::Seedance::new(needs_key(env.keys, "seedream")?, env.http.clone())),
        "gemini-images" => Arc::new(nodecraft_providers::gemini_images::GeminiImages::new(needs_key(env.keys, "gemini")?, env.http.clone())),
        "gemini-video" => Arc::new(nodecraft_providers::gemini_video::GeminiVideo::new(needs_key(env.keys, "gemini")?, env.http.clone())),
        "google-veo" => Arc::new(google_veo(env)?),
        "google-tts" => Arc::new(google_tts(env)?),
        "google-gemini-images" => Arc::new(google_gemini_images(env)?),
        "fal" => Arc::new(nodecraft_providers::fal::Fal::new(needs_key(env.keys, "fal")?, env.http.clone())),
        "tripo" => Arc::new(nodecraft_providers::tripo::Tripo::new(needs_key(env.keys, "tripo")?, env.http.clone())),
        "meshy" => Arc::new(nodecraft_providers::meshy::Meshy::new(needs_key(env.keys, "meshy")?, env.http.clone())),
        "higgsfield" => Arc::new(higgsfield(env)?),
        "kling" => Arc::new(nodecraft_providers::kling::Kling::new(needs_key(env.keys, "kling")?, env.http.clone())),
        "elevenlabs" => Arc::new(elevenlabs(env)?),
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
                Ok(provider) if provider.id() == slot_adapter(adapter) => {
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
        self.routing_for(selections, &crate::settings::ProviderSettings::default())
    }

    /// [`Self::routing`], where a selection may be a ComfyUI server's [`comfy_slot`]: each server
    /// is its own provider instance, capped at the jobs at once `settings` gives it (#864).
    pub fn routing_for<'a>(
        &self,
        selections: impl IntoIterator<Item = (NodeId, &'a str)>,
        settings: &crate::settings::ProviderSettings,
    ) -> Result<ProviderRouting, Error> {
        let mut chosen: BTreeMap<NodeId, &str> = BTreeMap::new();
        for (node, slot) in selections {
            if chosen.insert(node, slot).is_some_and(|before| before != slot) {
                return Err(Error::new(ErrorKind::InvalidInput, "a node is routed to two providers"));
            }
        }
        let mut routing = ProviderRouting::new();
        let mut bound = BTreeMap::new();
        for (node, slot) in chosen {
            if !bound.contains_key(slot) {
                let provider = match self.readiness.get(slot) {
                    Some(Readiness::Ready) => self.adapters.get(slot).ok_or_else(|| Error::new(ErrorKind::Internal, "a ready adapter is missing"))?,
                    Some(other) => return Err(Error::new(ErrorKind::Unsupported, other.message(slot_adapter(slot)).unwrap_or_default())),
                    None => return Err(Error::new(ErrorKind::NotFound, format!("{slot} hasn't been set up in this session"))),
                };
                let binding = match comfy_slot_server(slot) {
                    Some(name) => {
                        let instance = comfy_instance(name).ok_or_else(|| Error::new(ErrorKind::Internal, "a server's instance id can't be made"))?;
                        let limit = settings.comfy_server(name).and_then(crate::settings::ComfyServer::max_jobs);
                        routing.bind_named_with_limit(provider.clone(), instance, limit)?
                    }
                    None => routing.bind_default(provider.clone())?,
                };
                bound.insert(slot, binding);
            }
            if let Some(binding) = bound.get(slot) {
                routing.select(node, slot_adapter(slot), binding.target())?;
            }
        }
        Ok(routing)
    }
}

#[cfg(test)]
mod tests;
