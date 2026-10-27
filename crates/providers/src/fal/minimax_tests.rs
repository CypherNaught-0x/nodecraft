//! Original offline MiniMax fixtures. No paid HTTP or vendor media.
use super::*;
use crate::{JobInput, SubmitContext};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::sync::Mutex;
const MP3: &[u8] = include_bytes!("fixtures/minimax27-tone.mp3");
const MP4: &[u8] = include_bytes!("fixtures/minimax27-motion.mp4");
fn wav(seconds: u32) -> Vec<u8> {
    let n = seconds * 176400;
    let mut b = Vec::with_capacity(n as usize + 44);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(n + 36).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&44100u32.to_le_bytes());
    b.extend_from_slice(&176400u32.to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&n.to_le_bytes());
    b.resize(n as usize + 44, 0);
    b
}
fn job(model: &str) -> ProviderJob {
    let mut j = ProviderJob {
        model: model.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A paper boat moves gently".into() }],
        idempotency_key: "original-minimax27".into(),
    };
    if model == MINIMAX_I2V {
        j.inputs.push(JobInput::Media {
            role: "first_frame".into(),
            mime: "image/png".into(),
            bytes: nodecraft_media::encode_png(64, 64, &vec![120; 64 * 64 * 4], &Default::default()).unwrap().into(),
        });
    }
    j
}
fn request(j: &ProviderJob) -> Value {
    serde_json::from_slice(&request::prepare(j, Limits::default()).unwrap().1).unwrap()
}
fn record(model: &str, seconds: u64) -> Record {
    Record {
        model: Model::read(model).unwrap(),
        requested: 1,
        width: 0,
        height: 0,
        duration_ms: if model == MINIMAX_SPEECH { 3_600_000 } else { (seconds * 1000) as u32 },
    }
}
fn key() -> Secret {
    Secret::new("public-minimax27-fixture-key").unwrap()
}
#[test]
fn minimax27_exact_four_request_contracts_and_prices_have_no_hidden_premium_defaults() {
    for model in [MINIMAX_VIDEO, MINIMAX_I2V] {
        let mut j = job(model);
        j.params.insert("duration".into(), json!(15));
        j.params.insert("resolution".into(), json!("768P"));
        j.params.insert("seed".into(), json!(4294967295u64));
        let b = request(&j);
        assert_eq!(b["duration"], 15);
        assert_eq!(b["resolution"], "768P");
        assert_eq!(b["prompt_expansion_mode"], "disabled");
        assert_eq!(b["sync_mode"], false);
        assert_eq!(b["enable_safety_checker"], true);
        assert!(b.get("target_audio_url").is_none());
        assert!(b.get("num_images").is_none());
        assert!(b.get("end_image_url").is_none());
        assert_eq!(b.get("aspect_ratio").is_some(), model == MINIMAX_VIDEO);
        assert_eq!(b.get("image_url").is_some(), model == MINIMAX_I2V);
    }
    let speech = request(&job(MINIMAX_SPEECH));
    assert_eq!(
        speech,
        json!({"prompt":"A paper boat moves gently","voice_setting":{"voice_id":"Wise_Woman","speed":1.0,"pitch":0,"vol":1,"english_normalization":false},
        "audio_setting":{"format":"mp3","sample_rate":32000,"bitrate":128000,"channel":1},"output_format":"url","enable_safety_checker":true})
    );
    let music = request(&job(MINIMAX_MUSIC));
    assert_eq!(
        music,
        json!({"prompt":"A paper boat moves gently","lyrics":"[instrumental]","duration":60,"guidance_scale":1.7,"num_inference_steps":30,"enable_safety_checker":true})
    );
}
#[test]
fn minimax27_all_invalid_inputs_stop_before_encoding_or_http() {
    for model in [MINIMAX_VIDEO, MINIMAX_I2V, MINIMAX_SPEECH, MINIMAX_MUSIC] {
        for (name, value) in
            [("sync_mode", json!(true)), ("num_outputs", json!(2)), ("enable_safety_checker", json!(false)), ("seed", json!(-1)), ("duration", json!(1.5))]
        {
            let mut j = job(model);
            j.params.insert(name.into(), value);
            assert!(request::prepare(&j, Limits::default()).is_err(), "{model} {name}");
        }
        let mut j = job(model);
        j.inputs.push(j.inputs[0].clone());
        assert!(request::prepare(&j, Limits::default()).is_err());
    }
    let mut j = job(MINIMAX_SPEECH);
    j.inputs[0] = JobInput::Text { role: "prompt".into(), text: "wait <#99.99#>".into() };
    assert!(request::prepare(&j, Limits::default()).is_err());
    let mut j = job(MINIMAX_MUSIC);
    j.params.insert("lyrics".into(), json!("[verse] these words would be dropped"));
    assert!(request::prepare(&j, Limits::default()).is_err());
    for value in [0, 4, 16, u64::MAX] {
        let mut j = job(MINIMAX_VIDEO);
        j.params.insert("duration".into(), json!(value));
        assert!(request::prepare(&j, Limits::default()).is_err());
    }
    let mut j = job(MINIMAX_I2V);
    j.inputs[1] = JobInput::Media { role: "first_frame".into(), mime: "image/svg+xml".into(), bytes: Arc::from(&b"<svg/>"[..]) };
    assert!(request::prepare(&j, Limits::default()).is_err());
    let mut j = job(MINIMAX_VIDEO);
    j.inputs[0] = JobInput::Text { role: "prompt".into(), text: "x".repeat(8193) };
    assert!(request::prepare(&j, Limits::default()).is_err());
}
#[test]
fn minimax27_complete_escaped_extent_and_lower_only_limits_survive_restoration() {
    let j = job(MINIMAX_I2V);
    let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
    let mut l = Limits { request_bytes: bytes.len(), ..Default::default() };
    assert!(request::prepare(&j, l).is_ok());
    l.request_bytes -= 1;
    assert!(request::prepare(&j, l).is_err());
    for model in [MINIMAX_VIDEO, MINIMAX_I2V, MINIMAX_SPEECH, MINIMAX_MUSIC] {
        let r = record(model, 5);
        let base = format!("https://queue.fal.run/{}/requests/public27", r.model.root());
        let h =
            r.handle("public27", &Routes { status: format!("{base}/status").into(), result: base.clone().into(), cancel: format!("{base}/cancel").into() }, "");
        assert_eq!(Record::read(&h, &key()).unwrap().0.duration_ms, r.duration_ms);
        assert_eq!(Limits::default().for_model(r.model).output_bytes, 256 * 1024 * 1024);
        let mut bad = h.clone();
        bad.data["duration_ms"] = json!(u64::MAX);
        assert!(Record::read(&bad, &key()).is_err());
        let mut bad = h.clone();
        bad.data.as_object_mut().unwrap().remove("duration_ms");
        assert!(Record::read(&bad, &key()).is_err());
    }
    assert_eq!(Limits::default().for_model(Model::Audio).output_bytes, 16 * 1024 * 1024);
}
#[test]
fn minimax27_real_mp3_mp4_and_exact_pcm_wav_are_whole_bounded_containers() {
    for (model, b, mime) in [(MINIMAX_VIDEO, MP4, "video/mp4"), (MINIMAX_SPEECH, MP3, "audio/mpeg"), (MINIMAX_MUSIC, wav(1).as_slice(), "audio/wav")] {
        let r = record(model, 5);
        assert_eq!(media::validate(b, r, Limits::default(), &mut 0).unwrap(), mime);
        for damaged in [&b[..b.len() - 1], &b[..20]] {
            assert!(media::validate(damaged, r, Limits::default(), &mut 0).is_err());
        }
        let mut junk = b.to_vec();
        junk.extend_from_slice(b"trailing junk");
        assert!(media::validate(&junk, r, Limits::default(), &mut 0).is_err());
    }
    let large = wav(120);
    assert!(large.len() > 16 * 1024 * 1024);
    assert!(media::validate(&large, record(MINIMAX_MUSIC, 120), Limits::default(), &mut 0).is_ok());
    assert!(media::validate(&large, record(MINIMAX_MUSIC, 119), Limits::default(), &mut 0).is_err());
    let mut mono = wav(1);
    mono[22..24].copy_from_slice(&1u16.to_le_bytes());
    assert!(media::validate(&mono, record(MINIMAX_MUSIC, 5), Limits::default(), &mut 0).is_err());
    let l = Limits { output_bytes: large.len() - 1, ..Default::default() };
    assert!(media::validate(&large, record(MINIMAX_MUSIC, 120), l, &mut 0).is_err());
}
fn result(model: &str, len: usize) -> Value {
    let mut out = if matches!(model, MINIMAX_VIDEO | MINIMAX_I2V) {
        json!({"video":{"url":"https://v3.fal.media/files/public27/result.mp4","file_size":len}})
    } else {
        json!({"audio":{"url":"https://v3.fal.media/files/public27/result","file_size":len}})
    };
    if model == MINIMAX_SPEECH {
        out["duration_ms"] = json!(1044);
    }
    if model == MINIMAX_MUSIC {
        out["duration"] = json!(1);
        out["seed"] = json!(27);
    }
    out
}
struct Wire {
    model: &'static str,
    bytes: Vec<u8>,
    lost: bool,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable HTTP forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = r.url.clone();
        let method = r.method;
        let cap = r.max_response_bytes;
        self.requests.lock().unwrap().push(r);
        Box::pin(async move {
            let root = format!("https://queue.fal.run/{}/requests/public27", Model::read(self.model).unwrap().root());
            let body = if method == HttpMethod::Post {
                if self.lost {
                    return Err(ProviderError::new(ErrorKind::Network, "private rejected detail"));
                }
                serde_json::to_vec(&json!({"request_id":"public27","status":"IN_QUEUE","status_url":format!("{root}/status"),"response_url":root,"cancel_url":format!("{root}/cancel")})).unwrap()
            } else if method == HttpMethod::Put {
                return Ok(HttpResponse { status: 202, headers: vec![], body: br#"{"status":"CANCELLATION_REQUESTED"}"#.to_vec() });
            } else if url.ends_with("/status") {
                br#"{"status":"COMPLETED"}"#.to_vec()
            } else if url == root {
                serde_json::to_vec(&result(self.model, self.bytes.len())).unwrap()
            } else {
                assert!(url.starts_with("https://v3.fal.media/"));
                self.bytes.clone()
            };
            assert!(body.len() <= cap);
            Ok(HttpResponse { status: 200, headers: vec![], body })
        })
    }
}
#[test]
fn minimax27_all_canonical_queue_routes_resume_download_cancel_and_credential_boundaries() {
    for (model, bytes) in [(MINIMAX_VIDEO, MP4.to_vec()), (MINIMAX_I2V, MP4.to_vec()), (MINIMAX_SPEECH, MP3.to_vec()), (MINIMAX_MUSIC, wav(1))] {
        let w = Arc::new(Wire { model, bytes: bytes.clone(), lost: false, requests: Mutex::default() });
        let p = Fal::new(key(), w.clone());
        let Submission::Queued(h) = block_on(p.submit(&job(model))).unwrap() else { panic!("queued") };
        let h: JobHandle = serde_json::from_value(serde_json::to_value(h).unwrap()).unwrap();
        let JobProgress::Done(done) = block_on(p.poll(&h)).unwrap() else { panic!("done") };
        assert!(matches!(done.outputs.as_slice(),[Output::Bytes{bytes:b,..}] if b.as_ref()==bytes));
        block_on(p.cancel(&h)).unwrap();
        let requests = w.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[0].url, format!("https://queue.fal.run/{model}"));
        assert_eq!(requests[1].url, format!("https://queue.fal.run/{}/requests/public27/status", Model::read(model).unwrap().root()));
        assert!(requests[3].headers.is_empty());
        assert_eq!(requests[3].max_response_bytes, 256 * 1024 * 1024);
        for r in requests.iter().filter(|r| r.url.starts_with("https://queue.fal.run/")) {
            assert!(r.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization")));
        }
    }
}
#[test]
fn minimax27_bad_envelopes_identity_metadata_and_origins_refuse_before_media_get() {
    let r = record(MINIMAX_MUSIC, 5);
    for mut value in [
        result(MINIMAX_MUSIC, 100),
        json!({"audio":{"url":"https://v3.fal.media/file"}}),
        json!({"audio":{"url":"https://evil.test/file"},"seed":1,"duration":1}),
    ] {
        if value.get("duration").is_some() && value["audio"]["url"].as_str().unwrap().contains("public27") {
            value["duration"] = json!(6);
        }
        assert!(response::outputs(&serde_json::to_vec(&value).unwrap(), r, Limits::default(), &key()).is_err());
    }
    let mut j = job(MINIMAX_MUSIC);
    j.params.insert("duration".into(), json!(300));
    assert!(request::prepare(&j, Limits::default()).is_ok());
    j.params.insert("duration".into(), json!(301));
    assert!(request::prepare(&j, Limits::default()).is_err());
}
#[test]
fn minimax27_validation_releases_but_uncertain_paid_delivery_never_claims_no_charge_or_replays() {
    let w = Arc::new(Wire { model: MINIMAX_SPEECH, bytes: vec![], lost: true, requests: Mutex::default() });
    let p = Fal::new(key(), w.clone());
    let mut j = job(MINIMAX_SPEECH);
    j.params.insert("voice".into(), json!("unknown"));
    let context = SubmitContext::new();
    assert!(block_on(p.submit_with_delivery(&j, &context)).is_err());
    assert!(w.requests.lock().unwrap().is_empty());
    assert!(context.finish_error().is_some(), "local refusal proves no charge");
    assert!(!p.retry_ambiguous_submit());
    let context = SubmitContext::new();
    assert!(block_on(p.submit_with_delivery(&job(MINIMAX_SPEECH), &context)).is_err());
    assert!(context.finish_error().is_none(), "an ambiguous paid reply proves no release");
    assert_eq!(w.requests.lock().unwrap().len(), 1);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "explicit local FFmpeg only; NODECRAFT_MINIMAX27_FFMPEG"]
fn minimax27_authored_mp3_and_video_decode_with_the_actual_native_tool() {
    let tool = std::path::PathBuf::from(std::env::var_os("NODECRAFT_MINIMAX27_FFMPEG").unwrap());
    for name in ["minimax27-tone.mp3", "minimax27-motion.mp4"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fal/fixtures").join(name);
        let decoded =
            std::process::Command::new(&tool).args(["-nostdin", "-hide_banner", "-v", "error", "-i"]).arg(path).args(["-f", "null", "-"]).output().unwrap();
        assert!(decoded.status.success(), "{name}: {}", String::from_utf8_lossy(&decoded.stderr));
    }
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid MiniMax smoke; requires BOTH NODECRAFT_LIVE_MINIMAX=1 and NODECRAFT_LIVE_MINIMAX_ACKNOWLEDGE_COST=1 plus FAL_KEY"]
fn minimax27_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_MINIMAX").ok().as_deref() != Some("1")
        || std::env::var("NODECRAFT_LIVE_MINIMAX_ACKNOWLEDGE_COST").ok().as_deref() != Some("1")
    {
        return;
    }
    let remote = match std::env::var("NODECRAFT_LIVE_MINIMAX_MODE").ok().as_deref() {
        Some("video") => MINIMAX_VIDEO,
        Some("frame") => MINIMAX_I2V,
        Some("speech") => MINIMAX_SPEECH,
        Some("music") | None => MINIMAX_MUSIC,
        _ => panic!("choose video/frame/speech/music"),
    };
    let mut j = job(remote);
    if remote == MINIMAX_MUSIC {
        j.params.insert("duration".into(), json!(8));
    }
    let f = Fal::new(Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap(), Arc::new(crate::ReqwestHttp::new().unwrap()));
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let Submission::Queued(h) = f.submit(&j).await.unwrap() else { panic!("queued") };
        for _ in 0..300 {
            match f.poll(&h).await.unwrap() {
                JobProgress::Done(done) => {
                    assert_eq!(done.outputs.len(), 1);
                    return;
                }
                JobProgress::Failed(_) => panic!("live model failed"),
                _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            }
        }
        panic!("live task timed out; saved handle may still be billed");
    });
}

#[test]
fn minimax27_lowered_geometry_and_pcm_caps_refuse_before_payment_and_forged_audio_metadata_never_imports() {
    for model in [MINIMAX_VIDEO, MINIMAX_I2V] {
        let l = Limits { output_width: 31, ..Default::default() };
        assert!(request::prepare(&job(model), l).is_err());
    }
    let mut j = job(MINIMAX_MUSIC);
    j.params.insert("duration".into(), json!(120));
    let l = Limits { output_bytes: 16 * 1024 * 1024, ..Default::default() };
    assert!(request::prepare(&j, l).is_err());
    let mut forged = MP3.to_vec();
    let start = if forged.starts_with(b"ID3") { 10 + forged[6..10].iter().fold(0usize, |n, b| n * 128 + usize::from(*b)) } else { 0 };
    forged[start + 21..start + 25].copy_from_slice(b"Xing");
    forged[start + 25..start + 29].copy_from_slice(&1u32.to_be_bytes());
    forged[start + 29..start + 33].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(media::validate(&forged, record(MINIMAX_SPEECH, 5), Limits::default(), &mut 0).is_err());
}

#[test]
fn minimax27_fractional_frame_aspect_reserves_the_next_quantized_geometry_before_paid_submit() {
    // 480*47/44 = 512.727..., so a 512px-wide lowered host cannot retain the next 64px bucket.
    let mut j = job(MINIMAX_I2V);
    j.inputs[1] = JobInput::Media {
        role: "first_frame".into(),
        mime: "image/png".into(),
        bytes: nodecraft_media::encode_png(47, 44, &vec![120; 47 * 44 * 4], &Default::default()).unwrap().into(),
    };
    let mut l = Limits { output_width: 512, ..Default::default() };
    assert!(request::prepare(&j, l).is_err());
    l.output_width = 576;
    assert!(request::prepare(&j, l).is_ok());
}
