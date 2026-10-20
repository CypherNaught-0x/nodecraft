use serde::{Deserialize, Serialize};

use crate::{AssetRef, Integer, Number, Unsigned};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    Image,
    Video,
    Audio,
    Mesh,
    Document,
}

impl Modality {
    /// Whether an asset of this MIME essence (`image/png`) carries this modality.
    pub fn accepts_mime(self, essence: &str) -> bool {
        match self {
            Self::Text => essence.starts_with("text/"),
            Self::Image => essence.starts_with("image/"),
            Self::Video => essence.starts_with("video/"),
            Self::Audio => essence.starts_with("audio/"),
            Self::Mesh => essence.starts_with("model/"),
            Self::Document => essence == "application/pdf",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortType {
    pub modality: Modality,
    /// Whether this port accepts a list rather than one value.
    #[serde(default)]
    pub many: bool,
}

/// A wire value. Decoders must keep their recursion and input-size limits enabled;
/// consumers must likewise bound traversal of lists constructed programmatically.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Bool(bool),
    Integer(Integer),
    Unsigned(Unsigned),
    Number(Number),
    Text(String),
    Media(AssetRef),
    List(Vec<Value>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Blake3, Error, ErrorKind, MediaMeta};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn nested_values_round_trip(value in any::<String>().prop_map(Value::Text).prop_recursive(5, 128, 8, |inner| {
            proptest::collection::vec(inner, 0..8).prop_map(Value::List)
        })) {
            prop_assert_eq!(serde_json::from_str::<Value>(&serde_json::to_string(&value).unwrap()).unwrap(), value);
        }
    }

    #[test]
    fn modalities_ports_media_and_errors_round_trip() {
        for modality in [Modality::Text, Modality::Image, Modality::Video, Modality::Audio, Modality::Mesh, Modality::Document] {
            for many in [false, true] {
                let port = PortType { modality, many };
                assert_eq!(serde_json::from_str::<PortType>(&serde_json::to_string(&port).unwrap()).unwrap(), port);
            }
        }
        let value = Value::Media(AssetRef { hash: Blake3::from_bytes([1; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
        assert_eq!(serde_json::from_str::<Value>(&serde_json::to_string(&value).unwrap()).unwrap(), value);
        let error = Error::new(ErrorKind::Unsupported, "operation is not supported");
        assert_eq!(serde_json::from_str::<Error>(&serde_json::to_string(&error).unwrap()).unwrap(), error);
        assert_eq!(error.to_string(), "operation is not supported");
    }

    #[test]
    fn hostile_values_return_deserialization_errors() {
        let deep = format!("{}{{\"type\":\"text\",\"value\":\"leaf\"}}{}", "{\"type\":\"list\",\"value\":[".repeat(256), "]}".repeat(256));
        assert!(serde_json::from_str::<Value>(&deep).is_err());
        for json in ["null", "{}", "{\"type\":\"unknown\"}", "{\"type\":\"list\",\"value\":42}"] {
            assert!(serde_json::from_str::<Value>(json).is_err());
        }
        assert!(serde_json::from_str::<Modality>(r#""future_modality""#).is_err());
        assert_eq!(serde_json::from_str::<PortType>(r#"{"modality":"text"}"#).unwrap(), PortType { modality: Modality::Text, many: false });
    }
}
