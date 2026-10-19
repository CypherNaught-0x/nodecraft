//! Combining values: model-swap migration, layered overrides, bulk intersections and the
//! precedence of connected text over a node's own prompt.

use super::Contract;
use crate::{Error, ParamValues, Value, schema::invalid};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, mem::discriminant};

const MAX_LAYERS: usize = 8;
const MAX_CONNECTED: usize = 1024;
const MAX_SEPARATOR: usize = 64;

/// Why a stored value did not carry over to another contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// The source contract has no such parameter (an unknown stored key).
    NotInSource,
    NotInTarget,
    ControlChanged,
    UnitChanged,
    MeaningChanged,
    /// Across different owners (a model swap) a value carries over only when both sides tag
    /// the parameter with the same meaning; a matching name alone proves nothing.
    UnverifiedMeaning,
    /// Same kind of control, but the value is outside the target's range or options.
    InvalidInTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropped {
    pub param: String,
    pub reason: DropReason,
    pub value: Value,
}

/// Values to store after a swap, and every value that was left behind and why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    pub kept: ParamValues,
    pub dropped: Vec<Dropped>,
}

impl Contract {
    /// Whether `name` means the same thing here and in `target`: both have it, with the same
    /// kind of control, the same unit and no conflicting meaning tag. Between different
    /// namespaces (a model swap) both sides must also carry the same meaning tag; within one
    /// namespace (a descriptor upgrade) the name is the owner's own promise. Values still need
    /// checking against the target's bounds.
    pub fn compatible(&self, name: &str, target: &Self) -> Result<(), DropReason> {
        let Some(from) = self.schema.params.iter().find(|spec| spec.name == name) else {
            return Err(DropReason::NotInSource);
        };
        let Some(to) = target.schema.params.iter().find(|spec| spec.name == name) else {
            return Err(DropReason::NotInTarget);
        };
        if discriminant(&from.control) != discriminant(&to.control) {
            return Err(DropReason::ControlChanged);
        }
        let (Some(from_rule), Some(to_rule)) = (self.rule(name), target.rule(name)) else {
            return Err(DropReason::NotInSource);
        };
        if from_rule.unit != to_rule.unit {
            return Err(DropReason::UnitChanged);
        }
        if (from_rule.meaning.is_some() || to_rule.meaning.is_some()) && from_rule.meaning != to_rule.meaning {
            return Err(DropReason::MeaningChanged);
        }
        if self.namespace != target.namespace && from_rule.meaning.is_none() {
            return Err(DropReason::UnverifiedMeaning);
        }
        Ok(())
    }

    /// Carry explicit values to `target` (a model swap or a descriptor upgrade). A value is
    /// kept only when the parameter is compatible and the value is valid in the target; it is
    /// never coerced. Both contracts must validate.
    pub fn migrate(&self, target: &Self, values: &ParamValues) -> Result<Migration, Error> {
        self.validate()?;
        target.validate()?;
        if values.len() > crate::schema::MAX_PARAMS {
            return Err(invalid("parameter values exceed count limit"));
        }
        let mut migration = Migration::default();
        for (name, value) in values {
            let reason = match self.compatible(name, target) {
                Err(reason) => Some(reason),
                Ok(()) => target
                    .schema
                    .params
                    .iter()
                    .find(|spec| spec.name == *name)
                    .and_then(|spec| spec.control.validate_value_checked(value).err())
                    .map(|_| DropReason::InvalidInTarget),
            };
            match reason {
                None => {
                    migration.kept.insert(name.clone(), value.clone());
                }
                Some(reason) => migration.dropped.push(Dropped { param: name.clone(), reason, value: value.clone() }),
            }
        }
        Ok(migration)
    }

    /// Parameter names every contract has with a compatible meaning: the fields a bulk edit
    /// across `contracts` may offer, in the first contract's order.
    pub fn shared_params(contracts: &[&Self]) -> Vec<String> {
        let Some((first, rest)) = contracts.split_first() else {
            return Vec::new();
        };
        first.schema.params.iter().filter(|spec| rest.iter().all(|other| first.compatible(&spec.name, other).is_ok())).map(|spec| spec.name.clone()).collect()
    }
}

/// One field across a bulk selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Shared {
    /// Every target holds this value.
    Same(Value),
    /// Targets disagree, or some have a value and others none.
    Mixed,
    /// No target holds a value.
    Unset,
}

/// Summarise one field over many targets; `None` is a target without a value.
pub fn shared_value<'a>(values: impl IntoIterator<Item = Option<&'a Value>>) -> Shared {
    let mut values = values.into_iter();
    let Some(first) = values.next() else {
        return Shared::Unset;
    };
    if values.all(|value| value == first) { first.map_or(Shared::Unset, |value| Shared::Same(value.clone())) } else { Shared::Mixed }
}

/// Merge override layers, lowest precedence first. The documented order is technique
/// defaults, then model presets, then the node's own values (which bulk edits write); schema
/// defaults sit below them all and are applied by [`Contract::resolve`].
pub fn overlay(layers: &[&ParamValues]) -> Result<ParamValues, Error> {
    if layers.len() > MAX_LAYERS {
        return Err(invalid("too many override layers"));
    }
    let mut merged = ParamValues::new();
    for layer in layers {
        if layer.len() > crate::schema::MAX_PARAMS {
            return Err(invalid("parameter values exceed count limit"));
        }
        merged.extend(layer.iter().map(|(name, value)| (name.clone(), value.clone())));
        if merged.len() > crate::schema::MAX_PARAMS {
            return Err(invalid("parameter values exceed count limit"));
        }
    }
    Ok(merged)
}

/// How text arriving on wires combines with a node's own prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPrecedence {
    /// Connected text first, then the node's own text.
    #[default]
    ConnectedFirst,
    /// The node's own text first, then connected text.
    LocalFirst,
    /// Connected text, if any, replaces the node's own text.
    ConnectedReplaces,
    /// Wires are ignored.
    LocalOnly,
}

/// One piece of connected text. `order` is its position among the port's wires (the order the
/// user arranged); `tie` (an edge id's bytes) settles equal positions, so the result never
/// depends on storage or iteration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectedText<'a> {
    pub order: u32,
    pub tie: [u8; 16],
    pub text: &'a str,
}

/// Combine connected and local text deterministically; empty pieces are skipped.
pub fn compose_text(precedence: TextPrecedence, local: Option<&str>, connected: &[ConnectedText<'_>], separator: &str) -> Result<String, Error> {
    if connected.len() > MAX_CONNECTED || separator.len() > MAX_SEPARATOR {
        return Err(invalid("too many connected texts or separator too long"));
    }
    let ignored = precedence == TextPrecedence::LocalOnly;
    let mut wired: Vec<_> = connected.iter().filter(|piece| !ignored && !piece.text.is_empty()).collect();
    wired.sort_by_key(|piece| (piece.order, piece.tie));
    let mut seen = BTreeSet::new();
    if !wired.iter().all(|piece| seen.insert((piece.order, piece.tie))) {
        return Err(invalid("two connected texts share one position"));
    }
    let wired = wired.into_iter().map(|piece| piece.text);
    let local = local.filter(|text| !text.is_empty());
    let pieces: Vec<&str> = match precedence {
        TextPrecedence::ConnectedFirst => wired.chain(local).collect(),
        TextPrecedence::LocalFirst => local.into_iter().chain(wired).collect(),
        TextPrecedence::ConnectedReplaces => {
            let wired: Vec<_> = wired.collect();
            if wired.is_empty() { local.into_iter().collect() } else { wired }
        }
        TextPrecedence::LocalOnly => local.into_iter().collect(),
    };
    let total = pieces.iter().fold(separator.len().saturating_mul(pieces.len()), |sum, piece| sum.saturating_add(piece.len()));
    if total > crate::schema::MAX_TEXT {
        return Err(invalid("combined text exceeds the text limit"));
    }
    Ok(pieces.join(separator))
}
