//! Fetching run-time option lists (#529): the checkpoints, LoRAs and samplers a ComfyUI server
//! has, the restyle presets (#179) and the account's Soul IDs (#180) Higgsfield lists, and the
//! ElevenLabs voices (#670), for the catalog models whose parameter
//! contracts name them (docs/descriptor-contract.md, Option sources). Each provider is asked on
//! its own, so one that isn't set up doesn't keep the others' lists from being fetched.
//!
//! `options.refresh` asks the host's [`OPTION_SERVICE`] to fetch every list the catalog names;
//! the host runs [`fetch`] off the UI thread and hands the outcome to [`finish`], which installs
//! the lists as the session's [`OptionLists`] resource. A list that can't be fetched, or is
//! refused, keeps what was installed before, so a server that is briefly down never empties a
//! picker. `options.status` reports the last outcome. Handlers do no I/O.
use crate::commands::{CommandSpec, Context, Effect, Phase, Resources, params};
use crate::generation_catalog::GenerationCatalog;
use crate::provider_setup::HostEnv;
use crate::session::Session;
use nodecraft_types::{Choice, Error, ErrorKind, MAX_OPTION_BYTES, MAX_OPTION_LISTS, MAX_OPTIONS, OptionKey, OptionLists};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The host service that fetches option lists.
pub const OPTION_SERVICE: &str = "options.fetch";

const MAX_VALUE_BYTES: usize = 4096;

/// Where each source id is read from a ComfyUI server: the node class and the combo input whose
/// choices are the list (recorded in docs/decisions/2026-10-20-option-fetch.md).
const COMFY_SOURCES: &[(&str, &str, &str)] = &[
    ("comfyui.checkpoints", "CheckpointLoaderSimple", "ckpt_name"),
    ("comfyui.image_checkpoints", "ImageOnlyCheckpointLoader", "ckpt_name"),
    ("comfyui.loras", "LoraLoader", "lora_name"),
    ("comfyui.vaes", "VAELoader", "vae_name"),
    ("comfyui.upscale_models", "UpscaleModelLoader", "model_name"),
    ("comfyui.samplers", "KSampler", "sampler_name"),
    ("comfyui.schedulers", "KSampler", "scheduler"),
];

fn comfy_source(id: &str) -> Option<(&'static str, &'static str)> {
    COMFY_SOURCES.iter().find(|(source, _, _)| *source == id).map(|(_, class, input)| (*class, *input))
}

/// The lists Higgsfield answers.
const HIGGSFIELD_SOURCES: [&str; 2] = [nodecraft_providers::higgsfield::RESTYLE_PRESETS, nodecraft_providers::higgsfield::SOUL_IDS];

/// The lists ElevenLabs answers (#670).
const ELEVENLABS_SOURCES: [&str; 1] = [nodecraft_providers::elevenlabs::VOICES];

/// Whether NodeCraft knows where to fetch source `id`.
fn known(id: &str) -> bool {
    comfy_source(id).is_some() || HIGGSFIELD_SOURCES.contains(&id) || ELEVENLABS_SOURCES.contains(&id)
}

/// A list a catalog model's contract names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Wanted {
    owner: String,
    source: String,
    /// The parameter that narrows it, for a scoped source.
    scope: Option<String>,
}

/// Every list the catalog's contracts name, once each. The same source is one list per model
/// that names it; past the store's limits [`finish`] reports the ones it refused.
fn wanted(catalog: &GenerationCatalog) -> Vec<Wanted> {
    let mut wanted = BTreeSet::new();
    for model in catalog.run_catalog().catalog().models() {
        let Some(contract) = &model.parameter_contract else { continue };
        for rule in &contract.rules {
            if let Some(options) = &rule.options {
                wanted.insert(Wanted { owner: contract.namespace.clone(), source: options.id.clone(), scope: options.scope.clone() });
            }
        }
    }
    wanted.into_iter().collect()
}

fn wanted_in(resources: &Resources) -> Vec<Wanted> {
    GenerationCatalog::current(resources).map(wanted).unwrap_or_default()
}

/// The source ids among `wanted` a host can fetch.
fn fetchable(wanted: &[Wanted]) -> Vec<String> {
    let ids: BTreeSet<&str> = wanted.iter().filter(|w| w.scope.is_none() && known(&w.source)).map(|w| w.source.as_str()).collect();
    ids.into_iter().map(str::to_owned).collect()
}

/// One source's values as the server listed them, or why they can't be used.
type Listed = Result<Values, String>;

/// A source's usable values, in the server's order, with the labels a person reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values {
    values: Vec<Choice>,
    /// Empty, oversized or repeated values left out.
    skipped: usize,
}

/// What a fetch brought back, by source id. Small enough to send between threads: the server's
/// full node catalog is dropped as soon as the lists are read from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    lists: BTreeMap<String, Listed>,
    /// Lists whose provider isn't set up (no server, no key), and why: not a failure, since most
    /// people use only some providers.
    unavailable: BTreeMap<String, String>,
}

impl Fetched {
    /// Records why the lists `ids` of one provider weren't read.
    fn fail<'a>(&mut self, ids: impl IntoIterator<Item = &'a String>, failure: &Failure) {
        for id in ids {
            match failure {
                Failure::NotSetUp(error) => {
                    self.unavailable.insert(id.clone(), error.message.clone());
                }
                Failure::Failed(error) => {
                    self.lists.insert(id.clone(), Err(error.message.clone()));
                }
            }
        }
    }
}

/// Why a provider's lists weren't read: it isn't set up, or asking it failed.
enum Failure {
    NotSetUp(Error),
    Failed(Error),
}

impl Failure {
    fn error(&self) -> &Error {
        match self {
            Self::NotSetUp(error) | Self::Failed(error) => error,
        }
    }
}

/// What a fetch's request asks for.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRequest {
    pub sources: Vec<String>,
}

/// Reads a list's `(value, label)` pairs, refusing a list past [`MAX_OPTIONS`] values or the
/// store's [`MAX_OPTION_BYTES`] rather than truncating it (a truncated list would refuse values
/// the server has). Blank, oversized and repeated values are left out and counted with the
/// `skipped` the provider already left out: the store refuses a whole list for one of them.
fn values<'a>(options: impl IntoIterator<Item = (&'a str, &'a str)>, skipped: usize) -> Listed {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    let mut skipped = skipped;
    // As the store counts it: the value twice, once as the value and once in its key, and the label.
    let mut bytes = 0usize;
    for (value, label) in options {
        let bad = |s: &str| s.trim().is_empty() || s.len() > MAX_VALUE_BYTES;
        if bad(value) || bad(label) || !seen.insert(value) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        if values.len() >= MAX_OPTIONS {
            return Err(format!("the server lists more than {MAX_OPTIONS} values"));
        }
        bytes = bytes.saturating_add(value.len().saturating_mul(2)).saturating_add(label.len());
        if bytes > MAX_OPTION_BYTES {
            return Err("the server's list is larger than 16 MiB".into());
        }
        values.push(Choice { value: value.to_owned(), label: label.to_owned() });
    }
    Ok(Values { values, skipped })
}

/// Fetches `sources` from the providers in `env`'s settings: the ComfyUI server's lists from its
/// node catalog, the Higgsfield presets and Soul IDs and the ElevenLabs voices from their APIs.
/// The desktop awaits this on its worker; the browser polls the future on the page, with its
/// immediate origin key store. A provider that can't be asked refuses
/// its own lists; the fetch fails only when none could be.
pub async fn fetch(env: &HostEnv<'_>, sources: &[String]) -> Result<Fetched, Error> {
    let sources: Vec<&String> = sources.iter().take(MAX_OPTION_LISTS).collect();
    let mut fetched = Fetched::default();
    let mut failed: Vec<Failure> = Vec::new();
    let mut asked = 0usize;
    let comfy: Vec<(&String, (&str, &str))> = sources.iter().filter_map(|s| comfy_source(s).map(|at| (*s, at))).collect();
    if !comfy.is_empty() {
        asked += 1;
        match comfy_lists(env, &comfy).await {
            Ok(listed) => fetched.lists.extend(listed),
            Err(failure) => {
                fetched.fail(comfy.iter().map(|(id, _)| *id), &failure);
                failed.push(failure);
            }
        }
    }
    let higgsfield: Vec<&String> = sources.iter().copied().filter(|s| HIGGSFIELD_SOURCES.contains(&s.as_str())).collect();
    if !higgsfield.is_empty() {
        asked += 1;
        match higgsfield_lists(env, &higgsfield).await {
            Ok(listed) => fetched.lists.extend(listed),
            Err(failure) => {
                fetched.fail(higgsfield.iter().copied(), &failure);
                failed.push(failure);
            }
        }
    }
    let elevenlabs: Vec<&String> = sources.iter().copied().filter(|s| ELEVENLABS_SOURCES.contains(&s.as_str())).collect();
    if !elevenlabs.is_empty() {
        asked += 1;
        match elevenlabs_lists(env, &elevenlabs).await {
            Ok(listed) => fetched.lists.extend(listed),
            Err(failure) => {
                fetched.fail(elevenlabs.iter().copied(), &failure);
                failed.push(failure);
            }
        }
    }
    // Nothing could be asked: one error says why, as before there was more than one provider.
    if asked > 0 && failed.len() == asked {
        let message = failed.iter().map(|f| f.error().message.as_str()).collect::<Vec<_>>().join("; ");
        // A provider that was asked and failed says more than one that isn't set up: a retry may work.
        let first = failed.iter().find(|f| matches!(f, Failure::Failed(_))).or(failed.first());
        let kind = first.map_or(ErrorKind::Internal, |f| f.error().kind);
        return Err(Error::new(kind, message));
    }
    for source in sources {
        if !fetched.unavailable.contains_key(source.as_str()) {
            fetched.lists.entry(source.clone()).or_insert_with(|| Err("NodeCraft doesn't know where to fetch this list".into()));
        }
    }
    Ok(fetched)
}

fn not_set_up(adapter: &str) -> impl FnOnce(crate::provider_setup::Readiness) -> Failure + '_ {
    move |readiness| Failure::NotSetUp(Error::new(ErrorKind::Unsupported, readiness.message(adapter).unwrap_or_else(|| format!("{adapter} isn't set up"))))
}

/// The ComfyUI lists in `wanted`, read from the server's node catalog.
async fn comfy_lists(env: &HostEnv<'_>, wanted: &[(&String, (&str, &str))]) -> Result<Vec<(String, Listed)>, Failure> {
    let provider = crate::provider_setup::comfy_profile::catalog_provider(env).map_err(not_set_up("ComfyUI"))?;
    let catalog = provider
        .catalog()
        .await
        .map_err(|error| Failure::Failed(Error::new(provider_kind(error.kind), format!("ComfyUI's node list couldn't be read: {}", error.message))))?;
    Ok(wanted
        .iter()
        .map(|(id, (class, input))| {
            let listed = match catalog.options(class, input) {
                Some(options) => values(options.iter().map(|o| (o.as_str(), o.as_str())), 0),
                None => Err(format!("the server has no {class} node with a {input} list")),
            };
            ((*id).clone(), listed)
        })
        .collect())
}

/// The Higgsfield lists in `wanted`, each asked for on its own. Fails only when none could be
/// read, with the first list's error.
async fn higgsfield_lists(env: &HostEnv<'_>, wanted: &[&String]) -> Result<Vec<(String, Listed)>, Failure> {
    use nodecraft_providers::higgsfield::{RESTYLE_PRESETS, SOUL_IDS};
    let provider = crate::provider_setup::higgsfield(env).map_err(not_set_up("Higgsfield"))?;
    let failed = |what: &str, error: nodecraft_providers::ProviderError| {
        Error::new(provider_kind(error.kind), format!("Higgsfield's {what} couldn't be read: {}", error.message))
    };
    let mut listed = Vec::new();
    let mut first = None;
    for id in wanted {
        let read = match id.as_str() {
            RESTYLE_PRESETS => provider
                .restyle_presets()
                .await
                .map(|p| values(p.presets.iter().map(|p| (p.id.as_str(), p.name.as_str())), p.skipped))
                .map_err(|e| failed("restyle presets", e)),
            SOUL_IDS => {
                provider.soul_ids().await.map(|s| values(s.ids.iter().map(|s| (s.id.as_str(), s.name.as_str())), s.skipped)).map_err(|e| failed("Soul IDs", e))
            }
            _ => continue,
        };
        match read {
            Ok(values) => listed.push(((*id).clone(), values)),
            Err(error) => {
                listed.push(((*id).clone(), Err(error.message.clone())));
                first.get_or_insert(error);
            }
        }
    }
    match first {
        Some(error) if listed.iter().all(|(_, l)| l.is_err()) => Err(Failure::Failed(error)),
        _ => Ok(listed),
    }
}

/// The ElevenLabs voices: the library's premade voices and the account's own (#670).
async fn elevenlabs_lists(env: &HostEnv<'_>, wanted: &[&String]) -> Result<Vec<(String, Listed)>, Failure> {
    let provider = crate::provider_setup::elevenlabs(env).map_err(not_set_up("ElevenLabs"))?;
    let voices = provider
        .voices()
        .await
        .map_err(|error| Failure::Failed(Error::new(provider_kind(error.kind), format!("ElevenLabs's voices couldn't be read: {}", error.message))))?;
    let listed = values(voices.voices.iter().map(|v| (v.id.as_str(), v.name.as_str())), voices.skipped);
    Ok(wanted.iter().map(|id| ((*id).clone(), listed.clone())).collect())
}

pub(crate) fn provider_kind(kind: nodecraft_providers::ErrorKind) -> ErrorKind {
    use nodecraft_providers::ErrorKind as P;
    match kind {
        P::Auth => ErrorKind::Authentication,
        P::Network => ErrorKind::Network,
        P::Timeout => ErrorKind::Timeout,
        P::Busy | P::RateLimited => ErrorKind::Busy,
        P::Budget => ErrorKind::Budget,
        P::BlockedContent => ErrorKind::ContentBlocked,
        P::UnsupportedInput => ErrorKind::Unsupported,
        // A url that isn't a ComfyUI server, or one that answers with an error or a reply too
        // large or garbled to read: the server's problem, not NodeCraft's.
        P::NotFound => ErrorKind::NotFound,
        P::Server => ErrorKind::Network,
        P::Malformed => ErrorKind::InvalidInput,
        _ => ErrorKind::Internal,
    }
}

/// A list [`finish`] installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Installed {
    pub owner: String,
    pub source: String,
    pub values: usize,
    /// Empty, oversized or repeated values the server listed and that were left out.
    pub skipped: usize,
}

/// A list [`finish`] did not install, and why. What was installed before stays.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Refused {
    pub owner: String,
    pub source: String,
    pub reason: String,
}

/// The outcome of one fetch, which `options.status` reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub id: u64,
    pub installed: Vec<Installed>,
    pub refused: Vec<Refused>,
    /// Lists whose provider isn't set up, and why; what was installed before stays.
    pub unavailable: Vec<Refused>,
    /// Why nothing could be fetched (no server, a server that didn't answer).
    pub error: Option<String>,
}

/// The last fetch's report, a session resource.
struct LastFetch(Report);

/// A different provider destination invalidates both lists and their previous outcome.
pub fn invalidate(session: &mut Session) {
    session.swap_resource::<OptionLists>(None);
    session.swap_resource::<LastFetch>(None);
}

/// Installs what fetch `id` brought back for every list the catalog names, and records the
/// outcome for `options.status`. A failed fetch, and each refused list, keep the lists installed
/// before.
pub fn finish(session: &mut Session, id: u64, fetched: Result<Fetched, Error>) -> Report {
    let wanted = session.generation_catalog().map(wanted).unwrap_or_default();
    let mut lists = session.swap_resource::<OptionLists>(None).unwrap_or_default();
    let mut report = Report { id, installed: Vec::new(), refused: Vec::new(), unavailable: Vec::new(), error: None };
    match &fetched {
        Err(error) => report.error = Some(error.message.clone()),
        Ok(fetched) => {
            for want in wanted {
                let refuse = |reason: String| Refused { owner: want.owner.clone(), source: want.source.clone(), reason };
                if let Some(scope) = &want.scope {
                    report.refused.push(refuse(format!("lists narrowed by another parameter ({scope}) can't be fetched yet")));
                    continue;
                }
                if let Some(reason) = fetched.unavailable.get(&want.source) {
                    report.unavailable.push(refuse(reason.clone()));
                    continue;
                }
                let listed = fetched.lists.get(&want.source).cloned().unwrap_or_else(|| {
                    // The catalog changed while the fetch ran and now names a list it didn't ask for.
                    Err(if known(&want.source) { "not part of this fetch; refresh again" } else { "NodeCraft doesn't know where to fetch this list" }.into())
                });
                let Values { values, skipped } = match listed {
                    Ok(values) => values,
                    Err(reason) => {
                        report.refused.push(refuse(reason));
                        continue;
                    }
                };
                let count = values.len();
                match lists.set(OptionKey { owner: &want.owner, source: &want.source, scope: None }, values) {
                    Ok(()) => report.installed.push(Installed { owner: want.owner.clone(), source: want.source.clone(), values: count, skipped }),
                    Err(error) => report.refused.push(refuse(error.message)),
                }
            }
        }
    }
    session.swap_resource(Some(lists));
    session.swap_resource(Some(LastFetch(report.clone())));
    report
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

fn enabled_refresh(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    if !c.services.contains(OPTION_SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't fetch option lists; use a desktop or browser app"));
    }
    if fetchable(&wanted_in(c.resources)).is_empty() {
        return Err(Error::new(ErrorKind::Unsupported, "no model in the catalog takes a list NodeCraft can fetch"));
    }
    Ok(())
}

fn refresh(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_refresh(c, value)?;
    let sources = fetchable(&wanted_in(c.resources));
    Ok(Effect::Service { capability: OPTION_SERVICE, request: json!({"sources": sources}) })
}

fn enabled_status(_: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    Ok(())
}

fn status(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: NoParams = params(value)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let last = c.resources.get::<LastFetch>().map(|last| &last.0);
    Ok(Effect::None { result: json!({"last": last}) })
}

fn empty_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "properties": {}, "required": []})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "options.refresh",
            label: "Refresh option lists",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {pending: id}; fetches every run-time option list the catalog's models name (the checkpoints, LoRAs, VAEs, upscale models, samplers and schedulers the ComfyUI server in settings has, Higgsfield's restyle presets, whose labels are the preset names, the Higgsfield account's ready SOUL 2 Soul IDs, labelled by name, and the ElevenLabs voices the account can use, its own and the premade ones, labelled by name) and installs them, so node.options and edits see them. The host answers later: read the outcome with options.status. A list that can't be fetched keeps what was installed before. Refused when no model names a fetchable list, or the host can't fetch (the CLI).",
            enabled: enabled_refresh,
            run: refresh,
        },
        CommandSpec {
            id: "options.status",
            label: "Option list status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {last: {id, installed: [{owner, source, values, skipped}], refused: [{owner, source, reason}], unavailable: [{owner, source, reason}], error} | null}; the outcome of the last options.refresh (unavailable: lists whose provider has no server or key set up), or null before one finishes. Read-only.",
            enabled: enabled_status,
            run: status,
        },
    ]
}

#[cfg(test)]
mod tests;
