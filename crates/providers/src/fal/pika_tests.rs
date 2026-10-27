//! Authored offline fixtures exercise the real currentPika2.5 fal adapter. No paid calls.
use super::tests::{handle, png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};
const KEY: &str = "pika29-public-fixture-not-a-key";
const CLIP: &[u8] = include_bytes!("fixtures/pika29.mp4");
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
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn reply(v: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&v).unwrap() })
}
fn key() -> Secret {
    Secret::new(KEY).unwrap()
}
fn fal(w: &Arc<Wire>) -> Fal {
    Fal::new(key(), w.clone())
}
fn job() -> ProviderJob {
    ProviderJob {
        model: PIKA_I2V.into(),
        params: Default::default(),
        inputs: vec![
            crate::JobInput::Text { role: "prompt".into(), text: "A paper kite rises above an empty meadow".into() },
            crate::JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: png(2, 3).into() },
        ],
        idempotency_key: "original-pika29-operation".into(),
    }
}
#[test]
fn pika29_exact_wire_schema_all_durations_and_raster_codecs_preserve_original() {
    use base64::Engine;
    for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg, image::ImageFormat::WebP] {
        let mut image = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 2, image::Rgb([20, 100, 200]))).write_to(&mut image, format).unwrap();
        let mime = match format {
            image::ImageFormat::Png => "image/png",
            image::ImageFormat::Jpeg => "image/jpeg",
            _ => "image/webp",
        };
        for seconds in [5, 10] {
            let mut j = job();
            j.params.insert("duration_seconds".into(), json!(seconds));
            j.inputs[1] = crate::JobInput::Media { role: "first_frame".into(), mime: mime.into(), bytes: image.get_ref().clone().into() };
            let (_, body) = request::prepare(&j, Limits::default()).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"prompt":"A paper kite rises above an empty meadow","image_url":format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(image.get_ref())),"resolution":"720p","duration":seconds})
            );
        }
    }
}
#[test]
fn pika29_invalid_roles_options_and_rasters_refuse_before_http() {
    let mut jobs = vec![];
    for (key, value) in [
        ("duration_seconds", json!(6)),
        ("duration_seconds", json!("5")),
        ("duration_seconds", json!(true)),
        ("duration_seconds", json!(5.5)),
        ("resolution", json!("1080p")),
        ("seed", json!(1)),
        ("negative_prompt", json!("none")),
        ("fps", json!(24)),
        ("enable_safety_checker", json!(false)),
        ("sync_mode", json!(true)),
        ("num_outputs", json!(2)),
    ] {
        let mut j = job();
        j.params.insert(key.into(), value);
        jobs.push(j);
    }
    for text in [" ".into(), "x".repeat(2049)] {
        let mut j = job();
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text };
        jobs.push(j);
    }
    let mut j = job();
    j.inputs.pop();
    jobs.push(j);
    let mut j = job();
    j.inputs.push(j.inputs[1].clone());
    jobs.push(j);
    let mut j = job();
    j.inputs[0] = j.inputs[1].clone();
    jobs.push(j);
    for (role, mime, bytes) in [
        ("last_frame", "image/png", png(2, 3)),
        ("image", "image/png", png(2, 3)),
        ("first_frame", "image/jpeg", png(2, 3)),
        ("first_frame", "image/png", png(1025, 1)),
        ("first_frame", "image/png", vec![0; 1024 * 1024 + 1]),
        ("first_frame", "image/svg+xml", b"<svg/>".to_vec()),
        ("first_frame", "image/jpeg", include_bytes!("fixtures/oriented.jpg").to_vec()),
    ] {
        let mut j = job();
        j.inputs[1] = crate::JobInput::Media { role: role.into(), mime: mime.into(), bytes: bytes.into() };
        jobs.push(j);
    }
    // A declared animation chunk must refuse before decoding even when first-frame pixels exist.
    let mut animated = png(2, 3);
    animated.splice(8..8, [0, 0, 0, 8, b'a', b'c', b'T', b'L', 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut j = job();
    j.inputs[1] = crate::JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: animated.into() };
    jobs.push(j);
    let wire = Arc::new(Wire::default());
    for j in jobs {
        assert_eq!(block_on(fal(&wire).submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    }
    assert!(wire.requests.lock().unwrap().is_empty());
}
#[test]
fn pika29_complete_escaped_json_and_uri_extent_are_checked_before_codec_work() {
    let mut j = job();
    j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "\"\n\\".repeat(300) };
    let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: bytes.len(), ..Limits::default() }).is_ok());
    super::tests::reset();
    assert!(request::prepare(&j, Limits { request_bytes: bytes.len() - 1, ..Limits::default() }).is_err());
    assert_eq!(super::tests::stages(), [0, 0, 0]);
    assert!(request::prepare(&j, Limits { text_bytes: 8, ..Limits::default() }).is_err());
}
#[test]
fn pika29_queue_resume_poll_cancel_and_no_credential_download_use_owner_app() {
    let wire = Wire::new(vec![
        reply(receipt(PIKA_I2V)),
        reply(json!({"status":"IN_QUEUE","queue_position":2})),
        reply(json!({"status":"IN_PROGRESS"})),
        reply(json!({"status":"COMPLETED"})),
        reply(
            json!({"video":{"url":"https://storage.googleapis.com/falserverless/original/pika29.mp4","content_type":"video/mp4","file_size":CLIP.len(),"width":64,"height":48}}),
        ),
        Ok(HttpResponse { status: 200, headers: vec![], body: CLIP.to_vec() }),
        Ok(HttpResponse { status: 202, headers: vec![], body: br#"{"status":"CANCELLATION_REQUESTED"}"#.to_vec() }),
    ]);
    let f = fal(&wire);
    let Submission::Queued(h) = block_on(f.submit(&job())).unwrap() else { panic!() };
    assert_eq!(h, handle(PIKA_I2V));
    let h: JobHandle = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
    assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Queued { position: Some(2) }));
    assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "video/mp4".into(), bytes: CLIP.into() }]);
    block_on(f.cancel(&h)).unwrap();
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests[0].url, format!("{ORIGIN}/{PIKA_I2V}"));
    assert_eq!(requests[1].url, format!("{ORIGIN}/fal-ai/pika/requests/r_1/status"));
    assert!(requests[5].headers.is_empty());
    assert_eq!(requests[6].method, HttpMethod::Put);
    assert!(!f.retry_ambiguous_submit());
}
#[test]
fn pika29_output_origin_envelopes_declared_metadata_and_caps_are_scoped() {
    let r = Record::read(&handle(PIKA_I2V), &key()).unwrap().0;
    let l = Limits::default().for_model(Model::Pika);
    assert_eq!(l.output_bytes, 256 * 1024 * 1024);
    assert_eq!(l.total_output_bytes, l.output_bytes);
    assert_eq!(Limits::default().for_model(Model::Image).output_bytes, 16 * 1024 * 1024);
    for url in [
        "https://evil.example/a.mp4",
        "https://storage.googleapis.com/another-bucket/a.mp4",
        "https://storage.googleapis.com/falserverless/../a",
        "https://storage.googleapis.com:443/falserverless/a",
        "https://storage.googleapis.com/falserverless/a?key=x",
        "https://storage.googleapis.com/falserverless/%2e/a",
    ] {
        assert!(response::outputs(&serde_json::to_vec(&json!({"video":{"url":url}})).unwrap(), r, l, &key()).is_err());
    }
    for v in [
        json!({"images":[]}),
        json!({"video":{"url":"https://fal.media/a","content_type":"image/png"}}),
        json!({"video":{"url":"https://fal.media/a","file_size":268435457u64}}),
        json!({"video":{"url":"https://fal.media/a"},"audio":{}}),
    ] {
        assert!(response::outputs(&serde_json::to_vec(&v).unwrap(), r, l, &key()).is_err());
    }
    let mut forged = handle(PIKA_I2V);
    forged.data["status_url"] = json!("https://queue.fal.run/fal-ai/pika/v2.5/image-to-video/requests/r_1/status");
    assert!(Record::read(&forged, &key()).is_err());
    let other = Record::read(&handle(LTX), &key()).unwrap().0;
    assert!(
        response::outputs(
            &serde_json::to_vec(&json!({"video":{"url":"https://storage.googleapis.com/falserverless/a"}})).unwrap(),
            other,
            Limits::default().for_model(Model::LtxText),
            &key()
        )
        .is_err()
    );
}
#[test]
fn pika29_actual_mp4_timing_geometry_metadata_and_aggregate_are_bounded() {
    let l = Limits::default().for_model(Model::Pika);
    let r = Record::read(&handle(PIKA_I2V), &key()).unwrap().0;
    assert!(media::validate(CLIP, r, l, &mut 0).is_ok());
    assert!(media::validate(b"not mp4", r, l, &mut 0).is_err());
    for l in [Limits { output_width: 1, ..l }, Limits { metadata_bytes: 10, ..l }, Limits { total_output_bytes: 1, ..l }] {
        assert!(media::validate(CLIP, r, l, &mut 0).is_err());
    }
    for (atom, offset, replacement) in [(b"mvhd", 16, 0u32), (b"mvhd", 20, u32::MAX), (b"tkhd", 80, u32::MAX)] {
        let mut b = CLIP.to_vec();
        let at = b.windows(4).position(|s| s == atom).unwrap();
        b[at + offset..at + offset + 4].copy_from_slice(&replacement.to_be_bytes());
        assert!(media::validate(&b, r, l, &mut 0).is_err());
    }
}
#[test]
fn pika29_accepted_download_failure_retains_handle_and_large_video_finishes_without_post() {
    let mut clip = CLIP.to_vec();
    let padding = 17 * 1024 * 1024usize;
    clip.extend_from_slice(&u32::try_from(padding + 8).unwrap().to_be_bytes());
    clip.extend_from_slice(b"free");
    clip.resize(clip.len() + padding, 0);
    let output = json!({"video":{"url":"https://v3.fal.media/original.mp4","file_size":clip.len()}});
    let wire = Wire::new(vec![
        reply(receipt(PIKA_I2V)),
        reply(json!({"status":"COMPLETED"})),
        reply(output.clone()),
        Err(ProviderError::new(ErrorKind::Network, "temporary CDN issue")),
        reply(json!({"status":"COMPLETED"})),
        reply(output),
        Ok(HttpResponse { status: 200, headers: vec![], body: clip }),
    ]);
    let f = fal(&wire);
    let Submission::Queued(h) = block_on(f.submit(&job())).unwrap() else { panic!() };
    assert!(block_on(f.poll(&h)).is_err());
    let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
    assert!(matches!(done.outputs.as_slice(),[Output::Bytes{bytes,..}] if bytes.len()>16*1024*1024));
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.method == HttpMethod::Post).count(), 1);
    assert_eq!(requests[6].max_response_bytes, 256 * 1024 * 1024);
    assert!(requests[6].headers.is_empty());
}
#[test]
fn pika29_error_mapping_redacts_secrets_and_uncertain_submit_has_no_replay() {
    for (status, kind) in [
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (403, ErrorKind::Auth),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Server),
        (503, ErrorKind::Busy),
        (504, ErrorKind::Timeout),
    ] {
        let wire = Wire::new(vec![Ok(HttpResponse { status, headers: vec![("Retry-After".into(), "2".into())], body: KEY.as_bytes().to_vec() })]);
        let error = block_on(fal(&wire).submit(&job())).unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(!error.message.contains(KEY));
    }
    let wire = Wire::new(vec![Err(ProviderError::new(ErrorKind::Network, KEY))]);
    let f = fal(&wire);
    let e = block_on(f.submit(&job())).unwrap_err();
    assert!(!e.message.contains(KEY));
    assert!(!f.retry_ambiguous_submit());
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid live smoke: NODECRAFT_LIVE_PIKA=1, NODECRAFT_LIVE_PIKA_COST=0.25USD and FAL_KEY; one5s720pvideo"]
fn pika29_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_PIKA").as_deref() != Ok("1") {
        return;
    }
    assert_eq!(std::env::var("NODECRAFT_LIVE_PIKA_COST").as_deref(), Ok("0.25USD"));
    let key = Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let f = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let Submission::Queued(h) = f.submit(&job()).await.unwrap() else { panic!() };
        for _ in 0..180 {
            match f.poll(&h).await.unwrap() {
                JobProgress::Done(done) => {
                    assert!(matches!(done.outputs.as_slice(),[Output::Bytes{mime,..}] if mime=="video/mp4"));
                    return;
                }
                JobProgress::Failed(e) => panic!("{}", e.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            }
        }
        let _ = f.cancel(&h).await;
        panic!("Pika live timeout; accepted handle may remain billable");
    });
}

#[test]
fn pika29_download_metadata_mismatch_and_redirect_never_replace_the_accepted_task() {
    for (metadata, status, headers, bytes) in [
        (json!({"width":65}), 200, vec![], CLIP.to_vec()),
        (json!({"file_size":CLIP.len()+1}), 200, vec![], CLIP.to_vec()),
        (json!({}), 302, vec![("Location".into(), "https://evil.example/a".into())], vec![]),
        (json!({}), 200, vec![], png(2, 3)),
        (json!({}), 200, vec![("Content-Length".into(), "1".into())], CLIP.to_vec()),
    ] {
        let mut file = json!({"url":"https://v3.fal.media/original.mp4"});
        file.as_object_mut().unwrap().extend(metadata.as_object().unwrap().clone());
        let wire = Wire::new(vec![
            reply(receipt(PIKA_I2V)),
            reply(json!({"status":"COMPLETED"})),
            reply(json!({"video":file})),
            Ok(HttpResponse { status, headers, body: bytes }),
        ]);
        let f = fal(&wire);
        let Submission::Queued(h) = block_on(f.submit(&job())).unwrap() else { panic!() };
        assert!(block_on(f.poll(&h)).is_err());
        assert!(Record::read(&h, &key()).is_ok());
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests.iter().filter(|r| r.method == HttpMethod::Post).count(), 1);
        assert!(requests[3].headers.is_empty());
    }
}
#[test]
fn pika29_animated_webp_flags_and_oversized_png_chunks_refuse_without_codecs() {
    let mut animated = b"RIFF".to_vec();
    animated.extend_from_slice(&22u32.to_le_bytes());
    animated.extend_from_slice(b"WEBPVP8X");
    animated.extend_from_slice(&10u32.to_le_bytes());
    animated.extend_from_slice(&[2; 10]);
    let mut over = png(2, 3);
    over[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    for (mime, bytes) in [("image/webp", animated), ("image/png", over)] {
        let mut j = job();
        j.inputs[1] = crate::JobInput::Media { role: "first_frame".into(), mime: mime.into(), bytes: bytes.into() };
        super::tests::reset();
        assert!(request::prepare(&j, Limits::default()).is_err());
        assert_eq!(super::tests::stages(), [0, 0, 0]);
    }
}
