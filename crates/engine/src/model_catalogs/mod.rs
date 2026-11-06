//! Live model catalogs: the models a provider lists itself, read at run time and added to the
//! generation catalog next to the curated ones.
//!
//! - **Sources.** OpenRouter (its chat models, `GET /api/v1/models`, and its image models,
//!   `GET /api/v1/images/models`; its speech models, `GET /api/v1/models?output_modalities=speech`;
//!   and its video models, `GET /api/v1/videos/models`; all public), read when an `openrouter` key account is kept; a LiteLLM proxy
//!   (`GET /v1/models`, and `GET /model/info` when the key may read it), read when its address
//!   is set in provider settings.
//! - **Flow.** `models.catalog_refresh` asks the host's `models.catalog_fetch` service, which
//!   reads the listings off the UI thread; [`finish`] turns them into catalog models and
//!   publishes a new catalog snapshot, as plugins do. A source that can't be read keeps the
//!   models read from it before; a source that is no longer set up loses them.
//! - **Untrusted.** Every entry is bounded and validated ([`listing`]); one that doesn't make a
//!   valid model is skipped on its own. Ids are `openrouter.<id>` and `litellm.<id>`, mapped
//!   into the catalog's alphabet; the provider's own id is sent unchanged.
//! - **Cost.** A listed chat price becomes the usual token quote; a price the listing can't give
//!   up front (OpenRouter's images, a LiteLLM model without a price) is `by_usage`, so a run needs
//!   the unknown-price consent and is charged what the provider reports.
//! - **Size.** The catalog's admission budget holds a few hundred listed models next to the
//!   curated ones. Models are kept in priority order (LiteLLM, OpenRouter images, free chat
//!   models, then the rest) until the budget is reached; the report says how many were left out.
//!
//! Nothing here is saved: listed models exist for the session, and are read again at start.
mod listing;
mod price;
#[cfg(test)]
mod tests;

use crate::commands::{CommandSpec, Context, Effect, Phase, Resources, params};
use crate::provider_setup::HostEnv;
use crate::session::Session;
use nodecraft_catalog::{ModelDescriptor, Pricing};
use nodecraft_providers::{HttpRequest, secrets::Account};
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The host capability that reads the listings.
pub const SERVICE: &str = "models.catalog_fetch";
/// The adapters whose models only come from a listing.
pub const LISTED_PROVIDERS: [&str; 5] = ["openrouter", "openrouter-images", "openrouter-speech", "openrouter-video", "litellm"];
/// Most listed models offered at once, whatever the budget would hold.
pub const MAX_LISTED: usize = 640;
/// Largest listing reply read.
const MAX_LISTING_BYTES: usize = 16 * 1024 * 1024;
const TIMEOUT_MS: u64 = 30_000;
const OPENROUTER: &str = "https://openrouter.ai/api/v1";

/// A provider that lists its own models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Litellm,
    Openrouter,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Litellm, Source::Openrouter];

    pub fn label(self) -> &'static str {
        match self {
            Self::Litellm => "LiteLLM",
            Self::Openrouter => "OpenRouter",
        }
    }
}

/// What reading one source found.
#[derive(Clone, Debug)]
pub enum Listing {
    Listed(Vec<ModelDescriptor>),
    /// Read in part: these models, and the models read before stay for the providers whose own
    /// list couldn't be read (OpenRouter's images, speech or video), with why.
    Partial(Vec<ModelDescriptor>, Vec<&'static str>, Error),
    /// Not set up (no key, no address): its models are taken away.
    Off(String),
    /// Set up but unreadable: the models read before stay.
    Failed(Error),
}

/// What [`fetch`] read, by source.
#[derive(Clone, Debug, Default)]
pub struct Fetched(pub BTreeMap<Source, Listing>);

/// What a refresh did, as `models.catalog_status` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub id: u64,
    pub sources: Vec<SourceReport>,
    /// Listed models in the catalog now.
    pub offered: usize,
    /// Listed models left out because the catalog was full.
    pub left_out: usize,
    /// Why nothing changed, when the catalog couldn't be published.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceReport {
    pub source: Source,
    /// Models read from it this time.
    pub listed: Option<usize>,
    /// Why it wasn't read: not set up.
    pub off: Option<String>,
    /// Why reading it failed; the models read before are kept.
    pub error: Option<String>,
}

impl Report {
    /// One line for a notice.
    pub fn summary(&self) -> String {
        if let Some(error) = &self.error {
            return format!("Provider model lists couldn't be added: {error}");
        }
        let mut parts = Vec::new();
        for source in &self.sources {
            let label = source.source.label();
            if let (Some(n), Some(error)) = (source.listed, &source.error) {
                parts.push(format!("{label}: {n} models; one of its lists couldn't be read, so its models from before stay ({error})"));
            } else if let Some(n) = source.listed {
                parts.push(format!("{label}: {n} models"));
            } else if let Some(error) = &source.error {
                parts.push(format!("{label}: couldn't read its models ({error})"));
            }
        }
        if parts.is_empty() {
            return "No provider lists its models here yet: add an OpenRouter key or a LiteLLM address.".into();
        }
        let mut line = parts.join(" · ");
        if self.left_out > 0 {
            line.push_str(&format!(" · {} left out (the catalog is full)", self.left_out));
        }
        line
    }
}

/// The listed models in the catalog, by source, so a source that fails keeps its last ones.
#[derive(Default)]
struct Listed(BTreeMap<Source, Vec<ModelDescriptor>>);
struct LastRefresh(Report);

fn get(url: &str, key: Option<&nodecraft_providers::secrets::Secret>) -> HttpRequest {
    let request = HttpRequest::get(url).header("Accept", "application/json").timeout_ms(TIMEOUT_MS).max_response_bytes(MAX_LISTING_BYTES);
    match key {
        Some(key) => request.secret_header("Authorization", format!("Bearer {}", key.expose())),
        None => request,
    }
}

/// `message` with every copy of the key masked; error text reaches agents (`models.catalog_status`).
fn masked(message: &str, key: Option<&nodecraft_providers::secrets::Secret>) -> String {
    match key.map(|key| key.expose()).filter(|key| !key.is_empty()) {
        Some(key) => message.replace(key, "…"),
        None => message.to_owned(),
    }
}

async fn read(env: &HostEnv<'_>, request: HttpRequest, what: &str, key: Option<&nodecraft_providers::secrets::Secret>) -> Result<Vec<u8>, Error> {
    // Never redirected: the key goes to the host it was set up for only.
    let reply = env
        .http
        .send_no_redirect(request)
        .await
        .map_err(|error| Error::new(crate::option_fetch::provider_kind(error.kind), masked(&format!("{what}: {}", error.message), key)))?;
    if reply.status != 200 {
        let error = nodecraft_providers::ProviderError::from_status(reply.status, &reply.body);
        // A refused key's reply may echo it (or a hash of it): say only that it was refused.
        let message = if matches!(reply.status, 401 | 403) {
            format!("{what}: the key was refused (HTTP {})", reply.status)
        } else {
            format!("{what}: {}", error.message)
        };
        return Err(Error::new(crate::option_fetch::provider_kind(error.kind), masked(&message, key)));
    }
    Ok(reply.body)
}

fn key(env: &HostEnv<'_>, account: &str) -> Result<Option<nodecraft_providers::secrets::Secret>, Error> {
    let account = Account::new(account).map_err(|_| Error::new(ErrorKind::Internal, "invalid keychain account"))?;
    env.keys.get(&account).map_err(|error| Error::new(ErrorKind::Unsupported, format!("the keychain refused: {error}")))
}

async fn openrouter(env: &HostEnv<'_>, keyed: bool) -> Listing {
    if !keyed {
        return Listing::Off("no OpenRouter key in API keys".into());
    }
    // The listings are public, so the key isn't read (or the keychain asked) just to list them.
    let chat = match read(env, get(&format!("{OPENROUTER}/models"), None), "OpenRouter's model list", None).await {
        Ok(body) => body,
        Err(error) => return Listing::Failed(error),
    };
    let images = read(env, get(&format!("{OPENROUTER}/images/models"), None), "OpenRouter's image model list", None).await;
    let prices = read(env, get(&format!("{OPENROUTER}/models?output_modalities=image"), None), "OpenRouter's image prices", None).await.ok();
    let mut models = match listing::openrouter_chat(&chat) {
        Ok(models) => models,
        Err(error) => return Listing::Failed(error),
    };
    // A list that can't be read keeps that kind's models read before; the others still come.
    let mut kept = Vec::new();
    let mut failure = None;
    let parsed = images.and_then(|images| listing::openrouter_images(&images, prices.as_deref()));
    let speech = read(env, get(&format!("{OPENROUTER}/models?output_modalities=speech"), None), "OpenRouter's speech model list", None)
        .await
        .and_then(|body| listing::openrouter_speech(&body));
    let video = read(env, get(&format!("{OPENROUTER}/videos/models"), None), "OpenRouter's video model list", None)
        .await
        .and_then(|body| listing::openrouter_video(&body));
    for (provider, result) in [
        (nodecraft_providers::openrouter_images::ID, parsed),
        (nodecraft_providers::openrouter_media::SPEECH_ID, speech),
        (nodecraft_providers::openrouter_media::VIDEO_ID, video),
    ] {
        match result {
            Ok(listed) => models.extend(listed),
            Err(error) => {
                kept.push(provider);
                failure.get_or_insert(error);
            }
        }
    }
    match failure {
        Some(error) => Listing::Partial(models, kept, error),
        None => Listing::Listed(models),
    }
}

async fn litellm(env: &HostEnv<'_>) -> Listing {
    let base = match crate::browser_providers::server_url(env.settings.litellm_url.as_deref()) {
        Ok(Some(url)) => url.trim_end_matches('/').to_owned(),
        Ok(None) => return Listing::Off("no LiteLLM address in API keys".into()),
        Err(error) => return Listing::Failed(error),
    };
    // A proxy may run without keys.
    let key = match key(env, "litellm") {
        Ok(key) => key,
        Err(error) => return Listing::Failed(error),
    };
    let models = match read(env, get(&format!("{base}/v1/models"), key.as_ref()), "the LiteLLM model list", key.as_ref()).await {
        Ok(body) => body,
        Err(error) => return Listing::Failed(error),
    };
    // Admin-only on some proxies: without it every model is a chat model priced by the run.
    let info = read(env, get(&format!("{base}/model/info"), key.as_ref()), "LiteLLM model details", key.as_ref()).await.ok();
    match listing::litellm(&models, info.as_deref()) {
        Ok(models) => Listing::Listed(models),
        Err(error) => Listing::Failed(error),
    }
}

/// Reads every source's listing. Never fails as a whole: each source says what happened.
/// `openrouter_keyed` says whether an `openrouter` key account is kept (from key settings, so the
/// keychain isn't read to find out).
pub async fn fetch(env: &HostEnv<'_>, openrouter_keyed: bool) -> Fetched {
    let mut fetched = BTreeMap::new();
    fetched.insert(Source::Litellm, litellm(env).await);
    fetched.insert(Source::Openrouter, openrouter(env, openrouter_keyed).await);
    Fetched(fetched)
}

/// Priority when the catalog can't hold every listed model: the proxy the person runs, then
/// image, video and speech models (fewest, and only listed here), free chat models, then the rest.
pub(crate) fn rank(model: &ModelDescriptor) -> u8 {
    match (model.provider.as_str(), &model.pricing) {
        ("litellm", _) => 0,
        ("openrouter-images" | "openrouter-video" | "openrouter-speech", _) => 1,
        (_, Pricing::Free) => 2,
        _ => 3,
    }
}

/// The listed models to offer, in priority order, without duplicate ids (two remote ids can map
/// to one catalog id; the first listed wins) or ids the curated catalog already uses.
fn ordered(listed: &BTreeMap<Source, Vec<ModelDescriptor>>, taken: &BTreeSet<String>) -> Vec<ModelDescriptor> {
    let mut all: Vec<ModelDescriptor> = listed.values().flatten().cloned().collect();
    all.sort_by_key(rank);
    let mut seen = BTreeSet::new();
    all.retain(|model| !taken.contains(&model.id) && seen.insert(model.id.clone()));
    all.truncate(MAX_LISTED);
    all
}

/// Installs what [`fetch`] read and reports it.
pub fn finish(session: &mut Session, id: u64, fetched: Fetched) -> Report {
    let mut listed = session.swap_resource::<Listed>(None).unwrap_or_default();
    let mut sources = Vec::new();
    for (source, listing) in fetched.0 {
        let mut report = SourceReport { source, listed: None, off: None, error: None };
        match listing {
            Listing::Listed(models) => {
                report.listed = Some(models.len());
                listed.0.insert(source, models);
            }
            Listing::Partial(mut models, kept, error) => {
                let before = listed.0.remove(&source).unwrap_or_default();
                models.extend(before.into_iter().filter(|model| kept.contains(&model.provider.as_str())));
                report.listed = Some(models.len());
                report.error = Some(error.message);
                listed.0.insert(source, models);
            }
            Listing::Off(why) => {
                report.off = Some(why);
                listed.0.remove(&source);
            }
            Listing::Failed(error) => report.error = Some(error.message),
        }
        sources.push(report);
    }
    let taken: BTreeSet<String> = match session.generation_catalog() {
        Ok(catalog) => catalog.models_except(&LISTED_PROVIDERS).map(|model| model.id.clone()).collect(),
        Err(_) => BTreeSet::new(),
    };
    let offered = ordered(&listed.0, &taken);
    let wanted = offered.len();
    let (kept, error) = match session.publish_listed_models(offered) {
        Ok(kept) => (kept, None),
        Err(error) => (0, Some(error.message)),
    };
    session.swap_resource(Some(listed));
    let report = Report { id, sources, offered: kept, left_out: wanted.saturating_sub(kept), error };
    session.swap_resource(Some(LastRefresh(report.clone())));
    report
}

/// The last refresh's report.
pub fn last(session: &Session) -> Option<&Report> {
    session.resource::<LastRefresh>().map(|last| &last.0)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

fn schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {}})
}

fn enabled_refresh(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    if !c.services.contains(SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't read provider model lists; the desktop app can"));
    }
    Ok(())
}

fn refresh(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_refresh(c, value)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({}) })
}

fn enabled_status(_: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    Ok(())
}

fn listed_counts(resources: &Resources) -> Value {
    let mut counts = serde_json::Map::new();
    if let Some(listed) = resources.get::<Listed>() {
        for (source, models) in &listed.0 {
            counts.insert(source.label().to_lowercase(), json!(models.len()));
        }
    }
    Value::Object(counts)
}

fn status(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: NoParams = params(value)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let last = c.resources.get::<LastRefresh>().map(|last| &last.0);
    Ok(Effect::None { result: json!({"last": last, "listed": listed_counts(c.resources)}) })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "models.catalog_refresh",
            label: "Refresh provider model lists",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{} -> {pending: id}; reads the models OpenRouter (with an `openrouter` key) and a LiteLLM proxy (with its address set) list, and offers them in model pickers as `openrouter.<id>` and `litellm.<id>`. The host answers later: read the outcome with models.catalog_status. A list that can't be read keeps the models read before. Refused when the host can't fetch (the CLI).",
            enabled: enabled_refresh,
            run: refresh,
        },
        CommandSpec {
            id: "models.catalog_status",
            label: "Provider model list status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{} -> {last: {id, sources: [{source, listed, off, error}], offered, left_out, error} | null, listed: {source: count}}; what the last models.catalog_refresh read.",
            enabled: enabled_status,
            run: status,
        },
    ]
}
