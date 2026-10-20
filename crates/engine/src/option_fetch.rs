//! Fetching run-time option lists (#529): the checkpoints, LoRAs and samplers a ComfyUI server
//! has, for the catalog models whose parameter contracts name them (docs/descriptor-contract.md,
//! Option sources).
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
use nodecraft_types::{Choice, Error, ErrorKind, MAX_OPTION_LISTS, MAX_OPTIONS, OptionKey, OptionLists};
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
    ("comfyui.loras", "LoraLoader", "lora_name"),
    ("comfyui.vaes", "VAELoader", "vae_name"),
    ("comfyui.upscale_models", "UpscaleModelLoader", "model_name"),
    ("comfyui.samplers", "KSampler", "sampler_name"),
    ("comfyui.schedulers", "KSampler", "scheduler"),
];

fn comfy_source(id: &str) -> Option<(&'static str, &'static str)> {
    COMFY_SOURCES.iter().find(|(source, _, _)| *source == id).map(|(_, class, input)| (*class, *input))
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
    let ids: BTreeSet<&str> = wanted.iter().filter(|w| w.scope.is_none() && comfy_source(&w.source).is_some()).map(|w| w.source.as_str()).collect();
    ids.into_iter().map(str::to_owned).collect()
}

/// One source's values as the server listed them, or why they can't be used.
type Listed = Result<Values, String>;

/// A source's usable values, in the server's order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values {
    values: Vec<String>,
    /// Empty, oversized or repeated values left out.
    skipped: usize,
}

/// What a fetch brought back, by source id. Small enough to send between threads: the server's
/// full node catalog is dropped as soon as the lists are read from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    lists: BTreeMap<String, Listed>,
}

/// What a fetch's request asks for.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRequest {
    pub sources: Vec<String>,
}

/// Reads one combo input's choices, refusing a list past [`MAX_OPTIONS`] rather than truncating
/// it (a truncated list would refuse values the server has).
fn values(options: &[String]) -> Listed {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    let mut skipped = 0usize;
    for value in options {
        if value.is_empty() || value.len() > MAX_VALUE_BYTES || !seen.insert(value.as_str()) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        if values.len() >= MAX_OPTIONS {
            return Err(format!("the server lists more than {MAX_OPTIONS} values"));
        }
        values.push(value.clone());
    }
    Ok(Values { values, skipped })
}

/// Fetches `sources` from the ComfyUI server in `env`'s settings. The host calls it off the UI
/// thread: it reads the server's token from the keychain and calls the server.
pub async fn fetch(env: &HostEnv<'_>, sources: &[String]) -> Result<Fetched, Error> {
    let provider = crate::provider_setup::comfyui(env)
        .map_err(|readiness| Error::new(ErrorKind::Unsupported, readiness.message("ComfyUI").unwrap_or_else(|| "ComfyUI isn't set up".into())))?;
    let catalog =
        provider.catalog().await.map_err(|error| Error::new(provider_kind(error.kind), format!("ComfyUI's node list couldn't be read: {}", error.message)))?;
    let mut lists = BTreeMap::new();
    for source in sources.iter().take(MAX_OPTION_LISTS) {
        let listed = match comfy_source(source) {
            Some((class, input)) => match catalog.options(class, input) {
                Some(options) => values(options),
                None => Err(format!("the server has no {class} node with a {input} list")),
            },
            None => Err("NodeCraft doesn't know where to fetch this list".into()),
        };
        lists.insert(source.clone(), listed);
    }
    Ok(Fetched { lists })
}

fn provider_kind(kind: nodecraft_providers::ErrorKind) -> ErrorKind {
    use nodecraft_providers::ErrorKind as P;
    match kind {
        P::Auth => ErrorKind::Authentication,
        P::Network => ErrorKind::Network,
        P::Timeout => ErrorKind::Timeout,
        P::Busy | P::RateLimited => ErrorKind::Busy,
        P::Budget => ErrorKind::Budget,
        P::BlockedContent => ErrorKind::ContentBlocked,
        P::UnsupportedInput => ErrorKind::Unsupported,
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
    /// Why nothing could be fetched (no server, a server that didn't answer).
    pub error: Option<String>,
}

/// The last fetch's report, a session resource.
struct LastFetch(Report);

/// Installs what fetch `id` brought back for every list the catalog names, and records the
/// outcome for `options.status`. A failed fetch, and each refused list, keep the lists installed
/// before.
pub fn finish(session: &mut Session, id: u64, fetched: Result<Fetched, Error>) -> Report {
    let wanted = session.generation_catalog().map(wanted).unwrap_or_default();
    let mut lists = session.swap_resource::<OptionLists>(None).unwrap_or_default();
    let mut report = Report { id, installed: Vec::new(), refused: Vec::new(), error: None };
    match &fetched {
        Err(error) => report.error = Some(error.message.clone()),
        Ok(fetched) => {
            for want in wanted {
                let refuse = |reason: String| Refused { owner: want.owner.clone(), source: want.source.clone(), reason };
                if let Some(scope) = &want.scope {
                    report.refused.push(refuse(format!("lists narrowed by another parameter ({scope}) can't be fetched yet")));
                    continue;
                }
                let listed = fetched.lists.get(&want.source).cloned().unwrap_or_else(|| Err("NodeCraft doesn't know where to fetch this list".into()));
                let Values { values, skipped } = match listed {
                    Ok(values) => values,
                    Err(reason) => {
                        report.refused.push(refuse(reason));
                        continue;
                    }
                };
                let count = values.len();
                let choices = values.into_iter().map(|value| Choice { label: value.clone(), value }).collect();
                match lists.set(OptionKey { owner: &want.owner, source: &want.source, scope: None }, choices) {
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
        return Err(Error::new(ErrorKind::Unsupported, "this host can't fetch option lists; the desktop app can"));
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
            params_doc: "{} -> {pending: id}; fetches every run-time option list the catalog's models name (the checkpoints, LoRAs, VAEs, upscale models, samplers and schedulers the ComfyUI server in settings has) and installs them, so node.options and edits see them. The host answers later: read the outcome with options.status. A list that can't be fetched keeps what was installed before. Refused when no model names a fetchable list, or the host can't fetch (the CLI).",
            enabled: enabled_refresh,
            run: refresh,
        },
        CommandSpec {
            id: "options.status",
            label: "Option list status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {last: {id, installed: [{owner, source, values, skipped}], refused: [{owner, source, reason}], error} | null}; the outcome of the last options.refresh, or null before one finishes. Read-only.",
            enabled: enabled_status,
            run: status,
        },
    ]
}

#[cfg(test)]
mod tests;
