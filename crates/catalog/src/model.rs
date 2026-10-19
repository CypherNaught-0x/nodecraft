//! Model descriptors: what a model makes, from which inputs, with which parameters, at what
//! price. Catalog files are untrusted data and every descriptor is validated before use.
use crate::pricing::Pricing;
use nodecraft_types::{Currency, Error, ErrorKind, Modality, Money, ParamControl, ParamSchema, ParamValues};
use serde::Deserialize;

pub const MAX_MODES: usize = 32;
pub const MAX_ROLES: usize = 16;
pub const MAX_TEXT: usize = 4096;

fn invalid(model: &str, message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, format!("model `{model}`: {message}"))
}

/// Lowercase ASCII letters, digits and `.`, `-`, `_`, `/` (e.g. `fal.flux-dev`), in
/// `/`-separated segments that are neither empty nor `.`/`..`, so an id is safe as a path.
pub fn is_id(value: &str) -> bool {
    nodecraft_types::model_namespace(value)
}

/// One input a mode takes, e.g. the prompt, a reference image, or a first frame.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRole {
    pub role: String,
    pub modality: Modality,
    #[serde(default)]
    pub min: u8,
    #[serde(default = "one")]
    pub max: u8,
    /// The node can fill this role itself (its prompt field), so it counts as present even
    /// when nothing is wired. A wired text input replaces the inline prompt. Text roles only.
    #[serde(default)]
    pub inline: bool,
}

fn one() -> u8 {
    1
}

/// One way to use a model: what it makes and from which inputs.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeSpec {
    pub id: String,
    pub output: Modality,
    #[serde(default)]
    pub inputs: Vec<InputRole>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDescriptor {
    pub id: String,
    /// The provider adapter that runs it (`comfyui`, `fal`, `openai`, …).
    pub provider: String,
    /// The provider's own name for the model, or the workflow template for ComfyUI.
    pub provider_model: String,
    pub label: String,
    pub description: String,
    pub modes: Vec<ModeSpec>,
    /// Resolutions, aspect ratios, durations and other settings, as a parameter schema.
    #[serde(default)]
    pub params: ParamSchema,
    /// Optional semantic rules for precisely this model's schema. Absent preserves the
    /// original schema-only catalog interpretation.
    #[serde(default)]
    pub parameter_contract: Option<nodecraft_types::Contract>,
    pub pricing: Pricing,
    /// Typical seconds from submit to result.
    pub latency_s: u32,
    /// 1 (draft) to 5 (best available).
    pub quality: u8,
}

impl ModelDescriptor {
    pub fn validate(&self) -> Result<(), Error> {
        let id = if is_id(&self.id) { self.id.as_str() } else { "?" };
        if !is_id(&self.id) {
            return Err(invalid(id, "ids are 1-128 lowercase letters, digits, '.', '-', '_' or '/'"));
        }
        if !is_id(&self.provider) {
            return Err(invalid(id, "the provider must be a lowercase id"));
        }
        for (field, text) in [("provider_model", &self.provider_model), ("label", &self.label), ("description", &self.description)] {
            if text.trim().is_empty() || text.len() > MAX_TEXT || text.chars().any(char::is_control) {
                return Err(invalid(id, &format!("`{field}` must be 1-{MAX_TEXT} bytes of printable text")));
            }
        }
        if !(1..=5).contains(&self.quality) {
            return Err(invalid(id, "quality is 1 to 5"));
        }
        if self.modes.is_empty() || self.modes.len() > MAX_MODES {
            return Err(invalid(id, &format!("a model has 1-{MAX_MODES} modes")));
        }
        let mut modes = std::collections::BTreeSet::new();
        for mode in &self.modes {
            if !is_id(&mode.id) || !modes.insert(mode.id.as_str()) {
                return Err(invalid(id, "mode ids must be unique lowercase ids"));
            }
            if mode.inputs.len() > MAX_ROLES {
                return Err(invalid(id, &format!("a mode has at most {MAX_ROLES} inputs")));
            }
            let mut roles = std::collections::BTreeSet::new();
            for role in &mode.inputs {
                if !is_id(&role.role) || !roles.insert(role.role.as_str()) {
                    return Err(invalid(id, "input roles must be unique lowercase ids"));
                }
                if role.max == 0 || role.min > role.max {
                    return Err(invalid(id, &format!("input `{}` needs 0 <= min <= max and max >= 1", role.role)));
                }
                if role.inline && role.modality != Modality::Text {
                    return Err(invalid(id, &format!("input `{}`: only text inputs can be inline", role.role)));
                }
            }
        }
        self.params.validate().map_err(|error| invalid(id, &error.message))?;
        if let Some(contract) = &self.parameter_contract {
            contract.validate()?;
            if contract.owner != nodecraft_types::ParamOwner::Model || contract.namespace != self.id || contract.schema != self.params {
                return Err(invalid(id, "parameter contract must own this exact model id and schema"));
            }
        }
        if let Pricing::PerUnit { unit, count, default_count, .. } = &self.pricing {
            if *default_count == Some(0) {
                return Err(invalid(id, "`default_count` must be at least 1"));
            }
            match count {
                None if default_count.is_none() && !unit.usage_based() => {
                    return Err(invalid(id, "per-unit pricing needs a `count` parameter or an explicit `default_count`"));
                }
                None => {}
                Some(count) => {
                    let spec = self.params.params.iter().find(|spec| spec.name == *count);
                    let Some(spec) = spec else {
                        return Err(invalid(id, &format!("pricing counts parameter `{count}`, which the model doesn't have")));
                    };
                    // Usage units may count from zero; anything else must count at least one, or a
                    // zero would quote a paid job as free.
                    let positive = match &spec.control {
                        ParamControl::Unsigned { min, .. } => unit.usage_based() || min.is_some_and(|min| min.0 >= 1),
                        ParamControl::Integer { min, .. } => min.is_some_and(|min| min.0 >= if unit.usage_based() { 0 } else { 1 }),
                        ParamControl::Slider { min, .. } => min.is_some_and(|min| if unit.usage_based() { min.get() >= 0.0 } else { min.get() > 0.0 }),
                        _ => false,
                    };
                    if !positive {
                        return Err(invalid(id, &format!("pricing parameter `{count}` must be a number with a minimum of at least 1")));
                    }
                    if spec.default.is_none() && default_count.is_none() && !unit.usage_based() {
                        return Err(invalid(id, &format!("pricing parameter `{count}` needs a default (or set `default_count`)")));
                    }
                }
            }
        }
        Ok(())
    }

    /// Executable model settings. Semantic contracts use the same submission operation as
    /// the production generation kind; schema-only inventories retain their legacy rules.
    pub fn effective_params(&self, params: &ParamValues, ready: bool) -> Result<ParamValues, Error> {
        self.validate()?;
        if let Some(contract) = &self.parameter_contract {
            return contract.resolve_selected(params, ready)?.submission();
        }
        let resolved = self.params.resolve_view(params, ready, true)?;
        Ok(resolved.values.into_iter().filter(|(k, _)| resolved.visible.contains(*k)).map(|(k, v)| (k.to_owned(), v.clone())).collect())
    }

    /// The price of one job with these parameters: schema defaults are applied first, so the
    /// quote counts what the run will actually ask for. `None` when the cost depends on usage.
    pub fn quote(&self, params: &ParamValues) -> Result<Option<Money>, Error> {
        // A descriptor built or changed in code must meet the same rules as one loaded from a
        // file, or e.g. a zero `default_count` would quote a paid job as free.
        self.validate()?;
        if let Some(contract) = &self.parameter_contract {
            return self.pricing.quote(&contract.resolve(params, false)?.submission()?, self.pricing.currency().unwrap_or(Currency::USD));
        }
        let resolved = self.params.resolve(params, false)?;
        let visible: ParamValues = resolved.visible_values().map(|(k, v)| (k.clone(), v.clone())).collect();
        self.pricing.quote(&visible, self.pricing.currency().unwrap_or(Currency::USD))
    }

    /// [`Self::quote`] for a node's whole parameter map, borrowing it: parameters this model's
    /// schema doesn't know are skipped, known ones are checked against their controls in place
    /// (so an invalid or oversized value is refused before anything is copied), and defaults
    /// fill the rest. Legacy schemas remain borrowed; explicit parameter contracts create
    /// bounded resolved settings only after validation. A map over the schema's parameter-count limit is
    /// refused outright, as for every other resolution.
    pub fn quote_selected(&self, params: &ParamValues) -> Result<Option<Money>, Error> {
        self.validate()?;
        if let Some(contract) = &self.parameter_contract {
            return self.pricing.quote(&contract.resolve_selected(params, false)?.submission()?, self.pricing.currency().unwrap_or(Currency::USD));
        }
        let resolved = self.params.resolve_view(params, false, true)?;
        let get = |name: &str| if resolved.visible.contains(name) { resolved.values.get(name).copied() } else { None };
        self.pricing.quote_lookup(get, self.pricing.currency().unwrap_or(Currency::USD))
    }

    pub fn mode(&self, id: &str) -> Option<&ModeSpec> {
        self.modes.iter().find(|mode| mode.id == id)
    }
}
