//! Custom endpoints (#960): a base URL the person sets for a catalog provider whose API another
//! server can serve, such as a LiteLLM proxy, vLLM or LM Studio in front of OpenAI.
//!
//! - **Which providers.** [`PROVIDERS`] lists them, with the default each keeps when none is set.
//!   Settings panels and the `providers.settings_set` schema are generated from it.
//! - **Where it is kept.** `[providers.base_urls]` in settings (the browser keeps it with the
//!   per-site server addresses), by catalog provider id. Only the URL: the key stays in the
//!   provider's keychain account and goes to the configured host only.
//! - **What is accepted.** [`nodecraft_providers::base_url::BaseUrl`]: HTTPS, or HTTP for a local
//!   or private IP literal or `localhost`; no credentials, query or fragment. A set but unusable,
//!   unreadable or misspelled entry keeps the adapter from being built: it never falls back to
//!   the default host.
//! - **Shared keys.** An adapter that reads the same keychain account as a provider with a base
//!   URL, but has none of its own, isn't built ([`shared_key_conflict`]), and `keys.test` doesn't
//!   send that key to the provider's own API ([`account_redirected`]).
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
use nodecraft_providers::base_url::{BaseUrl, INVALID_BASE_URL};
use nodecraft_types::{Error, ErrorKind, Money, TargetSelection};
use serde::{Deserialize, Serialize};
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
    /// Whether the browser build can use one. The browser's `fetch` follows redirects itself and
    /// drops only `Authorization` across origins, so Anthropic's `x-api-key` could follow a
    /// redirect to another host; OpenAI Images needs a no-redirect transport the browser lacks.
    pub browser: bool,
}

/// Every provider that takes a custom base URL. The base includes the API's version path, as
/// the defaults show.
pub const PROVIDERS: &[CustomEndpoint] = &[
    CustomEndpoint { provider: "openai", label: "OpenAI chat", default: "https://api.openai.com/v1", browser: true },
    CustomEndpoint { provider: "anthropic", label: "Anthropic chat", default: "https://api.anthropic.com/v1", browser: false },
    CustomEndpoint { provider: "openai-images", label: "OpenAI Images", default: "https://api.openai.com/v1", browser: false },
];

/// Whether this build is the browser's.
const BROWSER: bool = cfg!(target_arch = "wasm32");

/// The entry for `provider`, when it takes a custom base URL.
pub fn endpoint(provider: &str) -> Option<&'static CustomEndpoint> {
    PROVIDERS.iter().find(|entry| entry.provider == provider)
}

/// The providers whose base URL this build can use: all of them natively, fewer in the browser.
pub fn available() -> impl Iterator<Item = &'static CustomEndpoint> {
    PROVIDERS.iter().filter(|entry| !BROWSER || entry.browser)
}

/// One `[providers.base_urls]` entry as read. Anything but text is kept as it was written, so it
/// is saved back unchanged, and it refuses its provider rather than leaving it at its default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BaseUrlEntry {
    Text(String),
    Unreadable(serde_json::Value),
}

impl BaseUrlEntry {
    /// The URL text, when the entry is text.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Unreadable(_) => None,
        }
    }
    /// Whether the entry says anything: blank text means "use the default".
    fn is_set(&self) -> bool {
        self.text().is_none_or(|text| !text.trim().is_empty())
    }
}

impl From<&str> for BaseUrlEntry {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for BaseUrlEntry {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

/// The key a `[providers.base_urls]` value that isn't a table is kept under: it refuses every
/// provider in [`PROVIDERS`] until the person fixes it.
pub const UNREADABLE_TABLE: &str = "*";
/// The most entries read; a longer table is cut there.
const MAX_ENTRIES: usize = 64;

/// The refusal for an unusable base URL. It never quotes the URL.
pub fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, INVALID_BASE_URL)
}

fn unreadable() -> Error {
    Error::new(
        ErrorKind::InvalidInput,
        "its custom base URL entry isn't a plain text URL, or is spelled like one with different case or separators; fix or remove it",
    )
}

fn not_in_browser(entry: &CustomEndpoint) -> Error {
    Error::new(ErrorKind::Unsupported, format!("a custom base URL for {} can't be used in the browser; use the desktop app or the CLI", entry.label))
}

/// The refusal for a settings key no provider in [`PROVIDERS`] answers to. It never quotes it.
pub fn unknown_provider() -> Error {
    let names: Vec<&str> = available().map(|entry| entry.provider).collect();
    Error::new(ErrorKind::InvalidInput, format!("a custom base URL can be set only for {}", names.join(", ")))
}

/// A settings key as a provider id: trimmed, lower case, `_` read as `-`.
fn normalized(key: &str) -> String {
    key.trim().to_lowercase().replace('_', "-")
}

/// Checks `value` for saving: `Ok(None)` for blank (use the default), else the normalised URL.
pub fn check(value: Option<&str>) -> Result<Option<String>, Error> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => BaseUrl::parse(value).map(|url| Some(url.as_str().to_owned())).map_err(|_| invalid()),
    }
}

/// Checks a whole map for saving: providers this build can use only, blank entries dropped,
/// URLs normalised.
pub fn check_all(urls: &BTreeMap<String, Option<String>>) -> Result<BTreeMap<String, BaseUrlEntry>, Error> {
    check_all_for(urls, BROWSER)
}

fn check_all_for(urls: &BTreeMap<String, Option<String>>, browser: bool) -> Result<BTreeMap<String, BaseUrlEntry>, Error> {
    let mut checked = BTreeMap::new();
    for (provider, value) in urls {
        let entry = endpoint(provider).ok_or_else(unknown_provider)?;
        if let Some(url) = check(value.as_deref())? {
            if browser && !entry.browser {
                return Err(not_in_browser(entry));
            }
            checked.insert(entry.provider.to_owned(), BaseUrlEntry::Text(url));
        }
    }
    Ok(checked)
}

/// Whether settings say anything about `provider`'s base URL: a set entry, a misspelled key that
/// reads as it, or an unreadable table. Such a provider never silently uses its default host.
fn entry_set(settings: &ProviderSettings, provider: &str) -> bool {
    (endpoint(provider).is_some() && settings.base_urls.contains_key(UNREADABLE_TABLE))
        || settings.base_urls.iter().any(|(key, entry)| normalized(key) == provider && entry.is_set())
}

/// The base URL set for `provider`: `Ok(None)` when none is set (or it is blank), so the
/// adapter keeps its default; an error when one is set but unusable, unreadable or misspelled.
pub fn base_url(settings: &ProviderSettings, provider: &str) -> Result<Option<BaseUrl>, Error> {
    base_url_for(settings, provider, BROWSER)
}

fn base_url_for(settings: &ProviderSettings, provider: &str, browser: bool) -> Result<Option<BaseUrl>, Error> {
    let Some(entry) = endpoint(provider) else { return Ok(None) };
    if !entry_set(settings, provider) {
        return Ok(None);
    }
    if browser && !entry.browser {
        return Err(not_in_browser(entry));
    }
    // A key that differs from the id but reads as it (`OpenAI`, `openai_images`) refuses: the
    // person meant this provider, so its default host is not what they asked for.
    let misspelled = settings.base_urls.iter().any(|(key, value)| key != provider && normalized(key) == provider && value.is_set());
    if misspelled || settings.base_urls.contains_key(UNREADABLE_TABLE) {
        return Err(unreadable());
    }
    match settings.base_urls.get(provider) {
        Some(BaseUrlEntry::Text(text)) => BaseUrl::parse(text).map(Some).map_err(|_| invalid()),
        Some(BaseUrlEntry::Unreadable(_)) | None => Err(unreadable()),
    }
}

/// Why `adapter` must not be built because it shares its keychain account with a provider whose
/// key is set up for a custom base URL while it has none of its own: it would send that key to
/// its own API. `None` when it may be built.
pub fn shared_key_conflict(settings: &ProviderSettings, adapter: &str) -> Option<Error> {
    let account = crate::provider_setup::key_account(adapter)?;
    if entry_set(settings, adapter) {
        return None;
    }
    let other = PROVIDERS.iter().find(|entry| {
        entry.provider != adapter && crate::provider_setup::key_account(entry.provider) == Some(account) && entry_set(settings, entry.provider)
    })?;
    let fix = if endpoint(adapter).is_some() {
        "set a custom base URL for it too, or remove that one"
    } else {
        "it has no custom base URL setting, so remove that one to use it"
    };
    Some(Error::new(
        ErrorKind::Unsupported,
        format!("the {account} key is set up for {}'s custom base URL, and this provider would send it to its own API; {fix}", other.label),
    ))
}

/// Whether the key in keychain `account` is set up for a custom base URL: some provider that
/// reads it has a base URL entry. `keys.test` doesn't send such a key to the provider's own API.
pub fn account_redirected(settings: &ProviderSettings, account: &str) -> bool {
    PROVIDERS.iter().any(|entry| crate::provider_setup::key_account(entry.provider) == Some(account) && entry_set(settings, entry.provider))
}

/// Reads `[providers.base_urls]` leniently. Text entries are kept as written; anything else is
/// kept as [`BaseUrlEntry::Unreadable`], which refuses its provider. A value that isn't a table
/// is kept under [`UNREADABLE_TABLE`], which refuses them all. Nothing here resets `[providers]`
/// or falls back to a default host.
pub(crate) fn lenient_base_urls<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, BaseUrlEntry>, D::Error> {
    let read = <Option<serde_json::Value> as Deserialize>::deserialize(deserializer)?;
    Ok(match read {
        None | Some(serde_json::Value::Null) => BTreeMap::new(),
        Some(serde_json::Value::Object(entries)) => entries
            .into_iter()
            .filter_map(|(key, value)| match value {
                serde_json::Value::Null => None,
                serde_json::Value::String(text) => Some((key, BaseUrlEntry::Text(text))),
                other => Some((key, BaseUrlEntry::Unreadable(other))),
            })
            .take(MAX_ENTRIES)
            .collect(),
        Some(other) => [(UNREADABLE_TABLE.to_owned(), BaseUrlEntry::Unreadable(other))].into(),
    })
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
