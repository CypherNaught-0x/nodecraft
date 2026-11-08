//! Amazon Bedrock's OpenAI-compatible Chat Completions route on `bedrock-runtime` (#46).
//!
//! AWS documents `https://bedrock-runtime.{region}.amazonaws.com/openai/v1/chat/completions`
//! authenticated with a Bedrock API key as `Authorization: Bearer` (checked 2026-11-03,
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/inference-chat-completions.html>). That is
//! the existing OpenAI dialect at a regional base, so this is a preset, not a new wire. The native
//! Converse API is [`Endpoint::bedrock_converse`] (#1150), with the same key and Region. SigV4 is
//! not spoken; no AWS credential discovery happens here: the key is whatever the caller read from
//! its secret store.

use super::{ApiKey, Endpoint};
use crate::{ErrorKind, ProviderError};

/// The provider id, scheduler lane and keychain account.
pub const BEDROCK: &str = "bedrock";

/// Longest region code accepted (`ap-southeast-4` is 14; GovCloud codes are longer).
const MAX_REGION: usize = 32;

/// Shared by settings readiness and the preset. A region is a code such as `us-east-1` or
/// `us-gov-west-1`: lowercase ASCII words joined by hyphens, ending in a number. Errors never
/// echo the supplied value.
pub fn validate_bedrock_region(region: &str) -> Result<(), ProviderError> {
    let bad = || ProviderError::new(ErrorKind::UnsupportedInput, "set bedrock_region to an AWS Region code such as us-east-1");
    if region.is_empty() || region.len() > MAX_REGION {
        return Err(bad());
    }
    let parts: Vec<&str> = region.split('-').collect();
    let Some((last, words)) = parts.split_last() else { return Err(bad()) };
    if words.len() < 2
        || last.is_empty()
        || last.len() > 2
        || !last.bytes().all(|c| c.is_ascii_digit())
        || words.iter().any(|w| w.is_empty() || !w.bytes().all(|c| c.is_ascii_lowercase()))
    {
        return Err(bad());
    }
    // China (`amazonaws.com.cn`), the European Sovereign Cloud and the isolated US partitions are
    // not served from `amazonaws.com`, which is the only domain this preset builds.
    let first = words.first().copied().unwrap_or_default();
    let isolated = words.get(..2).is_some_and(|prefix| matches!(prefix, ["us", "iso" | "isob" | "isof"]));
    if matches!(first, "cn" | "eusc") || isolated {
        return Err(ProviderError::new(
            ErrorKind::UnsupportedInput,
            "the Bedrock preset supports AWS Regions on amazonaws.com only, not China, European Sovereign Cloud or isolated partitions",
        ));
    }
    Ok(())
}

impl Endpoint {
    /// Bedrock's `bedrock-runtime` OpenAI-compatible base in `region`, with a Bedrock API key.
    /// The request's model is a Bedrock model id such as `openai.gpt-oss-120b-1:0`, unchanged.
    pub fn bedrock(region: &str, key: ApiKey) -> Result<Self, ProviderError> {
        let region = region.trim();
        validate_bedrock_region(region)?;
        Ok(Self::openai_compatible(BEDROCK, &format!("https://bedrock-runtime.{region}.amazonaws.com/openai/v1"), Some(key)))
    }

    /// Bedrock's native Converse API on `bedrock-runtime` in `region`, with a Bedrock API key as
    /// `Authorization: Bearer` (#1150). The request's model is a Bedrock model id, inference
    /// profile id or ARN, such as `us.anthropic.claude-sonnet-4-6`, sent in the path.
    pub fn bedrock_converse(region: &str, key: ApiKey) -> Result<Self, ProviderError> {
        let region = region.trim();
        validate_bedrock_region(region)?;
        Ok(Self { id: BEDROCK.into(), dialect: super::Dialect::Converse, base_url: format!("https://bedrock-runtime.{region}.amazonaws.com"), key: Some(key) })
    }
}
