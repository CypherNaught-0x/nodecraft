//! Synthetic official-shape replies; no paid provider recordings.
use super::*;
use crate::{HttpMethod, HttpRequest, HttpResponse, JobInput, Output};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::sync::Mutex;

const KEY: &str = "fixture-secret-not-real";
/// An ID3v2.4 header and one MPEG-1 Layer III frame header.
const MP3: &[u8] = b"ID3\x04\0\0\0\0\0\0\xFF\xFB\x90\x64\0\0\0\0";

struct Canned {
    reply: Mutex<Option<Result<HttpResponse, ProviderError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Canned {
    fn new(reply: Result<HttpResponse, ProviderError>) -> Arc<Self> {
        Arc::new(Self { reply: Mutex::new(Some(reply)), requests: Mutex::default() })
    }
    fn request(&self) -> HttpRequest {
        let requests = self.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "exactly one request");
        requests[0].clone()
    }
    fn sent(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl Http for Canned {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-following fallback forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.reply.lock().unwrap().take().expect("exactly one request");
        Box::pin(async move { reply })
    }
}
fn reply(status: u16, body: &[u8]) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status, body: body.to_vec(), headers: vec![] })
}
fn audio() -> Result<HttpResponse, ProviderError> {
    reply(200, MP3)
}
fn job(model: &str, params: Value) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: params.as_object().cloned().unwrap_or_default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: " Hello there, 雪. ".into() }],
        idempotency_key: "fixture-key-no-server-dedup".into(),
    }
}
fn provider(http: &Arc<Canned>) -> ElevenLabs {
    ElevenLabs::new(Secret::new(KEY).unwrap(), http.clone())
}
fn run(j: &ProviderJob, r: Result<HttpResponse, ProviderError>) -> (Result<Submission, ProviderError>, Arc<Canned>) {
    let http = Canned::new(r);
    let result = block_on(provider(&http).submit(j));
    (result, http)
}
fn done(r: Result<Submission, ProviderError>) -> Completed {
    let Submission::Finished(c) = r.unwrap() else { panic!("synchronous") };
    c
}
fn body(r: &HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn header<'a>(r: &'a HttpRequest, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}
fn check_auth(r: &HttpRequest) {
    assert_eq!(r.method, HttpMethod::Post);
    assert_eq!(header(r, "xi-api-key"), Some(KEY));
    assert!(r.secret_headers.contains(&"xi-api-key".to_owned()));
    assert_eq!(r.max_response_bytes, Limits::HARD.output_bytes);
}

#[test]
fn speech_sends_the_documented_body_and_counts_the_characters() {
    let params =
        json!({"voice": "JBFqnCBsd6RMkjVDRZzb", "stability": 0.4, "similarity_boost": 0.9, "style": 0.2, "speed": 1.1, "seed": 7, "language_code": "en"});
    let (result, http) = run(&job(V3, params), audio());
    let completed = done(result);
    let [Output::Bytes { mime, bytes }] = completed.outputs.as_slice() else { panic!("one MP3") };
    assert_eq!((mime.as_str(), &bytes[..]), ("audio/mpeg", MP3));
    // The trimmed text: "Hello there, 雪." is 15 characters.
    assert_eq!(completed.usage.billable_units.map(|u| u.millionths()), Some(15_000_000));
    assert_eq!(completed.usage.cost, None);
    let r = http.request();
    check_auth(&r);
    assert_eq!(r.url, "https://api.elevenlabs.io/v1/text-to-speech/JBFqnCBsd6RMkjVDRZzb?output_format=mp3_44100_128");
    assert_eq!(header(&r, "Content-Type"), Some("application/json"));
    assert_eq!(
        body(&r),
        json!({"text": "Hello there, 雪.", "model_id": "eleven_v3", "seed": 7, "language_code": "en",
            "voice_settings": {"stability": 0.4, "similarity_boost": 0.9, "style": 0.2, "speed": 1.1}})
    );
}

#[test]
fn speech_defaults_and_multilingual_never_sends_a_language() {
    let (result, http) = run(&job(MULTILINGUAL_V2, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "language_code": "de"})), audio());
    done(result);
    let r = http.request();
    assert_eq!(r.url, "https://api.elevenlabs.io/v1/text-to-speech/EXAVITQu4vr4xnSDxMaL?output_format=mp3_44100_128");
    assert_eq!(
        body(&r),
        json!({"text": "Hello there, 雪.", "model_id": "eleven_multilingual_v2",
            "voice_settings": {"stability": 0.5, "similarity_boost": 0.75, "style": 0.0, "speed": 1.0}})
    );
}

#[test]
fn a_voice_id_overrides_the_choice_and_can_never_reshape_the_url() {
    let (result, http) = run(&job(FLASH_V2_5, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "voice_id": " abcDEF123 "})), audio());
    done(result);
    assert_eq!(http.request().url, "https://api.elevenlabs.io/v1/text-to-speech/abcDEF123?output_format=mp3_44100_128");
    // An empty override falls back to the chosen voice.
    let (result, http) = run(&job(FLASH_V2_5, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "voice_id": "  "})), audio());
    done(result);
    assert!(http.request().url.contains("/EXAVITQu4vr4xnSDxMaL?"));
    let long = "a".repeat(65);
    for hostile in ["../../v1/user", "abc?x=1", "abc#frag", "abc/def", "abc%2F", "ab cd", "abc\u{0}", "é", long.as_str()] {
        for model in [V3, VOICE_CHANGER] {
            let mut j = job(model, json!({"voice_id": hostile}));
            if model == VOICE_CHANGER {
                j.inputs = vec![JobInput::Media { role: "audio".into(), mime: "audio/mpeg".into(), bytes: Arc::from(MP3) }];
            }
            let (result, http) = run(&j, audio());
            assert_eq!(result.unwrap_err().kind, ErrorKind::UnsupportedInput, "{hostile:?}");
            assert_eq!(http.sent(), 0, "{hostile:?} was refused locally");
        }
    }
    let (result, http) = run(&job(V3, json!({})), audio());
    assert_eq!(result.unwrap_err().kind, ErrorKind::UnsupportedInput, "no voice at all");
    assert_eq!(http.sent(), 0);
}

#[test]
fn text_caps_follow_each_speech_model() {
    for (model, max) in [(V3, 5_000), (MULTILINGUAL_V2, 10_000), (FLASH_V2_5, 40_000)] {
        let mut fits = job(model, json!({"voice": "EXAVITQu4vr4xnSDxMaL"}));
        fits.inputs = vec![JobInput::Text { role: "prompt".into(), text: "雪".repeat(max) }];
        let (result, _) = run(&fits, audio());
        assert_eq!(done(result).usage.billable_units.map(|u| u.millionths()), Some(max as u64 * 1_000_000), "{model}");
        let mut over = fits.clone();
        over.inputs = vec![JobInput::Text { role: "prompt".into(), text: "雪".repeat(max + 1) }];
        let (result, http) = run(&over, audio());
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "{model}");
        assert!(error.message.contains(&max.to_string()), "{}", error.message);
        assert_eq!(http.sent(), 0);
    }
}

#[test]
fn bad_inputs_and_parameters_are_refused_before_sending() {
    let voice = json!({"voice": "EXAVITQu4vr4xnSDxMaL"});
    let mut no_prompt = job(V3, voice.clone());
    no_prompt.inputs.clear();
    let mut blank = job(V3, voice.clone());
    blank.inputs = vec![JobInput::Text { role: "prompt".into(), text: " \n ".into() }];
    let mut two = job(V3, voice.clone());
    two.inputs.push(JobInput::Text { role: "prompt".into(), text: "again".into() });
    let cases = [
        no_prompt,
        blank,
        two,
        job("eleven_unknown", voice.clone()),
        job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "stability": 1.5})),
        job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "speed": 0.5})),
        job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "style": "loud"})),
        job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "seed": 4_294_967_296_u64})),
        job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "seed": -1})),
        job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "language_code": "EN"})),
        job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL", "language_code": "en&x=1"})),
        job(V3, json!({"voice": 5})),
        job(SOUND_EFFECTS, json!({"duration_seconds": 31.0})),
        job(SOUND_EFFECTS, json!({"duration_seconds": 0.5})),
        job(SOUND_EFFECTS, json!({"loop": "yes"})),
        job(MUSIC, json!({"duration": 2})),
        job(MUSIC, json!({"duration": 601})),
        job(MUSIC, json!({"duration": 30.5})),
    ];
    for (n, j) in cases.iter().enumerate() {
        let http = Canned::new(audio());
        let scope = crate::SubmitContext::new();
        let error = block_on(provider(&http).submit_with_delivery(j, &scope)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "case {n}: {}", error.message);
        assert_eq!(http.sent(), 0, "case {n}");
        assert!(scope.finish_error().is_some(), "case {n}: nothing was sent, so it was not accepted");
    }
    let mut long = job(SOUND_EFFECTS, json!({}));
    long.inputs = vec![JobInput::Text { role: "prompt".into(), text: "a".repeat(5_001) }];
    let (result, http) = run(&long, audio());
    assert_eq!(result.unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert_eq!(http.sent(), 0);
}

#[test]
fn sound_effects_and_music_send_their_documented_bodies() {
    let (result, http) = run(&job(SOUND_EFFECTS, json!({"duration_seconds": 2.5, "prompt_influence": 0.7, "loop": true})), audio());
    assert_eq!(done(result).usage.billable_units, None);
    let r = http.request();
    check_auth(&r);
    assert_eq!(r.url, "https://api.elevenlabs.io/v1/sound-generation?output_format=mp3_44100_128");
    assert_eq!(
        body(&r),
        json!({"text": "Hello there, 雪.", "model_id": "eleven_text_to_sound_v2", "duration_seconds": 2.5, "prompt_influence": 0.7, "loop": true})
    );
    let (result, http) = run(&job(SOUND_EFFECTS, json!({})), audio());
    done(result);
    assert_eq!(
        body(&http.request()),
        json!({"text": "Hello there, 雪.", "model_id": "eleven_text_to_sound_v2", "duration_seconds": 5.0, "prompt_influence": 0.3, "loop": false})
    );

    let (result, http) = run(&job(MUSIC, json!({"duration": 600, "force_instrumental": true})), audio());
    done(result);
    let r = http.request();
    check_auth(&r);
    assert_eq!(r.url, "https://api.elevenlabs.io/v1/music?output_format=mp3_44100_128");
    assert_eq!(body(&r), json!({"prompt": "Hello there, 雪.", "music_length_ms": 600_000, "model_id": "music_v1", "force_instrumental": true}));
    let (result, http) = run(&job(MUSIC, json!({})), audio());
    done(result);
    assert_eq!(body(&http.request())["music_length_ms"], 30_000);
}

fn voice_job(bytes: &[u8], params: Value) -> ProviderJob {
    let mut j = job(VOICE_CHANGER, params);
    // The claimed type is ignored: the bytes decide.
    j.inputs = vec![JobInput::Media { role: "audio".into(), mime: "application/octet-stream".into(), bytes: Arc::from(bytes) }];
    j
}

#[test]
fn the_voice_changer_uploads_a_sniffed_recording_as_multipart() {
    let wav = b"RIFF\x24\0\0\0WAVEfmt \x10\0\0\0";
    let params = json!({"voice": "XrExE9yKIg1WjnnlVkGX", "stability": 0.3, "similarity_boost": 0.8, "remove_background_noise": true, "seed": 42});
    let (result, http) = run(&voice_job(wav, params), audio());
    done(result);
    let r = http.request();
    check_auth(&r);
    assert_eq!(r.url, "https://api.elevenlabs.io/v1/speech-to-speech/XrExE9yKIg1WjnnlVkGX?output_format=mp3_44100_128");
    assert!(header(&r, "Content-Type").is_some_and(|t| t.starts_with("multipart/form-data; boundary=")));
    let text = String::from_utf8_lossy(&r.body);
    for part in [
        "name=\"model_id\"\r\n\r\neleven_multilingual_sts_v2\r\n",
        "name=\"remove_background_noise\"\r\n\r\ntrue\r\n",
        "name=\"seed\"\r\n\r\n42\r\n",
        "name=\"audio\"; filename=\"input.wav\"\r\nContent-Type: audio/wav\r\n\r\n",
    ] {
        assert!(text.contains(part), "{part:?} in {text}");
    }
    assert!(r.body.windows(wav.len()).any(|w| w == wav));
    let settings = text.split("name=\"voice_settings\"\r\n\r\n").nth(1).and_then(|rest| rest.split("\r\n").next()).unwrap();
    assert_eq!(serde_json::from_str::<Value>(settings).unwrap(), json!({"stability": 0.3, "similarity_boost": 0.8}));

    let (result, http) = run(&voice_job(MP3, json!({"voice": "XrExE9yKIg1WjnnlVkGX"})), audio());
    done(result);
    let text = String::from_utf8_lossy(&http.request().body).into_owned();
    assert!(text.contains("filename=\"input.mp3\"\r\nContent-Type: audio/mpeg"));
    assert!(text.contains("name=\"remove_background_noise\"\r\n\r\nfalse\r\n") && !text.contains("name=\"seed\""));
}

#[test]
fn the_voice_changer_refuses_missing_foreign_and_oversized_recordings() {
    let params = json!({"voice": "XrExE9yKIg1WjnnlVkGX"});
    let mut none = job(VOICE_CHANGER, params.clone());
    none.inputs.clear();
    let mut two = voice_job(MP3, params.clone());
    two.inputs.push(two.inputs[0].clone());
    let png = voice_job(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR", params.clone());
    let text = voice_job(b"just some words", params.clone());
    for (n, j) in [none, two, png, text].iter().enumerate() {
        let (result, http) = run(j, audio());
        assert_eq!(result.unwrap_err().kind, ErrorKind::UnsupportedInput, "case {n}");
        assert_eq!(http.sent(), 0, "case {n}");
    }
    let limits = Limits { input_bytes: 16, ..Limits::HARD };
    let http = Canned::new(audio());
    let p = ElevenLabs::with_limits(Secret::new(KEY).unwrap(), http.clone(), limits).unwrap();
    let big = [MP3, &[0u8; 8]].concat();
    assert_eq!(block_on(p.submit(&voice_job(&big, params))).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert_eq!(http.sent(), 0);
}

#[test]
fn limits_may_only_be_lowered() {
    let http = Canned::new(audio());
    let key = || Secret::new(KEY).unwrap();
    assert!(ElevenLabs::with_limits(key(), http.clone(), Limits { output_bytes: Limits::HARD.output_bytes + 1, ..Limits::HARD }).is_err());
    assert!(ElevenLabs::with_limits(key(), http.clone(), Limits { input_bytes: Limits::HARD.input_bytes + 1, ..Limits::HARD }).is_err());
    assert!(ElevenLabs::with_limits(key(), http.clone(), Limits { description_chars: 5_001, ..Limits::HARD }).is_err());
    assert!(ElevenLabs::with_limits(key(), http, Limits { output_bytes: 1, ..Limits::HARD }).is_ok());
}

#[test]
fn a_reply_that_is_not_mp3_or_too_large_is_malformed() {
    let voice = json!({"voice": "EXAVITQu4vr4xnSDxMaL"});
    for bad in [&b""[..], b"{\"ok\":true}", b"RIFF\x24\0\0\0WAVEfmt ", b"\x89PNG\r\n\x1a\n"] {
        let (result, _) = run(&job(V3, voice.clone()), reply(200, bad));
        assert_eq!(result.unwrap_err().kind, ErrorKind::Malformed, "{bad:?}");
    }
    let http = Canned::new(reply(200, &[MP3, &[0u8; 64]].concat()));
    let p = ElevenLabs::with_limits(Secret::new(KEY).unwrap(), http.clone(), Limits { output_bytes: 32, ..Limits::HARD }).unwrap();
    assert_eq!(block_on(p.submit(&job(V3, voice))).unwrap_err().kind, ErrorKind::Malformed);
    assert_eq!(http.request().max_response_bytes, 32);
}

/// Every documented error, its kind, and whether it proves nothing was generated.
#[test]
fn errors_map_to_shared_kinds_and_only_documented_refusals_clear_the_charge() {
    let detail = |status: &str| serde_json::to_vec(&json!({"detail": {"status": status, "message": format!("{status} happened")}})).unwrap();
    let validation = serde_json::to_vec(&json!({"detail": [{"loc": ["body", "text"], "msg": "field required", "type": "value_error.missing"}]})).unwrap();
    let cases: Vec<(u16, Vec<u8>, ErrorKind, bool)> = vec![
        (401, detail("invalid_api_key"), ErrorKind::Auth, true),
        (401, detail("quota_exceeded"), ErrorKind::Budget, true),
        (400, detail("quota_exceeded"), ErrorKind::Budget, true),
        (400, detail("voice_not_found"), ErrorKind::UnsupportedInput, true),
        (400, detail("max_character_limit_exceeded"), ErrorKind::UnsupportedInput, true),
        (429, detail("too_many_concurrent_requests"), ErrorKind::RateLimited, true),
        (429, detail("system_busy"), ErrorKind::Busy, true),
        (422, validation, ErrorKind::UnsupportedInput, true),
        // Undocumented shapes and statuses stay possibly billed.
        (401, detail("detected_unusual_activity"), ErrorKind::Auth, false),
        (401, b"{}".to_vec(), ErrorKind::Auth, false),
        (422, b"{\"detail\": []}".to_vec(), ErrorKind::UnsupportedInput, false),
        (400, detail("something_new"), ErrorKind::UnsupportedInput, false),
        (402, detail("payment_required"), ErrorKind::Budget, false),
        (403, b"forbidden".to_vec(), ErrorKind::Auth, false),
        (404, detail("not_found"), ErrorKind::NotFound, false),
        (429, b"slow down".to_vec(), ErrorKind::RateLimited, false),
        (500, detail("system_busy"), ErrorKind::Busy, false),
        (500, b"<html>oops</html>".to_vec(), ErrorKind::Server, false),
        (503, b"".to_vec(), ErrorKind::Busy, false),
        (504, b"".to_vec(), ErrorKind::Timeout, false),
        (302, b"".to_vec(), ErrorKind::Malformed, false),
    ];
    for (status, reply_body, kind, refusal) in cases {
        let http = Canned::new(reply(status, &reply_body));
        let scope = crate::SubmitContext::new();
        let error = block_on(provider(&http).submit_with_delivery(&job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL"})), &scope)).unwrap_err();
        assert_eq!(error.kind, kind, "HTTP {status} {}", String::from_utf8_lossy(&reply_body));
        assert!(error.message.contains(&status.to_string()), "{}", error.message);
        assert_eq!(scope.finish_error().is_some(), refusal, "HTTP {status} {}: refusal evidence", String::from_utf8_lossy(&reply_body));
    }
}

#[test]
fn retry_after_is_kept_within_an_hour() {
    for (value, expected) in [("7", Some(7_000)), ("3600", Some(3_600_000)), ("3601", None), ("soon", None)] {
        let response = HttpResponse { status: 429, body: b"{}".to_vec(), headers: vec![("retry-after".into(), value.into())] };
        let (result, _) = run(&job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL"})), Ok(response));
        assert_eq!(result.unwrap_err().retry_after_ms, expected, "{value}");
    }
}

#[test]
fn a_lost_reply_is_possibly_billed_and_never_retried() {
    let http = Canned::new(Err(ProviderError::new(ErrorKind::Network, format!("connection reset; key {KEY}"))));
    let p = provider(&http);
    assert!(!p.retry_ambiguous_submit());
    let scope = crate::SubmitContext::new();
    let error = block_on(p.submit_with_delivery(&job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL"})), &scope)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("may have been generated and billed") && !error.message.contains(KEY), "{}", error.message);
    assert!(scope.finish_error().is_none(), "the request went out");
    // A success after arming is accepted, as usual.
    let http = Canned::new(audio());
    let scope = crate::SubmitContext::new();
    assert!(block_on(provider(&http).submit_with_delivery(&job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL"})), &scope)).is_ok());
}

#[test]
fn the_key_never_appears_in_an_error() {
    let echo = serde_json::to_vec(&json!({"detail": {"status": "invalid_api_key", "message": format!("Invalid API key: {KEY}")}})).unwrap();
    let excerpt = format!("bad key {KEY}").into_bytes();
    let field = serde_json::to_vec(&json!({"detail": [{"loc": ["header"], "msg": format!("got {KEY}"), "type": "x"}]})).unwrap();
    for (status, b) in [(401, echo), (500, excerpt), (422, field)] {
        let (result, _) = run(&job(V3, json!({"voice": "EXAVITQu4vr4xnSDxMaL"})), reply(status, &b));
        let error = result.unwrap_err();
        assert!(!error.message.contains(KEY), "{}", error.message);
        assert!(!format!("{error:?}").contains(KEY));
    }
}

#[test]
fn polling_and_cancelling_are_unsupported() {
    let http = Canned::new(audio());
    let p = provider(&http);
    assert_eq!(p.id(), "elevenlabs");
    let handle = JobHandle { remote_id: "x".into(), data: Value::Null };
    assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(block_on(p.cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert_eq!(http.sent(), 0);
}
