//! Export-only leaf indices. Generic extraction/copy retain their existing list contract.
use crate::commands::Context;
use nodecraft_types::{CanvasId, Error, ErrorKind, GenerationId, NodeId, Value};
/// Work is shared across an entire export enumeration, including empty list containers.
pub const MAX_VISITED: usize = 65_536;
pub const MAX_DEPTH: usize = 32;
#[derive(Default)]
pub struct Budget {
    visited: usize,
}
impl Budget {
    pub fn charge(&mut self) -> Result<(), Error> {
        self.visited = self.visited.checked_add(1).filter(|n| *n <= MAX_VISITED).ok_or_else(limit)?;
        Ok(())
    }
}
fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn limit() -> Error {
    invalid("The result collection exceeds export limits; choose fewer results or a smaller collection")
}
/// Every non-list value consumes an index, even when its type cannot be exported as media.
#[derive(Clone, Copy, Debug)]
pub struct Leaf<'a> {
    pub value: &'a Value,
    pub item: Option<usize>,
    pub fan: Option<usize>,
}
struct Frame<'a> {
    values: &'a [Value],
    next: usize,
    fan: Option<usize>,
    top: bool,
}
pub struct Leaves<'a, 'b> {
    pending: Option<&'a Value>,
    frames: Vec<Frame<'a>>,
    budget: &'b mut Budget,
    index: usize,
    list: bool,
    fanned: bool,
    done: bool,
}
/// Iterative preorder traversal uses O(depth) storage, never one allocation per child.
pub fn iter<'a, 'b>(value: &'a Value, fanned: bool, budget: &'b mut Budget) -> Leaves<'a, 'b> {
    Leaves { pending: Some(value), frames: Vec::new(), budget, index: 0, list: matches!(value, Value::List(_)), fanned, done: false }
}
impl<'a> Iterator for Leaves<'a, '_> {
    type Item = Result<Leaf<'a>, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            let (value, fan, root) = if let Some(value) = self.pending.take() {
                (value, None, true)
            } else {
                let frame = self.frames.last_mut()?;
                let Some(value) = frame.values.get(frame.next) else {
                    self.frames.pop();
                    continue;
                };
                let fan = if frame.top && self.fanned { Some(frame.next) } else { frame.fan };
                frame.next = match frame.next.checked_add(1) {
                    Some(next) => next,
                    None => {
                        self.done = true;
                        return Some(Err(limit()));
                    }
                };
                (value, fan, false)
            };
            if let Err(error) = self.budget.charge() {
                self.done = true;
                return Some(Err(error));
            }
            if let Value::List(values) = value {
                if self.frames.len() >= MAX_DEPTH {
                    self.done = true;
                    return Some(Err(limit()));
                }
                if self.frames.try_reserve(1).is_err() {
                    self.done = true;
                    return Some(Err(limit()));
                }
                self.frames.push(Frame { values, next: 0, fan, top: root });
                continue;
            }
            let item = self.list.then_some(self.index);
            self.index = match self.index.checked_add(1) {
                Some(index) => index,
                None => {
                    self.done = true;
                    return Some(Err(limit()));
                }
            };
            return Some(Ok(Leaf { value, item, fan }));
        }
    }
}
/// Look up the exact export index, without changing generic history extract/copy semantics.
pub fn lookup<'a>(value: &'a Value, fanned: bool, item: Option<usize>, budget: &mut Budget) -> Result<Leaf<'a>, Error> {
    match (value, item) {
        (Value::List(_), None) => return Err(invalid("that output is a collection: choose an export item")),
        (_, Some(_)) if !matches!(value, Value::List(_)) => return Err(invalid("that output is a single value, not a collection")),
        _ => {}
    }
    if item.is_some_and(|i| i >= MAX_VISITED) {
        return Err(limit());
    }
    for leaf in iter(value, fanned, budget) {
        let leaf = leaf?;
        if leaf.item == item {
            return Ok(leaf);
        }
    }
    Err(Error::new(ErrorKind::NotFound, "the collection has no such export item"))
}
pub(crate) fn result_value<'a>(
    c: &'a Context<'_>,
    canvas: CanvasId,
    id: NodeId,
    generation: GenerationId,
    port: Option<&str>,
    item: Option<usize>,
    budget: &mut Budget,
) -> Result<Leaf<'a>, Error> {
    let source = c
        .document
        .canvases()
        .get(&canvas)
        .and_then(|canvas| canvas.nodes().get(&id))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "the result node is unavailable"))?;
    let generation = source.history.iter().find(|g| g.id == generation).ok_or_else(|| Error::new(ErrorKind::NotFound, "the node has no such result"))?;
    if generation.outputs.len() != source.outputs.len() {
        return Err(Error::new(ErrorKind::Conflict, "this result was made for different outputs than the node has now"));
    }
    let output = match port {
        Some(name) => source.outputs.iter().position(|p| p.name == name).ok_or_else(|| Error::new(ErrorKind::NotFound, "the node has no such output"))?,
        None => 0,
    };
    let value = generation.outputs.get(output).ok_or_else(|| Error::new(ErrorKind::NotFound, "the result has no value for that output"))?;
    lookup(value, !generation.items.is_empty(), item, budget)
}
#[cfg(test)]
mod tests;
