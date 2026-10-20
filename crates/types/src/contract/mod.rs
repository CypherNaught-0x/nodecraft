//! The versioned parameter contract (#162): what each parameter means, when it applies, how
//! its value is chosen and what it affects. It wraps a [`ParamSchema`] (controls, bounds,
//! literal defaults) with the rules a schema alone cannot state. The specification is
//! `docs/descriptor-contract.md`.

mod admission;
#[cfg(test)]
mod admission469_tests;
mod condition;
mod merge;
mod options;
#[cfg(test)]
mod options_tests;
mod resolve;
#[cfg(test)]
mod tests;

pub use condition::{Comparison, Condition};
pub use merge::{ConnectedText, DropReason, Dropped, Migration, Shared, TextPrecedence, compose_text, overlay, shared_value};
pub use options::{MAX_OPTION_BYTES, MAX_OPTION_LISTS, MAX_OPTIONS, NoOptions, OptionKey, OptionLists, OptionLookup, OptionSource};
pub use resolve::{Effective, Issue, IssueCode, ParamOptions, ParamState, Resolution};

use crate::{Error, ErrorKind, ParamControl, ParamSchema, ParamSpec, schema::invalid};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_CONDITION_DEPTH: usize = 8;
const MAX_CONDITION_NODES: usize = 64;
const MAX_CONDITION_VALUES: usize = 64;
const MAX_CONTRACT_CONDITION_NODES: usize = 4096;
const MAX_CHECKS: usize = 64;
const MAX_MESSAGE: usize = 512;
const MAX_NAMESPACE: usize = 128;

/// Contract wire version. A reader accepts its own major version and any minor version: minor
/// versions only add optional fields, majors may change meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContractVersion {
    pub major: u16,
    pub minor: u16,
}

impl ContractVersion {
    pub const CURRENT: Self = Self { major: 1, minor: 2 };
}

impl Default for ContractVersion {
    fn default() -> Self {
        Self::CURRENT
    }
}

/// Who defines a parameter set: a node kind, a model in the catalog, or a deterministic action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamOwner {
    Node,
    Model,
    Action,
}

/// The unit a numeric value is measured in. Migration keeps a value only when units match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    #[default]
    None,
    Pixels,
    Seconds,
    Milliseconds,
    Frames,
    FramesPerSecond,
    Hertz,
    Decibels,
    Degrees,
    Percent,
    Ratio,
    Tokens,
    Count,
    Bytes,
}

/// How a value is chosen when none was set explicitly. `Unset` (no value, not submitted),
/// `Auto` (the provider decides; not submitted), `Literal` (the schema default) and
/// `RandomSeed` (a fresh seed each run, recorded with the generation) never coincide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultPolicy {
    Unset,
    Literal,
    Auto,
    RandomSeed,
}

/// What happens to an explicit value while its parameter is hidden or disabled. Either way it
/// is never submitted; `Reset` additionally tells editors to clear it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiddenPolicy {
    #[default]
    Keep,
    Reset,
}

/// Which derived facts a parameter's effective value changes. `identity` covers staleness and
/// cache keys; `cost` the price quote; `shape` the output's size, duration or format; `jobs`
/// how many jobs a run plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Affects {
    #[serde(default = "yes")]
    pub identity: bool,
    #[serde(default)]
    pub cost: bool,
    #[serde(default)]
    pub shape: bool,
    #[serde(default)]
    pub jobs: bool,
}

fn yes() -> bool {
    true
}

impl Default for Affects {
    fn default() -> Self {
        Self { identity: true, cost: false, shape: false, jobs: false }
    }
}

/// Per-parameter rules beyond the schema. Parameters without one get [`ParamRule::implied`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamRule {
    pub param: String,
    #[serde(default)]
    pub unit: Unit,
    /// Absent: `Literal` when the schema has a default, else `Unset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<DefaultPolicy>,
    /// Shown only while this holds (and the schema's own `visible_if`, if any). Names earlier
    /// parameters only, so conditions cannot form cycles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<Condition>,
    /// Shown but read-only unless this holds. Same reference rule as `visible_when`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_when: Option<Condition>,
    #[serde(default)]
    pub hidden: HiddenPolicy,
    #[serde(default)]
    pub affects: Affects,
    /// A stable meaning tag (`seed`, `aspect_ratio`, `guidance`). When either side of a model
    /// swap declares one, both must match for a value to carry over: a shared name is not
    /// evidence of a shared meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meaning: Option<String>,
    /// Since 1.1: the value must come from a list the host fetches at run time. Only single-line
    /// text and model parameters take one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<OptionSource>,
}

impl ParamRule {
    pub fn implied(param: &str) -> Self {
        Self {
            param: param.to_owned(),
            unit: Unit::None,
            default: None,
            visible_when: None,
            enabled_when: None,
            hidden: HiddenPolicy::Keep,
            affects: Affects::default(),
            meaning: None,
            options: None,
        }
    }
    fn default_policy(&self, spec: &ParamSpec) -> DefaultPolicy {
        self.default.unwrap_or(if spec.default.is_some() { DefaultPolicy::Literal } else { DefaultPolicy::Unset })
    }
}

/// A cross-field check: whenever `when` holds (always, if absent), `require` must hold too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Condition>,
    pub require: Condition,
    /// Public-safe text shown to the user and returned to agents.
    pub message: String,
}

/// A parameter set with its rules. Validate before use; every other method assumes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    #[serde(default)]
    pub version: ContractVersion,
    /// Dotted lowercase owner id (`text.generate`, `fal.flux_dev`); a parameter's stable id is
    /// `<namespace>/<name>`.
    pub namespace: String,
    pub owner: ParamOwner,
    pub schema: ParamSchema,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<ParamRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
}

pub(crate) fn namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NAMESPACE
        && value.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

/// Lossless catalog-model namespace. Parameter names contain no slash, so the final slash
/// separates a stable parameter id from its exact model identity.
pub fn model_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NAMESPACE
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_' | b'/'))
        && value.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

impl Contract {
    /// A contract that adds nothing to `schema`: every parameter gets its implied rule.
    pub fn from_schema(namespace: impl Into<String>, owner: ParamOwner, schema: ParamSchema) -> Self {
        Self { version: ContractVersion::CURRENT, namespace: namespace.into(), owner, schema, rules: Vec::new(), checks: Vec::new() }
    }

    /// The stable, namespaced id of a parameter.
    pub fn param_id(&self, name: &str) -> String {
        format!("{}/{name}", self.namespace)
    }

    /// The rule for `name`, or its implied rule. `None` if there is no such parameter.
    pub fn rule(&self, name: &str) -> Option<std::borrow::Cow<'_, ParamRule>> {
        self.schema.params.iter().any(|spec| spec.name == name).then(|| {
            self.rules.iter().find(|rule| rule.param == name).map_or_else(|| std::borrow::Cow::Owned(ParamRule::implied(name)), std::borrow::Cow::Borrowed)
        })
    }

    /// Rejects unsupported versions, duplicate or dangling rules, inconsistent policies,
    /// conditions that look ahead or name unknown parameters, and oversized data.
    pub fn validate(&self) -> Result<(), Error> {
        if self.version.major != ContractVersion::CURRENT.major {
            return Err(Error::new(ErrorKind::Unsupported, "this parameter contract needs a newer NodeCraft"));
        }
        if !(if self.owner == ParamOwner::Model { model_namespace(&self.namespace) } else { namespace(&self.namespace) }) {
            return Err(invalid("a contract namespace must be a bounded lowercase owner id"));
        }
        self.schema.validate()?;
        if self.rules.len() > self.schema.params.len() || self.checks.len() > MAX_CHECKS {
            return Err(invalid("a contract has more rules than parameters or too many checks"));
        }
        let mut rules = BTreeMap::new();
        for rule in &self.rules {
            if rules.insert(rule.param.as_str(), rule).is_some() {
                return Err(invalid("a parameter has more than one rule"));
            }
        }
        let mut budget = MAX_CONTRACT_CONDITION_NODES;
        let mut earlier: BTreeMap<&str, &ParamSpec> = BTreeMap::new();
        for spec in &self.schema.params {
            if let Some(rule) = rules.remove(spec.name.as_str()) {
                validate_rule(spec, rule, &earlier, &mut budget)?;
            }
            earlier.insert(spec.name.as_str(), spec);
        }
        if !rules.is_empty() {
            return Err(invalid("a rule names a parameter the schema does not have"));
        }
        let mut ids = BTreeSet::new();
        for check in &self.checks {
            if !crate::schema::name(&check.id) || !ids.insert(check.id.as_str()) || check.message.trim().is_empty() || check.message.len() > MAX_MESSAGE {
                return Err(invalid("checks need unique lowercase ids and a short message"));
            }
            if let Some(when) = &check.when {
                when.validate(&earlier, &mut budget)?;
            }
            check.require.validate(&earlier, &mut budget)?;
        }
        Ok(())
    }
}

fn validate_rule(spec: &ParamSpec, rule: &ParamRule, earlier: &BTreeMap<&str, &ParamSpec>, budget: &mut usize) -> Result<(), Error> {
    let numeric = matches!(spec.control, ParamControl::Slider { .. } | ParamControl::Integer { .. } | ParamControl::Unsigned { .. });
    if rule.unit != Unit::None && !numeric {
        return Err(invalid("only numeric parameters have units"));
    }
    match rule.default {
        Some(DefaultPolicy::Literal) if spec.default.is_none() => return Err(invalid("a literal default policy needs a schema default")),
        Some(DefaultPolicy::Auto | DefaultPolicy::Unset) if spec.default.is_some() => {
            return Err(invalid("a parameter with a schema default cannot also be auto or unset"));
        }
        Some(DefaultPolicy::RandomSeed) if !matches!(spec.control, ParamControl::Seed { .. }) || spec.default.is_some() => {
            return Err(invalid("only seed parameters without a literal default can default to a random seed"));
        }
        _ => {}
    }
    if rule.meaning.as_ref().is_some_and(|meaning| !crate::schema::name(meaning)) {
        return Err(invalid("a meaning tag is a lowercase name"));
    }
    for condition in [&rule.visible_when, &rule.enabled_when].into_iter().flatten() {
        condition.validate(earlier, budget)?;
    }
    if let Some(source) = &rule.options {
        if !matches!(spec.control, ParamControl::Text { multiline: false, .. } | ParamControl::Model) || !namespace(&source.id) {
            return Err(invalid("an option source needs a dotted id on a single-line text or model parameter"));
        }
        if let Some(scope) = &source.scope {
            let controller = earlier.get(scope.as_str()).ok_or_else(|| invalid("an option scope must name an earlier parameter"))?;
            if !matches!(controller.control, ParamControl::Text { multiline: false, .. } | ParamControl::Choice { .. } | ParamControl::Model) {
                return Err(invalid("an option scope must be a single-line text, choice or model parameter"));
            }
        }
    }
    Ok(())
}
