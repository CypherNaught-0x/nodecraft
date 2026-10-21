//! Pricing rules and quotes. Prices are written as decimal strings (`"0.04 USD"`) and held as
//! exact micros, so a quote never rounds through floating point.
use nodecraft_types::{Currency, Error, ErrorKind, Money, ParamValues, Rounding, Value};
use serde::{Deserialize, Deserializer};

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// A price parsed from `"<amount> <CUR>"`: up to six decimals, a three-letter currency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price(pub Money);

impl std::str::FromStr for Price {
    type Err = Error;
    fn from_str(text: &str) -> Result<Self, Error> {
        let bad = || invalid("a price is written as \"<amount> <currency>\", e.g. \"0.04 USD\"");
        let (amount, currency) = text.trim().split_once(' ').ok_or_else(bad)?;
        let currency = Currency::try_from(currency.trim().to_owned()).map_err(|_| bad())?;
        let (whole, fraction) = match amount.split_once('.') {
            Some((_, "")) => return Err(bad()),
            Some((whole, fraction)) => (whole, fraction),
            None => (amount, ""),
        };
        if whole.is_empty() || fraction.len() > 6 || !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let whole: u64 = whole.parse().map_err(|_| bad())?;
        // Pad the fraction to micros: "04" → 40000.
        let micros_fraction: u64 = format!("{fraction:0<6}").parse().map_err(|_| bad())?;
        let micros = whole.checked_mul(1_000_000).and_then(|w| w.checked_add(micros_fraction)).ok_or_else(|| invalid("price is too large"))?;
        Ok(Self(Money::new(currency, micros)))
    }
}

impl<'de> Deserialize<'de> for Price {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(|error: Error| serde::de::Error::custom(error.message))
    }
}

/// What a per-unit price counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Image,
    Second,
    Megapixel,
    /// Priced per 1000; usage-based (LLM tokens), so without a `count` the quote waits for
    /// the run.
    ThousandTokens,
    ThousandCharacters,
    /// Priced per 1000 training steps (#167), counted from the trainer's steps setting; known
    /// before the run, so not usage-based.
    ThousandSteps,
}

impl Unit {
    /// How many counted units one priced unit covers.
    fn per(self) -> u64 {
        match self {
            Self::ThousandTokens | Self::ThousandCharacters | Self::ThousandSteps => 1000,
            Self::Image | Self::Second | Self::Megapixel => 1,
        }
    }
    pub fn usage_based(self) -> bool {
        matches!(self, Self::ThousandTokens | Self::ThousandCharacters)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pricing {
    /// Local or included models.
    Free,
    /// Composite measured usage with no reliable pre-submit amount. Never a zero/free quote.
    ByUsage { currency: Currency },
    /// One price per job.
    PerCall { price: Price },
    /// `price` per unit. The unit count is the value of parameter `count` (e.g. `num_images`,
    /// `duration`) after schema defaults, or `default_count` when the model has no such
    /// parameter value. `count` must be the only parameter that scales the cost.
    PerUnit {
        unit: Unit,
        price: Price,
        #[serde(default)]
        count: Option<String>,
        #[serde(default)]
        default_count: Option<u64>,
    },
}

impl Pricing {
    pub fn currency(&self) -> Option<Currency> {
        match self {
            Self::Free => None,
            Self::ByUsage { currency } => Some(*currency),
            Self::PerCall { price } | Self::PerUnit { price, .. } => Some(price.0.currency),
        }
    }

    /// The price of one job given its **resolved** parameters (schema defaults applied; use
    /// [`crate::ModelDescriptor::quote`]), rounded up, or `None` when it depends on usage the
    /// run hasn't measured yet. Free models quote zero in `fallback`.
    pub(crate) fn quote(&self, params: &ParamValues, fallback: Currency) -> Result<Option<Money>, Error> {
        self.quote_lookup(|name| params.get(name), fallback)
    }

    /// Price already validated, effective visible values without owning a parameter map.
    /// The caller must apply schema defaults/visibility and descriptor validation first.
    pub fn quote_lookup<'a>(&self, get: impl Fn(&str) -> Option<&'a Value>, fallback: Currency) -> Result<Option<Money>, Error> {
        match self {
            Self::Free => Ok(Some(Money::zero(fallback))),
            Self::ByUsage { .. } => Ok(None),
            Self::PerCall { price } => Ok(Some(price.0)),
            Self::PerUnit { unit, price, count, default_count } => {
                let value = count.as_ref().and_then(|name| get(name));
                let per = unit.per();
                match (value, default_count) {
                    (None, Some(n)) => price.0.checked_mul_ratio(*n, per, Rounding::Up).map(Some),
                    (None, None) if unit.usage_based() => Ok(None),
                    (None, None) => Err(invalid("this model's price needs a count")),
                    (Some(Value::Unsigned(n)), _) => price.0.checked_mul_ratio(n.0, per, Rounding::Up).map(Some),
                    (Some(Value::Integer(n)), _) => {
                        let n = u64::try_from(n.0).map_err(|_| invalid("a price count can't be negative"))?;
                        price.0.checked_mul_ratio(n, per, Rounding::Up).map(Some)
                    }
                    (Some(Value::Number(n)), _) => {
                        let micros = ceil_micros(n.get()).ok_or_else(|| invalid("a price count is out of range"))?;
                        let denominator = per.checked_mul(1_000_000).ok_or_else(|| invalid("a price count is out of range"))?;
                        price.0.checked_mul_ratio(micros, denominator, Rounding::Up).map(Some)
                    }
                    (Some(_), _) => Err(invalid("a price count parameter must be a number")),
                }
            }
        }
    }
}

/// What a chat model bills for the tokens a run reports using (#595), per 1000 tokens. A run is
/// quoted by [`Pricing`] before it is sent (an upper bound on the reply) and charged at these
/// rates once the reply says how many tokens it used.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenRates {
    /// Per 1000 prompt tokens.
    pub input: Price,
    /// Per 1000 reply tokens, thinking included.
    pub output: Price,
    /// The rates for a long prompt, if the model has them.
    #[serde(default)]
    pub long: Option<LongPrompt>,
}

/// Rates that apply to the whole request once its prompt has more than `above` tokens.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongPrompt {
    pub above: u64,
    pub input: Price,
    pub output: Price,
}

impl TokenRates {
    /// Every price these rates hold.
    pub(crate) fn prices(&self) -> impl Iterator<Item = Price> + '_ {
        [self.input, self.output].into_iter().chain(self.long.iter().flat_map(|long| [long.input, long.output]))
    }

    /// What `input` prompt tokens and `output` reply tokens cost, each part rounded up to the micro.
    pub fn cost(&self, input: u64, output: u64) -> Result<Money, Error> {
        let (input_price, output_price) = match &self.long {
            Some(long) if input > long.above => (long.input, long.output),
            _ => (self.input, self.output),
        };
        let input = input_price.0.checked_mul_ratio(input, 1000, Rounding::Up)?;
        input.checked_add(output_price.0.checked_mul_ratio(output, 1000, Rounding::Up)?)
    }
}

/// `⌈x × 10⁶⌉`, never below either reading of the count: the float's exact binary value, or the
/// shortest decimal that is written into the provider request (which can be slightly larger).
/// `None` for negative, non-finite or too-large values.
fn ceil_micros(x: f64) -> Option<u64> {
    // `-0.0` is zero (it would print as "-0").
    let x = if x == 0.0 { 0.0 } else { x };
    Some(binary_ceil_micros(x)?.max(decimal_ceil_micros(x)?))
}

/// `⌈d × 10⁶⌉` for `d`, the shortest decimal that reads back as `x` (Rust's `Display`, which never
/// uses an exponent). That is the value serde writes, or the next-larger shortest decimal on exact
/// ties, so never below it.
fn decimal_ceil_micros(x: f64) -> Option<u64> {
    let text = format!("{x}");
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let whole: u64 = whole.parse().ok()?;
    let (kept, rest) = fraction.split_at(fraction.len().min(6));
    let kept: u64 = format!("{kept:0<6}").parse().ok()?;
    let round_up = u64::from(rest.bytes().any(|b| b != b'0'));
    whole.checked_mul(1_000_000)?.checked_add(kept)?.checked_add(round_up)
}

/// `⌈x × 10⁶⌉` computed exactly from the float's binary value.
fn binary_ceil_micros(x: f64) -> Option<u64> {
    if !x.is_finite() || x < 0.0 {
        return None;
    }
    let bits = x.to_bits();
    let exponent = i32::try_from((bits >> 52) & 0x7ff).ok()?;
    let fraction = bits & ((1u64 << 52) - 1);
    // x = mantissa × 2^power, exactly.
    let (mantissa, power) = if exponent == 0 { (fraction, -1074) } else { (fraction | (1u64 << 52), exponent - 1075) };
    let scaled = u128::from(mantissa).checked_mul(1_000_000)?;
    let micros = if power >= 0 {
        scaled.checked_mul(1u128.checked_shl(u32::try_from(power).ok()?)?)?
    } else {
        let shift = u32::try_from(-power).ok()?;
        if shift >= 128 {
            u128::from(scaled > 0)
        } else {
            let denominator = 1u128 << shift;
            scaled / denominator + u128::from(scaled % denominator != 0)
        }
    };
    u64::try_from(micros).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::{Integer, Number, Unsigned};

    fn usd(micros: u64) -> Money {
        Money::new(Currency::USD, micros)
    }

    #[test]
    fn prices_parse_exactly_and_reject_junk() {
        assert_eq!("0.04 USD".parse::<Price>().unwrap().0, usd(40_000));
        assert_eq!("12 EUR".parse::<Price>().unwrap().0, Money::new(Currency::EUR, 12_000_000));
        assert_eq!("0.000001 USD".parse::<Price>().unwrap().0, usd(1));
        for bad in
            ["", "0.04", "USD 0.04", ".5 USD", "1. USD", "1.0000001 USD", "-1 USD", "1e3 USD", "1,5 USD", "0.04 usd", "99999999999999999999 USD", "٣ USD"]
        {
            assert!(bad.parse::<Price>().is_err(), "{bad}");
        }
    }

    #[test]
    fn quotes_count_units_and_round_up() {
        let price = "0.05 USD".parse::<Price>().unwrap();
        let per_second = Pricing::PerUnit { unit: Unit::Second, price, count: Some("duration".into()), default_count: Some(5) };
        let mut params = ParamValues::new();
        assert_eq!(per_second.quote(&params, Currency::USD).unwrap(), Some(usd(250_000)));
        params.insert("duration".into(), Value::Number(Number::new(2.5).unwrap()));
        assert_eq!(per_second.quote(&params, Currency::USD).unwrap(), Some(usd(125_000)));
        params.insert("duration".into(), Value::Number(Number::new(0.0000001).unwrap()));
        // A sliver of a unit still costs at least one micro.
        assert_eq!(per_second.quote(&params, Currency::USD).unwrap(), Some(usd(1)));
        params.insert("duration".into(), Value::Unsigned(Unsigned(3)));
        assert_eq!(per_second.quote(&params, Currency::USD).unwrap(), Some(usd(150_000)));
        params.insert("duration".into(), Value::Integer(Integer(-1)));
        assert!(per_second.quote(&params, Currency::USD).is_err());
        params.insert("duration".into(), Value::Unsigned(Unsigned(u64::MAX)));
        assert_eq!(per_second.quote(&params, Currency::USD).unwrap_err().kind, ErrorKind::Overflow);
        params.insert("duration".into(), Value::Number(Number::new(2e13).unwrap()));
        assert!(per_second.quote(&params, Currency::USD).is_err());
        params.insert("duration".into(), Value::Text("long".into()));
        assert!(per_second.quote(&params, Currency::USD).is_err());
    }

    #[test]
    fn fractional_counts_convert_exactly_and_round_up() {
        assert_eq!(ceil_micros(2.5), Some(2_500_000));
        assert_eq!(ceil_micros(0.0), Some(0));
        assert_eq!(ceil_micros(0.0000001), Some(1));
        assert_eq!(ceil_micros(f64::MIN_POSITIVE / 4.0), Some(1));
        // 0.1 is slightly above one tenth in binary, so it costs one micro more, never less.
        assert_eq!(ceil_micros(0.1), Some(100_001));
        // The review's case: plain f64 multiplication would round this down to …020.
        assert_eq!(ceil_micros(10000000000.000021), Some(10_000_000_000_000_021));
        // The binary value of this count is …0.001953125, but the request says …0.002: quote that.
        assert_eq!(ceil_micros(10000000000000.002), Some(10_000_000_000_000_002_000));
        assert_eq!(decimal_ceil_micros(1.0000004), Some(1_000_001));
        assert_eq!(decimal_ceil_micros(3.0), Some(3_000_000));
        assert_eq!(ceil_micros(-0.0), Some(0));
        for bad in [-1.0, f64::NAN, f64::INFINITY, 1e300, 1.9e13] {
            assert_eq!(ceil_micros(bad), None, "{bad}");
        }
    }

    #[test]
    fn thousand_units_divide_their_count() {
        let price = "0.002 USD".parse::<Price>().unwrap();
        let tokens = Pricing::PerUnit { unit: Unit::ThousandTokens, price, count: Some("max_tokens".into()), default_count: None };
        let mut params = ParamValues::new();
        // Usage-based without a count: quoted at run time.
        assert_eq!(tokens.quote(&params, Currency::USD).unwrap(), None);
        params.insert("max_tokens".into(), Value::Unsigned(Unsigned(4096)));
        // 4.096 thousand × 0.002 = 0.008192, rounded up to the micro.
        assert_eq!(tokens.quote(&params, Currency::USD).unwrap(), Some(usd(8_192)));
        params.insert("max_tokens".into(), Value::Unsigned(Unsigned(1)));
        assert_eq!(tokens.quote(&params, Currency::USD).unwrap(), Some(usd(2)));
        assert_eq!(Pricing::Free.quote(&ParamValues::new(), Currency::EUR).unwrap(), Some(Money::zero(Currency::EUR)));
    }

    #[test]
    fn token_rates_charge_measured_usage_and_switch_for_a_long_prompt() {
        let rates: TokenRates =
            toml::from_str("input = \"0.0001 USD\"\noutput = \"0.0005 USD\"\nlong = { above = 100000, input = \"0.0005 USD\", output = \"0.0025 USD\" }")
                .unwrap();
        // 1,000 × 0.0001 + 2,000 × 0.0005.
        assert_eq!(rates.cost(1_000, 2_000).unwrap(), usd(1_100));
        assert_eq!(rates.cost(100_000, 0).unwrap(), usd(10_000), "at the threshold: the short rate");
        assert_eq!(rates.cost(100_001, 1_000).unwrap(), usd(50_001 + 2_500), "over it: the long rate for the whole request");
        assert_eq!(rates.cost(1, 1).unwrap(), usd(2), "each part rounds up");
        assert_eq!(rates.cost(0, 0).unwrap(), usd(0));
        assert!(rates.cost(u64::MAX, u64::MAX).is_err());
        assert!(toml::from_str::<TokenRates>("input = \"1 USD\"\noutput = \"1 USD\"\nper = 5").is_err(), "unknown fields");
    }
}
