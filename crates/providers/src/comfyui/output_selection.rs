//! Saved workflow result ordering. Legacy handles keep their existing numeric-node order.
use crate::{ErrorKind, ProviderError, ProviderJob};
use serde_json::{Value, json};
use std::collections::BTreeSet;
const KEY: &str = "comfy_output_nodes_v1";
const MAX_NODES: usize = 64;
fn invalid() -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, "the saved workflow outputs are malformed or exceed their limits")
}
fn checked(value: &Value) -> Result<Vec<&str>, ProviderError> {
    let values = value.as_array().filter(|v| !v.is_empty() && v.len() <= MAX_NODES).ok_or_else(invalid)?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        let text = value.as_str().filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)).ok_or_else(invalid)?;
        if !seen.insert(text) {
            return Err(invalid());
        }
        out.push(text);
    }
    Ok(out)
}
pub(crate) fn save(mut data: Value, job: &ProviderJob) -> Result<Value, ProviderError> {
    data = super::template::mesh::save_policy(data, job)?;
    let Some(text) = super::template::card_text(job, super::template::CARD_OUTPUTS) else { return Ok(data) };
    if text.len() > 64 * 1024 {
        return Err(invalid());
    }
    let value: Value = serde_json::from_str(text).map_err(|_| invalid())?;
    checked(&value)?;
    if data.is_null() {
        data = json!({});
    }
    let object = data.as_object_mut().ok_or_else(invalid)?;
    object.insert(KEY.into(), value);
    Ok(data)
}
pub(crate) fn read(data: &Value) -> Result<Option<Vec<&str>>, ProviderError> {
    super::template::mesh::policy(data)?;
    data.get(KEY).map(checked).transpose()
}
/// Unknown legacy/cloud outputs without node ids stay at the end, rather than losing a paid
/// result. Named unselected outputs are excluded, including internal saver dependencies.
pub(crate) fn arrange<T>(items: &mut Vec<T>, order: Option<&[&str]>, node: impl Fn(&T) -> &str) {
    let Some(order) = order else { return };
    items.retain(|item| node(item).is_empty() || order.contains(&node(item)));
    items.sort_by_key(|item| order.iter().position(|id| *id == node(item)).unwrap_or(order.len()));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mapping947_saved_order_filters_unselected_nodes_and_preserves_legacy_unknowns() {
        let mut ids = vec!["2".to_owned(), "3".into(), "4".into(), "".into()];
        arrange(&mut ids, Some(&["3", "2"]), String::as_str);
        assert_eq!(ids, ["3", "2", ""]);
        let mut ids = vec!["2".to_owned(), "3".into()];
        arrange(&mut ids, None, String::as_str);
        assert_eq!(ids, ["2", "3"]);
        for value in [json!([]), json!(["x", "x"]), json!([3]), json!(["x".repeat(257)]), json!(["bad\nnode"]), json!(vec!["x"; 65])] {
            assert!(checked(&value).is_err());
        }
        assert!(read(&Value::Null).unwrap().is_none());
    }
}
