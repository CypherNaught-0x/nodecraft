use super::*;
use crate::{HttpResponse, Output};
use futures::executor::block_on;
use serde_json::json;
use std::sync::Mutex;

const WAV: &[u8] = include_bytes!("../fal/fixtures/tone.wav");
const KEY: &str = "public-deepgram-fixture";
fn job() -> ProviderJob {
    ProviderJob {
        model: MODEL.into(),
        params: Default::default(),
        inputs: vec![JobInput::Media { role: "audio".into(), mime: "audio/wav".into(), bytes: Arc::from(WAV) }],
        idempotency_key: "public-key".into(),
    }
}
fn reply(text: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: vec![],
        body: serde_json::to_vec(&json!({"metadata":{"duration":1.5},"results":{"channels":[{"alternatives":[{"transcript":text,"words":[]}]}]}})).unwrap(),
    }
}
struct Wire {
    response: Result<HttpResponse, ProviderError>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Wire {
    fn new(response: Result<HttpResponse, ProviderError>) -> Arc<Self> {
        Arc::new(Self { response, requests: Mutex::default() })
    }
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable transport must not be called")
    }
    fn send_no_redirect(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(req);
        Box::pin(async { self.response.clone() })
    }
}
fn provider(http: Arc<Wire>) -> Deepgram {
    Deepgram::new(Secret::new(KEY).unwrap(), http)
}

#[test]
fn deepgram40_binary_request_and_transcript_preserve_unknown_cost() {
    for text in ["A public harbour.", ""] {
        let http = Wire::new(Ok(reply(text)));
        let p = provider(http.clone());
        let Submission::Finished(done) = block_on(p.submit(&job())).unwrap() else { panic!("sync") };
        assert_eq!(done.outputs, [Output::Text(text.into())]);
        assert_eq!(done.usage, crate::Usage::default());
        let requests = http.requests.lock().unwrap();
        let request = requests.first().unwrap();
        assert_eq!(request.url, URL);
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.body, WAV);
        assert_eq!(request.max_response_bytes, MAX_RESPONSE_BYTES);
        assert_eq!(request.secret_headers, ["authorization"]);
        assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Token {KEY}")));
        assert!(request.headers.iter().any(|(k, v)| k == "User-Agent" && v == crate::USER_AGENT));
        assert!(!format!("{request:?}").contains(KEY));
        assert!(!p.retry_ambiguous_submit());
    }
}

#[test]
fn deepgram40_local_invalid_input_proves_no_submit() {
    let mut cases = vec![];
    let mut j = job();
    j.model = "nova-3&callback=private".into();
    cases.push(j);
    let mut j = job();
    j.params.insert("callback".into(), json!("https://example.test"));
    cases.push(j);
    let mut j = job();
    j.idempotency_key = "k".repeat(257);
    cases.push(j);
    let mut j = job();
    j.inputs.clear();
    cases.push(j);
    let mut j = job();
    j.inputs.push(JobInput::Text { role: "prompt".into(), text: "x".into() });
    cases.push(j);
    for (role, mime, bytes) in [
        ("other", "audio/wav", WAV.to_vec()),
        ("audio", "image/png", WAV.to_vec()),
        ("audio", "audio/wav", b"RIFFbad".to_vec()),
        ("audio", "audio/wav", vec![0; MAX_INPUT_BYTES + 1]),
    ] {
        let mut j = job();
        j.inputs = vec![JobInput::Media { role: role.into(), mime: mime.into(), bytes: Arc::from(bytes) }];
        cases.push(j);
    }
    let http = Wire::new(Ok(reply("hello")));
    let p = provider(http.clone());
    for j in cases {
        let scope = SubmitContext::new();
        assert_eq!(block_on(p.submit_with_delivery(&j, &scope)).unwrap_err().kind, ErrorKind::UnsupportedInput);
        assert!(scope.finish_error().is_some());
    }
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn deepgram40_http_errors_are_bounded_and_never_echo_secrets() {
    for (status, kind) in [
        (202, ErrorKind::Malformed),
        (302, ErrorKind::Malformed),
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (403, ErrorKind::Auth),
        (404, ErrorKind::NotFound),
        (408, ErrorKind::Timeout),
        (422, ErrorKind::UnsupportedInput),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Server),
        (503, ErrorKind::Busy),
    ] {
        let http =
            Wire::new(Ok(HttpResponse { status, headers: vec![("retry-after".into(), "2".into())], body: format!("{{\"err_msg\":\"{KEY}\"}}").into_bytes() }));
        let scope = SubmitContext::new();
        let e = block_on(provider(http).submit_with_delivery(&job(), &scope)).unwrap_err();
        assert_eq!(e.kind, kind, "{status}");
        assert_eq!(e.retry_after_ms, Some(2000));
        assert!(!e.message.contains(KEY));
        assert!(scope.finish_error().is_none(), "HTTP status alone does not prove nonbilling");
    }
    for kind in [ErrorKind::Network, ErrorKind::Timeout, ErrorKind::BlockedContent, ErrorKind::Cancelled] {
        let http = Wire::new(Err(ProviderError::new(kind, KEY)));
        let e = block_on(provider(http).submit(&job())).unwrap_err();
        assert_eq!(e.kind, kind);
        assert!(!e.message.contains(KEY));
    }
}

#[test]
fn deepgram40_malformed_transcripts_are_not_published() {
    for body in [
        b"not JSON".to_vec(),
        br#"{"results":{"channels":[]}}"#.to_vec(),
        br#"{"results":{"channels":[{"alternatives":[]}]}}"#.to_vec(),
        br#"{"results":{"channels":[{"alternatives":[{"transcript":"one","transcript":"two"}]}]}}"#.to_vec(),
        reply(KEY).body,
        reply(&"x".repeat(MAX_TRANSCRIPT_BYTES + 1)).body,
        vec![b' '; MAX_RESPONSE_BYTES + 1],
    ] {
        let http = Wire::new(Ok(HttpResponse { status: 200, headers: vec![], body }));
        assert_eq!(block_on(provider(http).submit(&job())).unwrap_err().kind, ErrorKind::Malformed);
    }
}

#[test]
fn deepgram40_lost_reply_and_cancellation_never_claim_remote_stop() {
    let http = Wire::new(Err(ProviderError::new(ErrorKind::Network, KEY)));
    let p = provider(http.clone());
    let scope = SubmitContext::new();
    assert!(block_on(p.submit_with_delivery(&job(), &scope)).is_err());
    assert!(scope.finish_error().is_none());
    let handle = JobHandle { remote_id: "public".into(), data: serde_json::Value::Null };
    assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(block_on(p.cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn deepgram40_catalog_requires_audio_and_unknown_price_acceptance() {
    use nodecraft_catalog::{Catalog, Pricing, Wiring};
    use nodecraft_types::Modality;
    let catalog = Catalog::builtin().unwrap();
    let model = catalog.get("deepgram.nova-3").unwrap();
    assert_eq!(model.provider_model, MODEL);
    assert!(matches!(model.pricing, Pricing::ByUsage { .. }));
    assert!(model.usage.is_none());
    assert!(Wiring::new(Modality::Text).with(Modality::Audio, 1).fit(model).is_ok());
    assert!(Wiring::new(Modality::Text).with_inline_prompt().fit(model).is_err());
    assert!(Wiring::new(Modality::Text).with(Modality::Audio, 2).fit(model).is_err());
}

#[test]
fn deepgram40_documented_refusals_prove_no_charge_but_unknown_replies_do_not() {
    for (status, code, kind) in [
        (401, "INVALID_AUTH", ErrorKind::Auth),
        (403, "INSUFFICIENT_PERMISSIONS", ErrorKind::Auth),
        (402, "ASR_PAYMENT_REQUIRED", ErrorKind::Budget),
        (400, "Bad Request", ErrorKind::UnsupportedInput),
        (400, "INVALID_AUDIO", ErrorKind::UnsupportedInput),
        (422, "ASR_UNPROCESSABLE_ENTITY", ErrorKind::UnsupportedInput),
        (429, "TOO_MANY_REQUESTS", ErrorKind::RateLimited),
    ] {
        let scope = SubmitContext::new();
        let body = serde_json::to_vec(&json!({"err_code":code,"err_msg":format!("Private error {KEY}\n"),"request_id":"public-request"})).unwrap();
        let http = Wire::new(Ok(HttpResponse { status, headers: vec![("Retry-After".into(), "2".into())], body }));
        let error = block_on(provider(http).submit_with_delivery(&job(), &scope)).unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.retry_after_ms, Some(2000));
        assert!(!error.message.contains(KEY));
        assert!(scope.finish_error().is_some(), "{status}/{code} proves refusal before processing");
    }
    for (status, body) in [
        (429, b"not JSON".to_vec()),
        (429, br#"{"err_code":"TOO_MANY_REQUESTS"}"#.to_vec()),
        (429, br#"{"err_code":"TOO_MANY_REQUESTS","err_msg":"rate limit","results":{}}"#.to_vec()),
        (429, br#"{"err_code":"TOO_MANY_REQUESTS","err_code":"TOO_MANY_REQUESTS","err_msg":"rate limit"}"#.to_vec()),
        (429, br#"{"err_code":"UNKNOWN_CODE","err_msg":"unknown"}"#.to_vec()),
        (401, br#"{"err_code":"TOO_MANY_REQUESTS","err_msg":"wrong status"}"#.to_vec()),
        (500, br#"{"err_code":"TOO_MANY_REQUESTS","err_msg":"server failure"}"#.to_vec()),
        (429, vec![b' '; MAX_RESPONSE_BYTES + 1]),
    ] {
        let scope = SubmitContext::new();
        let http = Wire::new(Ok(HttpResponse { status, headers: vec![], body }));
        assert!(block_on(provider(http).submit_with_delivery(&job(), &scope)).is_err());
        assert!(scope.finish_error().is_none(), "unknown/oversized replies stay possibly billed");
    }
}
