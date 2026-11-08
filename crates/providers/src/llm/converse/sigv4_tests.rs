//! Converse with SigV4-signed AWS credentials (#1163), against the mock transport: never AWS.

use super::tests::{Chunked, exception, request, tool_stream};
use super::*;
use crate::aws::AwsCredentials;
use crate::http::{Http, HttpResponse};
use crate::llm::tests::Canned;
use crate::llm::{BedrockAuth, LlmClient};
use crate::provider::BoxFuture;
use futures::executor::block_on;
use std::sync::Arc;

const ACCESS: &str = "AKIDEXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9SAPBSM22wDOk4x4HIZ8j4FZTwdQWLWsKWHGBuFqwAeMicRXmxfpSPfIeoIYRqTflfKD8YUuwthAx7mSEI/qkPpKPi/kMcGdQrmGdeehM4IC1NtBmUpp2wUE8phUZampKsburEDy0KPkyQDYwT7WZ0wq5VSXDvp75YU9HFvlRd8Tx6q6fE8YQcHNVXAkiY9q6d+xo0rKwT38xVqr7ZD0u0iPPkUL64lIZbqBAz+scqKmlzm8FDrypNC9Yjc8fPOLn9FX9KSYvKTr4rvx3iSIlTJabIQwj2ICCR/oLxBA==";

fn aws(token: Option<&str>) -> BedrockAuth {
    BedrockAuth::Aws(AwsCredentials::new(ACCESS, SECRET, token).unwrap())
}

fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// The signature headers a request carries, checked for shape: the exact signature is the
/// signer's own test, with a fixed clock.
fn assert_signed(sent: &HttpRequest, path: &str, token: bool) {
    assert_eq!(sent.url, format!("https://bedrock-runtime.us-west-2.amazonaws.com{path}"));
    let authorization = header(sent, "authorization").unwrap();
    let signed_headers = if token {
        "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token,"
    } else {
        "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date,"
    };
    assert!(authorization.starts_with(&format!("AWS4-HMAC-SHA256 Credential={ACCESS}/")), "{authorization}");
    assert!(authorization.contains("/us-west-2/bedrock/aws4_request, ") && authorization.contains(signed_headers), "{authorization}");
    assert!(!authorization.contains("Bearer") && !authorization.contains(SECRET));
    assert_eq!(header(sent, "x-amz-content-sha256"), Some(crate::aws::sigv4::sha256_hex(&sent.body).as_str()), "the hashed payload");
    assert_eq!(header(sent, "x-amz-date").map(str::len), Some(16));
    assert_eq!(header(sent, "x-amz-security-token"), token.then_some(TOKEN));
    assert!(sent.secret_headers.iter().any(|n| n == "authorization"));
}

#[test]
fn whole_converse_replies_work_with_signed_aws_credentials() {
    for token in [None, Some(TOKEN)] {
        let http =
            Canned::new(200, r#"{"output":{"message":{"content":[{"text":"Hi."}]}},"stopReason":"end_turn","usage":{"inputTokens":3,"outputTokens":2}}"#);
        let client = LlmClient::bedrock_converse("us-west-2", aws(token), http.clone()).unwrap();
        assert_eq!(client.endpoint().key, None, "no bearer key");
        let mut req = request();
        req.model = "us.anthropic.claude-haiku-4-5-20251001-v1:0".into();
        let out = block_on(client.chat(&req)).unwrap();
        assert_eq!((out.text.as_str(), out.stop), ("Hi.", StopReason::EndTurn));
        assert_signed(&http.request(), "/model/us.anthropic.claude-haiku-4-5-20251001-v1%3A0/converse", token.is_some());
        let printed = format!("{client:?} {:?}", http.request());
        assert!(!printed.contains(SECRET) && !printed.contains(TOKEN), "{printed}");
    }
}

#[test]
fn streamed_converse_replies_work_with_signed_aws_credentials() {
    let http = Chunked::new(tool_stream(), 7);
    let client = LlmClient::bedrock_converse("us-west-2", aws(Some(TOKEN)), http.clone()).unwrap();
    let mut pieces = String::new();
    let out = block_on(client.chat_stream(&request(), &mut |p| pieces.push_str(p))).unwrap();
    assert_eq!((pieces.as_str(), out.stop), ("Checking.", StopReason::ToolUse));
    let sent = http.seen.lock().unwrap()[0].clone();
    assert_signed(&sent, "/model/us.anthropic.claude-sonnet-4-6/converse-stream", true);
    assert_eq!(header(&sent, "accept"), Some("application/vnd.amazon.eventstream"));
}

#[test]
fn the_openai_route_signs_too_and_api_keys_stay_bearer() {
    let http = Canned::new(
        200,
        r#"{"id":"x","object":"chat.completion","created":0,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
    );
    let client = LlmClient::bedrock_chat("us-west-2", aws(None), http.clone()).unwrap();
    assert_eq!((client.endpoint().id.as_str(), client.endpoint().dialect), ("bedrock", Dialect::OpenAi));
    block_on(client.chat(&request())).unwrap();
    let sent = http.request();
    assert_eq!(sent.url, "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1/chat/completions");
    assert!(header(&sent, "authorization").unwrap().starts_with("AWS4-HMAC-SHA256 "));
    // With an API key nothing is signed, on either route.
    for converse in [false, true] {
        let http = Canned::new(200, "{}");
        let key = BedrockAuth::ApiKey(crate::llm::ApiKey::new("bedrock-public-fixture-key"));
        let client =
            if converse { LlmClient::bedrock_converse("us-west-2", key, http.clone()) } else { LlmClient::bedrock_chat("us-west-2", key, http.clone()) };
        let _ = block_on(client.unwrap().chat(&request()));
        let sent = http.request();
        assert_eq!(header(&sent, "authorization"), Some("Bearer bedrock-public-fixture-key"));
        assert!(!sent.headers.iter().any(|(n, _)| n.to_ascii_lowercase().starts_with("x-amz")));
    }
    for region in ["", "cn-north-1", "us-east-1.evil.example"] {
        assert!(LlmClient::bedrock_converse(region, aws(None), http.clone()).is_err(), "{region}");
    }
}

#[test]
fn signature_clock_and_expiry_refusals_are_actionable_and_quote_nothing() {
    let quoted = format!(
        "The request signature we calculated does not match the signature you provided. The Canonical String for this request should have been 'x-amz-security-token:{TOKEN}'"
    );
    for (name, message, says) in [
        ("SignatureDoesNotMatch", quoted.as_str(), BAD_SIGNATURE),
        ("InvalidSignatureException", "Signature expired: 20150830T123600Z is now earlier than 20150830T124600Z (20150830T125100Z - 5 min.)", SKEWED),
        ("RequestTimeTooSkewed", "The difference between the request time and the current time is too large.", SKEWED),
        ("ExpiredTokenException", "The security token included in the request is expired", EXPIRED),
        ("UnrecognizedClientException", "The security token included in the request is invalid.", UNRECOGNISED),
        ("RequestExpired", "Request has expired.", SKEWED),
        ("InvalidSignatureException", "Signature not yet current: 20150830T123600Z is still later than 20150830T122100Z (20150830T121600Z + 5 min.)", SKEWED),
    ] {
        let body = json!({ "message": message }).to_string();
        let http =
            Canned::with_headers(403, &body, vec![("x-amzn-errortype".into(), format!("{name}:http://internal.amazon.com/coral/com.amazon.coral.service/"))]);
        let client = LlmClient::bedrock_converse("us-west-2", aws(Some(TOKEN)), http).unwrap();
        let error = block_on(client.chat(&request())).unwrap_err();
        assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Auth, says), "{name}");
        // The same names inside a stream.
        let client = LlmClient::bedrock_converse("us-west-2", aws(Some(TOKEN)), Chunked::new(exception(name, message), 9)).unwrap();
        let error = block_on(client.chat_stream(&request(), &mut |_| {})).unwrap_err();
        assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Auth, says), "{name} streamed");
    }
}

/// The OpenAI-compatible route (the one the catalog's cards use) answers credential refusals in
/// AWS's shape too: they get the same words, not AWS's text, whole and streamed.
#[test]
fn the_openai_route_maps_aws_credential_refusals_the_same_way() {
    let quoted =
        format!("The request signature we calculated does not match. The Canonical String for this request should have been 'x-amz-security-token:{TOKEN}'");
    for (name, in_body, says) in [
        ("SignatureDoesNotMatch", false, BAD_SIGNATURE),
        ("RequestTimeTooSkewed", true, SKEWED),
        ("ExpiredTokenException", false, EXPIRED),
        ("UnrecognizedClientException", true, UNRECOGNISED),
    ] {
        // The name in `x-amzn-ErrorType`, or only in the body's `__type`.
        let (headers, body) = if in_body {
            (vec![], json!({ "__type": format!("com.amazon.coral.service#{name}"), "message": quoted }).to_string())
        } else {
            (
                vec![("x-amzn-errortype".to_owned(), format!("{name}:http://internal.amazon.com/coral/com.amazon.coral.service/"))],
                json!({ "message": quoted }).to_string(),
            )
        };
        for streamed in [false, true] {
            let http = Canned::with_headers(403, &body, headers.clone());
            let client = LlmClient::bedrock_chat("us-west-2", aws(Some(TOKEN)), http).unwrap();
            let error = if streamed { block_on(client.chat_stream(&request(), &mut |_| {})) } else { block_on(client.chat(&request())) }.unwrap_err();
            assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Auth, says), "{name} streamed={streamed}");
        }
    }
    // Other Bedrock errors on that route, and other OpenAI-compatible servers, read as before.
    let body = json!({ "error": { "message": "slow down", "code": "rate_limit_exceeded" } }).to_string();
    let error = block_on(LlmClient::bedrock_chat("us-west-2", aws(None), Canned::new(429, &body)).unwrap().chat(&request())).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (ErrorKind::RateLimited, "slow down"));
    let named = vec![("x-amzn-errortype".to_owned(), "SignatureDoesNotMatch".to_owned())];
    let other = crate::llm::Endpoint::openai_compatible("vllm", "https://gateway.example/v1", None);
    let error = block_on(LlmClient::new(other, Canned::with_headers(403, r#"{"error":{"message":"theirs"}}"#, named)).chat(&request())).unwrap_err();
    assert_eq!(error.message, "theirs");
}

mod delivery {
    use super::*;
    use crate::{CancelSignal, Clock, JobInput, ProviderJob, RunLimits};

    struct Never;
    impl CancelSignal for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(futures::future::pending())
        }
    }
    struct Instant;
    impl Clock for Instant {
        fn now_ms(&self) -> u64 {
            0
        }
        fn sleep(&self, _: u64) -> BoxFuture<'static, ()> {
            Box::pin(async {})
        }
    }

    fn job() -> ProviderJob {
        ProviderJob {
            model: "us.anthropic.claude-sonnet-4-6".into(),
            params: [("max_tokens".into(), json!(64))].into_iter().collect(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "A harbour".into() }],
            idempotency_key: "public-fixture-1163".into(),
        }
    }

    /// Counts what reaches the network.
    struct Counting(std::sync::Mutex<usize>);
    impl Http for Counting {
        fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            *self.0.lock().unwrap() += 1;
            Box::pin(async { Ok(HttpResponse { status: 500, headers: vec![], body: vec![] }) })
        }
    }

    #[test]
    fn expired_credentials_are_refused_unsent_and_a_sent_request_stays_possibly_billed() {
        let now = crate::aws::system_seconds().unwrap();
        for stream_text in [false, true] {
            let expired = AwsCredentials::new(ACCESS, SECRET, Some(TOKEN)).unwrap().with_expiry(now.saturating_sub(1));
            let http = Arc::new(Counting(std::sync::Mutex::new(0)));
            let client = LlmClient::bedrock_converse("us-west-2", BedrockAuth::Aws(expired), http.clone()).unwrap();
            let limits = RunLimits { stream_text, ..RunLimits::default() };
            let f = block_on(crate::run_to_completion(&client, &job(), &Never, &Instant, limits, |_| {})).unwrap_err();
            assert_eq!(f.error.kind, ErrorKind::Auth, "{f:?}");
            assert!(!f.uncertain, "refused before sending: certainly not billed ({f:?})");
            assert_eq!(*http.0.lock().unwrap(), 0);
        }
        // A signed request that went out and failed may have been billed, and is never resent.
        let http = Arc::new(Counting(std::sync::Mutex::new(0)));
        let client = LlmClient::bedrock_converse("us-west-2", aws(None), http.clone()).unwrap();
        let f = block_on(crate::run_to_completion(&client, &job(), &Never, &Instant, RunLimits::default(), |_| {})).unwrap_err();
        assert!(f.uncertain, "{f:?}");
        assert_eq!(*http.0.lock().unwrap(), 1);
    }
}
