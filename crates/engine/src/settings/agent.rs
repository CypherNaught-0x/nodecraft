//! The in-canvas agent (#114): which chat endpoint and model it asks, whether it asks before it
//! acts, and how much auto mode may accept for runs. Keys stay in the keychain; read the hostile
//! fields through the accessors.
use serde::{Deserialize, Serialize};

/// The chat endpoint the agent talks to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEndpoint {
    /// Anthropic's API; the key is the `anthropic` account's.
    #[default]
    Anthropic,
    /// OpenAI's API; the `openai` account's key.
    Openai,
    /// OpenRouter; the `openrouter` account's key.
    Openrouter,
    /// A local Ollama server at `base_url` (or its default address); no key.
    Ollama,
    /// A local LM Studio server at `base_url` (or its default address); no key.
    LmStudio,
}

impl AgentEndpoint {
    /// The keychain account holding the endpoint's key, for endpoints that need one.
    pub fn account(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("anthropic"),
            Self::Openai => Some("openai"),
            Self::Openrouter => Some("openrouter"),
            Self::Ollama | Self::LmStudio => None,
        }
    }
}

/// Whether the agent asks before it changes the project or starts a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    #[default]
    Assist,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    pub endpoint: AgentEndpoint,
    /// The model as the endpoint names it; empty means the endpoint's default (Anthropic only).
    /// Read through [`AgentSettings::model`].
    pub model: String,
    /// A local server's address, such as `http://127.0.0.1:11434`. Read through
    /// [`AgentSettings::base_url`].
    pub base_url: Option<String>,
    pub mode: AgentMode,
    /// What auto mode may accept for runs per session, in US cents. Read through
    /// [`AgentSettings::budget_usd_micros`].
    pub budget_usd_cents: u64,
    /// Whether the sidebar is open; saved when it is toggled, so the next start opens it the same way.
    pub open: bool,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self { endpoint: AgentEndpoint::default(), model: String::new(), base_url: None, mode: AgentMode::default(), budget_usd_cents: 100, open: false }
    }
}

impl AgentSettings {
    /// Longest model name or address read from the file.
    pub const MAX_TEXT_BYTES: usize = 512;
    /// The largest budget read from the file: 1000 USD.
    pub const MAX_BUDGET_USD_CENTS: u64 = 100_000;

    /// The model from the file, trimmed; `None` when unset or unusable (too long, control
    /// characters).
    pub fn model(&self) -> Option<&str> {
        bounded(&self.model)
    }

    /// The local server's address from the file, when it is an `http(s)` URL.
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref().and_then(bounded).filter(|url| url.starts_with("http://") || url.starts_with("https://"))
    }

    /// The auto-mode budget in micro-dollars, capped at [`Self::MAX_BUDGET_USD_CENTS`].
    pub fn budget_usd_micros(&self) -> u64 {
        self.budget_usd_cents.min(Self::MAX_BUDGET_USD_CENTS).saturating_mul(10_000)
    }
}

fn bounded(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty() && text.len() <= AgentSettings::MAX_TEXT_BYTES && !text.chars().any(char::is_control)).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_agent_settings_are_bounded() {
        let settings = AgentSettings {
            model: format!("  {}  ", "m".repeat(AgentSettings::MAX_TEXT_BYTES + 1)),
            base_url: Some("file:///etc/passwd".into()),
            budget_usd_cents: u64::MAX,
            ..AgentSettings::default()
        };
        assert_eq!(settings.model(), None);
        assert_eq!(settings.base_url(), None);
        assert_eq!(settings.budget_usd_micros(), 1_000_000_000);
        let settings = AgentSettings { model: " claude-x \n".into(), base_url: Some("http://127.0.0.1:1234".into()), ..AgentSettings::default() };
        assert_eq!(settings.model(), Some("claude-x"));
        assert_eq!(settings.base_url(), Some("http://127.0.0.1:1234"));
        let settings = AgentSettings { model: "a\u{7}b".into(), ..AgentSettings::default() };
        assert_eq!(settings.model(), None, "control characters inside");
        assert_eq!(AgentSettings::default().budget_usd_micros(), 1_000_000);
    }

    #[test]
    fn the_agent_section_reads_its_names() {
        let parsed: AgentSettings = serde_json::from_value(serde_json::json!({"endpoint": "lm_studio", "mode": "auto", "budget_usd_cents": 250})).unwrap();
        assert_eq!(parsed.endpoint, AgentEndpoint::LmStudio);
        assert_eq!(parsed.mode, AgentMode::Auto);
        assert_eq!(parsed.budget_usd_micros(), 2_500_000);
        assert_eq!(parsed.endpoint.account(), None);
        assert_eq!(AgentEndpoint::Anthropic.account(), Some("anthropic"));
    }
}
