#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft catalog: provider and model descriptors, capability matching and pricing.
//!
//! Models are data (`assets/catalog/*.toml`, format in `docs/catalog.md`): adding a model on an
//! existing provider needs no code. Everything here is pure and builds for wasm.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod model;
pub mod pricing;
pub mod select;

pub use model::{InputRole, ModeSpec, ModelDescriptor};
pub use pricing::{LongPrompt, Price, Pricing, TokenRates, Unit};
pub use select::{Choice, Effort, MAX_MULTIPLE, Rejected, Request, Selection};

use nodecraft_types::{Error, ErrorKind, Modality, ParamValues};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Most models one catalog holds, and the largest catalog file read.
pub const MAX_MODELS: usize = 4096;
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

/// Catalog files shipped with the app, embedded so they work offline and on the web.
pub const BUILTIN: &[(&str, &str)] = &[
    ("azure-openai.toml", include_str!("../../../assets/catalog/azure-openai.toml")),
    ("anthropic.toml", include_str!("../../../assets/catalog/anthropic.toml")),
    ("deepgram.toml", include_str!("../../../assets/catalog/deepgram.toml")),
    ("fal.toml", include_str!("../../../assets/catalog/fal.toml")),
    ("luma.toml", include_str!("../../../assets/catalog/luma.toml")),
    ("hunyuan.toml", include_str!("../../../assets/catalog/hunyuan.toml")),
    ("alibaba.toml", include_str!("../../../assets/catalog/alibaba.toml")),
    ("grok.toml", include_str!("../../../assets/catalog/grok.toml")),
    ("recraft.toml", include_str!("../../../assets/catalog/recraft.toml")),
    ("ltx.toml", include_str!("../../../assets/catalog/ltx.toml")),
    ("comfyui.toml", include_str!("../../../assets/catalog/comfyui.toml")),
    ("huggingface.toml", include_str!("../../../assets/catalog/huggingface.toml")),
    ("gemini-llm.toml", include_str!("../../../assets/catalog/gemini-llm.toml")),
    ("gemini-images.toml", include_str!("../../../assets/catalog/gemini-images.toml")),
    ("gemini-video.toml", include_str!("../../../assets/catalog/gemini-video.toml")),
    ("google-gemini-images.toml", include_str!("../../../assets/catalog/google-gemini-images.toml")),
    ("google-veo.toml", include_str!("../../../assets/catalog/google-veo.toml")),
    ("google-tts.toml", include_str!("../../../assets/catalog/google-tts.toml")),
    ("openai.toml", include_str!("../../../assets/catalog/openai.toml")),
    ("openai-audio.toml", include_str!("../../../assets/catalog/openai-audio.toml")),
    ("openai-images.toml", include_str!("../../../assets/catalog/openai-images.toml")),
    ("replicate.toml", include_str!("../../../assets/catalog/replicate.toml")),
    ("runway.toml", include_str!("../../../assets/catalog/runway.toml")),
    ("seedream.toml", include_str!("../../../assets/catalog/seedream.toml")),
    ("bfl.toml", include_str!("../../../assets/catalog/bfl.toml")),
    ("seedance.toml", include_str!("../../../assets/catalog/seedance.toml")),
    ("tripo.toml", include_str!("../../../assets/catalog/tripo.toml")),
    ("elevenlabs.toml", include_str!("../../../assets/catalog/elevenlabs.toml")),
    ("meshy.toml", include_str!("../../../assets/catalog/meshy.toml")),
    ("higgsfield.toml", include_str!("../../../assets/catalog/higgsfield.toml")),
    ("kling.toml", include_str!("../../../assets/catalog/kling.toml")),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    model: Vec<ModelDescriptor>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    models: BTreeMap<String, ModelDescriptor>,
}

impl Catalog {
    /// Construct the same validated catalog from host/test descriptors.
    pub fn from_models(models: impl IntoIterator<Item = ModelDescriptor>) -> Result<Self, Error> {
        let mut catalog = Self::default();
        for model in models {
            catalog.insert(model)?;
        }
        Ok(catalog)
    }
    /// The catalog shipped with this build.
    pub fn builtin() -> Result<Self, Error> {
        let mut catalog = Self::default();
        for (name, text) in BUILTIN {
            catalog.merge(Self::from_toml(text).map_err(|error| Error::new(error.kind, format!("{name}: {}", error.message)))?)?;
        }
        Ok(catalog)
    }

    /// Parse and validate one catalog file.
    pub fn from_toml(text: &str) -> Result<Self, Error> {
        if text.len() > MAX_FILE_BYTES {
            return Err(Error::new(ErrorKind::InvalidInput, "catalog file is larger than 4 MiB"));
        }
        let file: File = toml::from_str(text).map_err(|error| Error::new(ErrorKind::InvalidInput, format!("invalid catalog file: {}", error.message())))?;
        let mut catalog = Self::default();
        for model in file.model {
            catalog.insert(model)?;
        }
        Ok(catalog)
    }

    /// Add another catalog's models; an id defined twice is an error.
    pub fn merge(&mut self, other: Self) -> Result<(), Error> {
        for model in other.models.into_values() {
            self.insert(model)?;
        }
        Ok(())
    }

    fn insert(&mut self, model: ModelDescriptor) -> Result<(), Error> {
        model.validate()?;
        if self.models.len() >= MAX_MODELS {
            return Err(Error::new(ErrorKind::InvalidInput, format!("a catalog holds at most {MAX_MODELS} models")));
        }
        if self.models.contains_key(&model.id) {
            return Err(Error::new(ErrorKind::Conflict, format!("model `{}` is defined twice", model.id)));
        }
        self.models.insert(model.id.clone(), model);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ModelDescriptor> {
        self.models.get(id)
    }

    pub fn models(&self) -> impl Iterator<Item = &ModelDescriptor> {
        self.models.values()
    }

    /// The models cards offer: every model but those with `cards = false`.
    pub fn card_models(&self) -> impl Iterator<Item = &ModelDescriptor> {
        self.models.values().filter(|model| model.cards)
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Every model for the picker (but those cards don't offer): compatible ones first, then by provider, selection tier (highest
    /// first) and label. Incompatible models carry the reason they're disabled.
    pub fn candidates<'a>(&'a self, wiring: &Wiring) -> Vec<Candidate<'a>> {
        let mut all: Vec<Candidate<'a>> = self.card_models().map(|model| Candidate { model, fit: wiring.fit(model) }).collect();
        all.sort_by(|a, b| {
            let key = |c: &Candidate<'a>| (c.fit.is_err(), c.model.provider.as_str(), std::cmp::Reverse(c.model.quality), c.model.label.as_str());
            key(a).cmp(&key(b))
        });
        all
    }

    /// [`Self::candidates`] grouped by provider, for the picker's sections (each group in the
    /// same order, compatible models first).
    pub fn by_provider(&self, wiring: &Wiring) -> BTreeMap<&str, Vec<Candidate<'_>>> {
        let mut groups: BTreeMap<&str, Vec<Candidate<'_>>> = BTreeMap::new();
        for candidate in self.candidates(wiring) {
            groups.entry(candidate.model.provider.as_str()).or_default().push(candidate);
        }
        groups
    }
}

/// What a node needs from its model: the modality it produces (`None`: any), how many of each
/// input modality are wired into it, and whether its own prompt field is filled in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wiring {
    pub output: Option<Modality>,
    pub inputs: Vec<(Modality, usize)>,
    /// The node has an inline prompt, which fills `inline` text roles when nothing is wired.
    pub inline_prompt: bool,
}

impl Wiring {
    pub fn new(output: Modality) -> Self {
        Self { output: Some(output), inputs: Vec::new(), inline_prompt: false }
    }
    pub fn with_inline_prompt(mut self) -> Self {
        self.inline_prompt = true;
        self
    }
    /// Add `count` wired inputs of `modality`.
    pub fn with(mut self, modality: Modality, count: usize) -> Self {
        match self.inputs.iter_mut().find(|(m, _)| *m == modality) {
            Some((_, n)) => *n = n.saturating_add(count),
            None => self.inputs.push((modality, count)),
        }
        self
    }
    fn wired(&self, modality: Modality) -> usize {
        self.inputs.iter().filter(|(m, _)| *m == modality).fold(0usize, |sum, (_, n)| sum.saturating_add(*n))
    }

    /// The first mode that fits, or why none does. Among modes that make the right output, the
    /// reported problem is the mildest (too many inputs, then a missing one, then an
    /// unsupported modality), so the message points at the closest match.
    pub fn fit<'a>(&self, model: &'a ModelDescriptor) -> Result<&'a ModeSpec, Mismatch> {
        let mut first_problem = None;
        for mode in &model.modes {
            if self.output.is_some_and(|output| output != mode.output) {
                continue;
            }
            match self.problem(mode) {
                None => return Ok(mode),
                Some(problem) => {
                    if first_problem.is_none_or(|best: Mismatch| problem.severity() < best.severity()) {
                        first_problem = Some(problem);
                    }
                }
            }
        }
        Err(first_problem.unwrap_or(Mismatch::Output { wanted: self.output.unwrap_or(Modality::Text) }))
    }

    fn problem(&self, mode: &ModeSpec) -> Option<Mismatch> {
        let mut checked: Vec<Modality> = Vec::new();
        for modality in self.inputs.iter().map(|(m, _)| *m).chain(mode.inputs.iter().map(|r| r.modality)) {
            if checked.contains(&modality) {
                continue;
            }
            checked.push(modality);
            let wired = self.wired(modality);
            // An inline role the node fills itself needs nothing wired.
            let (min, max) = mode.inputs.iter().filter(|r| r.modality == modality).fold((0usize, 0usize), |(lo, hi), r| {
                let min = if r.inline && self.inline_prompt { 0 } else { usize::from(r.min) };
                (lo.saturating_add(min), hi.saturating_add(usize::from(r.max)))
            });
            if max == 0 && wired > 0 {
                return Some(Mismatch::Unsupported { modality });
            }
            if wired < min {
                return Some(Mismatch::Missing { modality, min });
            }
            if wired > max {
                return Some(Mismatch::TooMany { modality, max });
            }
        }
        None
    }
}

/// Why a model can't take what's wired in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    Output { wanted: Modality },
    Unsupported { modality: Modality },
    Missing { modality: Modality, min: usize },
    TooMany { modality: Modality, max: usize },
}

fn word(modality: Modality) -> &'static str {
    match modality {
        Modality::Text => "text",
        Modality::Image => "image",
        Modality::Video => "video",
        Modality::Audio => "audio",
        Modality::Mesh => "3D",
        Modality::Document => "document",
        Modality::Style => "style",
    }
}

fn inputs(count: usize, modality: Modality) -> String {
    format!("{count} {} input{}", word(modality), if count == 1 { "" } else { "s" })
}

impl Mismatch {
    fn severity(&self) -> u8 {
        match self {
            Self::TooMany { .. } => 0,
            Self::Missing { .. } => 1,
            Self::Unsupported { .. } => 2,
            Self::Output { .. } => 3,
        }
    }

    /// The tooltip on a disabled model in the picker.
    pub fn message(&self) -> String {
        match self {
            Self::Output { wanted } => format!("Doesn't make {} output", word(*wanted)),
            Self::Unsupported { modality } => format!("Doesn't take {} inputs", word(*modality)),
            Self::Missing { modality, min } => format!("Needs at least {}", inputs(*min, *modality)),
            Self::TooMany { modality, max } => format!("Takes at most {}", inputs(*max, *modality)),
        }
    }
}

/// A model as the picker shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate<'a> {
    pub model: &'a ModelDescriptor,
    pub fit: Result<&'a ModeSpec, Mismatch>,
}

/// The result of switching a node to another model.
#[derive(Clone, Debug, PartialEq)]
pub struct Migration {
    /// Parameters for the new model: kept values plus the new model's defaults.
    pub params: ParamValues,
    /// Names the new model doesn't have.
    pub removed: Vec<String>,
    /// Names the new model has but whose value it can't take; they're back at its default.
    pub reset: Vec<String>,
}

/// Names listed in a swap notice before "and N more".
const NOTICE_NAMES: usize = 5;

fn listed(names: &[String]) -> String {
    let mut text = names.iter().take(NOTICE_NAMES).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > NOTICE_NAMES {
        text.push_str(&format!(" and {} more", names.len() - NOTICE_NAMES));
    }
    text
}

impl Migration {
    /// The notice shown after a swap, or `None` when nothing changed. `from` labels the names
    /// that only the old model had.
    pub fn notice(&self, from: &ModelDescriptor, to: &ModelDescriptor) -> Option<String> {
        let label = |model: &ModelDescriptor, name: &String| {
            model.params.params.iter().find(|spec| spec.name == *name).map_or_else(|| name.clone(), |spec| spec.label.clone())
        };
        let removed: Vec<String> = self.removed.iter().map(|name| label(from, name)).collect();
        let reset: Vec<String> = self.reset.iter().map(|name| label(to, name)).collect();
        let mut parts = Vec::new();
        if !removed.is_empty() {
            parts.push(format!("removed {} (not available there)", listed(&removed)));
        }
        if !reset.is_empty() {
            parts.push(format!("reset {} (out of range there)", listed(&reset)));
        }
        (!parts.is_empty()).then(|| format!("Switched to {}: {}.", to.label, parts.join("; ")))
    }
}

/// Most parameters and parameter text a model swap carries over (the parameter schema's own
/// limits).
const MAX_MIGRATED_PARAMS: usize = 256;
const MAX_MIGRATED_BYTES: usize = 1024 * 1024;

/// Text bytes in a value, iteratively (lists nest).
fn text_bytes(value: &nodecraft_types::Value) -> usize {
    let mut total = 0usize;
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            nodecraft_types::Value::Text(text) => total = total.saturating_add(text.len()),
            nodecraft_types::Value::Media(asset) => total = total.saturating_add(asset.mime.as_str().len()),
            nodecraft_types::Value::List(items) => stack.extend(items.iter()),
            _ => {}
        }
    }
    total
}

/// A parameter name for the notice, cut to 128 bytes at a character boundary.
fn short(name: &str) -> String {
    let end = (0..=name.len().min(128)).rev().find(|i| name.is_char_boundary(*i)).unwrap_or(0);
    name.get(..end).unwrap_or_default().to_owned()
}

/// Carry parameters over to `to` by name: values the new schema accepts are kept, the rest are
/// removed or reset and listed, and the new model's defaults fill the gaps.
pub fn migrate(params: &ParamValues, to: &ModelDescriptor) -> Result<Migration, Error> {
    to.params.validate()?;
    if params.len() > MAX_MIGRATED_PARAMS {
        return Err(Error::new(ErrorKind::InvalidInput, "too many parameters to carry over"));
    }
    // Decide and measure everything before copying any value.
    let mut keep = Vec::new();
    let (mut removed, mut reset) = (Vec::new(), Vec::new());
    let mut bytes = 0usize;
    for (name, value) in params {
        match to.params.params.iter().find(|spec| spec.name == *name) {
            Some(spec) if spec.control.validate_value(value).is_ok() => {
                bytes = bytes.saturating_add(text_bytes(value));
                keep.push(name);
            }
            Some(_) => reset.push(short(name)),
            None => removed.push(short(name)),
        }
    }
    // Absent and reset parameters use defaults; retained values override them. Measure those
    // defaults by reference too, before constructing the owned map passed to the resolver.
    for spec in &to.params.params {
        if !keep.contains(&&spec.name) {
            bytes = bytes.saturating_add(spec.default.as_ref().map_or(0, text_bytes));
        }
    }
    if bytes > MAX_MIGRATED_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, "parameters are too large to carry over"));
    }
    let kept: ParamValues = keep.into_iter().filter_map(|name| params.get(name).map(|value| (name.clone(), value.clone()))).collect();
    let params = to.params.resolve(&kept, false)?.values;
    Ok(Migration { params, removed, reset })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod comparison_tests;

#[cfg(test)]
mod grok_tests;

#[cfg(test)]
mod hunyuan_tests;

#[cfg(test)]
mod alibaba_tests;

#[cfg(test)]
mod recraft_tests;
