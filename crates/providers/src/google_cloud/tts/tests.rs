use super::super::tests::{NOW, PROJECT, Transport, fixed_clock, granted, ok, stored};
use super::*;
use crate::{HttpMethod, HttpResponse, JobInput, Output};
use base64::Engine as _;
use futures::executor::block_on;
use serde_json::{Value, json};

const TOKEN: &str = "ya29.fixture-token";

fn mp3() -> Vec<u8> {
    let mut bytes = b"\xFF\xFB\x90\x64".to_vec();
    bytes.resize(64, 0);
    bytes
}
fn spoken() -> Result<HttpResponse, ProviderError> {
    ok(json!({"audioContent": base64::engine::general_purpose::STANDARD.encode(mp3())}))
}
fn job(model: &str, text: &str, params: Value) -> ProviderJob {
    let Value::Object(params) = params else { panic!("params are an object") };
    ProviderJob { model: model.into(), params, inputs: vec![JobInput::Text { role: "prompt".into(), text: text.into() }], idempotency_key: "fixture".into() }
}
fn provider(t: &Arc<Transport>) -> GoogleTts {
    GoogleTts::new(&stored(), t.clone(), fixed_clock(NOW)).unwrap()
}
fn body(r: &crate::HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn header<'a>(r: &'a crate::HttpRequest, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}
/// The synthesize request a job sends, after the token exchange.
fn sent(j: &ProviderJob) -> (Completed, crate::HttpRequest) {
    let t = Transport::new(vec![granted(TOKEN), spoken()]);
    let Submission::Finished(done) = block_on(provider(&t).submit(j)).unwrap() else { panic!("speech is synchronous") };
    let requests = t.requests();
    assert_eq!(requests.len(), 2, "one token exchange, then one synthesis");
    let (redirects, r) = requests[1].clone();
    assert!(!redirects, "the paid request never follows a redirect");
    (done, r)
}

#[test]
fn a_classic_voice_sends_the_documented_body_and_counts_the_characters() {
    let j = job(NEURAL2, "  Héllo, world!  ", json!({"voice": "en-GB-Neural2-A", "speaking_rate": 1.25, "pitch": -2.5}));
    let (done, r) = sent(&j);
    assert_eq!((r.method, r.url.as_str()), (HttpMethod::Post, "https://texttospeech.googleapis.com/v1/text:synthesize"));
    assert_eq!(
        body(&r),
        json!({"input": {"text": "Héllo, world!"}, "voice": {"languageCode": "en-GB", "name": "en-GB-Neural2-A"},
            "audioConfig": {"audioEncoding": "MP3", "speakingRate": 1.25, "pitch": -2.5}})
    );
    // The token alone names the project: a quota-project header would demand a Service Usage role.
    assert_eq!(header(&r, "x-goog-user-project"), None);
    assert_eq!(header(&r, "user-agent"), Some(crate::USER_AGENT));
    assert!(r.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {TOKEN}")));
    assert!(r.secret_headers.iter().any(|n| n == "authorization"), "the token travels only as a secret header");
    assert_eq!(done.outputs, vec![Output::Bytes { mime: MP3.into(), bytes: Arc::from(mp3()) }]);
    assert_eq!(done.usage.billable_units, nodecraft_types::BillableUnits::from_millionths(13_000_000), "13 characters after trimming");
}

#[test]
fn ssml_goes_in_its_own_field_and_its_tags_are_billed() {
    let ssml = "<speak>Hi <break time=\"1s\"/> there</speak>";
    let (done, r) = sent(&job(WAVENET, ssml, json!({"voice": "en-US-Wavenet-D", "input_format": "ssml"})));
    assert_eq!(body(&r)["input"], json!({"ssml": ssml}));
    assert_eq!(body(&r)["audioConfig"], json!({"audioEncoding": "MP3", "speakingRate": 1.0, "pitch": 0.0}), "defaults are sent");
    let chars = u64::try_from(ssml.chars().count()).unwrap();
    assert_eq!(done.usage.billable_units, nodecraft_types::BillableUnits::from_millionths(chars * 1_000_000));
}

#[test]
fn chirp_and_studio_voices_send_no_pitch_and_chirp_paces_up_to_double() {
    let (_, r) = sent(&job(CHIRP3_HD, "Hi", json!({"voice": "en-US-Chirp3-HD-Kore", "speaking_rate": 2.0})));
    assert_eq!(body(&r)["audioConfig"], json!({"audioEncoding": "MP3", "speakingRate": 2.0}));
    let (_, r) = sent(&job(STUDIO, "Hi", json!({"voice": "en-US-Studio-O", "pitch": 5.0})));
    assert_eq!(body(&r)["audioConfig"], json!({"audioEncoding": "MP3", "speakingRate": 1.0}), "a pitch setting is ignored where it isn't offered");
    let (_, r) = sent(&job(STANDARD, "Hi", json!({"voice": "en-US-Standard-C", "voice_name": "cmn-CN-Standard-A"})));
    assert_eq!(body(&r)["voice"], json!({"languageCode": "cmn-CN", "name": "cmn-CN-Standard-A"}), "the typed name overrides the choice");
    let (_, r) = sent(&job(STANDARD, "Hi", json!({"voice": 7, "voice_name": "en-GB-Standard-A"})));
    assert_eq!(body(&r)["voice"]["name"], "en-GB-Standard-A", "a typed name stands even when the choice is unreadable");
}

#[test]
fn a_voice_from_another_family_or_of_another_shape_is_refused_before_anything_is_sent() {
    let cases = [
        // A Studio voice under the Standard price would be billed forty times what was quoted.
        (STANDARD, json!({"voice": "en-US-Studio-O"})),
        (NEURAL2, json!({"voice_name": "en-US-Chirp3-HD-Kore"})),
        (CHIRP3_HD, json!({"voice": "en-US-Chirp3-HD-"})),
        (NEURAL2, json!({"voice": "en-US-Neural2"})),
        (NEURAL2, json!({"voice": "en-US-Neural2-F/../x"})),
        (NEURAL2, json!({"voice": "EN-us-Neural2-F"})),
        (NEURAL2, json!({"voice": "Kore"})),
        (NEURAL2, json!({})),
        (NEURAL2, json!({"voice": 7})),
        (NEURAL2, json!({"voice": "en-US-Neural2-F", "speaking_rate": 4.5})),
        (CHIRP3_HD, json!({"voice": "en-US-Chirp3-HD-Kore", "speaking_rate": 2.5})),
        (NEURAL2, json!({"voice": "en-US-Neural2-F", "pitch": 21.0})),
        (NEURAL2, json!({"voice": "en-US-Neural2-F", "input_format": "markdown"})),
        (GEMINI_FLASH, json!({"voice": "Nobody"})),
        (GEMINI_FLASH, json!({"language_code": "en_US"})),
        (GEMINI_FLASH, json!({"speaker_1": "Sam"})),
        (GEMINI_FLASH, json!({"speaker_1": "Sam", "speaker_2": "Sam"})),
        (GEMINI_FLASH, json!({"speaker_1": "Sam Smith", "speaker_2": "Bob"})),
        (GEMINI_FLASH_LITE, json!({"speaker_1": "Sam", "speaker_2": "Bob"})),
        (GEMINI_FLASH, json!({"style": "x".repeat(4_001)})),
        ("chirp4", json!({"voice": "en-US-Chirp4-A"})),
    ];
    for (n, (model, params)) in cases.into_iter().enumerate() {
        let t = Transport::new(vec![granted(TOKEN), spoken()]);
        let scope = crate::SubmitContext::new();
        let error = block_on(provider(&t).submit_with_delivery(&job(model, "Hi", params), &scope)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "case {n}: {}", error.message);
        assert!(t.requests().is_empty(), "case {n}: nothing, not even a token request, is sent");
        assert!(scope.finish_error().is_some(), "case {n}: nothing was sent, so it was not accepted");
    }
}

#[test]
fn text_caps_follow_each_family_in_bytes() {
    for (model, params, max) in [(NEURAL2, json!({"voice": "en-US-Neural2-F"}), 5_000), (GEMINI_PRO, json!({}), 4_000)] {
        let (_, r) = sent(&job(model, &"a".repeat(max), params.clone()));
        assert_eq!(body(&r)["input"]["text"].as_str().map(str::len), Some(max));
        // Two bytes a character: half as many fit.
        let t = Transport::new(vec![]);
        let error = block_on(provider(&t).submit(&job(model, &"é".repeat(max / 2 + 1), params))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "{}", error.message);
        assert!(t.requests().is_empty());
    }
    let t = Transport::new(vec![]);
    let mut empty = job(NEURAL2, "   ", json!({"voice": "en-US-Neural2-F"}));
    assert_eq!(block_on(provider(&t).submit(&empty)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    empty.inputs.push(JobInput::Text { role: "prompt".into(), text: "two".into() });
    empty.inputs.remove(0);
    empty.inputs.push(JobInput::Text { role: "prompt".into(), text: "prompts".into() });
    assert_eq!(block_on(provider(&t).submit(&empty)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(t.requests().is_empty());
}

#[test]
fn a_gemini_voice_takes_a_style_prompt_and_reports_no_characters() {
    let (done, r) = sent(&job(GEMINI_FLASH, "Have a wonderful day!", json!({"voice": "charon", "style": "Say this cheerfully", "language_code": "en-IN"})));
    assert_eq!(
        body(&r),
        json!({"input": {"text": "Have a wonderful day!", "prompt": "Say this cheerfully"},
            "voice": {"languageCode": "en-IN", "modelName": GEMINI_FLASH, "name": "Charon"},
            "audioConfig": {"audioEncoding": "MP3"}})
    );
    assert_eq!(done.usage.billable_units, None, "Gemini-TTS bills tokens, which the reply doesn't report");
    let (_, r) = sent(&job(GEMINI_3_1_FLASH, "Hi", json!({})));
    assert_eq!(body(&r)["voice"], json!({"languageCode": "en-US", "modelName": GEMINI_3_1_FLASH, "name": "Kore"}), "defaults");
}

#[test]
fn two_named_speakers_make_a_dialogue_with_a_voice_each() {
    let text = "Sam: Hi Bob, how are you?\nBob: Doing well, and you?";
    let params = json!({"speaker_1": "Sam", "speaker_1_voice": "Kore", "speaker_2": "Bob", "speaker_2_voice": "Charon", "voice": "Puck"});
    let (_, r) = sent(&job(GEMINI_PRO, text, params));
    assert_eq!(
        body(&r)["voice"],
        json!({"languageCode": "en-US", "modelName": GEMINI_PRO, "multiSpeakerVoiceConfig": {"speakerVoiceConfigs": [
            {"speakerAlias": "Sam", "speakerId": "Kore"}, {"speakerAlias": "Bob", "speakerId": "Charon"}]}}),
        "the single voice gives way to the speakers'"
    );
    assert_eq!(body(&r)["input"], json!({"text": text}));
}

#[test]
fn limits_may_only_be_lowered() {
    let t = Transport::new(vec![]);
    assert!(GoogleTts::with_limits(&stored(), t.clone(), fixed_clock(NOW), Limits { response_bytes: 1024, output_bytes: 512 }).is_ok());
    let error = GoogleTts::with_limits(&stored(), t, fixed_clock(NOW), Limits { output_bytes: Limits::HARD.output_bytes + 1, ..Limits::HARD }).err().unwrap();
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
}

#[test]
fn a_reply_that_is_not_mp3_or_too_large_is_malformed() {
    let encode = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let replies = [
        ok(json!({"audioContent": encode(b"RIFF\0\0\0\0WAVEfmt ")})),
        ok(json!({"audioContent": ""})),
        ok(json!({"audioContent": "not base64!"})),
        ok(json!({"audio": encode(&mp3())})),
        Ok(HttpResponse { status: 200, headers: vec![], body: b"<html>".to_vec() }),
    ];
    for (n, reply) in replies.into_iter().enumerate() {
        let t = Transport::new(vec![granted(TOKEN), reply]);
        let error = block_on(provider(&t).submit(&job(GEMINI_FLASH, "Hi", json!({})))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "case {n}: {}", error.message);
    }
    let t = Transport::new(vec![granted(TOKEN), ok(json!({"audioContent": encode(&[0xFF; 900])}))]);
    let small = GoogleTts::with_limits(&stored(), t, fixed_clock(NOW), Limits { response_bytes: 4096, output_bytes: 512 }).unwrap();
    assert_eq!(block_on(small.submit(&job(GEMINI_FLASH, "Hi", json!({})))).unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn errors_map_to_shared_kinds_and_only_canonical_refusals_clear_the_charge() {
    let google = |code: u16, status: &str, message: &str| serde_json::to_vec(&json!({"error": {"code": code, "status": status, "message": message}})).unwrap();
    let cases: Vec<(u16, Vec<u8>, ErrorKind, bool)> = vec![
        (400, google(400, "INVALID_ARGUMENT", "Voice 'en-US-Neural2-Z' does not exist."), ErrorKind::UnsupportedInput, true),
        (401, google(401, "UNAUTHENTICATED", "Request had invalid authentication credentials."), ErrorKind::Auth, true),
        (403, google(403, "PERMISSION_DENIED", "Cloud Text-to-Speech API has not been used in project"), ErrorKind::Auth, true),
        (404, google(404, "NOT_FOUND", "Requested entity was not found."), ErrorKind::NotFound, true),
        (429, google(429, "RESOURCE_EXHAUSTED", "Quota exceeded."), ErrorKind::RateLimited, true),
        // Another shape or status stays possibly billed.
        (400, google(400, "FAILED_PRECONDITION", "odd"), ErrorKind::UnsupportedInput, false),
        (400, b"bad".to_vec(), ErrorKind::UnsupportedInput, false),
        (429, b"{}".to_vec(), ErrorKind::RateLimited, false),
        (500, google(500, "INTERNAL", "Internal error encountered."), ErrorKind::Server, false),
        (503, google(503, "UNAVAILABLE", "The service is currently unavailable."), ErrorKind::Busy, false),
        (504, b"".to_vec(), ErrorKind::Timeout, false),
        (302, b"".to_vec(), ErrorKind::Malformed, false),
    ];
    for (status, reply_body, kind, refusal) in cases {
        let t = Transport::new(vec![granted(TOKEN), Ok(HttpResponse { status, headers: vec![], body: reply_body.clone() })]);
        let scope = crate::SubmitContext::new();
        let error = block_on(provider(&t).submit_with_delivery(&job(GEMINI_FLASH, "Hi", json!({})), &scope)).unwrap_err();
        let shown = String::from_utf8_lossy(&reply_body);
        assert_eq!(error.kind, kind, "HTTP {status} {shown}");
        assert!(error.message.contains(&format!("HTTP {status}")), "{}", error.message);
        assert_eq!(scope.finish_error().is_some(), refusal, "HTTP {status} {shown}: refusal evidence");
    }
    let t = Transport::new(vec![
        granted(TOKEN),
        Ok(HttpResponse { status: 400, headers: vec![], body: google(400, "INVALID_ARGUMENT", "Voice 'x' does not exist.") }),
    ]);
    let error = block_on(provider(&t).submit(&job(GEMINI_FLASH, "Hi", json!({})))).unwrap_err();
    assert!(error.message.contains("Voice 'x' does not exist."), "Google's explanation of a bad request is shown: {}", error.message);
    let t = Transport::new(vec![granted(TOKEN), Ok(HttpResponse { status: 403, headers: vec![], body: google(403, "PERMISSION_DENIED", "denied") })]);
    let error = block_on(provider(&t).submit(&job(GEMINI_FLASH, "Hi", json!({})))).unwrap_err();
    assert!(error.message.contains(PROJECT) && error.message.contains("Text-to-Speech API"), "{}", error.message);
}

#[test]
fn a_refused_token_is_dropped_so_the_next_run_asks_for_a_fresh_one() {
    let unauthenticated = serde_json::to_vec(&json!({"error": {"code": 401, "status": "UNAUTHENTICATED", "message": "expired"}})).unwrap();
    let t = Transport::new(vec![granted(TOKEN), Ok(HttpResponse { status: 401, headers: vec![], body: unauthenticated }), granted("ya29.fresh"), spoken()]);
    let p = provider(&t);
    assert_eq!(block_on(p.submit(&job(GEMINI_FLASH, "Hi", json!({})))).unwrap_err().kind, ErrorKind::Auth);
    assert!(block_on(p.submit(&job(GEMINI_FLASH, "Hi", json!({})))).is_ok());
    let requests = t.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].1.headers.iter().any(|(n, v)| n == "Authorization" && v == "Bearer ya29.fresh"));
}

#[test]
fn a_lost_reply_is_possibly_billed_and_never_retried_and_the_token_never_appears_in_an_error() {
    let t = Transport::new(vec![granted(TOKEN), Err(ProviderError::new(ErrorKind::Network, format!("connection reset; Bearer {TOKEN}")))]);
    let p = provider(&t);
    assert!(!p.retry_ambiguous_submit());
    let scope = crate::SubmitContext::new();
    let error = block_on(p.submit_with_delivery(&job(GEMINI_FLASH, "Hi", json!({})), &scope)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("may have been made and billed") && !error.message.contains(TOKEN), "{}", error.message);
    assert!(scope.finish_error().is_none(), "the request went out");
    let echo = serde_json::to_vec(&json!({"error": {"code": 400, "status": "INVALID_ARGUMENT", "message": format!("bad header Bearer {TOKEN}")}})).unwrap();
    let t = Transport::new(vec![granted(TOKEN), Ok(HttpResponse { status: 400, headers: vec![], body: echo })]);
    let error = block_on(provider(&t).submit(&job(GEMINI_FLASH, "Hi", json!({})))).unwrap_err();
    assert!(!error.message.contains(TOKEN) && error.message.contains("withheld"), "{}", error.message);
}

#[test]
fn a_refused_token_exchange_sends_no_speech_request() {
    let t = Transport::new(vec![Ok(HttpResponse { status: 400, headers: vec![], body: br#"{"error":"invalid_grant"}"#.to_vec() })]);
    let scope = crate::SubmitContext::new();
    assert!(block_on(provider(&t).submit_with_delivery(&job(GEMINI_FLASH, "Hi", json!({})), &scope)).is_err());
    assert_eq!(t.requests().len(), 1, "only the free token request went out");
    assert!(scope.finish_error().is_some(), "nothing paid was sent");
}

#[test]
fn polling_and_cancelling_are_unsupported() {
    let t = Transport::new(vec![]);
    let p = provider(&t);
    let handle = JobHandle { remote_id: "x".into(), data: json!({}) };
    assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(block_on(p.cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert_eq!(p.id(), "google-tts");
}
