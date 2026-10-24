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
use crate::plugin_runtime::PluginStamp;
use crate::secrets::{Account, SecretStore};
use nodecraft_catalog::{InputRole, ModeSpec, ModelDescriptor, Price, Pricing, model::is_id};
use nodecraft_plugin::id::PLUGIN_PREFIX;
use nodecraft_plugin::manifest::{Capability, RegistrationKind, Runtime};
use nodecraft_plugin_store::Package;
use nodecraft_plugin_store::grants::Grant;
use nodecraft_providers::Provider;
use nodecraft_providers::broker::{Broker, Template};
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

/// One granted provider template.
#[derive(Debug)]
pub struct PluginProvider {
    pub stamp: PluginStamp,
    pub template: Template,
    revoked: AtomicBool,
}

impl PluginProvider {
    pub(crate) fn new(stamp: PluginStamp, template: Template) -> Self {
        Self { stamp, template, revoked: AtomicBool::new(false) }
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
    for (r, id) in package.manifest.registrations.iter().zip(package.manifest.registered_ids()) {
        if r.kind != RegistrationKind::Provider {
            continue;
        }
        let Some(template) = package.files.get(&r.descriptor).and_then(|bytes| Template::decode(bytes).ok()) else { continue };
        let reaches = rights.capabilities.contains(&Capability::Network) && template.origins().all(|o| rights.origins.contains(o.as_str()));
        let keyed = template.credential.is_none() || rights.capabilities.contains(&Capability::Secret);
        let provider = PluginProvider::new(stamp.clone(), template);
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
                modes: vec![ModeSpec { id: MODE.into(), output: m.output, inputs: inputs.clone(), params: None, audio: None, edit: None, document: None }],
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
/// `provider` is revoked.
pub fn broker(
    id: &str,
    provider: &Arc<PluginProvider>,
    keys: &dyn SecretStore,
    http: Arc<dyn Http>,
) -> Result<Arc<dyn Provider>, crate::provider_setup::Readiness> {
    use crate::provider_setup::Readiness;
    let key = match provider.key_account() {
        None if provider.template.credential.is_some() => return Err(Readiness::Failed("its keychain account name is invalid".into())),
        None => None,
        Some(name) => Some(crate::provider_setup::needs_key(keys, &name)?),
    };
    let held = Arc::clone(provider);
    let broker = Broker::new(id, provider.template.clone(), key, http).map_err(|e| Readiness::Failed(e.message))?.granted(Arc::new(move || !held.is_revoked()));
    Ok(Arc::new(broker))
}

#[cfg(test)]
mod tests;
