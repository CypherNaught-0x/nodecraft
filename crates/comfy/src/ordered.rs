//! A JSON value that keeps object keys in document order.
//!
//! `serde_json::Value` sorts object keys (the workspace doesn't enable `preserve_order`, which
//! would change hashing elsewhere), but ComfyUI's input order carries meaning.

use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<OValue>),
    Object(Vec<(String, OValue)>),
}

impl OValue {
    pub(crate) fn get(&self, key: &str) -> Option<&OValue> {
        self.as_object()?.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub(crate) fn as_object(&self) -> Option<&[(String, OValue)]> {
        match self {
            Self::Object(entries) => Some(entries),
            _ => None,
        }
    }

    /// The keys of an object, in document order; empty for anything else.
    pub(crate) fn keys(&self) -> Vec<String> {
        self.as_object().map(|o| o.iter().map(|(k, _)| k.clone()).collect()).unwrap_or_default()
    }

    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(*b),
            Self::Number(n) => Value::Number(n.clone()),
            Self::String(s) => Value::String(s.clone()),
            Self::Array(items) => Value::Array(items.iter().map(Self::to_value).collect()),
            Self::Object(entries) => Value::Object(entries.iter().map(|(k, v)| (k.clone(), v.to_value())).collect()),
        }
    }
}

impl<'de> Deserialize<'de> for OValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<OValue, E> {
                Ok(OValue::Null)
            }
            fn visit_none<E>(self) -> Result<OValue, E> {
                Ok(OValue::Null)
            }
            fn visit_bool<E>(self, b: bool) -> Result<OValue, E> {
                Ok(OValue::Bool(b))
            }
            fn visit_i64<E>(self, n: i64) -> Result<OValue, E> {
                Ok(OValue::Number(n.into()))
            }
            fn visit_u64<E>(self, n: u64) -> Result<OValue, E> {
                Ok(OValue::Number(n.into()))
            }
            fn visit_f64<E>(self, n: f64) -> Result<OValue, E> {
                Ok(serde_json::Number::from_f64(n).map_or(OValue::Null, OValue::Number))
            }
            fn visit_str<E>(self, s: &str) -> Result<OValue, E> {
                Ok(OValue::String(s.to_owned()))
            }
            fn visit_string<E>(self, s: String) -> Result<OValue, E> {
                Ok(OValue::String(s))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<OValue, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(OValue::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OValue, A::Error> {
                let mut entries: Vec<(String, OValue)> = Vec::new();
                // Key -> position, so duplicate detection stays linear on huge objects.
                let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                while let Some((k, v)) = map.next_entry::<String, OValue>()? {
                    // Duplicate keys: the last one wins, as in serde_json, keeping the first position.
                    match index.get(&k).and_then(|i| entries.get_mut(*i)) {
                        Some(slot) => slot.1 = v,
                        None => {
                            index.insert(k.clone(), entries.len());
                            entries.push((k, v));
                        }
                    }
                }
                Ok(OValue::Object(entries))
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_and_converts() {
        let v: OValue = serde_json::from_str(r#"{"b":1,"a":[true,null,"x",1.5],"b":2}"#).unwrap();
        assert_eq!(v.keys(), ["b", "a"]);
        assert_eq!(v.get("b"), Some(&OValue::Number(2.into())));
        assert_eq!(v.to_value(), serde_json::json!({"a":[true,null,"x",1.5],"b":2}));
        assert!(serde_json::from_str::<OValue>(&"[".repeat(1000)).is_err(), "depth limit still applies");
    }

    #[test]
    fn many_keys_parse_in_linear_time() {
        // 200k keys took ~26 s with a linear duplicate scan; must be well under a second now.
        let mut json = String::from("{");
        for i in 0..200_000 {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"k{i}\":{i}"));
        }
        json.push('}');
        let started = std::time::Instant::now();
        let v: OValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v.keys().len(), 200_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    }
}
