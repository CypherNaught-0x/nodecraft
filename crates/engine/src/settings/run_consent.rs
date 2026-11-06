//! Settings › Automation › Paid runs (#1079): what the person decided about automation clients
//! starting paid runs in the desktop app, per channel.
//!
//! Read like every other value from the file: an unknown decision means "ask", and a ceiling that
//! isn't a whole, non-negative number of micros is dropped (so that currency asks again). A bad
//! entry never resets the rest of the section.
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

/// The channels whose decisions are remembered, by the key they have in the file.
pub const RUN_CONSENT_CLIENTS: [&str; 3] = ["control", "mcp_server", "agent"];

/// Most currencies one ceiling names; more are dropped when read.
pub const MAX_CEILING_CURRENCIES: usize = 16;

/// One channel's standing decision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunConsentSetting {
    /// `"always"` or `"never"`. Anything else asks. Read through [`Self::decision`].
    pub decision: String,
    /// For `"always"`: the most one run may accept, in micros per currency code. A run that
    /// accepts more, or a currency not named here, asks again.
    #[serde(deserialize_with = "lenient_ceiling")]
    pub ceiling: BTreeMap<String, u64>,
}

/// What a channel's setting says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunConsentDecision {
    Ask,
    Always,
    Never,
}

impl RunConsentDecision {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ask" => Some(Self::Ask),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

impl RunConsentSetting {
    pub fn decision(&self) -> RunConsentDecision {
        match self.decision.as_str() {
            "always" => RunConsentDecision::Always,
            "never" => RunConsentDecision::Never,
            _ => RunConsentDecision::Ask,
        }
    }

    pub fn always(ceiling: BTreeMap<String, u64>) -> Self {
        Self { decision: "always".into(), ceiling }
    }

    pub fn never() -> Self {
        Self { decision: "never".into(), ceiling: BTreeMap::new() }
    }
}

/// Whether `code` looks like a currency code a quote uses: 1–16 ASCII letters, digits or `_`.
pub fn is_currency_code(code: &str) -> bool {
    (1..=16).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn lenient_ceiling<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, u64>, D::Error> {
    let raw = BTreeMap::<String, serde_json::Value>::deserialize(deserializer).unwrap_or_default();
    Ok(raw
        .into_iter()
        .filter(|(code, _)| is_currency_code(code))
        .filter_map(|(code, value)| Some((code, value.as_u64()?)))
        .take(MAX_CEILING_CURRENCIES)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct File {
        run_consent: BTreeMap<String, RunConsentSetting>,
    }

    #[test]
    fn bad_entries_ask_again_without_spoiling_the_rest() {
        let file: File = serde_json::from_value(serde_json::json!({"run_consent": {
            "control": {"decision": "always", "ceiling": {"USD": 500_000, "EUR": -3, "bad code!": 7, "CREDITS": "lots"}},
            "agent": {"decision": "sometimes"},
            "mcp_server": {"decision": "never", "ceiling": "none"},
        }}))
        .unwrap();
        let control = &file.run_consent["control"];
        assert_eq!(control.decision(), RunConsentDecision::Always);
        assert_eq!(control.ceiling, BTreeMap::from([("USD".to_owned(), 500_000)]));
        assert_eq!(file.run_consent["agent"].decision(), RunConsentDecision::Ask);
        assert_eq!(file.run_consent["mcp_server"].decision(), RunConsentDecision::Never);
        assert!(file.run_consent["mcp_server"].ceiling.is_empty());
    }
}
