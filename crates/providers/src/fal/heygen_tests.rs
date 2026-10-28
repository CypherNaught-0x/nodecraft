//! Procedural offline contracts for supplied-audio avatars and lip sync. No vendor media.
use super::tests::{handle, png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};
const CLIP: &[u8] = include_bytes!("fixtures/pika29.mp4");
const KEY: &str = "public-heygen43-offline-fixture";
#[derive(Default)]
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Wire {
    fn new(replies: Vec<Result<HttpResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies.into()), ..Default::default() })
    }
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn reply(value: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() })
}
fn key() -> Secret {
    Secret::new(KEY).unwrap()
}
fn wav(ms: u32) -> Vec<u8> {
    let size = ms * 16; // 8kHz mono16-bit PCM.
    let mut b = b"RIFF".to_vec();
    b.extend_from_slice(&(size + 36).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    b.extend_from_slice(&8000u32.to_le_bytes());
    b.extend_from_slice(&16000u32.to_le_bytes());
    b.extend_from_slice(b"\x02\0\x10\0data");
    b.extend_from_slice(&size.to_le_bytes());
    b.resize(b.len() + size as usize, 0);
    b
}
fn job(model: &str) -> ProviderJob {
    let (role, mime, bytes) = if model == HEYGEN_AVATAR { ("portrait", "image/png", png(2, 3)) } else { ("video", "video/mp4", CLIP.to_vec()) };
    ProviderJob {
        model: model.into(),
        params: Default::default(),
        idempotency_key: "original-heygen43-operation".into(),
        inputs: vec![
            crate::JobInput::Media { role: role.into(), mime: mime.into(), bytes: bytes.into() },
            crate::JobInput::Media { role: "speech".into(), mime: "audio/wav".into(), bytes: wav(1000).into() },
        ],
    }
}
#[test]
fn heygen43_all_aliases_exact_schema_and_supplied_media_without_tts_or_extra_fields() {
    use base64::Engine;
    for model in [HEYGEN_AVATAR, HEYGEN_SPEED, HEYGEN_PRECISION] {
        let mut j = job(model);
        if model != HEYGEN_AVATAR {
            j.params.insert("disable_music_track".into(), json!(true));
        }
        let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.as_object().unwrap().len(), if model == HEYGEN_AVATAR { 7 } else { 6 });
        for (i, field) in [(0, if model == HEYGEN_AVATAR { "image_url" } else { "video_url" }), (1, "audio_url")] {
            let crate::JobInput::Media { mime, bytes, .. } = &j.inputs[i] else { panic!() };
            assert_eq!(body[field], format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)));
        }
        if model == HEYGEN_AVATAR {
            assert_eq!(body["talking_style"], "stable");
            assert_eq!(body["resolution"], "720p");
            assert_eq!(body["aspect_ratio"], "16:9");
            assert_eq!(body["enable_safety_checker"], true);
            assert_eq!(body["caption"], false);
        } else {
            assert_eq!(body["enable_dynamic_duration"], false);
            assert_eq!(body["enable_caption"], false);
            assert_eq!(body["disable_music_track"], true);
            assert_eq!(body["enable_speech_enhancement"], false);
        }
    }
}
#[test]
fn heygen43_input_options_clocks_codecs_and_entire_multi_uri_extent_refuse_before_payment() {
    let mut invalid_jobs = vec![];
    for model in [HEYGEN_AVATAR, HEYGEN_SPEED, HEYGEN_PRECISION] {
        for (name, value) in [
            ("prompt", json!("not TTS")),
            ("voice", json!("unknown")),
            ("enable_dynamic_duration", json!(true)),
            ("enable_safety_checker", json!(false)),
            ("disable_music_track", json!("true")),
        ] {
            let mut j = job(model);
            j.params.insert(name.into(), value);
            invalid_jobs.push(j);
        }
        let mut j = job(model);
        j.inputs.pop();
        invalid_jobs.push(j);
        let mut j = job(model);
        j.inputs[1] = j.inputs[0].clone();
        invalid_jobs.push(j);
        for b in [wav(0), wav(10001), b"RIFFnot an audio file".to_vec()] {
            let mut j = job(model);
            j.inputs[1] = crate::JobInput::Media { role: "speech".into(), mime: "audio/wav".into(), bytes: b.into() };
            invalid_jobs.push(j);
        }
        let j = job(model);
        let (_, body) = request::prepare(&j, Limits::default()).unwrap();
        super::tests::reset();
        assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Default::default() }).is_err());
        assert_eq!(super::tests::stages(), [0, 0, 0]);
        assert!(request::prepare(&j, Limits { request_bytes: body.len(), input_bytes: 16044, ..Default::default() }).is_ok());
    }
    for b in [png(1025, 1), include_bytes!("fixtures/oriented.jpg").to_vec()] {
        let mut j = job(HEYGEN_AVATAR);
        j.inputs[0] = crate::JobInput::Media {
            role: "portrait".into(),
            mime: if b.starts_with(b"\xff\xd8") { "image/jpeg" } else { "image/png" }.into(),
            bytes: b.into(),
        };
        invalid_jobs.push(j);
    }
    for (kind, offset, value) in [(b"mvhd", 16, 0), (b"mvhd", 20, 10001), (b"mdhd", 20, u32::MAX), (b"tkhd", 80, 1281 << 16)] {
        let mut b = CLIP.to_vec();
        let at = b.windows(4).position(|s| s == kind).unwrap();
        b[at + offset..at + offset + 4].copy_from_slice(&value.to_be_bytes());
        let mut j = job(HEYGEN_SPEED);
        j.inputs[0] = crate::JobInput::Media { role: "video".into(), mime: "video/mp4".into(), bytes: b.into() };
        invalid_jobs.push(j);
    }
    let wire = Arc::new(Wire::default());
    let provider = Fal::new(key(), wire.clone());
    for j in invalid_jobs {
        assert!(block_on(provider.submit(&j)).is_err());
    }
    assert!(wire.requests.lock().unwrap().is_empty());
}
#[test]
fn heygen43_lowered_output_and_response_envelopes_have_positive_unsubmitted_proof() {
    let hard = Limits::VIDEO_HARD;
    for limits in [
        Limits { output_width: hard.output_width - 1, ..Default::default() },
        Limits { output_height: 1, ..Default::default() },
        Limits { total_pixels: 1, ..Default::default() },
        Limits { output_bytes: 0, ..Default::default() },
        Limits { total_output_bytes: 0, ..Default::default() },
        Limits { metadata_bytes: 0, ..Default::default() },
        Limits { response_bytes: 0, ..Default::default() },
    ] {
        let wire = Arc::new(Wire::default());
        let provider = Fal::with_limits(key(), wire.clone(), limits).unwrap();
        let context = crate::SubmitContext::new();
        super::tests::reset();
        assert!(block_on(provider.submit_with_delivery(&job(HEYGEN_SPEED), &context)).is_err());
        assert!(context.finish_error().is_some());
        assert!(wire.requests.lock().unwrap().is_empty());
        assert_eq!(super::tests::stages(), [0, 0, 0]);
    }
}
#[test]
fn heygen43_larger_input_request_envelopes_are_isolated_from_every_legacy_model() {
    let l = Limits::default();
    assert_eq!(l.for_model(Model::HeygenSpeed).input_bytes, 16 * 1024 * 1024);
    assert_eq!(l.for_model(Model::HeygenSpeed).request_bytes, 32 * 1024 * 1024);
    for model in [Model::Pika, Model::Topaz, Model::Image, Model::Speech, Model::RecraftVector, Model::RayFrames, Model::LtxFrames] {
        assert_eq!(l.for_model(model).input_bytes, 1024 * 1024);
        assert_eq!(l.for_model(model).request_bytes, 4 * 1024 * 1024);
    }
    assert_eq!(l.for_model(Model::GrokEdit).request_bytes, 8 * 1024 * 1024);
    let mut b = CLIP.to_vec();
    b.extend_from_slice(&(2 * 1024 * 1024u32 + 8).to_be_bytes());
    b.extend_from_slice(b"free");
    b.resize(b.len() + 2 * 1024 * 1024, 0);
    let mut j = job(HEYGEN_SPEED);
    j.inputs[0] = crate::JobInput::Media { role: "video".into(), mime: "video/mp4".into(), bytes: b.into() };
    assert!(request::prepare(&j, l).unwrap().1.len() > 1024 * 1024);
}

#[test]
fn heygen43_combined_minimax_aliases_keep_their_prior_input_request_and_media_envelopes() {
    let old = Limits::ALIBABA_HARD;
    for model in [Model::MiniMaxText, Model::MiniMaxFrames, Model::MiniMaxSpeech, Model::MiniMaxMusic] {
        let current = Limits::default().for_model(model);
        let previous = old.for_model(model);
        assert_eq!(current.input_bytes, 1024 * 1024);
        assert_eq!(current.request_bytes, 4 * 1024 * 1024);
        assert_eq!(current.text_bytes, 32 * 1024);
        assert_eq!(current.output_bytes, previous.output_bytes);
        assert_eq!(current.total_output_bytes, previous.total_output_bytes);
        assert_eq!(current.output_width, previous.output_width);
        assert_eq!(current.output_height, previous.output_height);
        assert_eq!(current.total_pixels, previous.total_pixels);
        assert_eq!(current.codec_bytes, previous.codec_bytes);
        assert_eq!(current.metadata_bytes, previous.metadata_bytes);
        assert_eq!(current.response_bytes, previous.response_bytes);
    }
}

#[test]
fn heygen43_missing_or_inconsistent_media_clocks_and_non_pcm_speech_refuse_locally() {
    let at = CLIP.windows(4).position(|s| s == b"mdhd").unwrap();
    let scale = u32::from_be_bytes(CLIP[at + 16..at + 20].try_into().unwrap());
    let mut shorter = CLIP.to_vec();
    shorter[at + 20..at + 24].copy_from_slice(&(scale * 4).to_be_bytes());
    let mut missing = CLIP.to_vec();
    missing[at..at + 4].copy_from_slice(b"free");
    for video in [shorter, missing] {
        let mut j = job(HEYGEN_SPEED);
        j.inputs[0] = crate::JobInput::Media { role: "video".into(), mime: "video/mp4".into(), bytes: video.into() };
        assert!(request::prepare(&j, Limits::default()).is_err());
    }
    let mut speech = wav(1000);
    speech[4..8].copy_from_slice(&32036u32.to_le_bytes());
    speech[20..22].copy_from_slice(&3u16.to_le_bytes());
    speech[28..32].copy_from_slice(&32000u32.to_le_bytes());
    speech[32..34].copy_from_slice(&4u16.to_le_bytes());
    speech[34..36].copy_from_slice(&32u16.to_le_bytes());
    speech[40..44].copy_from_slice(&32000u32.to_le_bytes());
    speech.resize(32044, 0);
    // A valid floating WAV passes the shared audio envelope but is outside PCM-only HeyGen.
    media::validate(&speech, Record { model: Model::Audio, requested: 1, width: 0, height: 0, duration_ms: 0 }, Limits::default(), &mut 0).unwrap();
    let mut j = job(HEYGEN_AVATAR);
    j.inputs[1] = crate::JobInput::Media { role: "speech".into(), mime: "audio/wav".into(), bytes: speech.into() };
    let wire = Arc::new(Wire::default());
    assert!(block_on(Fal::new(key(), wire.clone()).submit(&j)).is_err());
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn heygen43_resumed_paid_handle_intersects_current_output_limits_without_reposting() {
    let wire = Wire::new(vec![reply(receipt(HEYGEN_SPEED))]);
    let provider = Fal::new(key(), wire.clone());
    let Submission::Queued(h) = block_on(provider.submit(&job(HEYGEN_SPEED))).unwrap() else { panic!() };
    let h: JobHandle = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
    let resumed =
        Wire::new(vec![reply(json!({"status":"COMPLETED"})), reply(json!({"video":{"url":"https://v3b.fal.media/original.mp4","file_size":CLIP.len()}}))]);
    let provider = Fal::with_limits(key(), resumed.clone(), Limits { output_bytes: 1, ..Default::default() }).unwrap();
    assert!(block_on(provider.poll(&h)).is_err());
    assert!(Record::read(&h, &key()).is_ok());
    let requests = resumed.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.method == HttpMethod::Get));
}
#[test]
fn heygen43_canonical_queue_resume_download_sidecar_and_cancel_do_not_leak_credentials() {
    for model in [HEYGEN_AVATAR, HEYGEN_SPEED, HEYGEN_PRECISION] {
        let wire = Wire::new(vec![
            reply(receipt(model)),
            reply(json!({"status":"COMPLETED"})),
            reply(
                json!({"video":{"url":"https://v3b.fal.media/original.mp4","file_size":CLIP.len(),"width":64,"height":48},"caption_file":{"url":"https://evil.example/never-fetch"}}),
            ),
            Ok(HttpResponse { status: 200, headers: vec![], body: CLIP.to_vec() }),
            Ok(HttpResponse { status: 202, headers: vec![], body: br#"{"status":"CANCELLATION_REQUESTED"}"#.to_vec() }),
        ]);
        let provider = Fal::new(key(), wire.clone());
        let Submission::Queued(h) = block_on(provider.submit(&job(model))).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        let h: JobHandle = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
        let JobProgress::Done(done) = block_on(provider.poll(&h)).unwrap() else { panic!() };
        assert_eq!(done.outputs, vec![Output::Bytes { mime: "video/mp4".into(), bytes: CLIP.into() }]);
        block_on(provider.cancel(&h)).unwrap();
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests[0].url, format!("{ORIGIN}/{model}"));
        assert_eq!(requests[1].url, format!("{ORIGIN}/fal-ai/heygen/requests/r_1/status"));
        assert!(requests[3].headers.is_empty());
        assert_eq!(requests[3].max_response_bytes, 256 * 1024 * 1024);
        assert_eq!(requests[4].method, HttpMethod::Put);
        assert!(!provider.retry_ambiguous_submit());
    }
}
#[test]
fn heygen43_malformed_delivery_or_lost_paid_submit_never_sends_another_create() {
    for url in ["https://evil.example/video.mp4", "https://storage.googleapis.com/falserverless/video.mp4", "https://v3.fal.media:443/video.mp4"] {
        let wire = Wire::new(vec![reply(receipt(HEYGEN_SPEED)), reply(json!({"status":"COMPLETED"})), reply(json!({"video":{"url":url}}))]);
        let provider = Fal::new(key(), wire.clone());
        let Submission::Queued(h) = block_on(provider.submit(&job(HEYGEN_SPEED))).unwrap() else { panic!() };
        assert!(block_on(provider.poll(&h)).is_err());
        assert!(Record::read(&h, &key()).is_ok());
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
    let wire = Wire::new(vec![Err(ProviderError::new(ErrorKind::Network, KEY))]);
    let provider = Fal::new(key(), wire.clone());
    let error = block_on(provider.submit(&job(HEYGEN_AVATAR))).unwrap_err();
    assert!(!error.message.contains(KEY));
    assert!(!provider.retry_ambiguous_submit());
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid live smoke: explicit unknown-cost acknowledgement, FAL_KEY and user-supplied media"]
fn heygen43_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_HEYGEN").as_deref() != Ok("1") {
        return;
    }
    assert_eq!(std::env::var("NODECRAFT_LIVE_HEYGEN_ACKNOWLEDGE_UNKNOWN_COST").as_deref(), Ok("1"));
    let model = std::env::var("NODECRAFT_LIVE_HEYGEN_MODEL").unwrap_or_else(|_| HEYGEN_AVATAR.into());
    assert!([HEYGEN_AVATAR, HEYGEN_SPEED, HEYGEN_PRECISION].contains(&model.as_str()));
    let photo = model == HEYGEN_AVATAR;
    let read = |name: &str, cap: u64| {
        use std::io::Read;
        let file = std::fs::File::open(std::env::var(name).unwrap()).unwrap();
        let mut bytes = vec![];
        file.take(cap + 1).read_to_end(&mut bytes).unwrap();
        assert!(bytes.len() as u64 <= cap);
        bytes
    };
    let visual = read(if photo { "NODECRAFT_LIVE_HEYGEN_PHOTO" } else { "NODECRAFT_LIVE_HEYGEN_VIDEO" }, if photo { 1024 * 1024 } else { 16 * 1024 * 1024 });
    let audio = read("NODECRAFT_LIVE_HEYGEN_WAV", 1024 * 1024);
    let mime = if photo { nodecraft_media::probe_image(&visual, &Default::default()).unwrap().mime } else { "video/mp4" };
    let mut j = job(&model);
    j.inputs = vec![
        crate::JobInput::Media { role: if photo { "portrait" } else { "video" }.into(), mime: mime.into(), bytes: visual.into() },
        crate::JobInput::Media { role: "speech".into(), mime: "audio/wav".into(), bytes: audio.into() },
    ];
    let key = Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
        let provider = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let Submission::Queued(h) = provider.submit(&j).await.unwrap() else { panic!() };
        for _ in 0..300 {
            match provider.poll(&h).await.unwrap() {
                JobProgress::Done(done) => {
                    assert!(matches!(done.outputs.as_slice(),[Output::Bytes{mime,..}]if mime=="video/mp4"));
                    return;
                }
                JobProgress::Failed(e) => panic!("{}", e.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            }
        }
        let _ = provider.cancel(&h).await;
        panic!("HeyGen live timeout; accepted task may remain billable");
    });
}
