use serde::{Deserialize, Serialize};

use crate::{Error, ErrorKind};

/// How many units a provider says it billed a request (#437), in millionths so a fractional
/// count survives exactly. A unit is the provider's own: what one costs depends on its pricing
/// for the model, so this is evidence of use, never an amount of money. JSON stores it as a
/// decimal string (`"1.5"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BillableUnits {
    millionths: u64,
}

const PER_UNIT: u64 = 1_000_000;
/// Digits after the point a count keeps.
const PLACES: usize = 6;

impl BillableUnits {
    /// The most units a count holds: a billion.
    pub const MAX_UNITS: u64 = 1_000_000_000;

    /// A count of `millionths` of a unit, if it is no more than [`Self::MAX_UNITS`].
    pub fn from_millionths(millionths: u64) -> Option<Self> {
        (millionths <= Self::MAX_UNITS.saturating_mul(PER_UNIT)).then_some(Self { millionths })
    }

    pub const fn millionths(self) -> u64 {
        self.millionths
    }

    /// Reads a plain decimal count: digits, then optionally a point and one to six more digits.
    /// No sign, exponent, spaces or digit grouping, and nothing above [`Self::MAX_UNITS`].
    pub fn parse(text: &str) -> Result<Self, Error> {
        let invalid = || Error::new(ErrorKind::InvalidInput, "billable units must be a plain decimal of at most a billion, with at most six decimal places");
        let (whole, fraction) = match text.split_once('.') {
            Some((whole, fraction)) => (whole, Some(fraction)),
            None => (text, None),
        };
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        // A billion has ten digits; longer is too many however many leading zeros it has.
        if !digits(whole) || whole.len() > 10 || fraction.is_some_and(|f| !digits(f) || f.len() > PLACES) {
            return Err(invalid());
        }
        let whole: u64 = whole.parse().map_err(|_| invalid())?;
        let fraction = match fraction {
            Some(fraction) => {
                let padded = format!("{fraction:0<PLACES$}");
                padded.parse::<u64>().map_err(|_| invalid())?
            }
            None => 0,
        };
        let millionths = whole.checked_mul(PER_UNIT).and_then(|m| m.checked_add(fraction)).ok_or_else(invalid)?;
        Self::from_millionths(millionths).ok_or_else(invalid)
    }
}

impl TryFrom<String> for BillableUnits {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<BillableUnits> for String {
    fn from(value: BillableUnits) -> Self {
        value.to_string()
    }
}

impl std::fmt::Display for BillableUnits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (whole, fraction) = (self.millionths / PER_UNIT, self.millionths % PER_UNIT);
        if fraction == 0 {
            return write!(f, "{whole}");
        }
        let fraction = format!("{fraction:0>PLACES$}");
        write!(f, "{whole}.{}", fraction.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn plain_decimals_are_read_exactly() {
        let units = |text: &str| BillableUnits::parse(text).map(BillableUnits::millionths).ok();
        assert_eq!(units("0"), Some(0));
        assert_eq!(units("1"), Some(1_000_000));
        assert_eq!(units("1.5"), Some(1_500_000));
        assert_eq!(units("0.000001"), Some(1));
        assert_eq!(units("007.250"), Some(7_250_000));
        assert_eq!(units("1000000000"), Some(1_000_000_000_000_000));
        assert_eq!(units("1000000000.000000"), Some(1_000_000_000_000_000));
    }

    #[test]
    fn anything_else_is_refused() {
        for text in [
            "",
            " 1",
            "1 ",
            "+1",
            "-1",
            "1e3",
            "1E3",
            ".5",
            "1.",
            "1..5",
            "1.5.5",
            "0.0000001",
            "1,000",
            "1_000",
            "NaN",
            "inf",
            "1000000000.000001",
            "10000000000",
            "00000000001",
            "18446744073709551616",
            "١",
            "1\u{0}",
        ] {
            assert!(BillableUnits::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn it_is_a_decimal_string_in_json() {
        let units = BillableUnits::parse("2.25").unwrap();
        assert_eq!(serde_json::to_string(&units).unwrap(), "\"2.25\"");
        assert_eq!(serde_json::from_str::<BillableUnits>("\"2.25\"").unwrap(), units);
        assert!(serde_json::from_str::<BillableUnits>("2.25").is_err(), "a number could lose precision");
        assert!(serde_json::from_str::<BillableUnits>("\"-1\"").is_err());
    }

    proptest! {
        #[test]
        fn every_count_round_trips(millionths in 0..=BillableUnits::MAX_UNITS * PER_UNIT) {
            let units = BillableUnits::from_millionths(millionths).unwrap();
            prop_assert_eq!(BillableUnits::parse(&units.to_string()).unwrap(), units);
        }

        #[test]
        fn no_text_panics(text in "\\PC{0,24}") {
            let _ = BillableUnits::parse(&text);
        }
    }
}
