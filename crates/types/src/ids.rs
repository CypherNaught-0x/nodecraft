use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, ErrorKind};

// Randomness belongs to the platform, not this pure-data crate. Callers supply UUID bytes;
// the explicit constructor also makes fixtures deterministic on native and wasm targets.
macro_rules! ids {
    ($($name:ident),+ $(,)?) => { $(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub const fn from_bytes(bytes: [u8; 16]) -> Self { Self(Uuid::from_bytes(bytes)) }
            pub const fn as_bytes(&self) -> &[u8; 16] { self.0.as_bytes() }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = Error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self).map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid UUID"))
            }
        }
    )+ };
}

ids!(ProjectId, CanvasId, NodeId, EdgeId, FieldId, CommentId, TagId, ElementId, GenerationId, TechniqueId, ProviderInstanceId, ItemId);

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn ids_round_trip(bytes in any::<[u8; 16]>()) {
            let id = NodeId::from_bytes(bytes);
            prop_assert_eq!(id.to_string().parse::<NodeId>().unwrap(), id);
            prop_assert_eq!(serde_json::from_str::<NodeId>(&serde_json::to_string(&id).unwrap()).unwrap(), id);
            prop_assert_eq!(id.as_bytes(), &bytes);
        }

        #[test]
        fn arbitrary_id_text_is_fallible(text in ".{0,256}") {
            if let Ok(id) = text.parse::<EdgeId>() {
                prop_assert_eq!(id.to_string().parse::<EdgeId>().unwrap(), id);
            }
        }
    }

    #[test]
    fn ids_are_string_map_keys_and_reject_malformed_input() {
        let id = NodeId::from_bytes([7; 16]);
        let map = std::collections::BTreeMap::from([(id, "node")]);
        let encoded = serde_json::to_string(&map).unwrap();
        assert_eq!(serde_json::from_str::<std::collections::BTreeMap<NodeId, String>>(&encoded).unwrap().get(&id).unwrap(), "node");
        for json in ["12", "null", "\"bad id\"", "[]"] {
            assert!(serde_json::from_str::<NodeId>(json).is_err());
        }
    }
}
