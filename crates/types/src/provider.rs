//! Pure endpoint identities. No server address, credential or provider implementation is owned here.
use crate::{Error, ErrorKind, NodeId, ProviderInstanceId};
use serde::{Deserialize, Serialize};
use std::mem::size_of;

pub const MAX_ADAPTER_BYTES: usize = 512;
const MAX_SELECTIONS: usize = 100_000;
const MAX_SELECTION_BYTES: usize = 64 * 1024 * 1024;
fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, "invalid provider target or selection")
}
fn capacity() -> Error {
    Error::new(ErrorKind::Budget, "provider selection exceeds its ownership limit")
}
/// Adapter bytes plus an explicit None/Some instance namespace. Display strings are not keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "TargetWire", into = "TargetWire")]
pub struct ProviderTarget {
    adapter: String,
    instance: Option<ProviderInstanceId>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetWire {
    adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instance: Option<ProviderInstanceId>,
}
impl TryFrom<TargetWire> for ProviderTarget {
    type Error = Error;
    fn try_from(w: TargetWire) -> Result<Self, Error> {
        Self::new(&w.adapter, w.instance)
    }
}
impl From<ProviderTarget> for TargetWire {
    fn from(t: ProviderTarget) -> Self {
        Self { adapter: t.adapter, instance: t.instance }
    }
}
impl ProviderTarget {
    pub fn new(adapter: &str, instance: Option<ProviderInstanceId>) -> Result<Self, Error> {
        if adapter.len() > MAX_ADAPTER_BYTES {
            return Err(invalid());
        }
        let mut text = String::new();
        text.try_reserve_exact(adapter.len()).map_err(|_| capacity())?;
        if text.capacity() > MAX_ADAPTER_BYTES {
            return Err(capacity());
        }
        text.push_str(adapter);
        Ok(Self { adapter: text, instance })
    }
    pub fn default_instance(adapter: &str) -> Result<Self, Error> {
        Self::new(adapter, None)
    }
    pub fn named(adapter: &str, instance: ProviderInstanceId) -> Result<Self, Error> {
        Self::new(adapter, Some(instance))
    }
    pub fn adapter(&self) -> &str {
        &self.adapter
    }
    pub fn instance(&self) -> Option<ProviderInstanceId> {
        self.instance
    }
    pub fn logical_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.adapter.len())
    }
    pub fn try_clone(&self) -> Result<Self, Error> {
        Self::new(&self.adapter, self.instance)
    }
    /// Canonical lane for ALL provider targets, including legacy/default adapters.
    /// Execution/accounting must retain the tuple, never decode this lane into authority.
    pub fn lane_name(&self) -> Result<String, Error> {
        let n = self.adapter.len().checked_mul(2).and_then(|n| n.checked_add(if self.instance.is_some() { 37 } else { 4 })).ok_or_else(capacity)?;
        let mut out = String::new();
        out.try_reserve_exact(n).map_err(|_| capacity())?;
        out.push_str("p:");
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in self.adapter.bytes() {
            out.push(char::from(*HEX.get(usize::from(byte >> 4)).ok_or_else(invalid)?));
            out.push(char::from(*HEX.get(usize::from(byte & 15)).ok_or_else(invalid)?));
        }
        if let Some(instance) = self.instance {
            out.push_str(":i:");
            for byte in instance.as_bytes() {
                out.push(char::from(*HEX.get(usize::from(byte >> 4)).ok_or_else(invalid)?));
                out.push(char::from(*HEX.get(usize::from(byte & 15)).ok_or_else(invalid)?));
            }
        } else {
            out.push_str(":d");
        }
        Ok(out)
    }
}
/// One owned selection record; target is read-only after bounded construction.
#[derive(Clone, Debug)]
pub struct TargetChoice {
    node: NodeId,
    target: ProviderTarget,
}
impl TargetChoice {
    pub fn node(&self) -> NodeId {
        self.node
    }
    pub fn target(&self) -> &ProviderTarget {
        &self.target
    }
}
/// Sorted pure view available BEFORE the first eager planner callback.
#[derive(Clone, Debug)]
pub struct TargetSelection {
    choices: Vec<TargetChoice>,
    bytes: usize,
}
pub static EMPTY_TARGET_SELECTION: TargetSelection = TargetSelection::empty();
impl Default for TargetSelection {
    fn default() -> Self {
        Self::empty()
    }
}
impl TargetSelection {
    pub const fn empty() -> Self {
        Self { choices: Vec::new(), bytes: size_of::<Self>() }
    }
    pub fn logical_bytes(&self) -> usize {
        self.bytes
    }
    pub fn len(&self) -> usize {
        self.choices.len()
    }
    pub fn is_empty(&self) -> bool {
        self.choices.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &TargetChoice> {
        self.choices.iter()
    }
    pub fn get(&self, node: NodeId, adapter: &str) -> Option<&ProviderTarget> {
        let i = self.choices.binary_search_by(|c| c.node.cmp(&node).then_with(|| c.target.adapter().cmp(adapter))).ok()?;
        self.choices.get(i).map(|c| &c.target)
    }
    pub fn insert(&mut self, node: NodeId, target: &ProviderTarget) -> Result<(), Error> {
        let at = self.choices.binary_search_by(|c| c.node.cmp(&node).then_with(|| c.target.adapter().cmp(target.adapter()))).err().ok_or_else(invalid)?;
        let n = self.bytes.checked_add(size_of::<TargetChoice>()).and_then(|n| n.checked_add(target.adapter().len())).ok_or_else(capacity)?;
        if self.choices.len() >= MAX_SELECTIONS || n > MAX_SELECTION_BYTES {
            return Err(capacity());
        }
        let copy = target.try_clone()?;
        self.choices.try_reserve_exact(1).map_err(|_| capacity())?;
        if self.choices.capacity() > MAX_SELECTIONS {
            return Err(capacity());
        }
        self.choices.insert(at, TargetChoice { node, target: copy });
        self.bytes = n;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_is_not_a_provider_string_namespace() {
        let named = ProviderTarget::named("local", ProviderInstanceId::from_bytes([0; 16])).unwrap();
        let default = ProviderTarget::default_instance(&named.lane_name().unwrap()).unwrap();
        assert_ne!(named, default);
        assert_ne!(named.lane_name().unwrap(), default.lane_name().unwrap());
        assert_ne!(ProviderTarget::default_instance("local").unwrap().lane_name().unwrap(), "local");
        assert_ne!(named, ProviderTarget::default_instance("local").unwrap());
    }
    #[test]
    fn bounds_compaction_and_serialization_preserve_arbitrary_legacy_bytes() {
        for name in ["", "local", "é:/:d"] {
            let t = ProviderTarget::default_instance(name).unwrap();
            assert_eq!(serde_json::from_str::<ProviderTarget>(&serde_json::to_string(&t).unwrap()).unwrap(), t);
        }
        let mut spare = String::with_capacity(4096);
        spare.push('x');
        let t = ProviderTarget::default_instance(&spare).unwrap();
        assert!(t.adapter.capacity() <= 512);
        assert!(ProviderTarget::default_instance(&"x".repeat(513)).is_err());
        assert!(ProviderTarget::default_instance(&"x".repeat(512)).unwrap().lane_name().unwrap().len() < 2048);
        let mut selections = TargetSelection::empty();
        let n = NodeId::from_bytes([1; 16]);
        selections.insert(n, &t).unwrap();
        assert_eq!(selections.get(n, "x"), Some(&t));
        assert!(selections.insert(n, &t).is_err());
    }
}
