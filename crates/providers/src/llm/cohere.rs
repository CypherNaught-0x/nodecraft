//! Cohere's native v2 chat through rig, with NodeCraft's transport and reply limits (#1152).

use rig_core::Model;
use rig_core::providers::cohere::{CohereConfig, NativeChat};
use serde_json::Value;

use super::{ChatRequest, ChatResponse, LlmClient, MAX_REPLY_BLOCKS, openai, rig_chat};
use crate::rig_http::RigHttp;
use crate::{ErrorKind, ProviderError};

pub(super) fn error(status: u16, body: &[u8]) -> ProviderError {
    // Cohere's error body has a top-level message; the status determines the actionable kind.
    let mut error = ProviderError::from_status(status, b"");
    if let Ok(value) = serde_json::from_slice::<Value>(body)
        && let Some(message) = value.get("message").and_then(Value::as_str)
    {
        error.message = crate::error::excerpt(message.as_bytes());
    }
    error
}

const RULES: rig_chat::Rules = rig_chat::Rules {
    status: |status, _, body| error(status, body),
    event: |_| ProviderError::new(ErrorKind::Server, "Cohere returned an error event"),
    unended: "Cohere reply did not end the turn",
};

impl LlmClient {
    pub(super) async fn cohere_chat(
        &self,
        req: &ChatRequest,
        on_text: Option<&mut (dyn FnMut(&str) + Send)>,
        context: Option<&crate::SubmitContext>,
    ) -> Result<ChatResponse, ProviderError> {
        let req = self.validated(req)?;
        let (request, raw) = openai::request(&req, false, false)?;
        let stream = on_text.is_some();
        let (cap, timeout) = self.limits(&req, stream);
        let mut transport = RigHttp::new(self.http.clone(), timeout, cap)
            .with_delivery(context.map(crate::SubmitContext::link))
            .with_url(self.endpoint.url("v2/chat"))
            .with_fix(Box::new(move |body| openai::fix(body, &raw)))
            .with_guard(|| {
                Box::new(|data| {
                    let Ok(value) = serde_json::from_str::<Value>(data) else { return Ok(None) };
                    if value.get("index").and_then(Value::as_u64).is_some_and(|i| i >= MAX_REPLY_BLOCKS as u64) {
                        return Err(ProviderError::new(ErrorKind::Malformed, "the reply has too many parts or tool calls"));
                    }
                    Ok(None)
                })
            });
        if let Some(key) = &self.endpoint.key {
            transport = transport.with_secret("Authorization", format!("Bearer {}", key.expose()));
        }
        // No key, pricing catalog or environment discovery enters the wire.
        let wire = NativeChat::new(CohereConfig::new("").with_base_url(&self.endpoint.base_url), &req.model);
        let model = Model::new(wire, transport.clone()).with_idle_timeout(None);
        let response = rig_chat::send(model, &transport, request, on_text, &RULES).await?;
        openai::reply(&response, false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Canned;
    use super::super::{ApiKey, Endpoint, Message, StopReason, Tool};
    use super::*;
    use crate::http::ChunkSink;
    use crate::{BoxFuture, Http, HttpRequest, HttpResponse, Provider};
    use futures::executor::block_on;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn ask() -> ChatRequest {
        ChatRequest {
            model: "command-a-03-2025".into(),
            system: Some("Be brief".into()),
            messages: vec![Message::user("ping")],
            max_tokens: Some(64),
            temperature: None,
            tools: vec![],
        }
    }

    const REPLY: &str = r#"{"id":"r","finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"pong"}]},"usage":{"tokens":{"input_tokens":12,"output_tokens":2},"billed_units":{"input_tokens":9,"output_tokens":1}}}"#;

    #[test]
    fn native_chat_preserves_text_and_auth_on_its_fixed_route() {
        let http = Canned::new(200, REPLY);
        let client = LlmClient::new(Endpoint::cohere(ApiKey::new("fixture-secret")), http.clone());
        let reply = block_on(client.chat(&ask())).unwrap();
        assert_eq!((reply.text.as_str(), reply.stop), ("pong", StopReason::EndTurn));
        assert!(reply.usage.cost.is_none());
        let sent = http.request();
        assert_eq!(sent.url, "https://api.cohere.ai/v2/chat");
        assert!(sent.headers.contains(&("Authorization".into(), "Bearer fixture-secret".into())));
        assert!(sent.secret_headers.contains(&"authorization".into()));
        assert_eq!(http.body()["messages"][0], json!({"role":"system","content":"Be brief"}));
        assert_eq!(http.body()["messages"][1]["content"], "ping");
        assert_eq!(http.body()["max_tokens"], 64);
        assert!(!client.retry_ambiguous_submit());
        assert!(!format!("{client:?}").contains("fixture-secret"));
    }

    #[test]
    fn native_tool_calls_round_trip_and_bad_statuses_keep_their_kind() {
        let http = Canned::new(
            200,
            r#"{"finish_reason":"TOOL_CALL","message":{"role":"assistant","tool_calls":[{"id":"call_1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"Oslo\"}"}}]}}"#,
        );
        let tool = Tool {
            name: "weather".into(),
            description: "Weather".into(),
            input_schema: json!({"type":"object","properties":{"city":{"type":"string"}}}),
            strict: false,
        };
        let mut request = ChatRequest { tools: vec![tool], ..ask() };
        let reply = block_on(LlmClient::new(Endpoint::cohere(ApiKey::new("k")), http.clone()).chat(&request)).unwrap();
        assert_eq!(reply.stop, StopReason::ToolUse);
        assert_eq!(reply.tool_calls().next().unwrap().input, &json!({"city":"Oslo"}));
        assert_eq!(http.body()["tools"][0]["function"]["name"], "weather");
        request.messages.push(reply.message);
        request.messages.push(Message {
            role: super::super::Role::User,
            parts: vec![super::super::Part::ToolResult { id: "call_1".into(), content: "4 C".into(), is_error: false }],
        });
        let http = Canned::new(200, REPLY);
        block_on(LlmClient::new(Endpoint::cohere(ApiKey::new("k")), http.clone()).chat(&request)).unwrap();
        assert_eq!(http.body()["messages"][3]["role"], "tool");
        assert_eq!(http.body()["messages"][3]["tool_call_id"], "call_1");
        for (status, kind) in [(401, ErrorKind::Auth), (429, ErrorKind::RateLimited), (503, ErrorKind::Busy)] {
            let http = Canned::with_headers(status, r#"{"message":"fixture refusal"}"#, vec![("retry-after".into(), "3".into())]);
            let error = block_on(LlmClient::new(Endpoint::cohere(ApiKey::new("k")), http).chat(&ask())).unwrap_err();
            assert_eq!((error.kind, error.retry_after_ms), (kind, Some(3000)));
        }
    }

    struct Stream {
        body: Vec<u8>,
        seen: Mutex<Vec<HttpRequest>>,
    }
    impl Http for Stream {
        fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "stream expected")) })
        }
        fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
            self.seen.lock().unwrap().push(request);
            Box::pin(async move {
                for chunk in self.body.chunks(11) {
                    on_chunk(chunk)?;
                }
                Ok(HttpResponse { status: 200, headers: vec![], body: vec![] })
            })
        }
    }

    #[test]
    fn streamed_native_text_and_hostile_indices_are_bounded() {
        let events = [
            json!({"type":"message-start","id":"r","delta":{"message":{"role":"assistant"}}}),
            json!({"type":"content-start","index":0,"delta":{"message":{"content":{"type":"text","text":""}}}}),
            json!({"type":"content-delta","index":0,"delta":{"message":{"content":{"text":"pong"}}}}),
            json!({"type":"content-end","index":0}),
            json!({"type":"message-end","delta":{"finish_reason":"COMPLETE","usage":{"tokens":{"input_tokens":9,"output_tokens":1}}}}),
        ];
        let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
        let stream = Arc::new(Stream { body: body.into_bytes(), seen: Mutex::default() });
        let mut pieces = String::new();
        let reply = block_on(LlmClient::new(Endpoint::cohere(ApiKey::new("fixture")), stream).chat_stream(&ask(), &mut |s| pieces.push_str(s))).unwrap();
        assert_eq!((reply.text.as_str(), pieces.as_str()), ("pong", "pong"));
        let stream = Arc::new(Stream { body: b"data: {\"type\":\"tool-call-start\",\"index\":100000000000}\n\n".to_vec(), seen: Mutex::default() });
        let error = block_on(LlmClient::new(Endpoint::cohere(ApiKey::new("fixture")), stream).chat_stream(&ask(), &mut |_| {})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed);
    }
}
