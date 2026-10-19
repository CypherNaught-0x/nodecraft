//! Bounded, serialisable conditions over effective parameter values.

use super::{MAX_CONDITION_DEPTH, MAX_CONDITION_NODES, MAX_CONDITION_VALUES};
use crate::{Error, Number, ParamControl, ParamSpec, Value, schema::invalid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How a numeric parameter compares with a constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Lt,
    Lte,
    Gt,
    Gte,
}

/// A condition evaluated over a node's *active* parameters: a hidden or disabled parameter
/// counts as absent, so a hidden controller also hides its dependants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    /// The parameter has an explicit or literal-default value.
    Present {
        param: String,
    },
    Equals {
        param: String,
        value: Value,
    },
    In {
        param: String,
        values: Vec<Value>,
    },
    /// Numeric comparison; integers compare by value, not by their text.
    Compare {
        param: String,
        cmp: Comparison,
        value: Number,
    },
    All {
        of: Vec<Condition>,
    },
    Any {
        of: Vec<Condition>,
    },
    Not {
        condition: Box<Condition>,
    },
}

pub(crate) type Env<'a> = BTreeMap<&'a str, &'a Value>;

impl Condition {
    /// Check shape, bounds and references. `known` maps every parameter the condition may
    /// name to its spec; `budget` is shared across one contract so many small conditions
    /// cannot add up to an unbounded evaluation.
    pub(crate) fn validate(&self, known: &BTreeMap<&str, &ParamSpec>, budget: &mut usize) -> Result<(), Error> {
        let mut stack = vec![(self, 1_usize)];
        let mut nodes = 0_usize;
        while let Some((condition, depth)) = stack.pop() {
            nodes = nodes.saturating_add(1);
            *budget = budget.checked_sub(1).ok_or_else(|| invalid("contract conditions exceed their total size limit"))?;
            if depth > MAX_CONDITION_DEPTH || nodes > MAX_CONDITION_NODES {
                return Err(invalid("a condition is nested too deeply or has too many terms"));
            }
            let spec = |param: &str| known.get(param).copied().ok_or_else(|| invalid("a condition names a parameter it cannot see"));
            match condition {
                Self::Present { param } => {
                    spec(param)?;
                }
                Self::Equals { param, value } => spec(param)?.control.validate_value_checked(value)?,
                Self::In { param, values } => {
                    if values.is_empty() || values.len() > MAX_CONDITION_VALUES {
                        return Err(invalid("an `in` condition needs between 1 and 64 values"));
                    }
                    let control = &spec(param)?.control;
                    for value in values {
                        control.validate_value_checked(value)?;
                    }
                }
                Self::Compare { param, .. } => {
                    if !matches!(spec(param)?.control, ParamControl::Slider { .. } | ParamControl::Integer { .. } | ParamControl::Unsigned { .. }) {
                        // Seeds span all of u64, beyond what an f64 comparison holds exactly.
                        return Err(invalid("a comparison needs a numeric, non-seed parameter"));
                    }
                }
                Self::All { of } | Self::Any { of } => {
                    if of.is_empty() {
                        return Err(invalid("`all` and `any` need at least one condition"));
                    }
                    stack.extend(of.iter().map(|inner| (inner, depth.saturating_add(1))));
                }
                Self::Not { condition } => stack.push((condition, depth.saturating_add(1))),
            }
        }
        Ok(())
    }

    /// Evaluate a validated condition. Depth is bounded by validation, so recursion is too.
    pub(crate) fn holds(&self, env: &Env<'_>) -> bool {
        match self {
            Self::Present { param } => env.contains_key(param.as_str()),
            Self::Equals { param, value } => env.get(param.as_str()).is_some_and(|actual| *actual == value),
            Self::In { param, values } => env.get(param.as_str()).is_some_and(|actual| values.contains(actual)),
            Self::Compare { param, cmp, value } => env.get(param.as_str()).and_then(|actual| numeric(actual)).is_some_and(|actual| {
                let value = value.get();
                match cmp {
                    Comparison::Lt => actual < value,
                    Comparison::Lte => actual <= value,
                    Comparison::Gt => actual > value,
                    Comparison::Gte => actual >= value,
                }
            }),
            Self::All { of } => of.iter().all(|inner| inner.holds(env)),
            Self::Any { of } => of.iter().any(|inner| inner.holds(env)),
            Self::Not { condition } => !condition.holds(env),
        }
    }
}

fn numeric(value: &Value) -> Option<f64> {
    // i64/u64 to f64 rounds beyond 2^53; integer controls compared here are bounded far below
    // that, and seeds are not comparable.
    #[allow(clippy::cast_precision_loss)]
    match value {
        Value::Integer(number) => Some(number.0 as f64),
        Value::Unsigned(number) => Some(number.0 as f64),
        Value::Number(number) => Some(number.get()),
        _ => None,
    }
}
