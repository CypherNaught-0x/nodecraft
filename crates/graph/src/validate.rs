use std::collections::{BTreeMap, BTreeSet, VecDeque};

use nodecraft_types::{Error, ErrorKind, Value};

use crate::{Canvas, Node, Position, Project};

pub const MAX_CHANGES: usize = 10_000;
pub const MAX_VALUE_DEPTH: usize = 32;
pub const MAX_VALUE_ITEMS: usize = 4096;
const MAX_CANVASES: usize = 64;
const MAX_NODES: usize = 10_000;
const MAX_EDGES: usize = 50_000;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_PORTS: usize = 256;

pub(crate) fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

pub(crate) fn text(value: &str, limit: usize) -> Result<(), Error> {
    if value.len() > limit { Err(invalid("text exceeds the document limit")) } else { Ok(()) }
}

pub(crate) fn position(pos: Position) -> Result<(), Error> {
    if !pos.x.is_finite() || !pos.y.is_finite() || pos.x.abs() > 10_000_000.0 || pos.y.abs() > 10_000_000.0 {
        Err(invalid("position must be finite and within canvas bounds"))
    } else {
        Ok(())
    }
}

/// Iterative validation happens before any recursive Clone/Serialize of caller-owned values.
pub(crate) fn value(root: &Value) -> Result<(), Error> {
    let mut pending = vec![(root, 0usize)];
    let mut seen = 0usize;
    let mut text_bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        seen = seen.saturating_add(1);
        if depth > MAX_VALUE_DEPTH || seen > MAX_VALUE_ITEMS {
            return Err(invalid("value exceeds depth or item limits"));
        }
        match value {
            Value::Text(value) => {
                text_bytes = text_bytes.saturating_add(value.len());
                if text_bytes > MAX_TEXT_BYTES {
                    return Err(invalid("value exceeds text limit"));
                }
            }
            Value::Media(_) => {}
            Value::List(items) => {
                if seen.saturating_add(pending.len()).saturating_add(items.len()) > MAX_VALUE_ITEMS {
                    return Err(invalid("value exceeds item limit"));
                }
                pending.extend(items.iter().map(|item| (item, depth.saturating_add(1))));
            }
        }
    }
    Ok(())
}

pub(crate) fn node(node: &Node) -> Result<(), Error> {
    text(&node.kind, 256)?;
    if node.kind.trim().is_empty() {
        return Err(invalid("node kind must not be empty"));
    }
    text(&node.title, 4096)?;
    position(node.pos)?;
    if node.params.len() > 256 || node.history.len() > 1024 {
        return Err(invalid("node exceeds parameter or history limits"));
    }
    for (name, param) in &node.params {
        text(name, 256)?;
        if name.is_empty() {
            return Err(invalid("parameter name must not be empty"));
        }
        value(param)?;
    }
    for ports in [&node.inputs, &node.outputs] {
        if ports.len() > MAX_PORTS {
            return Err(invalid("node exceeds port limit"));
        }
        let mut names = BTreeSet::new();
        for port in ports {
            text(&port.name, 256)?;
            if port.name.is_empty() || !names.insert(&port.name) {
                return Err(invalid("port names must be nonempty and unique per direction"));
            }
        }
    }
    let mut generations = BTreeSet::new();
    for generation in &node.history {
        if !generations.insert(generation.id) {
            return Err(invalid("duplicate generation id"));
        }
        if generation.outputs.len() > 256 {
            return Err(invalid("generation exceeds output limit"));
        }
        for output in &generation.outputs {
            value(output)?;
        }
    }
    if node.active.is_some_and(|id| !generations.contains(&id)) {
        return Err(invalid("active generation is missing from history"));
    }
    Ok(())
}

impl Project {
    pub fn validate(&self) -> Result<(), Error> {
        text(&self.title, 4096)?;
        if self.canvases.len() > MAX_CANVASES {
            return Err(invalid("project exceeds canvas limit"));
        }
        let mut nodes = BTreeSet::new();
        let mut edges = BTreeSet::new();
        let mut fields = BTreeSet::new();
        let mut tags = BTreeSet::new();
        let mut comments = BTreeSet::new();
        let mut generations = BTreeSet::new();
        for (id, canvas) in &self.canvases {
            if *id != canvas.id {
                return Err(invalid("canvas map key does not match its id"));
            }
            canvas.validate()?;
            for node in canvas.nodes.values() {
                if !nodes.insert(node.id) {
                    return Err(invalid("duplicate node id across canvases"));
                }
                for generation in &node.history {
                    if !generations.insert(generation.id) {
                        return Err(invalid("duplicate generation id across nodes"));
                    }
                }
            }
            for id in canvas.edges.keys() {
                if !edges.insert(*id) {
                    return Err(invalid("duplicate edge id across canvases"));
                }
            }
            for id in canvas.fields.keys() {
                if !fields.insert(*id) {
                    return Err(invalid("duplicate field id across canvases"));
                }
            }
            for id in canvas.tags.keys() {
                if !tags.insert(*id) {
                    return Err(invalid("duplicate tag id across canvases"));
                }
            }
            for id in canvas.comments.keys() {
                if !comments.insert(*id) {
                    return Err(invalid("duplicate comment id across canvases"));
                }
            }
            if nodes.len() > MAX_NODES || edges.len() > MAX_EDGES || fields.len() > MAX_NODES || tags.len() > MAX_NODES || comments.len() > MAX_NODES {
                return Err(invalid("project exceeds entity limits"));
            }
        }
        Ok(())
    }
}

impl Canvas {
    pub fn validate(&self) -> Result<(), Error> {
        text(&self.title, 4096)?;
        if self.nodes.len() > MAX_NODES
            || self.edges.len() > MAX_EDGES
            || self.fields.len() > MAX_NODES
            || self.tags.len() > MAX_NODES
            || self.comments.len() > MAX_NODES
        {
            return Err(invalid("canvas exceeds entity limits"));
        }
        for (id, field) in &self.fields {
            if *id != field.id {
                return Err(invalid("field map key does not match its id"));
            }
            text(&field.title, 4096)?;
        }
        for (id, tag) in &self.tags {
            if *id != tag.id {
                return Err(invalid("tag map key does not match its id"));
            }
            text(&tag.title, 4096)?;
        }
        for (id, comment) in &self.comments {
            if *id != comment.id {
                return Err(invalid("comment map key does not match its id"));
            }
            text(&comment.text, MAX_TEXT_BYTES)?;
            position(comment.pos)?;
        }
        for (id, item) in &self.nodes {
            if *id != item.id {
                return Err(invalid("node map key does not match its id"));
            }
            node(item)?;
            if item.field.is_some_and(|id| !self.fields.contains_key(&id)) || item.tag.is_some_and(|id| !self.tags.contains_key(&id)) {
                return Err(invalid("node references a missing field or tag"));
            }
        }
        let mut orders = BTreeSet::new();
        let mut indegree: BTreeMap<_, usize> = self.nodes.keys().map(|id| (*id, 0)).collect();
        let mut downstream: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for (id, edge) in &self.edges {
            if *id != edge.id {
                return Err(invalid("edge map key does not match its id"));
            }
            let source = self.nodes.get(&edge.from.node).ok_or_else(|| invalid("edge source node is missing"))?;
            let target = self.nodes.get(&edge.to.node).ok_or_else(|| invalid("edge target node is missing"))?;
            let output = source.outputs.iter().find(|p| p.name == edge.from.port).ok_or_else(|| invalid("edge output port is missing"))?;
            let input = target.inputs.iter().find(|p| p.name == edge.to.port).ok_or_else(|| invalid("edge input port is missing"))?;
            if output.port_type.modality != input.port_type.modality || (output.port_type.many && !input.port_type.many) {
                return Err(invalid("edge port types are incompatible"));
            }
            if (!input.port_type.many && edge.order != 0) || !orders.insert((edge.to.node, &edge.to.port, edge.order)) {
                return Err(invalid("edge input order is occupied or invalid for a single input"));
            }
            let count = indegree.get_mut(&edge.to.node).ok_or_else(|| invalid("edge target node is missing"))?;
            *count = count.saturating_add(1);
            downstream.entry(edge.from.node).or_default().push(edge.to.node);
        }
        // Kahn's algorithm avoids recursion even for adversarial long chains and cycles.
        let mut ready: VecDeque<_> = indegree.iter().filter_map(|(id, count)| (*count == 0).then_some(*id)).collect();
        let mut visited = 0usize;
        while let Some(id) = ready.pop_front() {
            visited = visited.saturating_add(1);
            if let Some(targets) = downstream.get(&id) {
                for target in targets {
                    let count = indegree.get_mut(target).ok_or_else(|| invalid("edge target node is missing"))?;
                    *count = count.checked_sub(1).ok_or_else(|| invalid("invalid edge count"))?;
                    if *count == 0 {
                        ready.push_back(*target);
                    }
                }
            }
        }
        if visited != self.nodes.len() {
            return Err(invalid("graph contains a cycle"));
        }
        Ok(())
    }
}
