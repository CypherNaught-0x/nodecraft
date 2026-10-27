//! Model descriptors: what a model makes, from which inputs, with which parameters, at what
//! price. Catalog files are untrusted data and every descriptor is validated before use.
use crate::pricing::{Pricing, TokenRates, Unit};
use nodecraft_types::{
    AudioDuration, AudioUse, Currency, DocumentUse, Error, ErrorKind, MAX_AUDIO_MS, Modality, Money, ParamControl, ParamSchema, ParamValues, VideoEdit,
};
use serde::Deserialize;

pub const MAX_MODES: usize = 32;
pub const MAX_ROLES: usize = 16;
pub const MAX_TEXT: usize = 4096;
/// What the legacy `quality` field means in model comparison and automatic selection.
pub const SELECTION_TIER_HELP: &str =
    "Selection tier is NodeCraft's editorial priority for automatic selection: 1 lowest, 5 highest. Compare descriptions for each model's strengths.";

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
    /// The model settings this mode sends, when it doesn't take them all (#559): an
    /// audio-driven video mode has no duration, for example. Absent: every setting.
    #[serde(default)]
    pub params: Option<Vec<String>>,
    /// How the mode treats its audio input, when it has one.
    #[serde(default)]
    pub audio: Option<AudioUse>,
    /// The source video edit it makes, when it edits one (#179).
    #[serde(default)]
    pub edit: Option<VideoEdit>,
    /// How it reads wired PDFs, when it takes them (#591).
    #[serde(default)]
    pub document: Option<DocumentUse>,
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
    /// What a run is charged once its reply reports the tokens it used (chat models); without
    /// it a run is charged its quote, or what the provider says it cost.
    #[serde(default)]
    pub usage: Option<TokenRates>,
    /// Configured time hint in seconds, not a measured latency or service guarantee.
    pub latency_s: u32,
    /// Editorial selection priority, 1 (lowest) to 5 (highest). Kept under the original
    /// catalog field name; it is not a cross-provider output-quality benchmark.
    pub quality: u8,
    /// The base-model family whose trained styles it uses or trains (#167): a trainer (a mode
    /// that outputs a style) trains for it, a model with a style input takes styles of it. Set
    /// exactly when a mode outputs or takes a style.
    #[serde(default)]
    pub style_family: Option<String>,
    /// Whether model pickers and model selection offer it (default `true`). `false` for a model
    /// only one card kind runs, like the ComfyUI workflow card's (#9): it is still priced and
    /// admitted for that card, never offered on others.
    #[serde(default = "offered")]
    pub cards: bool,
}

fn offered() -> bool {
    true
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
            if let Some(names) = &mode.params {
                let mut seen = std::collections::BTreeSet::new();
                for name in names {
                    if !self.params.params.iter().any(|spec| spec.name == *name) || !seen.insert(name.as_str()) {
                        return Err(invalid(id, &format!("mode `{}` lists a setting the model doesn't have, or one twice", mode.id)));
                    }
                }
            }
            if let Some(audio) = &mode.audio {
                if !mode.inputs.iter().any(|role| role.role == audio.role && role.modality == Modality::Audio) {
                    return Err(invalid(id, &format!("mode `{}` describes audio for `{}`, which isn't one of its audio inputs", mode.id, audio.role)));
                }
                if audio.max_ms.is_some_and(|ms| ms == 0 || ms > MAX_AUDIO_MS) {
                    return Err(invalid(id, &format!("mode `{}`: `max_ms` is 1 to {MAX_AUDIO_MS}", mode.id)));
                }
            }
            if let Some(edit) = &mode.edit {
                if mode.output != Modality::Video
                    || !mode.inputs.iter().any(|role| role.role == edit.role && role.modality == Modality::Video && role.min == 1 && role.max == 1)
                {
                    return Err(invalid(
                        id,
                        &format!("mode `{}` edits `{}`, which isn't its one required video input, or doesn't make a video", mode.id, edit.role),
                    ));
                }
                edit.validate().map_err(|why| invalid(id, &format!("mode `{}`: {why}", mode.id)))?;
            }
            // A mode that takes PDFs says how many pages it reads, so runs are bounded and quoted.
            let documents = mode.inputs.iter().filter(|role| role.modality == Modality::Document).count();
            match &mode.document {
                Some(document) => {
                    if documents != 1 || !mode.inputs.iter().any(|role| role.role == document.role && role.modality == Modality::Document) {
                        return Err(invalid(id, &format!("mode `{}` describes PDFs for `{}`, which isn't its one document input", mode.id, document.role)));
                    }
                    document.validate().map_err(|why| invalid(id, &format!("mode `{}`: {}", mode.id, why.message)))?;
                }
                None if documents > 0 => return Err(invalid(id, &format!("mode `{}` takes PDFs, so it says how it reads them in `document`", mode.id))),
                None => {}
            }
        }
        let styled = self.modes.iter().any(|mode| mode.output == Modality::Style || mode.inputs.iter().any(|role| role.modality == Modality::Style));
        match &self.style_family {
            None if styled => return Err(invalid(id, "a model that trains or takes styles names its `style_family`")),
            Some(family) if !styled => return Err(invalid(id, &format!("`style_family` `{family}` is set, but no mode trains or takes a style"))),
            Some(family) if !nodecraft_types::style_family(family) => {
                return Err(invalid(id, "`style_family` is 1-64 lowercase letters, digits, `.`, `-` or `_`"));
            }
            _ => {}
        }
        self.params.validate().map_err(|error| invalid(id, &error.message))?;
        if let Some(contract) = &self.parameter_contract {
            contract.validate()?;
            if contract.owner != nodecraft_types::ParamOwner::Model || contract.namespace != self.id || contract.schema != self.params {
                return Err(invalid(id, "parameter contract must own this exact model id and schema"));
            }
        }
        if let Some(rates) = &self.usage {
            // A charge settles in the currency it was reserved in.
            let currency = self.pricing.currency();
            if currency.is_none() || rates.prices().any(|price| Some(price.0.currency) != currency) {
                return Err(invalid(id, "`usage` rates must be in the currency of the model's paid `pricing`"));
            }
            // A zero reply rate would settle every run of a paid model at nothing: a typo, not a price.
            if rates.output.0.micros == 0 || rates.long.as_ref().is_some_and(|long| long.output.0.micros == 0) {
                return Err(invalid(id, "`usage` output rates must be above zero for a paid model"));
            }
            if rates.long.as_ref().is_some_and(|long| long.above == 0) {
                return Err(invalid(id, "`usage.long.above` must be at least 1 prompt token"));
            }
        }
        if let Pricing::PerSourceSecond { param, prices } = &self.pricing {
            // Every mode edits the same source the same way, so one length prices any of them.
            let first = self.modes.first().and_then(|mode| mode.edit.as_ref());
            if first.is_none() || self.modes.iter().any(|mode| mode.edit.as_ref().map(|e| (&e.role, e.uses_ms)) != first.map(|e| (&e.role, e.uses_ms))) {
                return Err(invalid(id, "pricing per source second needs every mode to edit the same source"));
            }
            self.validate_choice_prices(param, prices)?;
        }
        if let Pricing::PerUnit { unit, count, default_count, .. } = &self.pricing {
            self.validate_count(*unit, count.as_deref(), *default_count)?;
        }
        if let Pricing::PerUnitByChoice { unit, count, default_count, param, prices, input_fee } = &self.pricing {
            if unit.usage_based() {
                return Err(invalid(id, "pricing by a setting counts units known before the run"));
            }
            self.validate_count(*unit, count.as_deref(), *default_count)?;
            self.validate_choice_prices(param, prices)?;
            if let Some(fee) = input_fee {
                if fee.price.0.micros == 0 || Some(fee.price.0.currency) != self.pricing.currency() {
                    return Err(invalid(id, "input image fees are above zero and in the model's currency"));
                }
                if self.modes.iter().any(|mode| !mode.inputs.iter().any(|input| input.role == fee.role && input.modality == Modality::Image && input.min > 0)) {
                    return Err(invalid(id, "input image fees name a required image role in every mode"));
                }
            }
        }
        Ok(())
    }

    /// A price for each option of choice setting `param`, which has a default and every mode
    /// sends.
    fn validate_choice_prices(&self, param: &str, prices: &std::collections::BTreeMap<String, crate::pricing::Price>) -> Result<(), Error> {
        let id = &self.id;
        let options = match self.params.params.iter().find(|spec| spec.name == param).map(|spec| (&spec.control, &spec.default)) {
            Some((ParamControl::Choice { options }, Some(_))) => options,
            _ => return Err(invalid(id, &format!("pricing reads `{param}`, which must be a choice setting with a default"))),
        };
        if options.len() != prices.len() || options.iter().any(|option| !prices.contains_key(&option.value)) {
            return Err(invalid(id, &format!("pricing lists a price for each `{param}` choice, and only those")));
        }
        let currency = self.pricing.currency();
        if prices.values().any(|price| Some(price.0.currency) != currency || price.0.micros == 0) {
            return Err(invalid(id, "listed prices are above zero and in one currency"));
        }
        if let Some(mode) = self.modes.iter().find(|mode| mode.params.as_ref().is_some_and(|names| !names.iter().any(|name| name == param))) {
            return Err(invalid(id, &format!("mode `{}` doesn't send `{param}`, which pricing reads", mode.id)));
        }
        Ok(())
    }

    /// A per-unit count a quote can trust: a positive number setting every mode sends, or an
    /// explicit default.
    fn validate_count(&self, unit: Unit, count: Option<&str>, default_count: Option<u64>) -> Result<(), Error> {
        let id = &self.id;
        if default_count == Some(0) {
            return Err(invalid(id, "`default_count` must be at least 1"));
        }
        match count {
            None if default_count.is_none() && !unit.usage_based() => {
                return Err(invalid(id, "per-unit pricing needs a `count` parameter or an explicit `default_count`"));
            }
            None => {}
            Some(count) => {
                let spec = self.params.params.iter().find(|spec| spec.name == count);
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
                // A quote counted from a setting a mode never sends would price a run wrongly.
                if let Some(mode) = self.modes.iter().find(|mode| mode.params.as_ref().is_some_and(|names| !names.iter().any(|name| name == count))) {
                    return Err(invalid(id, &format!("mode `{}` doesn't send `{count}`, which pricing counts", mode.id)));
                }
                // A clip as long as its audio isn't as long as a seconds setting says, so a
                // quote counted from one would be wrong; such a model is priced per call or by
                // usage until a track's length can be counted (#608).
                if unit == Unit::Second
                    && let Some(mode) = self.modes.iter().find(|mode| mode.audio.as_ref().is_some_and(|audio| audio.duration == AudioDuration::FollowsAudio))
                {
                    return Err(invalid(id, &format!("mode `{}` follows its audio's length, so pricing can't count seconds from `{count}`", mode.id)));
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

    /// The seeds a run draws for a job with these parameters, with each one's upper bound (#162).
    /// Only a semantic contract can default a seed to `random_seed`; a schema-only model draws
    /// none.
    pub fn random_seeds(&self, params: &ParamValues, ready: bool) -> Result<Vec<(String, Option<nodecraft_types::Unsigned>)>, Error> {
        self.validate()?;
        let Some(contract) = &self.parameter_contract else { return Ok(Vec::new()) };
        Ok(contract.random_seeds(&contract.resolve_selected(params, ready)?))
    }

    /// The seed parameters these parameters pin to a fixed value (#808): every job sends the same
    /// seed, so several variations of a deterministic model come back alike. A semantic contract
    /// says so of its own seeds; a schema-only model pins each seed it would send.
    pub fn pinned_seeds(&self, params: &ParamValues, ready: bool) -> Result<Vec<String>, Error> {
        self.validate()?;
        if let Some(contract) = &self.parameter_contract {
            return Ok(contract.pinned_seeds(&contract.resolve_selected(params, ready)?));
        }
        // A node's map holds more than its model's settings (its model, its prompt).
        let own: ParamValues =
            params.iter().filter(|(name, _)| self.params.params.iter().any(|spec| &spec.name == *name)).map(|(k, v)| (k.clone(), v.clone())).collect();
        let sent = self.effective_params(&own, ready)?;
        Ok(self
            .params
            .params
            .iter()
            .filter(|spec| matches!(spec.control, nodecraft_types::ParamControl::Seed { .. }) && sent.contains_key(&spec.name))
            .map(|spec| spec.name.clone())
            .collect())
    }

    /// The price of one job with these parameters: schema defaults are applied first, so the
    /// quote counts what the run will actually ask for. `None` when the cost depends on usage.
    pub fn quote(&self, params: &ParamValues) -> Result<Option<Money>, Error> {
        // A descriptor built or changed in code must meet the same rules as one loaded from a
        // file, or e.g. a zero `default_count` would quote a paid job as free.
        self.validate()?;
        if let Some(contract) = &self.parameter_contract {
            let values = contract.resolve(params, false)?.submission()?;
            return self.pricing.quote_source_inputs(|name| values.get(name), None, self.fixed_fee_count(), self.pricing.currency().unwrap_or(Currency::USD));
        }
        let resolved = self.params.resolve(params, false)?;
        let visible: ParamValues = resolved.visible_values().map(|(k, v)| (k.clone(), v.clone())).collect();
        self.pricing.quote_source_inputs(|name| visible.get(name), None, self.fixed_fee_count(), self.pricing.currency().unwrap_or(Currency::USD))
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
            let values = contract.resolve_selected(params, false)?.submission()?;
            return self.pricing.quote_source_inputs(|name| values.get(name), None, self.fixed_fee_count(), self.pricing.currency().unwrap_or(Currency::USD));
        }
        let resolved = self.params.resolve_view(params, false, true)?;
        let get = |name: &str| if resolved.visible.contains(name) { resolved.values.get(name).copied() } else { None };
        self.pricing.quote_source_inputs(get, None, self.fixed_fee_count(), self.pricing.currency().unwrap_or(Currency::USD))
    }

    /// [`Self::quote_selected`] for a job whose source video is `source_ms` long (#179): a model
    /// priced per source second counts the part of it the provider uses.
    pub fn quote_source(&self, params: &ParamValues, source_ms: Option<u64>) -> Result<Option<Money>, Error> {
        self.quote_source_count(params, source_ms, self.fixed_fee_count())
    }

    fn quote_source_count(&self, params: &ParamValues, source_ms: Option<u64>, input_count: Option<u64>) -> Result<Option<Money>, Error> {
        if !matches!(&self.pricing, Pricing::PerSourceSecond { .. } | Pricing::PerUnitByChoice { input_fee: Some(_), .. }) {
            return self.quote_selected(params);
        }
        self.validate()?;
        let used = source_ms.zip(self.edit()).map(|(ms, edit)| edit.used_ms(ms));
        let currency = self.pricing.currency().unwrap_or(Currency::USD);
        if let Some(contract) = &self.parameter_contract {
            let values = contract.resolve_selected(params, false)?.submission()?;
            return self.pricing.quote_source_inputs(|name| values.get(name), used, input_count, currency);
        }
        let resolved = self.params.resolve_view(params, false, true)?;
        let get = |name: &str| if resolved.visible.contains(name) { resolved.values.get(name).copied() } else { None };
        self.pricing.quote_source_inputs(get, used, input_count, currency)
    }

    /// The additional paid image role, when this model charges for its references.
    pub fn input_fee_role(&self) -> Option<&str> {
        match &self.pricing {
            Pricing::PerUnitByChoice { input_fee: Some(fee), .. } => Some(&fee.role),
            _ => None,
        }
    }

    fn fixed_fee_count(&self) -> Option<u64> {
        let role = self.input_fee_role()?;
        let first = self.modes.first()?.inputs.iter().find(|input| input.role == role)?;
        (first.min == first.max
            && self.modes.iter().all(|mode| mode.inputs.iter().any(|input| input.role == role && input.min == first.min && input.max == first.max)))
        .then_some(u64::from(first.min))
    }

    /// [`Self::quote_source`] plus `input_tokens` prompt tokens a request is known to send (a
    /// wired PDF's pages, #591) at the model's input rate; the rest of the prompt is billed on top,
    /// as before. A model priced only by usage stays unpriced.
    pub fn quote_inputs(&self, params: &ParamValues, source_ms: Option<u64>, input_tokens: u64) -> Result<Option<Money>, Error> {
        self.quote_inputs_count(params, source_ms, input_tokens, None)
    }

    /// [`Self::quote_inputs`] including a known number of paid reference images. A fixed role
    /// count (one first frame, for example) is known even before its upstream job finishes.
    pub fn quote_inputs_count(
        &self,
        params: &ParamValues,
        source_ms: Option<u64>,
        input_tokens: u64,
        input_count: Option<u64>,
    ) -> Result<Option<Money>, Error> {
        if let Some(count) = input_count
            && let Some(role) = self.input_fee_role()
            && self
                .modes
                .iter()
                .any(|mode| !mode.inputs.iter().any(|input| input.role == role && (u64::from(input.min)..=u64::from(input.max)).contains(&count)))
        {
            return Err(invalid(&self.id, "paid input count is outside its role bounds"));
        }
        let quote = self.quote_source_count(params, source_ms, input_count.or_else(|| self.fixed_fee_count()))?;
        let (Some(quote), Some(usage)) = (quote, &self.usage) else { return Ok(quote) };
        if input_tokens == 0 {
            return Ok(Some(quote));
        }
        Ok(Some(quote.checked_add(usage.cost(input_tokens, 0)?)?))
    }

    /// The source video edit its modes make (#179); every mode of a model priced per source
    /// second makes the same one.
    pub fn edit(&self) -> Option<&VideoEdit> {
        self.modes.iter().find_map(|mode| mode.edit.as_ref())
    }

    pub fn mode(&self, id: &str) -> Option<&ModeSpec> {
        self.modes.iter().find(|mode| mode.id == id)
    }
}
