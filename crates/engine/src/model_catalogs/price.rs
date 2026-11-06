//! Prices as listings write them: USD per token, as a decimal string (`"0.0000025"`, sometimes
//! `"2.5e-6"`) or a JSON number, turned into exact catalog prices without floating point.
use nodecraft_catalog::pricing::Price;
use nodecraft_types::{Currency, Money};
use serde_json::Value;

/// Highest per-1000-token price accepted: 1000 USD. Anything above is a listing error.
const MAX_MICROS_PER_K: u128 = 1_000_000_000;

/// A per-token amount as `(mantissa, power of ten)`; `None` for anything but a plain
/// non-negative decimal, optionally with an exponent.
fn decimal(text: &str) -> Option<(u128, i32)> {
    let text = text.trim();
    if text.is_empty() || text.len() > 40 {
        return None;
    }
    let (number, exponent) = match text.find(['e', 'E']) {
        Some(at) => (text.get(..at)?, text.get(at + 1..)?.parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) || whole.len() + fraction.len() > 30 {
        return None;
    }
    let digits = format!("{whole}{fraction}");
    let mantissa: u128 = if digits.is_empty() { 0 } else { digits.parse().ok()? };
    let power = exponent.checked_sub(i32::try_from(fraction.len()).ok()?)?;
    (-60..=30).contains(&power).then_some((mantissa, power))
}

/// Micros per 1000 tokens for a per-token USD amount, rounded up so a quote is never low.
fn micros_per_k((mantissa, power): (u128, i32)) -> Option<u128> {
    // USD per token × 1000 tokens × 1e6 micros = × 1e9.
    let shift = power.checked_add(9)?;
    let micros = if shift >= 0 {
        mantissa.checked_mul(10u128.checked_pow(u32::try_from(shift).ok()?)?)?
    } else {
        let divisor = 10u128.checked_pow(u32::try_from(-shift).ok()?)?;
        mantissa.div_ceil(divisor)
    };
    (micros <= MAX_MICROS_PER_K).then_some(micros)
}

/// The price per 1000 tokens for a listing's per-token USD value (string or number).
pub(crate) fn per_thousand_tokens(value: &Value) -> Option<Price> {
    let parsed = match value {
        Value::String(text) => decimal(text)?,
        // JSON numbers are re-read from their shortest text form, never through arithmetic.
        Value::Number(number) => decimal(&number.to_string())?,
        _ => return None,
    };
    let micros = u64::try_from(micros_per_k(parsed)?).ok()?;
    Some(Price(Money::new(Currency::USD, micros)))
}

/// Whether a listed per-token value is exactly zero.
pub(crate) fn is_zero(value: &Value) -> bool {
    let parsed = match value {
        Value::String(text) => decimal(text),
        Value::Number(number) => decimal(&number.to_string()),
        _ => None,
    };
    parsed.is_some_and(|(mantissa, _)| mantissa == 0)
}

/// USD per million tokens, for descriptions (`"$1.60"`), from a per-1000 price.
pub(crate) fn per_million_label(price: Price) -> String {
    // micros per 1000 tokens = USD per million tokens × 1000.
    let micros = price.0.micros;
    let cents = micros / 10;
    if cents == 0 && micros > 0 { "<$0.01".into() } else { format!("${}.{:02}", cents / 100, cents % 100) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn micros(value: Value) -> Option<u64> {
        per_thousand_tokens(&value).map(|price| price.0.micros)
    }

    #[test]
    fn listings_become_exact_prices_rounded_up() {
        assert_eq!(micros(json!("0.0000025")), Some(2500), "2.50 USD per million is 0.0025 per thousand");
        assert_eq!(micros(json!("0.0000016")), Some(1600));
        assert_eq!(micros(json!("0.0000000833333333333333")), Some(84), "rounded up");
        assert_eq!(micros(json!("2.5e-6")), Some(2500));
        assert_eq!(micros(json!(0.000002)), Some(2000));
        assert_eq!(micros(json!("0")), Some(0));
        assert_eq!(micros(json!(".5")), Some(500_000_000));
    }

    #[test]
    fn nonsense_and_absurd_prices_are_refused() {
        for value in [json!("-1"), json!("abc"), json!(""), json!("1e400"), json!("5"), json!(null), json!(true), json!("0x10"), json!("1.2.3"), json!(".")] {
            assert_eq!(micros(value.clone()), None, "{value}");
        }
        assert!(is_zero(&json!("0")) && is_zero(&json!(0)) && is_zero(&json!("0.000")));
        assert!(!is_zero(&json!("-1")) && !is_zero(&json!("0.1")));
    }

    #[test]
    fn labels_say_dollars_per_million() {
        assert_eq!(per_million_label(Price(Money::new(Currency::USD, 1600))), "$1.60");
        assert_eq!(per_million_label(Price(Money::new(Currency::USD, 3))), "<$0.01");
    }
}
