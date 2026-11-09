//! Key-only text endpoints through rig's vendor dialects (#1152). No provider discovery or
//! credential lookup happens here: the host supplies the key explicitly.

use super::{ApiKey, ChatRequest, Dialect, Endpoint, Part};
use crate::{ErrorKind, ProviderError};

impl Endpoint {
    /// Together AI's public Chat Completions route.
    pub fn together(key: ApiKey) -> Self {
        Self::openai_compatible("together", "https://api.together.xyz/v1", Some(key))
    }

    /// Moonshot's global API (a global API key, not its China or Anthropic route).
    pub fn moonshot(key: ApiKey) -> Self {
        Self::openai_compatible("moonshot", "https://api.moonshot.ai/v1", Some(key))
    }

    /// Z.AI's general API (a general API key, not a coding-plan key).
    pub fn zai(key: ApiKey) -> Self {
        Self::openai_compatible("zai", "https://api.z.ai/api/paas/v4", Some(key))
    }

    /// MiniMax's global text API. Its media catalog models continue using fal's account.
    pub fn minimax(key: ApiKey) -> Self {
        Self::openai_compatible("minimax", "https://api.minimax.io/v1", Some(key))
    }

    /// Perplexity Sonar, text only; tools are refused before sending.
    pub fn perplexity(key: ApiKey) -> Self {
        Self::openai_compatible("perplexity", "https://api.perplexity.ai", Some(key))
    }

    /// Hyperbolic's current text route; tools are refused before sending.
    /// The old api.hyperbolic.xyz route in rig 0.46 has been decommissioned.
    pub fn hyperbolic(key: ApiKey) -> Self {
        Self::openai_compatible("hyperbolic", "https://api.hyperbolic.ai/inference/v1", Some(key))
    }

    /// Mira's gateway. Choose a model from the account's model list.
    pub fn mira(key: ApiKey) -> Self {
        Self::openai_compatible("mira", "https://api.mira.network/v1", Some(key))
    }

    /// Cohere's native v2 Chat wire, not the OpenAI compatibility route.
    pub fn cohere(key: ApiKey) -> Self {
        Self { id: "cohere".into(), dialect: Dialect::Cohere, base_url: "https://api.cohere.ai".into(), key: Some(key) }
    }
}

pub(super) fn validate(endpoint: &Endpoint, req: &ChatRequest) -> Result<(), ProviderError> {
    let id = endpoint.id.as_str();
    if !matches!(id, "together" | "moonshot" | "zai" | "minimax" | "perplexity" | "hyperbolic" | "mira" | "cohere") {
        return Ok(());
    }
    if req.messages.iter().flat_map(|m| &m.parts).any(|p| matches!(p, Part::Image { .. } | Part::Document { .. })) {
        return Err(ProviderError::new(ErrorKind::UnsupportedInput, "this endpoint currently supports text only; remove images and documents"));
    }
    // Tool definitions are supplied as raw params to preserve their schemas. Check explicitly
    // rather than relying on the wire's capability check, which only sees typed tools.
    if matches!(id, "perplexity" | "hyperbolic" | "mira")
        && (!req.tools.is_empty() || req.messages.iter().flat_map(|m| &m.parts).any(|p| matches!(p, Part::ToolUse { .. } | Part::ToolResult { .. })))
    {
        return Err(ProviderError::new(ErrorKind::UnsupportedInput, "this endpoint does not support tool calling; use a text-only conversation"));
    }
    if id == "cohere" && req.tools.iter().any(|t| t.strict) {
        return Err(ProviderError::new(ErrorKind::UnsupportedInput, "per-tool strict schemas are not supported by this Cohere adapter"));
    }
    Ok(())
}
