//! Lossless integer wire values and finite numeric parameters.

use crate::{Error, ErrorKind};
use serde::{Deserialize, Serialize};

/// A finite floating-point parameter. Exact counts and seeds use the integer wrappers.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Number(f64);

impl Number {
    pub fn new(value: f64) -> Result<Self, Error> {
        if value.is_finite() { Ok(Self(value)) } else { Err(Error::new(ErrorKind::InvalidInput, "number must be finite")) }
    }
    pub const fn get(self) -> f64 {
        self.0
    }
}
// NaN is excluded by every constructor, including deserialization.
impl Eq for Number {}
impl TryFrom<f64> for Number {
    type Error = Error;
    fn try_from(value: f64) -> Result<Self, Error> {
        Self::new(value)
    }
}
impl From<Number> for f64 {
    fn from(value: Number) -> Self {
        value.0
    }
}

macro_rules! integer {
    ($name:ident, $inner:ty) => {
        /// An exact integer, serialized as a canonical decimal string for browser clients.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(pub $inner);
        impl TryFrom<String> for $name {
            type Error = Error;
            fn try_from(text: String) -> Result<Self, Error> {
                if text.len() > 20 {
                    return Err(Error::new(ErrorKind::InvalidInput, "integer exceeds range"));
                }
                let value: $inner = text.parse().map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid integer"))?;
                if value.to_string() != text {
                    return Err(Error::new(ErrorKind::InvalidInput, "integer must be a canonical decimal string"));
                }
                Ok(Self(value))
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0.to_string()
            }
        }
    };
}
integer!(Integer, i64);
integer!(Unsigned, u64);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn finite_numbers_survive_json_text_round_trip(bits in any::<u64>()) {
            let raw = f64::from_bits(bits);
            if let Ok(value) = Number::new(raw) {
                let json = serde_json::to_string(&value).unwrap();
                prop_assert_eq!(serde_json::from_str::<Number>(&json).unwrap(), value);
            }
        }
    }

    #[test]
    fn numeric_defaults_keep_exact_equality_after_save_and_load() {
        let value = Number::new(51.248178375505404).unwrap();
        assert_eq!(serde_json::from_str::<Number>(&serde_json::to_string(&value).unwrap()).unwrap(), value);
    }

    #[test]
    fn integer_boundaries_round_trip_without_browser_precision_loss() {
        for n in [i64::MIN, -1, 0, 1, i64::MAX] {
            let value = Value::Integer(Integer(n));
            let json = serde_json::to_value(&value).unwrap();
            assert_eq!(json["value"], n.to_string());
            assert_eq!(serde_json::from_value::<Value>(json).unwrap(), value);
        }
        for n in [0, 1, 9_007_199_254_740_993, u64::MAX] {
            let value = Value::Unsigned(Unsigned(n));
            let json = serde_json::to_value(&value).unwrap();
            assert_eq!(json["value"], n.to_string());
            assert_eq!(serde_json::from_value::<Value>(json).unwrap(), value);
        }
        for text in ["", "+1", "01", "-0", "1.0", " 1", "18446744073709551616"] {
            assert!(Unsigned::try_from(text.to_owned()).is_err());
            assert!(Integer::try_from(text.to_owned()).is_err());
        }
        assert!(Unsigned::try_from("-1".to_owned()).is_err());
        assert!(serde_json::from_str::<Unsigned>("42").is_err());
    }

    #[test]
    fn numeric_parameters_are_finite_and_serde_preserves_scalars() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Number::new(value).is_err());
            let decoder = serde::de::value::F64Deserializer::<serde::de::value::Error>::new(value);
            assert!(Number::deserialize(decoder).is_err());
        }
        for value in [Value::Bool(true), Value::Bool(false), Value::Number(Number::new(-0.25).unwrap())] {
            assert_eq!(serde_json::from_str::<Value>(&serde_json::to_string(&value).unwrap()).unwrap(), value);
        }
    }
}
