//! Logical ownership, excluding spare capacity/allocator overhead. Parent shells include
//! inline children; helpers below count only dynamic children unless named `owned`.
use super::*;
use nodecraft_graph::{Canvas, Node, Port};
use nodecraft_nodes::{GenerateInput, GenerateOutput};
use nodecraft_types::{Choice, ParamControl, ParamSpec};
use std::mem::size_of;

pub const LINK: usize = 48;
pub fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or_else(cap)
}
pub fn mul(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_mul(b).ok_or_else(cap)
}
pub fn entries<K, V>(n: usize) -> Result<usize, Error> {
    mul(n, add(size_of::<(K, V)>(), LINK)?)
}
pub fn values(value: &Value) -> Result<usize, Error> {
    fn walk(v: &Value, depth: usize, items: &mut usize) -> Result<usize, Error> {
        *items = add(*items, 1)?;
        if depth > nodecraft_graph::MAX_VALUE_DEPTH || *items > nodecraft_graph::MAX_VALUE_ITEMS {
            return Err(cap());
        }
        match v {
            Value::Text(t) => Ok(t.len()),
            Value::Media(a) => Ok(a.mime.as_str().len()),
            Value::List(v) => {
                let mut n = mul(v.len(), size_of::<Value>())?;
                for x in v {
                    n = add(n, walk(x, depth + 1, items)?)?;
                }
                Ok(n)
            }
            _ => Ok(0),
        }
    }
    walk(value, 0, &mut 0)
}
pub fn params(map: &ParamValues) -> Result<usize, Error> {
    let mut n = entries::<String, Value>(map.len())?;
    for (k, v) in map {
        n = add(n, add(k.len(), values(v)?)?)?;
    }
    Ok(n)
}
pub fn resolved(v: &nodecraft_types::BorrowedResolvedParams<'_>) -> Result<usize, Error> {
    let mut n = 0;
    for (k, value) in &v.values {
        if v.visible.contains(*k) {
            n = add(n, add(add(size_of::<(String, Value)>(), LINK)?, add(k.len(), values(value)?)?)?)?;
        }
    }
    Ok(n)
}
pub fn schema(s: &ParamSchema) -> Result<usize, Error> {
    let mut n = mul(s.params.len(), size_of::<ParamSpec>())?;
    for p in &s.params {
        n = add(n, add(p.name.len(), add(p.label.len(), p.description.as_ref().map_or(0, String::len))?)?)?;
        if let Some(v) = &p.default {
            n = add(n, values(v)?)?;
        }
        if let Some(c) = &p.visible_if {
            n = add(n, add(c.parameter.len(), values(&c.equals)?)?)?;
        }
        if let ParamControl::Choice { options } = &p.control {
            n = add(n, mul(options.len(), size_of::<Choice>())?)?;
            for x in options {
                n = add(n, add(x.value.len(), x.label.len())?)?;
            }
        }
    }
    Ok(n)
}
/// Upper bound for schema validation's indexes and two simultaneously live borrowed
/// resolutions. Kept separate from schema payload and admitted before calling types helpers.
pub fn scratch(s: &ParamSchema) -> Result<usize, Error> {
    // Covers bounded graph/descriptor port sets, mode/role sets and fixed
    // traversal frames while schema validation's per-control index is live.
    let mut n = 64 * 1024usize;
    for p in &s.params {
        n = add(n, add(1024, mul(4, p.name.len())?)?)?;
        if let ParamControl::Choice { options } = &p.control {
            n = add(n, mul(options.len(), size_of::<&str>() + LINK)?)?;
        }
    }
    if n > MAX_SCRATCH_BYTES { Err(cap()) } else { Ok(n) }
}
fn ports(ports: &[Port]) -> Result<usize, Error> {
    let mut n = mul(ports.len(), size_of::<Port>())?;
    for p in ports {
        n = add(n, p.name.len())?;
    }
    Ok(n)
}
fn node(n: &Node) -> Result<usize, Error> {
    let mut b = add(n.kind.len(), n.title.len())?;
    b = add(b, add(params(&n.params)?, params(&n.config)?)?)?;
    b = add(b, add(ports(&n.inputs)?, ports(&n.outputs)?)?)?;
    b = add(b, mul(n.history.len(), size_of::<nodecraft_graph::Generation>())?)?;
    for g in &n.history {
        b = add(b, mul(g.outputs.len(), size_of::<Value>())?)?;
        for v in &g.outputs {
            b = add(b, values(v)?)?;
        }
    }
    Ok(b)
}
/// Complete borrowed preflight before the engine clones a snapshot.
pub fn canvas(c: &Canvas) -> Result<usize, Error> {
    let mut b = add(size_of::<Canvas>(), c.title().len())?;
    b = add(b, entries::<NodeId, Node>(c.nodes().len())?)?;
    for n in c.nodes().values() {
        b = add(b, node(n)?)?;
    }
    b = add(b, entries::<nodecraft_types::EdgeId, nodecraft_graph::Edge>(c.edges().len())?)?;
    for e in c.edges().values() {
        b = add(b, add(e.from.port.len(), e.to.port.len())?)?;
    }
    b = add(b, entries::<nodecraft_types::FieldId, nodecraft_graph::Field>(c.fields().len())?)?;
    for f in c.fields().values() {
        b = add(b, f.title.len())?;
    }
    b = add(b, entries::<nodecraft_types::TagId, nodecraft_graph::Tag>(c.tags().len())?)?;
    for t in c.tags().values() {
        b = add(b, t.title.len())?;
    }
    b = add(b, entries::<nodecraft_types::CommentId, nodecraft_graph::Comment>(c.comments().len())?)?;
    for x in c.comments().values() {
        b = add(b, x.text.len())?;
    }
    if b > MAX_SNAPSHOT_BYTES { Err(cap()) } else { Ok(b) }
}
pub(crate) fn step(s: &crate::planner::Step) -> Result<usize, Error> {
    use crate::planner::{InputSource, PlannedInput};
    let mut n = add(size_of::<crate::planner::Step>() + LINK, add(s.kind.len(), s.mode.as_str().len())?)?;
    n = add(n, mul(s.after.len(), size_of::<NodeId>())?)?;
    n = add(n, mul(s.inputs.len(), size_of::<PlannedInput>())?)?;
    for i in &s.inputs {
        n = add(n, i.port.len())?;
        n = add(n, mul(i.from.len(), size_of::<InputSource>())?)?;
        for v in &i.from {
            n = add(
                n,
                match v {
                    InputSource::Value(v) => values(v)?,
                    InputSource::Step { port, .. } => port.len(),
                },
            )?;
        }
    }
    Ok(n)
}
pub(crate) fn job(job: &JobSpec, canonical: Option<&nodecraft_types::BorrowedResolvedParams<'_>>) -> Result<usize, Error> {
    let mut n = size_of::<JobSpec>();
    match job {
        JobSpec::Source { output, value, .. } => n = add(n, add(output.len(), values(value)?)?)?,
        JobSpec::Action { action, params: p, inputs, output, .. } => {
            n = add(n, add(action.len(), output.len())?)?;
            n = add(n, params(p)?)?;
            for value in inputs {
                n = add(n, values(value)?)?;
            }
        }
        JobSpec::Generate { provider, model, params: p, inputs, outputs, .. } => {
            n = add(n, add(provider.len(), model.len())?)?;
            n = add(
                n,
                match canonical {
                    Some(v) => resolved(v)?,
                    None => params(p)?,
                },
            )?;
            n = add(n, mul(inputs.len(), size_of::<GenerateInput>())?)?;
            for i in inputs {
                n = add(n, add(i.role.len(), values(&i.value)?)?)?;
            }
            n = add(n, mul(outputs.len(), size_of::<GenerateOutput>())?)?;
            for o in outputs {
                n = add(n, o.port.len())?;
            }
        }
    }
    Ok(n)
}

/// Dynamic children of a shared parameter contract. Bounds are checked while walking and
/// before any caller clones it; this is logical ownership, not allocator/RSS accounting.
pub fn contract(c: &nodecraft_types::Contract) -> Result<usize, Error> {
    use nodecraft_types::{Check, Condition, ParamRule};
    fn condition(c: &Condition, depth: usize) -> Result<usize, Error> {
        if depth > 8 {
            return Err(cap());
        }
        let body = match c {
            Condition::Present { param } | Condition::Compare { param, .. } => param.len(),
            Condition::Equals { param, value } => add(param.len(), values(value)?)?,
            Condition::In { param, values: vs } => {
                if vs.len() > 64 {
                    return Err(cap());
                }
                vs.iter().try_fold(add(param.len(), mul(vs.len(), size_of::<Value>())?)?, |n, v| add(n, values(v)?))?
            }
            Condition::All { of } | Condition::Any { of } => {
                if of.len() > 64 {
                    return Err(cap());
                }
                of.iter().try_fold(mul(of.len(), size_of::<Condition>())?, |n, c| add(n, condition(c, depth + 1)?))?
            }
            Condition::Not { condition: c } => add(size_of::<Condition>(), condition(c, depth + 1)?)?,
        };
        if body > MAX_WITNESS_BYTES { Err(cap()) } else { Ok(body) }
    }
    if c.rules.len() > 256 || c.checks.len() > 64 {
        return Err(cap());
    }
    let mut n = add(c.namespace.len(), schema(&c.schema)?)?;
    n = add(n, mul(c.rules.len(), size_of::<ParamRule>())?)?;
    for r in &c.rules {
        n = add(n, add(r.param.len(), r.meaning.as_ref().map_or(0, String::len))?)?;
        for term in [&r.visible_when, &r.enabled_when].into_iter().flatten() {
            n = add(n, condition(term, 1)?)?;
        }
    }
    n = add(n, mul(c.checks.len(), size_of::<Check>())?)?;
    for c in &c.checks {
        n = add(n, add(c.id.len(), c.message.len())?)?;
        if let Some(term) = &c.when {
            n = add(n, condition(term, 1)?)?;
        }
        n = add(n, condition(&c.require, 1)?)?;
    }
    if n > MAX_WITNESS_BYTES { Err(cap()) } else { Ok(n) }
}
