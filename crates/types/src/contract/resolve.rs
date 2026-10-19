//! Resolving stored values against a contract: what each parameter's effective value is,
//! whether it applies, and every problem found, reported together rather than first-only.

use super::{Affects, Contract, DefaultPolicy, HiddenPolicy, condition::Env};
use crate::{Error, ErrorKind, ParamValues, Value, schema::invalid};
use serde::{Deserialize, Serialize};

/// A parameter's effective value and where it came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
pub enum Effective {
    /// No value and no default; nothing is submitted.
    Unset,
    /// Stored on the node (by an edit, a bulk edit or a migration).
    Explicit(Value),
    /// The schema's literal default.
    Default(Value),
    /// The provider chooses; nothing is submitted.
    Auto,
    /// A fresh seed is drawn per run and recorded with the generation.
    RandomSeed,
}

impl Effective {
    /// The value a job submits, if any.
    pub fn value(&self) -> Option<&Value> {
        match self {
            Self::Explicit(value) | Self::Default(value) => Some(value),
            Self::Unset | Self::Auto | Self::RandomSeed => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamState {
    pub name: String,
    pub id: String,
    pub value: Effective,
    pub visible: bool,
    /// Visible and editable. Only enabled parameters are submitted.
    pub enabled: bool,
    /// The stored value should be cleared: it is hidden or disabled under `HiddenPolicy::Reset`.
    pub reset: bool,
    pub affects: Affects,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    /// A stored value has the wrong type or is out of bounds; it is ignored.
    InvalidValue,
    /// A stored key the contract does not know; kept on the node, never submitted.
    UnknownParam,
    /// A required, enabled parameter has no value.
    Required,
    /// A cross-field check failed.
    Check,
}

/// One structured problem. `message` is public-safe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub code: IssueCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    /// One entry per schema parameter, in schema order.
    pub params: Vec<ParamState>,
    pub issues: Vec<Issue>,
}

impl Resolution {
    /// The values a job sends: enabled parameters with an explicit or literal value. Refuses
    /// while any issue is open, so a hidden or invalid value can never reach a provider.
    pub fn submission(&self) -> Result<ParamValues, Error> {
        if let Some(issue) = self.issues.first() {
            return Err(Error::new(ErrorKind::InvalidInput, issue.message.clone()));
        }
        Ok(self.params.iter().filter(|state| state.enabled).filter_map(|state| state.value.value().map(|value| (state.name.clone(), value.clone()))).collect())
    }

    /// Enabled parameters whose effective setting is part of a result's identity. A literal
    /// default and the same value set explicitly are one identity (both report as
    /// [`Effective::Explicit`]), so typing in the default never makes a node stale; the auto
    /// and random-seed markers are kept, so switching to them does.
    pub fn identity(&self) -> impl Iterator<Item = (&str, Effective)> {
        self.params.iter().filter(|state| state.enabled && state.affects.identity && state.value != Effective::Unset).map(|state| {
            let value = match &state.value {
                Effective::Default(value) => Effective::Explicit(value.clone()),
                other => other.clone(),
            };
            (state.name.as_str(), value)
        })
    }

    pub fn get(&self, name: &str) -> Option<&ParamState> {
        self.params.iter().find(|state| state.name == name)
    }
}

impl Contract {
    /// Resolve model settings in a whole node map without copying a filtered input map.
    /// Foreign keys remain caller-owned and are ignored, while all known values and shared
    /// aggregate input limits are still checked before effective values are copied.
    pub fn resolve_selected(&self, supplied: &ParamValues, ready: bool) -> Result<Resolution, Error> {
        let mut resolution = self.resolve(supplied, ready)?;
        resolution.issues.retain(|issue| issue.code != IssueCode::UnknownParam);
        Ok(resolution)
    }

    /// Resolve stored values. Problems with the values become [`Issue`]s; only an invalid
    /// contract or oversized input is an `Err`. With `ready` false (placing an unconfigured
    /// node), missing required values are not reported.
    pub fn resolve(&self, supplied: &ParamValues, ready: bool) -> Result<Resolution, Error> {
        self.validate()?;
        if supplied.len() > crate::schema::MAX_PARAMS {
            return Err(invalid("parameter values exceed count limit"));
        }
        let mut bytes = 0_usize;
        for value in supplied.values() {
            bytes = bytes.saturating_add(crate::schema::value_text_bytes(value));
        }
        for spec in &self.schema.params {
            bytes = bytes.saturating_add(spec.default.as_ref().map_or(0, crate::schema::value_text_bytes));
        }
        if bytes > crate::schema::MAX_TEXT {
            return Err(invalid("resolved parameters exceed text limit"));
        }
        let mut issues = Vec::new();
        for key in supplied.keys() {
            if !self.schema.params.iter().any(|spec| spec.name == *key) {
                let message = if crate::schema::name(key) { format!("unknown parameter: {key}") } else { "unknown parameter".to_owned() };
                issues.push(Issue { code: IssueCode::UnknownParam, param: crate::schema::name(key).then(|| key.clone()), check: None, message });
            }
        }
        let mut env: Env<'_> = Env::new();
        let mut visible_names = std::collections::BTreeSet::new();
        let mut params = Vec::with_capacity(self.schema.params.len());
        for spec in &self.schema.params {
            let rule = self.rule(&spec.name).unwrap_or_else(|| std::borrow::Cow::Owned(super::ParamRule::implied(&spec.name)));
            let explicit = supplied.get(&spec.name).filter(|value| match spec.control.validate_value_checked(value) {
                Ok(()) => true,
                Err(_) => {
                    issues.push(Issue {
                        code: IssueCode::InvalidValue,
                        param: Some(spec.name.clone()),
                        check: None,
                        message: format!("{} has a value of the wrong type or out of range", spec.label),
                    });
                    false
                }
            });
            let value = match (explicit, rule.default_policy(spec), &spec.default) {
                (Some(value), _, _) => Effective::Explicit(value.clone()),
                (None, DefaultPolicy::Literal, Some(value)) => Effective::Default(value.clone()),
                (None, DefaultPolicy::Auto, _) => Effective::Auto,
                (None, DefaultPolicy::RandomSeed, _) => Effective::RandomSeed,
                _ => Effective::Unset,
            };
            let schema_visible = spec.visible_if.as_ref().is_none_or(|condition| {
                visible_names.contains(condition.parameter.as_str()) && env.get(condition.parameter.as_str()).copied() == Some(&condition.equals)
            });
            let visible = schema_visible && rule.visible_when.as_ref().is_none_or(|condition| condition.holds(&env));
            let enabled = visible && rule.enabled_when.as_ref().is_none_or(|condition| condition.holds(&env));
            if visible {
                visible_names.insert(spec.name.as_str());
            }
            if enabled {
                if let Some(value) = explicit.or(spec.default.as_ref().filter(|_| matches!(value, Effective::Default(_)))) {
                    env.insert(spec.name.as_str(), value);
                }
                if ready && spec.required && value == Effective::Unset {
                    issues.push(Issue {
                        code: IssueCode::Required,
                        param: Some(spec.name.clone()),
                        check: None,
                        message: format!("{} needs a value", spec.label),
                    });
                }
            }
            let reset = !enabled && rule.hidden == HiddenPolicy::Reset && supplied.contains_key(&spec.name);
            params.push(ParamState { name: spec.name.clone(), id: self.param_id(&spec.name), value, visible, enabled, reset, affects: rule.affects });
        }
        for check in &self.checks {
            if check.when.as_ref().is_none_or(|condition| condition.holds(&env)) && !check.require.holds(&env) {
                issues.push(Issue { code: IssueCode::Check, param: None, check: Some(check.id.clone()), message: check.message.clone() });
            }
        }
        Ok(Resolution { params, issues })
    }
}
