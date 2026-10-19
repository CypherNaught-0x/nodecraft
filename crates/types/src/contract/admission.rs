//! Borrowed admission before migration reports or override maps copy caller-owned values.
use crate::{
    Error, ParamValues, Value,
    schema::{MAX_PARAMS, MAX_TEXT, invalid},
};
use std::collections::BTreeSet;

// Match graph payload traversal bounds without making this L0 crate depend on graph.
const MAX_DEPTH: usize = 32;
const MAX_ITEMS: usize = 4096;
const MAX_NAME: usize = 128;

pub(super) fn values<'a>(maps: impl IntoIterator<Item = &'a ParamValues>) -> Result<(), Error> {
    let mut names = BTreeSet::new();
    let mut name_bytes = 0usize;
    let mut text_bytes = 0usize;
    let mut seen = 0usize;
    let mut pending = Vec::new();
    for map in maps {
        if map.len() > MAX_PARAMS {
            return Err(invalid("parameter values exceed count limit"));
        }
        for (name, value) in map {
            // Unknown keys are legitimate migration reports. Bound their bytes, without
            // imposing the known-schema identifier grammar or echoing their contents.
            if name.len() > MAX_NAME {
                return Err(invalid("parameter name exceeds byte limit"));
            }
            name_bytes = name_bytes.checked_add(name.len()).filter(|n| *n <= MAX_TEXT).ok_or_else(|| invalid("parameter names exceed byte limit"))?;
            if !names.contains(name.as_str()) && names.len() >= MAX_PARAMS {
                return Err(invalid("combined parameter values exceed count limit"));
            }
            names.insert(name.as_str());
            pending.push((value, 0usize));
            while let Some((value, depth)) = pending.pop() {
                seen = seen.checked_add(1).filter(|n| *n <= MAX_ITEMS).ok_or_else(|| invalid("parameter values exceed item limit"))?;
                if depth > MAX_DEPTH {
                    return Err(invalid("parameter value exceeds depth limit"));
                }
                let bytes = match value {
                    Value::Text(text) => text.len(),
                    Value::Media(asset) => asset.mime.as_str().len(),
                    Value::List(items) => {
                        // Bound the entire pending frontier BEFORE extending its scratch.
                        if seen.saturating_add(pending.len()).saturating_add(items.len()) > MAX_ITEMS {
                            return Err(invalid("parameter values exceed item limit"));
                        }
                        pending.extend(items.iter().map(|item| (item, depth.saturating_add(1))));
                        0
                    }
                    _ => 0,
                };
                text_bytes = text_bytes.checked_add(bytes).filter(|n| *n <= MAX_TEXT).ok_or_else(|| invalid("parameter values exceed text limit"))?;
            }
        }
    }
    Ok(())
}
