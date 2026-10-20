//! Option lists known only at run time (#162): the checkpoints a ComfyUI server has installed,
//! the voices an account owns. A rule names the list; the host supplies what it has fetched.

use crate::{Choice, Error, ErrorKind, schema::invalid};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The most values one list may hold.
pub const MAX_OPTIONS: usize = 16_384;
/// The most lists one [`OptionLists`] keeps.
pub const MAX_OPTION_LISTS: usize = 256;
/// The most text (values, labels and keys) one [`OptionLists`] keeps.
pub const MAX_OPTION_BYTES: usize = 16 * 1024 * 1024;
const MAX_OPTION_TEXT: usize = 4096;

/// Where a parameter's allowed values come from when they are only known at run time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionSource {
    /// Dotted lowercase id of the list (`comfyui.checkpoints`).
    pub id: String,
    /// An earlier parameter whose value narrows the list (a checkpoint's LoRAs). Absent: the
    /// list depends on the contract's namespace alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// One fetched list: a source within an owner, narrowed by the scope parameter's value when the
/// source declares one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OptionKey<'a> {
    /// The contract's namespace.
    pub owner: &'a str,
    pub source: &'a str,
    pub scope: Option<&'a str>,
}

/// What a host has fetched. A list it has not fetched, or could not, is unknown, never empty.
pub trait OptionLookup {
    /// Whether the host has the list.
    fn known(&self, key: OptionKey<'_>) -> bool;
    /// Whether `value` is on the list, or `None` while the list is unknown.
    fn contains(&self, key: OptionKey<'_>, value: &str) -> Option<bool>;
}

/// Knows no list, so nothing is refused for its options.
pub struct NoOptions;

impl OptionLookup for NoOptions {
    fn known(&self, _: OptionKey<'_>) -> bool {
        false
    }
    fn contains(&self, _: OptionKey<'_>, _: &str) -> Option<bool> {
        None
    }
}

type OwnedKey = (String, String, Option<String>);

struct List {
    choices: Vec<Choice>,
    values: BTreeSet<String>,
    bytes: usize,
}

/// A bounded set of fetched lists, which a host installs as a session resource. Setting a
/// list replaces it; clearing it makes it unknown again.
#[derive(Default)]
pub struct OptionLists {
    lists: BTreeMap<OwnedKey, List>,
    bytes: usize,
}

impl OptionLists {
    /// Record a fetched list in the host's order. Refuses duplicate or empty values, blank
    /// labels and lists past the size limits, keeping the previous list.
    pub fn set(&mut self, key: OptionKey<'_>, choices: Vec<Choice>) -> Result<(), Error> {
        if !super::namespace(key.source) || key.owner.is_empty() || key.owner.len() > 128 || key.scope.is_some_and(|scope| scope.len() > MAX_OPTION_TEXT) {
            return Err(invalid("an option list needs a dotted source id, an owner and a bounded scope"));
        }
        if choices.len() > MAX_OPTIONS {
            return Err(invalid("an option list exceeds 16,384 values"));
        }
        let mut bytes = key.owner.len().saturating_add(key.source.len()).saturating_add(key.scope.map_or(0, str::len));
        let mut values = BTreeSet::new();
        for choice in &choices {
            if choice.value.is_empty()
                || choice.value.len() > MAX_OPTION_TEXT
                || choice.label.trim().is_empty()
                || choice.label.len() > MAX_OPTION_TEXT
                || values.contains(choice.value.as_str())
            {
                return Err(invalid("option values must be unique and bounded, with nonempty labels"));
            }
            bytes = bytes.saturating_add(choice.value.len().saturating_mul(2)).saturating_add(choice.label.len());
            if bytes > MAX_OPTION_BYTES {
                return Err(Error::new(ErrorKind::InvalidInput, "too many fetched option lists; clear some first"));
            }
            values.insert(choice.value.clone());
        }
        let owned = owned(key);
        let replaced = self.lists.get(&owned).map_or(0, |list| list.bytes);
        let total = self.bytes.saturating_sub(replaced).saturating_add(bytes);
        if total > MAX_OPTION_BYTES || (!self.lists.contains_key(&owned) && self.lists.len() >= MAX_OPTION_LISTS) {
            return Err(Error::new(ErrorKind::InvalidInput, "too many fetched option lists; clear some first"));
        }
        self.bytes = total;
        self.lists.insert(owned, List { choices, values, bytes });
        Ok(())
    }

    /// Forget a list, so its values are unknown again.
    pub fn clear(&mut self, key: OptionKey<'_>) {
        if let Some(list) = self.lists.remove(&owned(key)) {
            self.bytes = self.bytes.saturating_sub(list.bytes);
        }
    }

    /// A fetched list in the host's order, or `None` while it is unknown.
    pub fn get(&self, key: OptionKey<'_>) -> Option<&[Choice]> {
        self.lists.get(&owned(key)).map(|list| list.choices.as_slice())
    }

    pub fn len(&self) -> usize {
        self.lists.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lists.is_empty()
    }
}

impl OptionLookup for OptionLists {
    fn known(&self, key: OptionKey<'_>) -> bool {
        self.lists.contains_key(&owned(key))
    }
    fn contains(&self, key: OptionKey<'_>, value: &str) -> Option<bool> {
        self.lists.get(&owned(key)).map(|list| list.values.contains(value))
    }
}

fn owned(key: OptionKey<'_>) -> OwnedKey {
    (key.owner.to_owned(), key.source.to_owned(), key.scope.map(str::to_owned))
}
