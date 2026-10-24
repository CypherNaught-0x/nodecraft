//! Synthetic API contracts: no network, recordings, secrets or paid requests.
use super::*;
use crate::{HttpRequest, HttpResponse, JobInput, Output};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::sync::Mutex;
const KEY: &str = "fixture-key-not-real";
const MP3: &[u8] = b"ID3\x04\0\0\0\0\0\0\xFF\xFB\x90\x64\0\0\0\0";
const WAV: &[u8] = include_bytes!("../fal/fixtures/tone.wav");
struct Canned {
    reply: Mutex<Option<Result<HttpResponse, ProviderError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Canned {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable transport forbidden")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.reply.lock().unwrap().take().expect("no transparent paid retry");
        Box::pin(async move { reply })
    }
}
fn job(model: &str) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: Default::default(),
        inputs: if model == SPEECH {
            vec![JobInput::Text { role: "prompt".into(), text: "Hello, 雪.".into() }]
        } else {
            vec![JobInput::Media { role: "audio".into(), mime: "audio/wav".into(), bytes: Arc::from(WAV) }]
        },
        idempotency_key: "no-remote-dedup-guarantee".into(),
    }
}
fn reply(status: u16, body: &[u8]) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status, body: body.to_vec(), headers: vec![] })
}
fn run(job: &ProviderJob, reply: Result<HttpResponse, ProviderError>) -> (Result<Submission, ProviderError>, Arc<Canned>, SubmitContext) {
    let http = Arc::new(Canned { reply: Mutex::new(Some(reply)), requests: Mutex::default() });
    let provider = OpenAiAudio::new(Secret::new(KEY).unwrap(), http.clone());
    assert!(!provider.retry_ambiguous_submit());
    let context = SubmitContext::new();
    (block_on(provider.submit_with_delivery(job, &context)), http, context)
}
fn done(result: Result<Submission, ProviderError>) -> Completed {
    let Submission::Finished(done) = result.unwrap() else { panic!("synchronous") };
    done
}
#[test]
fn speech_contract_keeps_token_cost_unknown_and_marks_auth_secret() {
    let mut job = job(SPEECH);
    job.params = json!({"voice":"cedar","speed":1.25,"instructions":"Speak gently"}).as_object().unwrap().clone();
    let (result, http, _) = run(&job, reply(200, MP3));
    let done = done(result);
    assert!(matches!(done.outputs.as_slice(), [Output::Bytes { mime, bytes }] if mime == "audio/mpeg" && bytes.as_ref() == MP3));
    assert_eq!(done.usage.cost, None);
    let requests = http.requests.lock().unwrap();
    let [request] = requests.as_slice() else { panic!("one request") };
    assert_eq!(request.url, "https://api.openai.com/v1/audio/speech");
    assert_eq!(request.max_response_bytes, OUTPUT_BYTES);
    assert!(request.secret_headers.contains(&"authorization".into()));
    assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Bearer {KEY}")));
    assert!(request.headers.iter().any(|(k, v)| k == "User-Agent" && v == crate::USER_AGENT));
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({"model":SPEECH,"input":"Hello, 雪.","voice":"cedar","speed":1.25,"instructions":"Speak gently","response_format":"mp3","stream_format":"audio"})
    );
}
#[test]
fn transcription_contract_uploads_exact_audio_and_uses_reported_duration() {
    let mut job = job(TRANSCRIBE);
    job.inputs.push(JobInput::Text { role: "prompt".into(), text: "NodeCraft vocabulary".into() });
    let (result, http, _) = run(&job, reply(200, br#"{"text":"Hello, world.","languages":[{"code":"en"}],"usage":{"type":"duration","seconds":1.5}}"#));
    let done = done(result);
    assert!(matches!(done.outputs.as_slice(), [Output::Text(text)] if text == "Hello, world."));
    assert_eq!(done.usage.cost.unwrap().micros, 113);
    let requests = http.requests.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request.url, "https://api.openai.com/v1/audio/transcriptions");
    assert_eq!(request.max_response_bytes, TRANSCRIPT_BYTES);
    let wire = String::from_utf8_lossy(&request.body);
    for part in [
        "name=\"model\"\r\n\r\ngpt-transcribe",
        "name=\"response_format\"\r\n\r\njson",
        "name=\"prompt\"\r\n\r\nNodeCraft vocabulary",
        "filename=\"recording.wav\"\r\nContent-Type: audio/wav",
    ] {
        assert!(wire.contains(part), "{part}");
    }
    assert!(request.body.windows(WAV.len()).any(|part| part == WAV));
}
#[test]
fn unknown_or_hostile_usage_never_becomes_a_fabricated_price() {
    for usage in [
        Value::Null,
        json!({"type":"tokens","total_tokens":20}),
        json!({"type":"duration"}),
        json!({"type":"duration","seconds":-1}),
        json!({"type":"duration","seconds":1e300}),
    ] {
        let (result, _, _) = run(&job(TRANSCRIBE), reply(200, &serde_json::to_vec(&json!({"text":"","usage":usage})).unwrap()));
        assert_eq!(done(result).usage.cost, None);
    }
}
#[test]
fn invalid_inputs_never_cross_the_paid_boundary() {
    let mut cases = vec![];
    for params in [
        json!({"voice":"custom"}),
        json!({"voice":2}),
        json!({"speed":0.0}),
        json!({"speed":4.1}),
        json!({"speed":"fast"}),
        json!({"instructions":"a".repeat(4097)}),
        json!({"instructions":"🌊".repeat(4097)}),
    ] {
        let mut j = job(SPEECH);
        j.params = params.as_object().unwrap().clone();
        cases.push(j);
    }
    for text in [String::new(), "雪".repeat(4097)] {
        let mut j = job(SPEECH);
        j.inputs = vec![JobInput::Text { role: "prompt".into(), text }];
        cases.push(j);
    }
    for model in [SPEECH, TRANSCRIBE] {
        let mut j = job(model);
        j.inputs.clear();
        cases.push(j);
        let mut j = job(model);
        j.inputs.push(j.inputs[0].clone());
        cases.push(j);
    }
    for (mime, bytes) in [("audio/wav", &b"not audio"[..]), ("audio/mpeg", WAV), ("image/png", b"\x89PNG\r\n\x1a\n")] {
        let mut j = job(TRANSCRIBE);
        j.inputs = vec![JobInput::Media { role: "audio".into(), mime: mime.into(), bytes: Arc::from(bytes) }];
        cases.push(j);
    }
    let mut realtime = job(SPEECH);
    realtime.model = "gpt-realtime-2.1-mini".into();
    cases.push(realtime);
    for j in cases {
        let (result, http, context) = run(&j, reply(200, MP3));
        assert_eq!(result.unwrap_err().kind, ErrorKind::UnsupportedInput);
        assert!(http.requests.lock().unwrap().is_empty());
        assert!(context.finish_error().is_some());
    }
    let mut j = job(SPEECH);
    j.inputs = vec![JobInput::Text { role: "prompt".into(), text: "雪".repeat(4096) }];
    j.params.insert("instructions".into(), json!("🌊".repeat(4096)));
    done(run(&j, reply(200, MP3)).0);
    let mut j = job(TRANSCRIBE);
    j.inputs = vec![JobInput::Media { role: "audio".into(), mime: "audio/wav".into(), bytes: vec![0; INPUT_BYTES + 1].into() }];
    let (result, http, context) = run(&j, reply(200, MP3));
    assert_eq!(result.unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(http.requests.lock().unwrap().is_empty());
    assert!(context.finish_error().is_some());
}
#[test]
fn a_context_cannot_inject_multipart_fields() {
    let first = request::prepare(&job(TRANSCRIBE)).unwrap();
    let boundary = first.headers.iter().find(|(k, _)| k == "Content-Type").unwrap().1.split("boundary=").nth(1).unwrap();
    let mut j = job(TRANSCRIBE);
    j.inputs.push(JobInput::Text { role: "prompt".into(), text: format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nother") });
    let (result, http, context) = run(&j, reply(200, MP3));
    assert_eq!(result.unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(http.requests.lock().unwrap().is_empty());
    assert!(context.finish_error().is_some());
}
#[test]
fn parsed_refusals_release_evidence_and_unknown_failures_stay_possibly_billed() {
    for (status, code, error_type, kind, refused) in [
        (401, "invalid_api_key", None, ErrorKind::Auth, true),
        (403, "unsupported_country_region_territory", None, ErrorKind::Auth, true),
        (400, "invalid_parameter", Some("invalid_request_error"), ErrorKind::UnsupportedInput, true),
        (400, "content_policy_violation", None, ErrorKind::BlockedContent, false),
        (400, "content_filter", Some("invalid_request_error"), ErrorKind::BlockedContent, false),
        (429, "insufficient_quota", None, ErrorKind::Budget, true),
        (429, "credit_balance_exhausted", None, ErrorKind::Budget, true),
        (429, "organization_spend_limit_exceeded", None, ErrorKind::Budget, true),
        (429, "project_spend_limit_exceeded", None, ErrorKind::Budget, true),
        (429, "organization_usage_limit_exceeded", None, ErrorKind::Budget, true),
        (429, "future_billing_code", Some("insufficient_quota"), ErrorKind::Budget, true),
        (429, "rate_limit_exceeded", None, ErrorKind::RateLimited, true),
        (429, "slow_down", Some("rate_limit_error"), ErrorKind::RateLimited, true),
        (429, "unknown", None, ErrorKind::RateLimited, false),
        (500, "insufficient_quota", Some("insufficient_quota"), ErrorKind::Server, false),
        (302, "redirect", None, ErrorKind::Malformed, false),
    ] {
        let body = serde_json::to_vec(&json!({"error":{"code":code,"type":error_type,"message":KEY}})).unwrap();
        let (result, http, context) = run(&job(SPEECH), reply(status, &body));
        let error = result.unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(!error.message.contains(KEY));
        assert_eq!(context.finish_error().is_some(), refused, "{status} {code}");
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }
    let (result, http, context) = run(&job(SPEECH), Err(ProviderError::new(ErrorKind::Network, KEY)));
    assert!(!result.unwrap_err().message.contains(KEY));
    assert!(context.finish_error().is_none());
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    for status in [400, 401, 403, 429, 500] {
        for body in [b"not json".as_slice(), b"{}", br#"{"error":{}}"#, br#"{"error":{"code":123}}"#] {
            let (_, _, context) = run(&job(SPEECH), reply(status, body));
            assert!(context.finish_error().is_none(), "unparsed or unclassified HTTP {status} stays uncertain");
        }
    }
    let mut oversized = br#"{"error":{"code":"invalid_api_key"}}"#.to_vec();
    oversized.resize(TRANSCRIPT_BYTES + 1, b' ');
    let (_, _, context) = run(&job(SPEECH), reply(401, &oversized));
    assert!(context.finish_error().is_none(), "an oversized refusal is not delivery evidence");
}
#[test]
fn malformed_and_oversized_replies_cannot_be_imported() {
    for (model, body) in
        [(SPEECH, b"not mp3".to_vec()), (TRANSCRIBE, b"{}".to_vec()), (TRANSCRIBE, vec![0; TRANSCRIPT_BYTES + 1]), (SPEECH, vec![0; OUTPUT_BYTES + 1])]
    {
        assert_eq!(run(&job(model), reply(200, &body)).0.unwrap_err().kind, ErrorKind::Malformed);
    }
}

#[test]
fn synchronous_lifecycle_and_retry_hints_are_bounded() {
    let http = Arc::new(Canned { reply: Mutex::new(None), requests: Mutex::default() });
    let provider = OpenAiAudio::new(Secret::new(KEY).unwrap(), http.clone());
    let handle = JobHandle { remote_id: "unused".into(), data: Value::Null };
    assert_eq!(provider.id(), "openai-audio");
    assert_eq!(block_on(provider.poll(&handle)).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(block_on(provider.cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(http.requests.lock().unwrap().is_empty());
    for (status, kind) in [(400, ErrorKind::UnsupportedInput), (404, ErrorKind::NotFound), (503, ErrorKind::Busy), (504, ErrorKind::Timeout)] {
        assert_eq!(run(&job(SPEECH), reply(status, b"{}")).0.unwrap_err().kind, kind);
    }
    for (hint, expected) in [("7", Some(7000)), ("3600", Some(3_600_000)), ("3601", None), ("18446744073709551615", None), ("soon", None)] {
        let response = HttpResponse { status: 429, headers: vec![("Retry-After".into(), hint.into())], body: b"{}".to_vec() };
        assert_eq!(run(&job(SPEECH), Ok(response)).0.unwrap_err().retry_after_ms, expected);
    }
}
