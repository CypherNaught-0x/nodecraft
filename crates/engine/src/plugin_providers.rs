//! Plugin providers (#174): the provider templates of enabled, granted provider operations
//! packages. Each template's models join the generation catalog under the template's
//! registered ID, so cards pick them like any model and runs quote their template price; the
//! host builds a [`Broker`] for one when a run needs it ([`broker`]).
//!
//! A template is published only when its grant lets it do what it asks: network for its
//! origins, and the secret capability when it places a credential. Like an action, a provider
//! carries the grant it was published under and is flagged revoked once that grant is gone, and
//! its broker sends nothing after that: no paid request, and no poll, cancel or download of a
//! job it sent before.
//!
//! Its template's `limits.concurrency` holds in two places: a run sends it no more jobs at once
//! (the broker sets its scheduler lane's cap), and every broker built for it shares one
//! [`RequestGate`], so all runs and jobs followed after a restart together keep that many
//! requests in flight at most. The providers of one package share a [`ByteBudget`] as well: their
//! jobs' downloads hold at most two jobs' worth of bytes in memory at once.
//!
//! Before each paid submit, and each poll, cancel or download, the broker asks a [`GrantCheck`]
//! whether the grant it was published under still holds. On the desktop that reads the store's
//! grants afresh, so a grant revoked in another NodeCraft window sharing the store stops new work
//! here at once, before this window has read the store again.
use crate::plugin_runtime::PluginStamp;
use crate::secrets::{Account, SecretStore};
use nodecraft_catalog::{InputRole, ModeSpec, ModelDescriptor, Price, Pricing, model::is_id};
use nodecraft_plugin::id::PLUGIN_PREFIX;
use nodecraft_plugin::manifest::{Capability, RegistrationKind, Runtime};
use nodecraft_plugin_store::Package;
use nodecraft_plugin_store::StoreBackend;
use nodecraft_plugin_store::grants::Grant;
use nodecraft_providers::Provider;
use nodecraft_providers::broker::{Broker, ByteBudget, RequestGate, Template};
use nodecraft_providers::http::Http;
use nodecraft_types::Modality;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a plugin model's single mode is called.
pub const MODE: &str = "generate";
/// The input a plugin model's card fills itself.
const PROMPT: &str = "prompt";
/// The most catalog models one package publishes; templates past it are left out.
pub const MAX_PACKAGE_MODELS: usize = 256;

/// Whether the grant a plugin provider was published under still holds, asked right before the
/// broker sends anything.
#[derive(Clone)]
pub struct GrantCheck(Option<Holds>);

/// Whether a stamp's grant holds, as the store says.
type Holds = Arc<dyn Fn(&PluginStamp) -> bool + Send + Sync>;

impl std::fmt::Debug for GrantCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "GrantCheck(store)" } else { "GrantCheck(this window)" })
    }
}

impl Default for GrantCheck {
    fn default() -> Self {
        Self::this_window()
    }
}

impl GrantCheck {
    /// Only what this window last read of the store: a session store no other window shares
    /// (the browser's), or a test's.
    pub fn this_window() -> Self {
        Self(None)
    }

    /// Reads the grants the store `open` opens keeps each time it is asked: the stamp's grant
    /// must be the live one for its digest, at its epoch. A store that can't be opened, or grants
    /// that can't be read, hold nothing.
    pub fn store<B: StoreBackend, F: Fn() -> Result<B, nodecraft_types::Error> + Send + Sync + 'static>(open: F) -> Self {
        Self(Some(Arc::new(move |stamp: &PluginStamp| {
            open()
                .and_then(|backend| nodecraft_plugin_store::grants::read(&backend))
                .is_ok_and(|grants| grants.holds(&stamp.package, &stamp.digest, stamp.epoch))
        })))
    }

    /// Whether `stamp`'s grant still holds.
    pub fn holds(&self, stamp: &PluginStamp) -> bool {
        self.0.as_ref().is_none_or(|check| check(stamp))
    }
}

/// One granted provider template.
#[derive(Debug)]
pub struct PluginProvider {
    pub stamp: PluginStamp,
    pub template: Template,
    revoked: AtomicBool,
    /// Its requests at once, for every broker built for it.
    gate: Arc<RequestGate>,
    /// What its package's jobs' downloads may hold in memory at once, shared by the package's
    /// providers.
    bytes: Arc<ByteBudget>,
}

impl PluginProvider {
    pub(crate) fn new(stamp: PluginStamp, template: Template, bytes: Arc<ByteBudget>) -> Self {
        let gate = RequestGate::new(usize::try_from(template.limits.concurrency).unwrap_or(1));
        Self { stamp, template, revoked: AtomicBool::new(false), gate, bytes }
    }

    /// Whether its grant is gone: its broker sends no paid request any more.
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    pub(crate) fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    /// The keychain account its credential is kept under: the package and the API origin's
    /// host, so neither another package nor an update that moves the origin reads it.
    pub fn key_account(&self) -> Option<String> {
        self.template.credential?;
        let name = format!("{PLUGIN_PREFIX}{}", self.stamp.package.as_str());
        Account::server(&name, self.template.origin.as_str()).ok().map(|a| a.as_str().to_owned())
    }
}

/// The provider templates of `package` its `grant` covers, by registered ID. A package that
/// isn't a provider operations package has none; a template that can't be read, or reaches
/// further than the grant, is left out.
pub(crate) fn of(package: &Package, grant: &Grant, stamp: &PluginStamp) -> Vec<(String, PluginProvider)> {
    if !matches!(package.manifest.runtime, Runtime::ProviderOperations { .. }) {
        return Vec::new();
    }
    let rights = &grant.rights;
    let mut out = Vec::new();
    let mut models = 0usize;
    let bytes = ByteBudget::new(nodecraft_providers::broker::adapter::MAX_PACKAGE_BYTES);
    for (r, id) in package.manifest.registrations.iter().zip(package.manifest.registered_ids()) {
        if r.kind != RegistrationKind::Provider {
            continue;
        }
        let Some(template) = package.files.get(&r.descriptor).and_then(|bytes| Template::decode(bytes).ok()) else { continue };
        let reaches = rights.capabilities.contains(&Capability::Network) && template.origins().all(|o| rights.origins.contains(o.as_str()));
        let keyed = template.credential.is_none() || rights.capabilities.contains(&Capability::Secret);
        let provider = PluginProvider::new(stamp.clone(), template, Arc::clone(&bytes));
        // A credential with nowhere to be kept would list models that never run.
        let kept = provider.template.credential.is_none() || provider.key_account().is_some();
        models = models.saturating_add(provider.template.models.len());
        if reaches && keyed && kept && models <= MAX_PACKAGE_MODELS {
            out.push((id.as_str().to_owned(), provider));
        }
    }
    out
}

/// The catalog models provider `id` serves: one per template model whose catalog ID
/// (`<id>/<model>`) is a lowercase catalog ID, priced per job at the template's price.
pub fn models(id: &str, provider: &PluginProvider) -> Vec<ModelDescriptor> {
    let template = &provider.template;
    let inputs: Vec<InputRole> = template
        .text_roles()
        .into_iter()
        .map(|role| InputRole { role: role.to_owned(), modality: Modality::Text, min: u8::from(role == PROMPT), max: 1, inline: role == PROMPT })
        .collect();
    template
        .models
        .iter()
        .filter_map(|m| {
            let model = ModelDescriptor {
                id: format!("{id}/{}", m.id),
                provider: id.to_owned(),
                provider_model: m.id.clone(),
                label: m.label.clone(),
                description: template.label.clone(),
                modes: vec![ModeSpec {
                    id: MODE.into(),
                    output: m.output,
                    inputs: inputs.clone(),
                    params: None,
                    audio: None,
                    edit: None,
                    document: None,
                    reads: None,
                }],
                params: Default::default(),
                parameter_contract: None,
                pricing: Pricing::PerCall { price: Price(m.price) },
                usage: None,
                latency_s: 30,
                quality: 3,
                style_family: None,
                cards: true,
            };
            (is_id(&model.id) && model.validate().is_ok()).then_some(model)
        })
        .collect()
}

/// The broker for provider `id`, reading its credential from `keys`. It sends nothing once
/// `provider` is revoked, or once `grants` says its grant no longer holds.
pub fn broker(
    id: &str,
    provider: &Arc<PluginProvider>,
    keys: &dyn SecretStore,
    http: Arc<dyn Http>,
    grants: GrantCheck,
) -> Result<Arc<dyn Provider>, crate::provider_setup::Readiness> {
    use crate::provider_setup::Readiness;
    let key = match provider.key_account() {
        None if provider.template.credential.is_some() => return Err(Readiness::Failed("its keychain account name is invalid".into())),
        None => None,
        Some(name) => Some(crate::provider_setup::needs_key(keys, &name)?),
    };
    let held = Arc::clone(provider);
    let broker = Broker::new(id, provider.template.clone(), key, http)
        .map_err(|e| Readiness::Failed(e.message))?
        .granted(Arc::new(move || !held.is_revoked() && grants.holds(&held.stamp)))
        .gated(Arc::clone(&provider.gate))
        .budgeted(Arc::clone(&provider.bytes));
    Ok(Arc::new(broker))
}

#[cfg(test)]
mod tests;
