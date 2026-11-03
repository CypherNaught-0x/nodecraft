//! Custom endpoints (#960): a base URL the person sets for a catalog provider whose API another
//! server can serve, such as a LiteLLM proxy, vLLM or LM Studio in front of OpenAI.
//!
//! - **Which providers.** [`PROVIDERS`] lists them, with the default each keeps when none is set.
//!   Settings panels and the `providers.settings_set` schema are generated from it.
//! - **Where it is kept.** `[providers.base_urls]` in settings (the browser keeps it with the
//!   per-site server addresses), by catalog provider id. Only the URL: the key stays in the
//!   provider's keychain account and goes to the configured host only.
//! - **What is accepted.** [`nodecraft_providers::base_url::BaseUrl`]: HTTPS, or HTTP for a local
//!   or private IP literal or `localhost`; no credentials, query or fragment. A set but unusable
//!   URL keeps the adapter from being built: it never falls back to the default host.
//! - **What it costs.** A catalog model keeps its identity and its catalog price (#961 scope).
//!   A step the catalog would price as free (or at zero) is never free on a custom endpoint: it
//!   becomes unknown-price ([`apply`]), so a capped run is blocked and an uncapped run needs the
//!   explicit partial-price consent, as for ComfyUI partner nodes. The quote binds each selected
//!   adapter's base URL ([`bind`]). Retries and reported spend follow the adapter
//!   (`LlmClient::with_base_url`): an uncertain submit is terminal, and a custom server's spend
//!   header is not read.
use crate::provider_routing::ProviderRouting;
use crate::settings::ProviderSettings;
use nodecraft_exec::cost::{Quote, Unpriced};
use nodecraft_providers::base_url::{BaseUrl, INVALID_BASE_URL, MAX_BASE_URL_BYTES};
use nodecraft_types::{Error, ErrorKind, Money, TargetSelection};
use serde::Deserialize;
use std::collections::BTreeMap;

/// A provider that can be served from a custom base URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomEndpoint {
    /// The catalog provider id, which is also the settings key.
    pub provider: &'static str,
    /// What panels call it.
    pub label: &'static str,
    /// The base URL used when none is set, including the version path.
    pub default: &'static str,
}

/// Every provider that takes a custom base URL. The base includes the API's version path, as
/// the defaults show.
pub const PROVIDERS: &[CustomEndpoint] = &[
    CustomEndpoint { provider: "openai", label: "OpenAI chat", default: "https://api.openai.com/v1" },
    CustomEndpoint { provider: "anthropic", label: "Anthropic chat", default: "https://api.anthropic.com/v1" },
    CustomEndpoint { provider: "openai-images", label: "OpenAI Images", default: "https://api.openai.com/v1" },
];

/// The entry for `provider`, when it takes a custom base URL.
pub fn endpoint(provider: &str) -> Option<&'static CustomEndpoint> {
    PROVIDERS.iter().find(|entry| entry.provider == provider)
}

/// The refusal for an unusable base URL. It never quotes the URL.
pub fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, INVALID_BASE_URL)
}

/// The refusal for a settings key no provider in [`PROVIDERS`] answers to. It never quotes it.
pub fn unknown_provider() -> Error {
    let names: Vec<&str> = PROVIDERS.iter().map(|entry| entry.provider).collect();
    Error::new(ErrorKind::InvalidInput, format!("a custom base URL can be set only for {}", names.join(", ")))
}

/// Checks `value` for saving: `Ok(None)` for blank (use the default), else the normalised URL.
pub fn check(value: Option<&str>) -> Result<Option<String>, Error> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => BaseUrl::parse(value).map(|url| Some(url.as_str().to_owned())).map_err(|_| invalid()),
    }
}

/// Checks a whole map for saving: known providers only, blank entries dropped, URLs normalised.
pub fn check_all(urls: &BTreeMap<String, Option<String>>) -> Result<BTreeMap<String, String>, Error> {
    let mut checked = BTreeMap::new();
    for (provider, value) in urls {
        let entry = endpoint(provider).ok_or_else(unknown_provider)?;
        if let Some(url) = check(value.as_deref())? {
            checked.insert(entry.provider.to_owned(), url);
        }
    }
    Ok(checked)
}

/// The base URL set for `provider`: `Ok(None)` when none is set (or it is blank), so the
/// adapter keeps its default; an error when one is set but unusable.
pub fn base_url(settings: &ProviderSettings, provider: &str) -> Result<Option<BaseUrl>, Error> {
    if endpoint(provider).is_none() {
        return Ok(None);
    }
    match settings.base_urls.get(provider).map(|value| value.trim()).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => BaseUrl::parse(value).map(Some).map_err(|_| invalid()),
    }
}

/// Reads `[providers.base_urls]` leniently: a hand-edited entry that isn't text (or a table that
/// isn't one) is dropped rather than resetting the whole `[providers]` section. Entries beyond
/// the known providers, or longer than a URL may be, are dropped too; the adapter checks the rest.
pub(crate) fn lenient_base_urls<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error> {
    let read = <Option<serde_json::Value> as Deserialize>::deserialize(deserializer)?;
    let Some(serde_json::Value::Object(entries)) = read else { return Ok(BTreeMap::new()) };
    Ok(entries
        .into_iter()
        .filter_map(|(provider, value)| {
            let entry = endpoint(&provider)?;
            let url = value.as_str()?.trim();
            (!url.is_empty() && url.len() <= MAX_BASE_URL_BYTES).then(|| (entry.provider.to_owned(), url.to_owned()))
        })
        .collect())
}

/// Why a step on a custom endpoint has no price.
pub const UNPRICED: &str = "it runs on a custom endpoint (base URL) with no price list, so NodeCraft can't tell what it charges";

/// Steps routed to an adapter built with a custom base URL are never free: a free or zero
/// price becomes unknown-price. A catalog price above zero is kept (#961). Runs for the shown
/// quote and its admitted run alike.
pub(crate) fn apply(routing: &ProviderRouting, quote: &mut Quote) -> Result<(), Error> {
    let targets = routing.targets();
    let mut changed = false;
    for step in &mut quote.steps {
        let free = match &step.per_job {
            Ok(None) => true,
            Ok(Some(amount)) => amount.micros == 0,
            Err(_) => false,
        };
        if !free {
            continue;
        }
        let custom = targets
            .iter()
            .filter(|choice| choice.node() == step.node)
            .any(|choice| routing.find_parts(choice.target().adapter(), choice.target().instance()).is_ok_and(|parts| parts.custom_endpoint().is_some()));
        if custom {
            step.per_job = Err(Unpriced::Invalid(UNPRICED.into()));
            changed = true;
        }
    }
    if changed {
        // A zero price left the totals; rebuild them from the steps still priced.
        quote.totals.clear();
        for step in &quote.steps {
            if let Ok(Some(total)) = step.total() {
                let sum = quote.totals.entry(total.currency).or_insert_with(|| Money::zero(total.currency));
                *sum = sum.checked_add(total)?;
            }
        }
    }
    Ok(())
}

/// Binds the base URL of every selected adapter built with one, so a quote taken before the
/// person changed it can't start a run (the routing nonce changes too; this says why).
pub(crate) fn bind(routing: &ProviderRouting, targets: &TargetSelection, hash: &mut blake3::Hasher) {
    let mut evidence = blake3::Hasher::new();
    evidence.update(b"nodecraft.custom-endpoints.v1");
    for choice in targets.iter() {
        let target = choice.target();
        let Ok(parts) = routing.find_parts(target.adapter(), target.instance()) else { continue };
        let Some(digest) = parts.custom_endpoint() else { continue };
        evidence.update(choice.node().as_bytes());
        evidence.update(&(target.adapter().len() as u64).to_le_bytes());
        evidence.update(target.adapter().as_bytes());
        evidence.update(digest);
    }
    hash.update(evidence.finalize().as_bytes());
}

#[cfg(test)]
mod tests;
