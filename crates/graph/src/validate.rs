use std::collections::{BTreeMap, BTreeSet, VecDeque};

use nodecraft_types::{Error, ErrorKind, Value};

use crate::{Canvas, Generation, Node, Position, Project};

pub const MAX_CHANGES: usize = 10_000;
pub const MAX_VALUE_DEPTH: usize = 32;
pub const MAX_VALUE_ITEMS: usize = 4096;
/// Most batches one fanned result's items combine.
pub const MAX_ITEM_AXES: usize = 16;
/// Parameters one recorded job may carry: the parameter schema's own limit
/// (`nodecraft_types::schema::MAX_PARAMS`).
const MAX_JOB_PARAMS: usize = 256;
/// Inputs one recorded job may carry.
const MAX_JOB_INPUTS: usize = 256;
pub const MAX_NODE_BYTES: usize = 8 * 1024 * 1024;
/// Most generations a node keeps.
pub const MAX_HISTORY: usize = 1024;
pub const MAX_CHANGESET_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PROJECT_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_CANVASES: usize = 64;
/// Most nodes on one canvas.
pub const MAX_NODES: usize = 10_000;
/// Most edges on one canvas.
pub const MAX_EDGES: usize = 50_000;
pub const MAX_COMMENT_BYTES: usize = 1024 * 1024;
pub const MAX_REPLY_BYTES: usize = 64 * 1024;
pub const MAX_COMMENT_REPLIES: usize = 256;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_PORTS: usize = 256;

#[cfg(test)]
thread_local! {
    pub(crate) static PROJECT_VALIDATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count JSON bytes without allocating a serialized copy. Call only after recursive values
/// have passed depth/item validation. The same cap applies to forward and inverse payloads.
pub(crate) fn byte_budget(value: &impl serde::Serialize, limit: usize) -> Result<(), Error> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.checked_sub(bytes.len()).ok_or_else(|| std::io::Error::other("document byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(limit), value).map_err(|_| invalid("serialized data exceeds byte limit"))
}

impl Generation {
    /// Whether this result could be recorded: its values within the document's limits and its
    /// size within a node's.
    pub fn validate(&self) -> Result<(), Error> {
        generation(self)
    }
}

/// Count a bounded encoded payload before retaining it in a generated inverse.
pub(crate) fn encoded_bytes(value: &impl serde::Serialize, limit: usize) -> Result<usize, Error> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes =
                self.bytes.checked_add(bytes.len()).filter(|total| *total <= self.limit).ok_or_else(|| std::io::Error::other("document byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| invalid("serialized data exceeds byte limit"))?;
    Ok(counter.bytes)
}

pub(crate) fn generation(generation: &Generation) -> Result<(), Error> {
    generation_values(generation)?;
    byte_budget(generation, MAX_NODE_BYTES)
}

fn generation_values(generation: &Generation) -> Result<(), Error> {
    if generation.outputs.len() > 256 {
        return Err(invalid("generation exceeds output limit"));
    }
    for output in &generation.outputs {
        value(output)?;
    }
    settings(generation)?;
    if generation.items.is_empty() {
        return match generation.incomplete {
            Some(_) => Err(invalid("only a fanned generation can be partial")),
            None => Ok(()),
        };
    }
    if generation.items.len() > MAX_VALUE_ITEMS {
        return Err(invalid("generation exceeds item limit"));
    }
    for tag in &generation.items {
        if tag.is_empty() || tag.len() > MAX_ITEM_AXES || !tag.windows(2).all(|pair| matches!(pair, [a, b] if a.batch < b.batch)) {
            return Err(invalid("an item tag must name each batch once, in order"));
        }
    }
    if generation.outputs.iter().any(|output| !matches!(output, Value::List(values) if values.len() == generation.items.len())) {
        return Err(invalid("a fanned generation needs one value per item on every output"));
    }
    if let Some(incomplete) = &generation.incomplete {
        let ascending = incomplete.missing.windows(2).all(|pair| matches!(pair, [a, b] if a.item < b.item));
        let in_range = incomplete.missing.last().is_some_and(|last| usize::try_from(last.item).is_ok_and(|item| item < generation.items.len()));
        if !ascending || !in_range || incomplete.missing.len() >= generation.items.len() {
            return Err(invalid("a partial generation names some, not all, of its items, once each, in order"));
        }
        for missing in &incomplete.missing {
            sent_keys(missing)?;
        }
    }
    Ok(())
}

/// A missing item's sent keys (#517): only on a failed item, one per job at most, in job order,
/// each a short visible-ASCII key of NodeCraft's own (`nodecraft:…`).
fn sent_keys(missing: &crate::MissingItem) -> Result<(), Error> {
    if missing.keys.is_empty() {
        return Ok(());
    }
    let ascending = missing.keys.windows(2).all(|pair| matches!(pair, [a, b] if a.job < b.job));
    let well_formed = missing
        .keys
        .iter()
        .all(|k| !k.key.is_empty() && k.key.len() <= crate::MAX_SENT_KEY && k.key.bytes().all(|b| b.is_ascii_graphic()) && k.key.starts_with("nodecraft:"));
    if missing.why != crate::Missing::Failed || missing.keys.len() > MAX_JOBS_PER_ITEM || !ascending || !well_formed {
        return Err(invalid("a missing item's sent keys are short ASCII keys of a failed item's jobs, once each, in order"));
    }
    Ok(())
}

/// More jobs than one item of a step can have.
const MAX_JOBS_PER_ITEM: usize = 256;

fn settings(generation: &Generation) -> Result<(), Error> {
    if generation.settings.len() > MAX_VALUE_ITEMS {
        return Err(invalid("generation exceeds job settings limit"));
    }
    for job in &generation.settings {
        text(&job.provider, 256)?;
        text(&job.model, 4096)?;
        if job.provider.is_empty() || job.model.is_empty() || job.params.len() > MAX_JOB_PARAMS {
            return Err(invalid("job settings need a provider, a model and a bounded parameter set"));
        }
        for (name, param) in &job.params {
            text(name, 256)?;
            value(param)?;
        }
        if job.inputs.len() > MAX_JOB_INPUTS {
            return Err(invalid("job settings exceed the input limit"));
        }
        for input in &job.inputs {
            text(&input.role, 256)?;
            value(&input.value)?;
        }
        if let Some(item) = job.item
            && usize::try_from(item).map_or(true, |item| item >= generation.items.len())
        {
            return Err(invalid("job settings name an item the result doesn't have"));
        }
    }
    Ok(())
}

pub(crate) fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

pub(crate) fn text(value: &str, limit: usize) -> Result<(), Error> {
    if value.len() > limit { Err(invalid("text exceeds the document limit")) } else { Ok(()) }
}
pub(crate) fn palette_color(value: &str) -> Result<(), Error> {
    text(value, 64)?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') {
        return Err(invalid("colour must be a nonempty lowercase palette identifier"));
    }
    Ok(())
}

pub(crate) fn comment(comment: &crate::Comment) -> Result<(), Error> {
    position(comment.pos)?;
    author(comment.author.as_deref())?;
    if comment.replies.len() > MAX_COMMENT_REPLIES {
        return Err(invalid("thread exceeds reply limit"));
    }
    let mut bytes = comment.text.len();
    let mut ids = BTreeSet::from([comment.id]);
    for reply in &comment.replies {
        text(&reply.text, MAX_REPLY_BYTES)?;
        author(reply.author.as_deref())?;
        bytes = bytes.saturating_add(reply.text.len());
        if !ids.insert(reply.id) {
            return Err(invalid("duplicate message id"));
        }
    }
    if bytes > MAX_COMMENT_BYTES {
        return Err(invalid("thread exceeds text limit"));
    }
    Ok(())
}

/// The longest author name a message keeps.
pub const MAX_AUTHOR_BYTES: usize = 128;

/// An author is a short, visible, single-line name.
pub fn validate_author(name: &str) -> Result<(), Error> {
    if name.trim().is_empty() || name.len() > MAX_AUTHOR_BYTES || name.chars().any(char::is_control) {
        Err(invalid("an author is 1 to 128 bytes on one line"))
    } else {
        Ok(())
    }
}

pub(crate) fn author(name: Option<&str>) -> Result<(), Error> {
    name.map_or(Ok(()), validate_author)
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
            Value::Media(_) | Value::Bool(_) | Value::Integer(_) | Value::Unsigned(_) | Value::Number(_) => {}
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
    if node.params.len() > 256 || node.config.len() > 256 || node.history.len() > MAX_HISTORY {
        return Err(invalid("node exceeds parameter or history limits"));
    }
    for (name, param) in node.params.iter().chain(node.config.iter()) {
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
        // The enclosing node byte budget includes every history entry; do not serialize
        // each entry again just to enforce a redundant per-generation byte budget.
        generation_values(generation)?;
    }
    if node.active.is_some_and(|id| !generations.contains(&id)) {
        return Err(invalid("active generation is missing from history"));
    }
    byte_budget(node, MAX_NODE_BYTES)
}

impl Project {
    pub fn validate(&self) -> Result<(), Error> {
        #[cfg(test)]
        PROJECT_VALIDATIONS.with(|count| count.set(count.get().saturating_add(1)));
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
        let mut versions = BTreeSet::new();
        for (id, canvas) in &self.canvases {
            if *id != canvas.id {
                return Err(invalid("canvas map key does not match its id"));
            }
            canvas.validate()?;
            if let Some(technique) = &canvas.technique
                && !versions.insert((technique.id, technique.version))
            {
                return Err(invalid("duplicate technique version"));
            }
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
            for root in canvas.comments.values() {
                for id in std::iter::once(root.id).chain(root.replies.iter().map(|reply| reply.id)) {
                    if !comments.insert(id) {
                        return Err(invalid("duplicate message id across project"));
                    }
                }
            }
            if nodes.len() > MAX_NODES || edges.len() > MAX_EDGES || fields.len() > MAX_NODES || tags.len() > MAX_NODES || comments.len() > MAX_NODES {
                return Err(invalid("project exceeds entity limits"));
            }
        }
        crate::skill::validate_skills(&self.skills)?;
        byte_budget(self, MAX_PROJECT_BYTES)
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
        match (&self.technique, self.kind) {
            (Some(technique), crate::CanvasKind::Technique) => technique.validate(self)?,
            (None, crate::CanvasKind::Technique) | (Some(_), _) => return Err(invalid("only a technique canvas describes a technique")),
            (None, _) => {}
        }
        for (id, field) in &self.fields {
            if *id != field.id {
                return Err(invalid("field map key does not match its id"));
            }
            field.validate()?;
        }
        for (id, tag) in &self.tags {
            if *id != tag.id {
                return Err(invalid("tag map key does not match its id"));
            }
            tag.validate()?;
        }
        let mut message_count = 0usize;
        for (id, comment) in &self.comments {
            message_count = message_count.saturating_add(1).saturating_add(comment.replies.len());
            if message_count > MAX_NODES {
                return Err(invalid("canvas exceeds message limit"));
            }
            if *id != comment.id {
                return Err(invalid("comment map key does not match its id"));
            }
            self::comment(comment)?;
            comment.world_position(self)?;
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
