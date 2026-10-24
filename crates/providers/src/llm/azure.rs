//! Azure OpenAI's GA v1 API uses the existing OpenAI Chat Completions dialect (#45).

use super::{ApiKey, ChatRequest, Endpoint, LlmClient, chat_request};
use crate::{ErrorKind, ProviderError, ProviderJob};

impl LlmClient {
    /// Only the generic Azure catalog card uses a parameter as its deployment. Other host
    /// catalogs and providers keep their literal provider_model; no cross-provider override.
    pub(super) fn provider_request(&self, job: &ProviderJob) -> Result<ChatRequest, ProviderError> {
        let mut request = chat_request(job)?;
        if self.endpoint.id == "azure-openai" {
            let deployment = if job.model == "nodecraft-deployment" {
                job.params.get("deployment").and_then(serde_json::Value::as_str).unwrap_or_default()
            } else {
                &job.model
            };
            if deployment.is_empty() || deployment.len() > 64 || !deployment.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')) {
                return Err(ProviderError::new(
                    ErrorKind::UnsupportedInput,
                    "set an Azure deployment name: 1–64 letters, digits, hyphens, underscores or dots",
                ));
            }
            request.model = deployment.to_owned();
        }
        Ok(request)
    }
}

impl Endpoint {
    /// Azure resource root or its `/openai/v1` base. The request's model is the deployment
    /// name, unchanged. API keys come from the caller's secret store; Entra login and the
    /// older dated deployment API are not supported by this preset.
    pub fn azure_openai(base: &str, key: ApiKey) -> Result<Self, ProviderError> {
        let base = base.trim();
        let bad =
            || ProviderError::new(ErrorKind::UnsupportedInput, "set an HTTPS Azure resource root or /openai/v1 address without credentials, query or fragment");
        if base.len() > 2048 || base.chars().any(char::is_control) {
            return Err(bad());
        }
        let mut url = url::Url::parse(base).map_err(|_| bad())?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(bad());
        }
        match url.path().trim_end_matches('/') {
            "" | "/openai/v1" => url.set_path("/openai/v1"),
            _ => return Err(bad()),
        }
        Ok(Self::openai_compatible("azure-openai", url.as_str(), Some(key)))
    }
}
