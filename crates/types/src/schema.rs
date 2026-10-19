//! Shared pure schema data for node kinds and model catalogs. Graph storage remains generic;
//! the owning node/model validates values against these controls before use.

use crate::{Error, ErrorKind, Integer, Modality, Number, Unsigned, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type ParamValues = BTreeMap<String, Value>;
pub(crate) const MAX_PARAMS: usize = 256;
pub(crate) const MAX_TEXT: usize = 1024 * 1024;
const MAX_CHOICES: usize = 16_384;
const MAX_OPTION_TEXT: usize = 4096;
#[cfg(test)]
thread_local! {
    static SCHEMA_VALIDATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CHOICE_INDEX_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CHOICE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn unit_step() -> Unsigned {
    Unsigned(1)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParamControl {
    Text {
        multiline: bool,
        max_bytes: usize,
    },
    Choice {
        options: Vec<Choice>,
    },
    Toggle,
    /// Step is a UI increment hint, not a floating-point quantization requirement.
    Slider {
        min: Option<Number>,
        max: Option<Number>,
        step: Number,
    },
    Integer {
        min: Option<Integer>,
        max: Option<Integer>,
        #[serde(default = "unit_step")]
        step: Unsigned,
    },
    Unsigned {
        min: Option<Unsigned>,
        max: Option<Unsigned>,
        #[serde(default = "unit_step")]
        step: Unsigned,
    },
    Color,
    /// A model identifier; catalog membership is checked by the planning layer.
    Model,
    Seed {
        max: Option<Unsigned>,
    },
    Asset {
        modality: Modality,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibleIf {
    pub parameter: String,
    pub equals: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    pub name: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub control: ParamControl,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// References an earlier parameter. A hidden controller also hides its dependants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_if: Option<VisibleIf>,
}

/// Public schema data must be validated before cloning defaults or constructing UI controls.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamSchema {
    pub params: Vec<ParamSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedParams {
    /// Includes hidden overrides for editor persistence. Provider adapters use visible_values().
    pub values: ParamValues,
    pub visible: BTreeSet<String>,
}

impl ResolvedParams {
    /// Only visible parameters may be passed to provider adapters or execution jobs.
    pub fn visible_values(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.values.iter().filter(|(name, _)| self.visible.contains(*name))
    }
    pub fn into_visible_values(self) -> ParamValues {
        self.values.into_iter().filter(|(name, _)| self.visible.contains(name)).collect()
    }
}

pub(crate) fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
pub(crate) fn name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl ParamControl {
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Text { max_bytes, .. } if *max_bytes > MAX_TEXT => Err(invalid("text control exceeds maximum size")),
            Self::Choice { options } => {
                if options.is_empty() || options.len() > MAX_CHOICES {
                    return Err(invalid("choice control exceeds option count limit"));
                }
                let mut seen = BTreeSet::new();
                for option in options {
                    #[cfg(test)]
                    CHOICE_VISITS.with(|count| count.set(count.get() + 1));
                    if option.value.is_empty()
                        || option.value.len() > MAX_OPTION_TEXT
                        || option.label.trim().is_empty()
                        || option.label.len() > MAX_OPTION_TEXT
                        || !seen.insert(&option.value)
                    {
                        return Err(invalid("choice options must have bounded unique values and nonempty labels"));
                    }
                }
                Ok(())
            }
            Self::Slider { min, max, step } if min.zip(*max).is_some_and(|(a, b)| a.get() > b.get()) || step.get() <= 0.0 => {
                Err(invalid("slider needs ordered bounds and a positive step"))
            }
            Self::Integer { min, max, step } if min.zip(*max).is_some_and(|(a, b)| a > b) || step.0 == 0 => {
                Err(invalid("integer control needs ordered bounds and positive step"))
            }
            Self::Unsigned { min, max, step } if min.zip(*max).is_some_and(|(a, b)| a > b) || step.0 == 0 => {
                Err(invalid("unsigned control needs ordered bounds and positive step"))
            }
            _ => Ok(()),
        }
    }

    /// Does not recurse into unknown/list values, so hostile programmatic defaults are rejected
    /// before Clone or Serialize. Schema controls currently accept scalar values only.
    pub fn validate_value(&self, value: &Value) -> Result<(), Error> {
        self.validate()?;
        self.validate_value_checked(value)
    }

    // The schema has already validated the control; do not scan large option lists again.
    pub(crate) fn validate_value_checked(&self, value: &Value) -> Result<(), Error> {
        let valid = match (self, value) {
            (Self::Text { max_bytes, .. }, Value::Text(text)) => text.len() <= *max_bytes,
            (Self::Choice { options }, Value::Text(text)) => options.iter().any(|option| {
                #[cfg(test)]
                CHOICE_VISITS.with(|count| count.set(count.get() + 1));
                option.value == *text
            }),
            (Self::Toggle, Value::Bool(_)) => true,
            (Self::Slider { min, max, .. }, Value::Number(number)) => {
                min.is_none_or(|min| number.get() >= min.get()) && max.is_none_or(|max| number.get() <= max.get())
            }
            (Self::Integer { min, max, step }, Value::Integer(number)) => {
                min.is_none_or(|min| *number >= min)
                    && max.is_none_or(|max| *number <= max)
                    && step.0 != 0
                    && (i128::from(number.0) - i128::from(min.unwrap_or(Integer(0)).0)) % i128::from(step.0) == 0
            }
            (Self::Unsigned { min, max, step }, Value::Unsigned(number)) => {
                min.is_none_or(|min| *number >= min)
                    && max.is_none_or(|max| *number <= max)
                    && step.0 != 0
                    && number.0.checked_sub(min.unwrap_or(Unsigned(0)).0).is_some_and(|delta| delta % step.0 == 0)
            }
            (Self::Color, Value::Text(text)) => matches!(text.len(), 7 | 9) && text.starts_with('#') && text.bytes().skip(1).all(|b| b.is_ascii_hexdigit()),
            (Self::Model, Value::Text(text)) => {
                !text.is_empty() && text.len() <= 512 && !text.chars().any(char::is_whitespace) && !text.chars().any(char::is_control)
            }
            (Self::Seed { max }, Value::Unsigned(number)) => max.is_none_or(|max| *number <= max),
            (Self::Asset { modality }, Value::Media(asset)) => match modality {
                Modality::Text => asset.mime.essence().starts_with("text/"),
                Modality::Image => asset.mime.essence().starts_with("image/"),
                Modality::Video => asset.mime.essence().starts_with("video/"),
                Modality::Audio => asset.mime.essence().starts_with("audio/"),
                Modality::Mesh => asset.mime.essence().starts_with("model/"),
                Modality::Document => asset.mime.essence() == "application/pdf",
            },
            _ => false,
        };
        if valid { Ok(()) } else { Err(invalid("parameter value does not match its control type or bounds")) }
    }
}

impl ParamSchema {
    pub fn validate(&self) -> Result<(), Error> {
        Self::validate_specs(self.params.iter())
    }

    /// Validate a disjoint schema layout without cloning any controls/defaults. Pointer
    /// scratch is bounded before allocation; ownership groups share the original limits.
    pub fn validate_parts(parts: &[&Self]) -> Result<(), Error> {
        if parts.len() > MAX_PARAMS {
            return Err(invalid("schema exceeds owner limit"));
        }
        let count = parts.iter().try_fold(0usize, |n, part| n.checked_add(part.params.len()).ok_or_else(|| invalid("schema exceeds parameter limit")))?;
        if count > MAX_PARAMS {
            return Err(invalid("schema exceeds parameter limit"));
        }
        let specs: Vec<_> = parts.iter().flat_map(|part| part.params.iter()).collect();
        Self::validate_specs(specs.into_iter())
    }

    fn validate_specs<'a>(specs: impl ExactSizeIterator<Item = &'a ParamSpec>) -> Result<(), Error> {
        #[cfg(test)]
        SCHEMA_VALIDATIONS.with(|count| count.set(count.get() + 1));
        if specs.len() > MAX_PARAMS {
            return Err(invalid("schema exceeds parameter limit"));
        }
        let mut seen: BTreeMap<&str, &ParamSpec> = BTreeMap::new();
        let mut bytes = 0_usize;
        for spec in specs {
            if !name(&spec.name)
                || spec.label.trim().is_empty()
                || spec.label.len() > 256
                || spec.description.as_ref().is_some_and(|text| text.len() > 4096)
                || seen.contains_key(spec.name.as_str())
            {
                return Err(invalid("parameters need bounded unique names and nonempty labels"));
            }
            spec.control.validate()?;
            bytes = bytes.saturating_add(spec.name.len()).saturating_add(spec.label.len()).saturating_add(spec.description.as_ref().map_or(0, String::len));
            if let ParamControl::Choice { options } = &spec.control {
                for option in options {
                    bytes = bytes.saturating_add(option.value.len()).saturating_add(option.label.len());
                }
            }
            if let Some(value) = &spec.default {
                spec.control.validate_value_checked(value)?;
                bytes = bytes.saturating_add(value_text_bytes(value));
            }
            if let Some(condition) = &spec.visible_if {
                let controller = seen.get(condition.parameter.as_str()).ok_or_else(|| invalid("visibility must reference an earlier parameter"))?;
                controller.control.validate_value_checked(&condition.equals)?;
                bytes = bytes.saturating_add(condition.parameter.len()).saturating_add(value_text_bytes(&condition.equals));
            }
            if bytes > MAX_TEXT {
                return Err(invalid("schema text exceeds aggregate limit"));
            }
            seen.insert(spec.name.as_str(), spec);
        }
        Ok(())
    }

    /// Missing optional values remain absent. Required, visible values are enforced only when
    /// `ready` is true, so an unconfigured source can still be placed on the canvas.
    pub fn resolve(&self, supplied: &ParamValues, ready: bool) -> Result<ResolvedParams, Error> {
        self.validate()?;
        let borrowed = self.resolve_checked(supplied, ready, None, false)?;
        Ok(ResolvedParams { values: borrowed.values.into_iter().map(|(name, value)| (name.to_owned(), value.clone())).collect(), visible: borrowed.visible })
    }

    /// Borrow effective values without cloning defaults. With `selected`, unrelated node
    /// parameters are omitted; job maps must pass false to reject every unknown key.
    /// Callers bounding allocation must admit schema validation/resolution scratch first.
    pub fn resolve_view<'a>(&'a self, supplied: &'a ParamValues, ready: bool, selected: bool) -> Result<BorrowedResolvedParams<'a>, Error> {
        self.validate()?;
        self.resolve_checked(supplied, ready, None, selected)
    }

    /// Validate immutable schema data once before checking many instances. The proof borrows
    /// the schema, so callers cannot change controls/defaults while reusing it.
    pub fn validated(&self) -> Result<ValidatedParamSchema<'_>, Error> {
        self.validate()?;
        let choices = self
            .params
            .iter()
            .filter_map(|spec| match &spec.control {
                ParamControl::Choice { options } => {
                    #[cfg(test)]
                    CHOICE_INDEX_BUILDS.with(|count| count.set(count.get() + 1));
                    Some((
                        spec.name.as_str(),
                        options
                            .iter()
                            .map(|option| {
                                #[cfg(test)]
                                CHOICE_VISITS.with(|count| count.set(count.get() + 1));
                                option.value.as_str()
                            })
                            .collect(),
                    ))
                }
                _ => None,
            })
            .collect();
        Ok(ValidatedParamSchema { schema: self, choices })
    }
}

/// An immutable schema whose controls, defaults and visibility conditions were checked.
pub struct ValidatedParamSchema<'a> {
    schema: &'a ParamSchema,
    choices: BTreeMap<&'a str, BTreeSet<&'a str>>,
}
/// Effective values borrow supplied scalars or schema defaults; no Value is cloned.
pub struct BorrowedResolvedParams<'a> {
    pub values: BTreeMap<&'a str, &'a Value>,
    pub visible: BTreeSet<String>,
}
impl ValidatedParamSchema<'_> {
    /// Uses the same value checks and effective-byte budget as `ParamSchema::resolve`, without
    /// rescanning choice controls or copying large defaults for each instance.
    pub fn resolve_borrowed<'a>(&'a self, supplied: &'a ParamValues, ready: bool) -> Result<BorrowedResolvedParams<'a>, Error> {
        self.schema.resolve_checked(supplied, ready, Some(&self.choices), false)
    }
}
impl ParamSchema {
    // Both entry points use identical admission and visibility semantics. Only reusable proofs
    // pay for Choice indexes; a one-shot caller checks membership directly in its control.
    fn resolve_checked<'a>(
        &'a self,
        supplied: &'a ParamValues,
        ready: bool,
        choices: Option<&BTreeMap<&str, BTreeSet<&str>>>,
        selected: bool,
    ) -> Result<BorrowedResolvedParams<'a>, Error> {
        let schema = self;
        if supplied.len() > MAX_PARAMS {
            return Err(invalid("parameter values exceed count limit"));
        }
        let mut bytes = 0_usize;
        for (key, value) in supplied {
            if selected && !schema.params.iter().any(|spec| spec.name == *key) {
                continue;
            }
            let spec = schema.params.iter().find(|spec| spec.name == *key).ok_or_else(|| {
                if name(key) {
                    Error::new(ErrorKind::InvalidInput, format!("unknown parameter: {key}; migrate this node to the current schema"))
                } else {
                    invalid("unknown parameter; migrate this node to the current schema")
                }
            })?;
            if let Some(options) = choices.and_then(|choices| choices.get(key.as_str())) {
                if !matches!(value, Value::Text(text) if options.contains(text.as_str())) {
                    return Err(invalid("parameter value does not match its control type or bounds"));
                }
            } else {
                spec.control.validate_value_checked(value)?;
            }
            bytes = bytes.saturating_add(value_text_bytes(value));
        }
        for spec in &schema.params {
            if !supplied.contains_key(&spec.name) {
                bytes = bytes.saturating_add(spec.default.as_ref().map_or(0, value_text_bytes));
            }
        }
        if bytes > MAX_TEXT {
            return Err(invalid("resolved parameters exceed text limit"));
        }
        let mut values: BTreeMap<&str, &Value> = supplied
            .iter()
            .filter(|(name, _)| !selected || schema.params.iter().any(|spec| spec.name == **name))
            .map(|(name, value)| (name.as_str(), value))
            .collect();
        let mut visible = BTreeSet::new();
        for spec in &schema.params {
            if !values.contains_key(spec.name.as_str())
                && let Some(value) = &spec.default
            {
                values.insert(&spec.name, value);
            }
            let show = spec
                .visible_if
                .as_ref()
                .is_none_or(|condition| visible.contains(&condition.parameter) && values.get(condition.parameter.as_str()).copied() == Some(&condition.equals));
            if show {
                visible.insert(spec.name.clone());
                if ready && spec.required && !values.contains_key(spec.name.as_str()) {
                    return Err(invalid("a required visible parameter has no value"));
                }
            }
        }
        Ok(BorrowedResolvedParams { values, visible })
    }
}

pub(crate) fn value_text_bytes(value: &Value) -> usize {
    match value {
        Value::Text(text) => text.len(),
        Value::Media(asset) => asset.mime.as_str().len(),
        _ => 0,
    }
}

#[cfg(test)]
mod borrowing_tests {
    use super::*;
    fn spec(name: &str, control: ParamControl, default: Value) -> ParamSpec {
        ParamSpec { name: name.into(), label: name.into(), description: None, control, required: false, default: Some(default), visible_if: None }
    }
    #[test]
    fn one_shot_resolution_matches_reusable_proof_without_building_indexes() {
        let mut schema = ParamSchema {
            params: vec![
                spec(
                    "choice",
                    ParamControl::Choice { options: (0..1024).map(|n| Choice { value: n.to_string(), label: n.to_string() }).collect() },
                    Value::Text("0".into()),
                ),
                spec("toggle", ParamControl::Toggle, Value::Bool(false)),
                spec("detail", ParamControl::Text { multiline: true, max_bytes: MAX_TEXT }, Value::Text("default".into())),
            ],
        };
        let detail = schema.params.last_mut().unwrap();
        detail.required = true;
        detail.visible_if = Some(VisibleIf { parameter: "toggle".into(), equals: Value::Bool(true) });
        let cases = [
            ParamValues::new(),
            ParamValues::from([("choice".into(), Value::Text("1023".into())), ("toggle".into(), Value::Bool(true))]),
            ParamValues::from([("choice".into(), Value::Text("missing".into()))]),
            ParamValues::from([("detail".into(), Value::Bool(false))]),
            ParamValues::from([("unknown".into(), Value::Bool(true))]),
            ParamValues::from([("detail".into(), Value::Text("x".repeat(MAX_TEXT)))]),
        ];
        CHOICE_INDEX_BUILDS.with(|count| count.set(0));
        let ordinary: Vec<_> = cases.iter().flat_map(|values| [false, true].map(|ready| schema.resolve(values, ready))).collect();
        assert_eq!(CHOICE_INDEX_BUILDS.with(|count| count.get()), 0, "one-shot resolution must not construct reusable membership indexes");
        let proof = schema.validated().unwrap();
        assert_eq!(CHOICE_INDEX_BUILDS.with(|count| count.get()), 1);
        for (actual, (values, ready)) in ordinary.iter().zip(cases.iter().flat_map(|values| [false, true].map(move |ready| (values, ready)))) {
            match (actual, proof.resolve_borrowed(values, ready)) {
                (Ok(owned), Ok(borrowed)) => {
                    assert_eq!(owned.visible, borrowed.visible);
                    assert_eq!(owned.values, borrowed.values.into_iter().map(|(name, value)| (name.to_owned(), value.clone())).collect());
                }
                (Err(a), Err(b)) => {
                    assert_eq!(a.kind, b.kind);
                    assert_eq!(a.message, b.message);
                }
                _ => panic!("owned and borrowed admission differ"),
            }
        }
        assert_eq!(CHOICE_INDEX_BUILDS.with(|count| count.get()), 1, "reusing the proof must not rebuild indexes");
        schema.params.last_mut().unwrap().default = None;
        assert!(schema.resolve(&ParamValues::from([("toggle".into(), Value::Bool(true))]), true).is_err());
    }
    #[test]
    fn shared_large_defaults_and_choice_controls_are_scanned_once_and_never_copied() {
        let schema = ParamSchema {
            params: vec![
                spec("large", ParamControl::Text { multiline: true, max_bytes: MAX_TEXT }, Value::Text("x".repeat(600_000))),
                spec(
                    "choice",
                    ParamControl::Choice { options: (0..16_384).map(|n| Choice { value: n.to_string(), label: n.to_string() }).collect() },
                    Value::Text("0".into()),
                ),
                spec("toggle", ParamControl::Toggle, Value::Bool(false)),
            ],
        };
        SCHEMA_VALIDATIONS.with(|count| count.set(0));
        CHOICE_VISITS.with(|count| count.set(0));
        let validated = schema.validated().unwrap();
        let baseline = CHOICE_VISITS.with(|count| count.get());
        assert!(baseline >= 16_384, "instrument actual nested option work");
        for n in 0..1000 {
            let supplied = ParamValues::from([("toggle".into(), Value::Bool(n % 2 == 0)), ("choice".into(), Value::Text(n.to_string()))]);
            let resolved = validated.resolve_borrowed(&supplied, false).unwrap();
            let default = schema.params.first().unwrap().default.as_ref().unwrap();
            assert!(std::ptr::eq(*resolved.values.get("large").unwrap(), default), "effective default is the original Value allocation");
            assert!(std::ptr::eq(*resolved.values.get("toggle").unwrap(), supplied.get("toggle").unwrap()));
        }
        assert_eq!(SCHEMA_VALIDATIONS.with(|count| count.get()), 1);
        assert_eq!(CHOICE_VISITS.with(|count| count.get()), baseline, "supplied Choice uses indexed membership, not option rescans");
    }
}
