//! DynamicCombo's public descriptor contract. Request inputs use dotted paths, not nested
//! dictionaries. Keep option branches compiled once; only the selected branch is materialized.
use super::{InputKind, InputSpec, dynamic, parse_input};
use crate::{InputValue, ordered::OValue};
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashSet;
const MAX_DEPTH: usize = 16;
const MAX_FIELDS: usize = 4096;
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Combo {
    pub selector: InputSpec,
    options: Vec<OptionFields>,
}
#[derive(Debug, Clone, PartialEq)]
struct OptionFields {
    key: String,
    inputs: Vec<InputSpec>,
    nested: Vec<Combo>,
    groups: Vec<dynamic::Autogrow>,
}
fn bounded(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
fn at(raw: &OValue, index: usize) -> Option<&OValue> {
    match raw {
        OValue::Array(values) => values.get(index),
        _ => None,
    }
}
impl Combo {
    pub(super) fn parse(name: &str, ordered: &OValue, raw: &Value, required: bool, count: &mut usize, depth: usize) -> Option<Self> {
        if depth > MAX_DEPTH || !bounded(name) {
            return None;
        }
        if raw.get(0)?.as_str()? != "COMFY_DYNAMICCOMBO_V3" || raw.get(1)?.get("forceInput").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        // Bound copies of nested metadata before compiling reusable branch descriptors.
        if depth == 0 {
            crate::json_bounds::Budget::default().value(raw, 0).ok()?;
        }
        let options = match at(ordered, 1)?.get("options")? {
            OValue::Array(options) => options,
            _ => return None,
        };
        if options.is_empty() || options.len() > 256 {
            return None;
        }
        let mut compiled = Vec::new();
        let mut seen = HashSet::new();
        let raw_options = raw.get(1)?.get("options")?.as_array()?;
        if raw_options.len() != options.len() {
            return None;
        }
        for (option, value) in options.iter().zip(raw_options) {
            let key = value.get("key")?.as_str()?;
            if !bounded(key) || !seen.insert(key.to_owned()) {
                return None;
            }
            let mut fields = OptionFields { key: key.into(), inputs: Vec::new(), nested: Vec::new(), groups: Vec::new() };
            let mut names = HashSet::new();
            let input_groups = option.get("inputs")?;
            input_groups.as_object()?;
            for (section, required) in [("required", true), ("optional", false)] {
                let entries = match input_groups.get(section) {
                    None => continue,
                    Some(raw) => raw.as_object()?,
                };
                for (leaf, spec) in entries {
                    if !bounded(leaf) || leaf.contains('.') || !names.insert(leaf.as_str()) {
                        return None;
                    }
                    *count = count.checked_add(1)?;
                    if *count > MAX_FIELDS {
                        return None;
                    }
                    let path = format!("{name}.{leaf}");
                    if !bounded(&path) {
                        return None;
                    }
                    let raw = value.get("inputs")?.get(section)?.get(leaf)?;
                    match raw.get(0).and_then(Value::as_str) {
                        Some("COMFY_DYNAMICCOMBO_V3") => {
                            let nested = Self::parse(&path, spec, raw, required, count, depth.saturating_add(1))?;
                            fields.inputs.push(nested.selector.clone());
                            fields.nested.push(nested);
                        }
                        Some("COMFY_AUTOGROW_V3") => fields.groups.push(dynamic::Autogrow::parse(&path, raw)?),
                        Some(kind) if kind.starts_with("COMFY_") && kind != "COMFY_MATCHTYPE_V3" => return None,
                        _ => fields.inputs.push(parse_input(&path, raw, required)?),
                    }
                }
            }
            compiled.push(fields);
        }
        let selector = InputSpec {
            name: name.into(),
            required,
            kind: InputKind::Combo { options: compiled.iter().map(|o| o.key.clone()).collect(), default: None, upload: None },
            control_after_generate: false,
            tooltip: None,
        };
        Some(Self { selector, options: compiled })
    }
    fn option<'a>(&'a self, values: &BTreeMap<String, InputValue>) -> Option<&'a OptionFields> {
        let Some(InputValue::Value(Value::String(key))) = values.get(&self.selector.name) else { return None };
        self.options.iter().find(|o| o.key == *key)
    }
    pub(super) fn children<'a>(&'a self, name: &str, choices: &std::collections::BTreeMap<String, String>) -> Result<Option<&'a [InputSpec]>, &'static str> {
        if self.selector.name == name {
            let key = choices.get(name).ok_or("a dynamic selector has no literal saved value")?;
            return self.options.iter().find(|o| o.key == *key).map(|o| Some(o.inputs.as_slice())).ok_or("a dynamic selector's saved option is unavailable");
        }
        let Some(option) = choices.get(&self.selector.name).and_then(|key| self.options.iter().find(|o| o.key == *key)) else { return Ok(None) };
        for nested in &option.nested {
            if let Some(fields) = nested.children(name, choices)? {
                return Ok(Some(fields));
            }
        }
        Ok(None)
    }
    pub(super) fn append<'a>(&'a self, values: &BTreeMap<String, InputValue>, out: &mut Vec<&'a InputSpec>) {
        let Some(option) = self.option(values) else { return };
        for spec in &option.inputs {
            out.push(spec);
            if let Some(nested) = option.nested.iter().find(|c| c.selector.name == spec.name) {
                nested.append(values, out);
            }
        }
    }
    pub(super) fn is_selector(&self, name: &str, values: &BTreeMap<String, InputValue>) -> bool {
        self.selector.name == name || self.option(values).is_some_and(|option| option.nested.iter().any(|nested| nested.is_selector(name, values)))
    }
    pub(super) fn dynamic_input<'a>(&'a self, name: &str, values: &BTreeMap<String, InputValue>) -> Result<Option<&'a InputSpec>, &'static str> {
        let Some(suffix) = name.strip_prefix(&self.selector.name).and_then(|s| s.strip_prefix('.')) else { return Ok(None) };
        if suffix.is_empty() {
            return Err("a dynamic input path is empty");
        }
        let Some(option) = self.option(values) else {
            return Err("a dynamic input has no selected option");
        };
        if let Some(spec) = option.inputs.iter().find(|s| s.name == name) {
            return Ok(Some(spec));
        }
        for nested in &option.nested {
            if let Some(spec) = nested.dynamic_input(name, values)? {
                return Ok(Some(spec));
            }
        }
        if let Some(spec) = dynamic::input(&option.groups, name)? {
            return Ok(Some(spec));
        }
        Err("this input is not declared by the selected dynamic option")
    }
}
#[cfg(test)]
mod tests;
