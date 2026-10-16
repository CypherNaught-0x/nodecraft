use serde::{Deserialize, Serialize};

use crate::{Error, ErrorKind};

/// An uppercase three-letter currency code. This validates syntax, not an external registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Currency([u8; 3]);

impl Currency {
    pub const USD: Self = Self(*b"USD");
    pub const EUR: Self = Self(*b"EUR");
}

impl TryFrom<String> for Currency {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let bytes: [u8; 3] = value.as_bytes().try_into().map_err(|_| Error::new(ErrorKind::InvalidInput, "currency must be three uppercase ASCII letters"))?;
        if !bytes.iter().all(u8::is_ascii_uppercase) {
            return Err(Error::new(ErrorKind::InvalidInput, "currency must be three uppercase ASCII letters"));
        }
        Ok(Self(bytes))
    }
}

impl From<Currency> for String {
    fn from(value: Currency) -> Self {
        value.0.into_iter().map(char::from).collect()
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from(*self))
    }
}

/// Non-negative millionths of a currency unit. No floating-point rounding in quotes/budgets.
/// JSON stores micros as a decimal string to preserve all 64 bits in browser clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    pub currency: Currency,
    #[serde(with = "micros_string")]
    pub micros: u64,
}

impl Money {
    pub const fn new(currency: Currency, micros: u64) -> Self {
        Self { currency, micros }
    }

    pub fn checked_add(self, other: Self) -> Result<Self, Error> {
        self.same_currency(other)?;
        self.micros
            .checked_add(other.micros)
            .map(|micros| Self::new(self.currency, micros))
            .ok_or_else(|| Error::new(ErrorKind::Overflow, "money addition overflow"))
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, Error> {
        self.same_currency(other)?;
        self.micros
            .checked_sub(other.micros)
            .map(|micros| Self::new(self.currency, micros))
            .ok_or_else(|| Error::new(ErrorKind::Overflow, "money subtraction would be negative"))
    }

    pub fn checked_mul(self, count: u64) -> Result<Self, Error> {
        self.micros
            .checked_mul(count)
            .map(|micros| Self::new(self.currency, micros))
            .ok_or_else(|| Error::new(ErrorKind::Overflow, "money multiplication overflow"))
    }

    fn same_currency(self, other: Self) -> Result<(), Error> {
        if self.currency == other.currency { Ok(()) } else { Err(Error::new(ErrorKind::CurrencyMismatch, "money arithmetic requires matching currencies")) }
    }
}

mod micros_string {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) || (value.len() > 1 && value.starts_with('0')) {
            return Err(D::Error::custom("micros must be a canonical unsigned decimal string"));
        }
        value.parse().map_err(|_| D::Error::custom("micros exceeds u64"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn money_round_trips_and_checks_arithmetic(a in any::<u64>(), b in any::<u64>()) {
            let amount = Money::new(Currency::USD, a);
            prop_assert_eq!(serde_json::from_str::<Money>(&serde_json::to_string(&amount).unwrap()).unwrap(), amount);
            prop_assert_eq!(amount.checked_add(Money::new(Currency::USD, b)).ok().map(|m| m.micros), a.checked_add(b));
            prop_assert_eq!(amount.checked_sub(Money::new(Currency::USD, b)).ok().map(|m| m.micros), a.checked_sub(b));
            prop_assert_eq!(amount.checked_mul(b).ok().map(|m| m.micros), a.checked_mul(b));
        }
    }

    #[test]
    fn rejects_invalid_money_and_currency_mismatches() {
        for currency in ["usd", "US", "USDD", "€", "U D", "12A"] {
            assert!(Currency::try_from(currency.to_owned()).is_err());
        }
        for micros in ["-1", "+1", "01", "1.5", "NaN", "18446744073709551616"] {
            let json = serde_json::json!({"currency":"USD", "micros": micros});
            assert!(serde_json::from_value::<Money>(json).is_err());
        }
        let usd = Money::new(Currency::USD, 1);
        let eur = Money::new(Currency::EUR, 1);
        assert_eq!(usd.checked_add(eur).unwrap_err().kind, ErrorKind::CurrencyMismatch);
        assert_eq!(usd.checked_sub(eur).unwrap_err().kind, ErrorKind::CurrencyMismatch);
        assert!(Money::new(Currency::USD, u64::MAX).checked_add(usd).is_err());
        assert!(usd.checked_sub(Money::new(Currency::USD, 2)).is_err());
    }
}
