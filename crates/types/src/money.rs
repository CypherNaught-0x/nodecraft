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

/// Rounding applied once, after multiplying a rate by the total usage.
/// Use `Up` for conservative budget quotes so non-zero fractional costs never become free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    Down,
    Up,
}

impl Money {
    pub const fn new(currency: Currency, micros: u64) -> Self {
        Self { currency, micros }
    }

    pub const fn zero(currency: Currency) -> Self {
        Self::new(currency, 0)
    }

    pub fn checked_sum(currency: Currency, amounts: impl IntoIterator<Item = Self>) -> Result<Self, Error> {
        amounts.into_iter().try_fold(Self::zero(currency), Self::checked_add)
    }

    pub fn checked_cmp(self, other: Self) -> Result<std::cmp::Ordering, Error> {
        self.same_currency(other)?;
        Ok(self.micros.cmp(&other.micros))
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
            .ok_or_else(|| Error::new(ErrorKind::Underflow, "money subtraction would be negative"))
    }

    pub fn checked_mul(self, count: u64) -> Result<Self, Error> {
        self.micros
            .checked_mul(count)
            .map(|micros| Self::new(self.currency, micros))
            .ok_or_else(|| Error::new(ErrorKind::Overflow, "money multiplication overflow"))
    }

    /// Scale an aggregate rate without first rounding its fractional per-unit price.
    /// For $0.075 per million tokens, use `Money::new(Currency::USD, 75_000)` and
    /// multiply by `(total_tokens, 1_000_000, Rounding::Up)` when quoting a budget.
    /// The intermediate product uses u128; only the final amount must fit u64.
    pub fn checked_mul_ratio(self, numerator: u64, denominator: u64, rounding: Rounding) -> Result<Self, Error> {
        if denominator == 0 {
            return Err(Error::new(ErrorKind::InvalidInput, "money ratio denominator must be non-zero"));
        }
        let product = u128::from(self.micros) * u128::from(numerator);
        let denominator = u128::from(denominator);
        let quotient = product / denominator;
        let micros = if rounding == Rounding::Up && product % denominator != 0 { quotient + 1 } else { quotient };
        let micros = u64::try_from(micros).map_err(|_| Error::new(ErrorKind::Overflow, "money ratio multiplication overflow"))?;
        Ok(Self::new(self.currency, micros))
    }

    fn same_currency(self, other: Self) -> Result<(), Error> {
        if self.currency == other.currency { Ok(()) } else { Err(Error::new(ErrorKind::CurrencyMismatch, "money arithmetic requires matching currencies")) }
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}.{:06}", self.currency, self.micros / 1_000_000, self.micros % 1_000_000)
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
        assert_eq!(usd.checked_sub(Money::new(Currency::USD, 2)).unwrap_err().kind, ErrorKind::Underflow);
    }

    #[test]
    fn aggregate_token_quotes_round_once_and_check_bounds() {
        let rate = Money::new(Currency::USD, 75_000);
        assert_eq!(rate.checked_mul_ratio(1, 1_000_000, Rounding::Down).unwrap().micros, 0);
        assert_eq!(rate.checked_mul_ratio(1, 1_000_000, Rounding::Up).unwrap().micros, 1);
        assert_eq!(rate.checked_mul_ratio(1_000_000, 1_000_000, Rounding::Up).unwrap(), rate);
        assert_eq!(rate.checked_mul_ratio(20, 1_000_000, Rounding::Up).unwrap().micros, 2);
        assert_eq!(rate.checked_mul_ratio(0, 1, Rounding::Up).unwrap(), Money::zero(Currency::USD));
        assert_eq!(rate.checked_mul_ratio(1, 0, Rounding::Up).unwrap_err().kind, ErrorKind::InvalidInput);
        let max = Money::new(Currency::USD, u64::MAX);
        assert_eq!(max.checked_mul_ratio(u64::MAX, u64::MAX, Rounding::Up).unwrap(), max);
        assert_eq!(max.checked_mul_ratio(2, 1, Rounding::Down).unwrap_err().kind, ErrorKind::Overflow);
        // The rounded-up result, not just the quotient, must fit u64.
        let half = Money::new(Currency::USD, 1_u64 << 63);
        assert_eq!(half.checked_mul_ratio(u64::MAX, 1_u64 << 63, Rounding::Up).unwrap(), max);
        let near_max = Money::new(Currency::USD, u64::MAX - 1);
        assert_eq!(near_max.checked_mul_ratio(u64::MAX - 1, u64::MAX - 2, Rounding::Down).unwrap(), max);
        assert_eq!(near_max.checked_mul_ratio(u64::MAX - 1, u64::MAX - 2, Rounding::Up).unwrap_err().kind, ErrorKind::Overflow);
    }

    #[test]
    fn budget_helpers_require_matching_currencies() {
        let usd = Money::new(Currency::USD, 5);
        let eur = Money::new(Currency::EUR, 5);
        assert_eq!(Money::checked_sum(Currency::USD, []).unwrap(), Money::zero(Currency::USD));
        assert_eq!(Money::checked_sum(Currency::USD, [usd, usd]).unwrap().micros, 10);
        assert_eq!(Money::checked_sum(Currency::USD, [eur]).unwrap_err().kind, ErrorKind::CurrencyMismatch);
        assert!(Money::checked_sum(Currency::USD, [Money::new(Currency::USD, u64::MAX), usd]).is_err());
        assert_eq!(usd.checked_cmp(eur).unwrap_err().kind, ErrorKind::CurrencyMismatch);
        assert_eq!(usd.checked_cmp(Money::zero(Currency::USD)).unwrap(), std::cmp::Ordering::Greater);
        assert_eq!(Money::new(Currency::USD, 1_075_001).to_string(), "USD 1.075001");
    }
}
