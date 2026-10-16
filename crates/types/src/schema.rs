//! Shared pure schema data for node kinds and model catalogs. Graph storage remains generic;
//! the owning node/model validates values against these controls before use.

use crate::{Error, ErrorKind, Integer, Modality, Number, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type ParamValues = BTreeMap<String, Value>;
const MAX_PARAMS: usize = 256;
const MAX_TEXT: usize = 1024 * 1024;

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
        min: Number,
        max: Number,
        step: Number,
    },
    Integer {
        min: Integer,
        max: Integer,
    },
    Color,
    /// A model identifier; catalog membership is checked by the planning layer.
    Model,
    Seed,
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
    pub values: ParamValues,
    pub visible: BTreeSet<String>,
}

fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl ParamControl {
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Text { max_bytes, .. } if *max_bytes > MAX_TEXT => Err(invalid("text control exceeds maximum size")),
            Self::Choice { options } => {
                if options.is_empty() || options.len() > MAX_PARAMS {
                    return Err(invalid("choice control must contain 1 to 256 options"));
                }
                let mut seen = BTreeSet::new();
                for option in options {
                    if option.value.is_empty() || option.value.len() > 256 || option.label.is_empty() || option.label.len() > 256 || !seen.insert(&option.value)
                    {
                        return Err(invalid("choice options must have bounded unique values and nonempty labels"));
                    }
                }
                Ok(())
            }
            Self::Slider { min, max, step } if min.get() > max.get() || step.get() <= 0.0 => Err(invalid("slider needs ordered bounds and a positive step")),
            Self::Integer { min, max } if min > max => Err(invalid("integer control needs ordered bounds")),
            _ => Ok(()),
        }
    }

    /// Does not recurse into unknown/list values, so hostile programmatic defaults are rejected
    /// before Clone or Serialize. Schema controls currently accept scalar values only.
    pub fn validate_value(&self, value: &Value) -> Result<(), Error> {
        self.validate()?;
        let valid = match (self, value) {
            (Self::Text { max_bytes, .. }, Value::Text(text)) => text.len() <= *max_bytes,
            (Self::Choice { options }, Value::Text(text)) => options.iter().any(|option| option.value == *text),
            (Self::Toggle, Value::Bool(_)) => true,
            (Self::Slider { min, max, .. }, Value::Number(number)) => number.get() >= min.get() && number.get() <= max.get(),
            (Self::Integer { min, max }, Value::Integer(number)) => number >= min && number <= max,
            (Self::Color, Value::Text(text)) => matches!(text.len(), 7 | 9) && text.starts_with('#') && text.bytes().skip(1).all(|b| b.is_ascii_hexdigit()),
            (Self::Model, Value::Text(text)) => {
                !text.is_empty() && text.len() <= 512 && !text.chars().any(char::is_whitespace) && !text.chars().any(char::is_control)
            }
            (Self::Seed, Value::Unsigned(_)) => true,
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
        if self.params.len() > MAX_PARAMS {
            return Err(invalid("schema exceeds parameter limit"));
        }
        let mut seen: BTreeMap<&str, &ParamSpec> = BTreeMap::new();
        let mut bytes = 0_usize;
        for spec in &self.params {
            if !name(&spec.name) || spec.label.is_empty() || spec.label.len() > 256 || seen.contains_key(spec.name.as_str()) {
                return Err(invalid("parameters need bounded unique names and nonempty labels"));
            }
            spec.control.validate()?;
            if let Some(value) = &spec.default {
                spec.control.validate_value(value)?;
                bytes = bytes.saturating_add(value_text_bytes(value));
            }
            if let Some(condition) = &spec.visible_if {
                let controller = seen.get(condition.parameter.as_str()).ok_or_else(|| invalid("visibility must reference an earlier parameter"))?;
                controller.control.validate_value(&condition.equals)?;
                bytes = bytes.saturating_add(value_text_bytes(&condition.equals));
            }
            if bytes > MAX_TEXT {
                return Err(invalid("schema defaults and conditions exceed text limit"));
            }
            seen.insert(spec.name.as_str(), spec);
        }
        Ok(())
    }

    /// Missing optional values remain absent. Required, visible values are enforced only when
    /// `ready` is true, so an unconfigured source can still be placed on the canvas.
    pub fn resolve(&self, supplied: &ParamValues, ready: bool) -> Result<ResolvedParams, Error> {
        self.validate()?;
        if supplied.len() > MAX_PARAMS {
            return Err(invalid("parameter values exceed count limit"));
        }
        let mut bytes = 0_usize;
        for (key, value) in supplied {
            let spec = self.params.iter().find(|spec| spec.name == *key).ok_or_else(|| invalid("unknown parameter"))?;
            spec.control.validate_value(value)?;
            bytes = bytes.saturating_add(value_text_bytes(value));
        }
        // Include defaults in the effective total before cloning anything.
        for spec in &self.params {
            if !supplied.contains_key(&spec.name) {
                bytes = bytes.saturating_add(spec.default.as_ref().map_or(0, value_text_bytes));
            }
        }
        if bytes > MAX_TEXT {
            return Err(invalid("resolved parameters exceed text limit"));
        }
        let mut values = supplied.clone();
        let mut visible = BTreeSet::new();
        for spec in &self.params {
            if !values.contains_key(&spec.name)
                && let Some(value) = &spec.default
            {
                values.insert(spec.name.clone(), value.clone());
            }
            let show = spec
                .visible_if
                .as_ref()
                .is_none_or(|condition| visible.contains(&condition.parameter) && values.get(&condition.parameter) == Some(&condition.equals));
            if show {
                visible.insert(spec.name.clone());
                if ready && spec.required && !values.contains_key(&spec.name) {
                    return Err(invalid("a required visible parameter has no value"));
                }
            }
        }
        Ok(ResolvedParams { values, visible })
    }
}

fn value_text_bytes(value: &Value) -> usize {
    match value {
        Value::Text(text) => text.len(),
        _ => 0,
    }
}
