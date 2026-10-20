//! Resolving stored values against a contract: what each parameter's effective value is,
//! whether it applies, and every problem found, reported together rather than first-only.

use super::{Affects, Contract, DefaultPolicy, HiddenPolicy, NoOptions, OptionKey, OptionLookup, condition::Env};
use crate::{Error, ErrorKind, ParamControl, ParamValues, ResolvedParams, Unsigned, Value, schema::invalid};
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
    /// The run-time list this parameter's value comes from, when its rule names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ParamOptions>,
}

/// Which fetched list an enabled parameter draws from, and whether the host has it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamOptions {
    pub source: String,
    /// The scope parameter's value. Absent for an unscoped source, or while the scope has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The host has fetched this list. While it has not, any value of the right type is kept
    /// and the provider checks it when the job is submitted.
    pub known: bool,
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
    /// The value is not on the list the host fetched for it (the checkpoint was removed from
    /// the server). Dropped when not `ready`, so other fields stay editable; run planning does
    /// not consult the lists yet.
    NotAnOption,
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

    /// The run path's view (#162). Every effective value is kept, so editors still see hidden
    /// overrides, and exactly the enabled ones are marked visible, so
    /// [`ResolvedParams::into_visible_values`] is [`Resolution::submission`]. A value problem
    /// always refuses; a missing value or failed check refuses only when `ready`, so a node can
    /// be edited through a state that a run would refuse. A run that would draw a random seed
    /// is unsupported here: only a catalog-admitted generation draws and records one.
    /// Parameters declared `identity: false` are sent but marked
    /// [`ResolvedParams::unhashed`], so a result's identity leaves them out.
    pub fn into_resolved(self, ready: bool) -> Result<ResolvedParams, Error> {
        self.into_resolved_where(ready, false, |_| true)
    }

    /// [`Resolution::into_resolved`] for a kind that submits through [`Resolution::submission`]
    /// itself and uses the visible set only as its result's identity (generation): parameters
    /// declared `identity: false` are left out of it, so editing them keeps results fresh. A
    /// random seed is allowed: the run draws it after admission ([`Contract::random_seeds`]).
    pub fn into_identity(self, ready: bool) -> Result<ResolvedParams, Error> {
        self.into_resolved_where(ready, true, |state| state.affects.identity)
    }

    fn into_resolved_where(mut self, ready: bool, seeds: bool, keep: impl Fn(&ParamState) -> bool) -> Result<ResolvedParams, Error> {
        if !ready {
            self.issues.retain(|issue| !matches!(issue.code, IssueCode::Required | IssueCode::Check | IssueCode::NotAnOption));
        }
        if let Some(issue) = self.issues.first() {
            return Err(Error::new(ErrorKind::InvalidInput, issue.message.clone()));
        }
        if ready
            && !seeds
            && let Some(seed) = self.params.iter().find(|state| state.enabled && state.value == Effective::RandomSeed)
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("{} draws a random seed per run, which only a catalog generation can do; set a seed", seed.name),
            ));
        }
        let visible = self.params.iter().filter(|state| state.enabled && keep(state)).map(|state| state.name.clone()).collect();
        let unhashed = self.params.iter().filter(|state| state.enabled && keep(state) && !state.affects.identity).map(|state| state.name.clone()).collect();
        let values = self
            .params
            .into_iter()
            .filter_map(|state| match state.value {
                Effective::Explicit(value) | Effective::Default(value) => Some((state.name, value)),
                Effective::Unset | Effective::Auto | Effective::RandomSeed => None,
            })
            .collect();
        Ok(ResolvedParams { values, visible, unhashed })
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
    /// The seeds a run draws for this resolution (#162): each enabled seed parameter whose
    /// effective setting is [`Effective::RandomSeed`], with its upper bound, in schema order.
    pub fn random_seeds(&self, resolution: &Resolution) -> Vec<(String, Option<Unsigned>)> {
        resolution
            .params
            .iter()
            .filter(|state| state.enabled && state.value == Effective::RandomSeed)
            .filter_map(|state| match self.schema.params.iter().find(|spec| spec.name == state.name).map(|spec| &spec.control) {
                Some(ParamControl::Seed { max }) => Some((state.name.clone(), *max)),
                _ => None,
            })
            .collect()
    }

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
        self.resolve_with(supplied, ready, &NoOptions)
    }

    /// [`Contract::resolve`] that also checks each enabled value with an option source against
    /// the list `options` has fetched for it. An unknown list refuses nothing.
    pub fn resolve_with(&self, supplied: &ParamValues, ready: bool, options: &dyn OptionLookup) -> Result<Resolution, Error> {
        self.validate()?;
        if supplied.len() > crate::schema::MAX_PARAMS {
            return Err(invalid("parameter values exceed count limit"));
        }
        let mut bytes = 0_usize;
        for value in supplied.values() {
            bytes = bytes.saturating_add(crate::schema::value_text_bytes(value));
        }
        // A stored value replaces its default, so only the defaults that apply are counted.
        for spec in self.schema.params.iter().filter(|spec| !supplied.contains_key(&spec.name)) {
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
            let listed = rule.options.as_ref().filter(|_| enabled).map(|source| {
                let scope = match &source.scope {
                    Some(scope) => env.get(scope.as_str()).and_then(|value| match value {
                        Value::Text(text) => Some(text.as_str()),
                        _ => None,
                    }),
                    None => None,
                };
                // A scoped list is unknown while its scope has no value.
                let key = (source.scope.is_none() || scope.is_some()).then_some(OptionKey { owner: &self.namespace, source: &source.id, scope });
                let known = key.is_some_and(|key| options.known(key));
                let member = match (key, value.value()) {
                    (Some(key), Some(Value::Text(text))) if known => options.contains(key, text),
                    _ => None,
                };
                if member == Some(false) {
                    issues.push(Issue {
                        code: IssueCode::NotAnOption,
                        param: Some(spec.name.clone()),
                        check: None,
                        message: format!("{} is not one of the available options; refresh the list or pick another", spec.label),
                    });
                }
                ParamOptions { source: source.id.clone(), scope: scope.map(str::to_owned), known }
            });
            let reset = !enabled && rule.hidden == HiddenPolicy::Reset && supplied.contains_key(&spec.name);
            params.push(ParamState {
                name: spec.name.clone(),
                id: self.param_id(&spec.name),
                value,
                visible,
                enabled,
                reset,
                affects: rule.affects,
                options: listed,
            });
        }
        for check in &self.checks {
            if check.when.as_ref().is_none_or(|condition| condition.holds(&env)) && !check.require.holds(&env) {
                issues.push(Issue { code: IssueCode::Check, param: None, check: Some(check.id.clone()), message: check.message.clone() });
            }
        }
        Ok(Resolution { params, issues })
    }
}
