//! Connection-only V3 Autogrow groups from public node descriptors. Keep the template once,
//! rather than allocating one InputSpec per possible numbered socket (which can be unbounded).
use super::{InputKind, InputSpec};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Autogrow {
    name: String,
    names: Vec<String>,
    prefix: Option<String>,
    maximum: Option<u64>,
    spec: InputSpec,
}
fn bounded(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}
impl Autogrow {
    pub(super) fn parse(name: &str, raw: &Value) -> Option<Self> {
        if raw.get(0)?.as_str()? != "COMFY_AUTOGROW_V3" || !bounded(name) {
            return None;
        }
        let template = raw.get(1)?.get("template")?;
        let required = template.get("input")?.get("required")?.as_object()?;
        if required.len() != 1 || template.get("input")?.get("optional").and_then(Value::as_object).is_some_and(|o| !o.is_empty()) {
            return None;
        }
        let (leaf, spec) = required.iter().next()?;
        let kind = spec.get(0)?.as_str()?;
        if !bounded(leaf) || !bounded(kind) || kind.starts_with("COMFY_") && kind != "COMFY_MATCHTYPE_V3" {
            return None;
        }
        // Positional widgets inside dynamic groups need their own protocol conversion. Only
        // connection-only templates are supported here, including explicitly forced inputs.
        if matches!(kind, "INT" | "FLOAT" | "STRING" | "BOOLEAN" | "COMBO")
            && spec.get(1).and_then(|o| o.get("forceInput")).and_then(Value::as_bool) != Some(true)
        {
            return None;
        }
        let names = match template.get("names") {
            Some(Value::Array(names)) if !names.is_empty() && names.len() <= 256 => {
                let mut seen = std::collections::HashSet::new();
                let mut out = Vec::with_capacity(names.len());
                for name in names {
                    let name = name.as_str().filter(|n| bounded(n))?;
                    if !seen.insert(name) {
                        return None;
                    }
                    out.push(name.to_owned());
                }
                out
            }
            None => Vec::new(),
            _ => return None,
        };
        let prefix = match template.get("prefix") {
            None => None,
            Some(p) => Some(p.as_str().filter(|p| bounded(p))?.to_owned()),
        };
        if names.is_empty() == prefix.is_none() {
            return None;
        }
        let maximum = match template.get("max") {
            None | Some(Value::Null) => None,
            Some(n) => Some(n.as_u64()?),
        };
        Some(Self {
            name: name.to_owned(),
            names,
            prefix,
            maximum,
            spec: InputSpec {
                name: leaf.to_owned(),
                required: false,
                kind: InputKind::Link(if kind == "COMFY_MATCHTYPE_V3" { "*" } else { kind }.to_owned()),
                control_after_generate: false,
                tooltip: None,
            },
        })
    }
    fn suffix<'a>(&self, name: &'a str) -> Option<&'a str> {
        if name == self.name {
            return Some("");
        }
        name.strip_prefix(&self.name)?.strip_prefix('.')
    }
    fn accepts(&self, suffix: &str) -> bool {
        if let Some(prefix) = &self.prefix {
            let Some(digits) = suffix.strip_prefix(prefix) else { return false };
            if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|b| b.is_ascii_digit()) || digits.len() > 1 && digits.starts_with('0') {
                return false;
            }
            let Some(index) = digits.parse::<u64>().ok() else { return false };
            return self.maximum.is_none_or(|maximum| index < maximum);
        }
        self.names
            .iter()
            .position(|name| name == suffix)
            .is_some_and(|index| self.maximum.is_none_or(|maximum| u64::try_from(index).is_ok_and(|index| index < maximum)))
    }
}

pub(super) fn input<'a>(groups: &'a [Autogrow], name: &str) -> Result<Option<&'a InputSpec>, &'static str> {
    let Some((group, suffix)) = groups.iter().filter_map(|g| g.suffix(name).map(|suffix| (g, suffix))).max_by_key(|(g, _)| g.name.len()) else {
        return Ok(None);
    };
    if !group.accepts(suffix) {
        return Err("this dynamic input is not declared by the server");
    }
    Ok(Some(&group.spec))
}

#[cfg(test)]
mod tests;
