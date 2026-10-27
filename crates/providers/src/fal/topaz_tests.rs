//! Original offline Topaz Precision fixtures; no paid network calls.
use super::tests::{png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::json;
use std::{collections::VecDeque, sync::Mutex};
const KEY: &str = "topaz37-public-fixture-not-a-key";
#[derive(Default)]
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects refused")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn reply(value: serde_json::Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() })
}
fn key() -> Secret {
    Secret::new(KEY).unwrap()
}
fn provider(w: &Arc<Wire>) -> Fal {
    Fal::new(key(), w.clone())
}
fn job() -> ProviderJob {
    ProviderJob {
        model: TOPAZ_IMAGE.into(),
        params: Default::default(),
        inputs: vec![crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png(2, 3).into() }],
        idempotency_key: "topaz37-public-operation".into(),
    }
}
fn handle() -> JobHandle {
    let (record, _) = request::prepare(&job(), Limits::default()).unwrap();
    let receipt = receipt(TOPAZ_IMAGE);
    record.handle(
        "r_1",
        &Routes {
            status: receipt["status_url"].as_str().unwrap().into(),
            result: receipt["response_url"].as_str().unwrap().into(),
            cancel: receipt["cancel_url"].as_str().unwrap().into(),
        },
        "",
    )
}
fn descriptor(size: usize, width: u32, height: u32) -> serde_json::Value {
    json!({"image":{"url":"https://v3.fal.media/files/public/topaz37.png","content_type":"image/png","file_size":size,"width":width,"height":height}})
}
#[test]
fn topaz37_fixed_request_preserves_input_and_all_integer_factors() {
    use base64::Engine;
    for factor in 1..=4 {
        let mut job = job();
        job.params.insert("upscale_factor".into(), json!(factor));
        let (record, bytes) = request::prepare(&job, Limits::default()).unwrap();
        assert_eq!((record.width, record.height), (2 * factor, 3 * factor));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            json!({
            "image_url":format!("data:image/png;base64,{}",base64::engine::general_purpose::STANDARD.encode(png(2,3))),
            "model":"Standard V2","upscale_factor":factor,"output_format":"png","crop_to_fill":false,"enable_safety_checker":true})
        );
    }
    let (record, _) = request::prepare(&job(), Limits::default()).unwrap();
    assert_eq!((record.width, record.height), (4, 6));
}
#[test]
fn topaz37_invalid_schema_and_images_never_submit() {
    let mut jobs = vec![];
    for (name, value) in [
        ("upscale_factor", json!(0)),
        ("upscale_factor", json!(5)),
        ("upscale_factor", json!(2.5)),
        ("upscale_factor", json!("2")),
        ("upscale_factor", json!(true)),
        ("model", json!("Faces")),
        ("seed", json!(1)),
        ("output_format", json!("jpeg")),
        ("crop_to_fill", json!(true)),
        ("sharpen", json!(1)),
        ("enable_safety_checker", json!(false)),
    ] {
        let mut j = job();
        j.params.insert(name.into(), value);
        jobs.push(j);
    }
    let mut j = job();
    j.inputs.clear();
    jobs.push(j);
    let mut j = job();
    j.inputs.push(j.inputs[0].clone());
    jobs.push(j);
    let mut j = job();
    j.inputs = vec![crate::JobInput::Text { role: "image".into(), text: "image".into() }];
    jobs.push(j);
    for (role, mime, bytes) in [
        ("other", "image/png", png(2, 3)),
        ("image", "image/jpeg", png(2, 3)),
        ("image", "image/svg+xml", b"<svg/>".to_vec()),
        ("image", "image/png", b"broken".to_vec()),
        ("image", "image/png", vec![0; Limits::HARD.input_bytes + 1]),
        ("image", "image/png", png(1025, 1)),
    ] {
        let mut j = job();
        j.inputs = vec![crate::JobInput::Media { role: role.into(), mime: mime.into(), bytes: bytes.into() }];
        jobs.push(j);
    }
    let w = Arc::new(Wire::default());
    for j in jobs {
        assert_eq!(block_on(provider(&w).submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    }
    assert!(w.requests.lock().unwrap().is_empty());
}
#[test]
fn topaz37_geometry_and_exact_request_are_admitted_before_payment() {
    let j = job();
    let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: bytes.len(), ..Limits::default() }).is_ok());
    for limits in [
        Limits { request_bytes: bytes.len() - 1, ..Limits::default() },
        Limits { output_width: 3, ..Limits::default() },
        Limits { output_height: 5, ..Limits::default() },
        Limits { total_pixels: 23, ..Limits::default() },
    ] {
        let w = Arc::new(Wire::default());
        assert!(block_on(Fal::with_limits(key(), w.clone(), limits).unwrap().submit(&j)).is_err());
        assert!(w.requests.lock().unwrap().is_empty());
    }
    let mut j = job();
    j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png(1024, 1024).into() }];
    j.params.insert("upscale_factor".into(), json!(4));
    let (record, _) = request::prepare(&j, Limits::default()).unwrap();
    assert_eq!((record.width, record.height), (4096, 4096));
    assert!(u64::from(record.width) * u64::from(record.height) < 24_000_000, "every accepted image stays in one price block");
}
#[test]
fn topaz37_queue_resume_download_and_cancel_keep_routes_and_credentials_separate() {
    let image = png(4, 6);
    let w = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            reply(receipt(TOPAZ_IMAGE)),
            reply(json!({"status":"IN_PROGRESS"})),
            reply(json!({"status":"COMPLETED"})),
            reply(descriptor(image.len(), 4, 6)),
            Ok(HttpResponse { status: 200, headers: vec![], body: image.clone() }),
            Ok(HttpResponse { status: 202, headers: vec![], body: serde_json::to_vec(&json!({"status":"CANCELLATION_REQUESTED"})).unwrap() }),
        ])),
        ..Default::default()
    });
    let f = provider(&w);
    let Submission::Queued(h) = block_on(f.submit(&job())).unwrap() else { panic!() };
    assert_eq!(h, handle());
    let h: JobHandle = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
    assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: image.into() }]);
    block_on(f.cancel(&h)).unwrap();
    assert!(!f.retry_ambiguous_submit());
    let r = w.requests.lock().unwrap();
    assert_eq!(r.len(), 6);
    assert_eq!(r[0].url, "https://queue.fal.run/topaz/upscale/image/precision");
    assert_eq!(r[1].url, "https://queue.fal.run/topaz/upscale/requests/r_1/status");
    assert_eq!(r[3].url, "https://queue.fal.run/topaz/upscale/requests/r_1");
    assert_eq!(r[5].url, "https://queue.fal.run/topaz/upscale/requests/r_1/cancel");
    assert!(r[4].headers.is_empty());
    assert_eq!(r[4].max_response_bytes, 256 * 1024 * 1024);
    for i in [0, 1, 2, 3, 5] {
        assert!(r[i].headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Key {KEY}")));
    }
}
#[test]
fn topaz37_hostile_restored_contracts_and_wrong_queue_roots_never_send() {
    for (name, value) in [
        ("width", json!(0)),
        ("height", json!(4097)),
        ("requested", json!(2)),
        ("status_url", json!("https://queue.fal.run/topaz/upscale/image/precision/requests/r_1/status")),
        ("response_url", json!("https://evil.test/r_1")),
        ("cancel_url", json!("https://queue.fal.run/topaz/upscale/requests/other/cancel")),
    ] {
        let mut h = handle();
        h.data[name] = value;
        let w = Arc::new(Wire::default());
        assert!(block_on(provider(&w).poll(&h)).is_err());
        assert!(block_on(provider(&w).cancel(&h)).is_err());
        assert!(w.requests.lock().unwrap().is_empty());
    }
}
#[test]
fn topaz37_bad_descriptors_and_actual_outputs_never_publish() {
    for body in [
        json!({"image":[]}),
        json!({"images":[]}),
        json!({"image":{"url":"https://evil.test/x"}}),
        json!({"image":{"url":"https://fal.media/x","content_type":"image/jpeg"}}),
        json!({"image":{"url":"https://fal.media/x","file_size":256*1024*1024+1}}),
        json!({"image":{"url":"https://fal.media/x","width":4097}}),
        json!({"image":{"url":"https://fal.media/x"},"images":[]}),
    ] {
        let w = Arc::new(Wire { replies: Mutex::new(VecDeque::from([reply(json!({"status":"COMPLETED"})), reply(body)])), ..Default::default() });
        assert!(block_on(provider(&w).poll(&handle())).is_err());
        assert_eq!(w.requests.lock().unwrap().len(), 2);
    }
    for (body, image) in
        [(descriptor(png(4, 6).len(), 3, 6), png(4, 6)), (descriptor(png(4, 5).len(), 4, 5), png(4, 5)), (descriptor(6, 4, 6), b"broken".to_vec()), {
            let mut image = png(4, 6);
            image.push(0);
            (descriptor(image.len(), 4, 6), image)
        }]
    {
        let w = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([
                reply(json!({"status":"COMPLETED"})),
                reply(body),
                Ok(HttpResponse { status: 200, headers: vec![], body: image }),
            ])),
            ..Default::default()
        });
        assert!(block_on(provider(&w).poll(&handle())).is_err());
    }
}
#[test]
fn topaz37_large_valid_png_and_lower_only_caps_survive_restoration() {
    use image::ImageEncoder;
    // Procedural original 16-bit RGB noise stays incompressible and exceeds the old 16 MiB image cap.
    let side = 2048;
    let mut raw = vec![0u8; side * side * 6];
    let mut state = 37u32;
    for byte in &mut raw {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state as u8;
    }
    let mut image = vec![];
    image::codecs::png::PngEncoder::new(&mut image).write_image(&raw, side as u32, side as u32, image::ExtendedColorType::Rgb16).unwrap();
    drop(raw);
    assert!(image.len() > 16 * 1024 * 1024);
    assert!(image.len() < 256 * 1024 * 1024);
    let mut h = handle();
    h.data["width"] = json!(side);
    h.data["height"] = json!(side);
    let h: JobHandle = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
    let size = image.len();
    let w = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            reply(json!({"status":"COMPLETED"})),
            reply(descriptor(size, side as u32, side as u32)),
            Ok(HttpResponse { status: 200, headers: vec![], body: image.clone() }),
        ])),
        ..Default::default()
    });
    let JobProgress::Done(done) = block_on(provider(&w).poll(&h)).unwrap() else { panic!() };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: image.into() }]);
    assert_eq!(w.requests.lock().unwrap()[2].max_response_bytes, 256 * 1024 * 1024);
    for limits in [Limits { output_bytes: size - 1, ..Limits::default() }, Limits { total_output_bytes: size - 1, ..Limits::default() }] {
        let w = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([reply(json!({"status":"COMPLETED"})), reply(descriptor(size, side as u32, side as u32))])),
            ..Default::default()
        });
        assert!(block_on(Fal::with_limits(key(), w.clone(), limits).unwrap().poll(&h)).is_err());
        assert_eq!(w.requests.lock().unwrap().len(), 2, "declared quota refused before media GET");
    }
    for model in [Model::Image, Model::Upscale, Model::RemoveBackground, Model::Ideogram] {
        assert_eq!(Limits::default().for_model(model).output_bytes, 16 * 1024 * 1024);
    }
}
#[test]
fn topaz37_oriented_inputs_refuse_before_payment() {
    let image = include_bytes!("fixtures/oriented.jpg");
    let mut j = job();
    j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(image.as_slice()) }];
    let w = Arc::new(Wire::default());
    let e = block_on(provider(&w).submit(&j)).unwrap_err();
    assert!(e.message.contains("EXIF"));
    assert!(w.requests.lock().unwrap().is_empty());
}

#[test]
fn topaz37_upright_jpeg_and_webp_keep_their_original_wire_bytes() {
    use base64::Engine;
    use image::ImageEncoder;
    let rgb = vec![80; 7 * 5 * 3];
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg).encode(&rgb, 7, 5, image::ExtendedColorType::Rgb8).unwrap();
    let mut webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut webp).write_image(&rgb, 7, 5, image::ExtendedColorType::Rgb8).unwrap();
    for (mime, bytes) in [("image/jpeg", jpeg), ("image/webp", webp)] {
        let original = bytes.clone();
        let mut j = job();
        j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: mime.into(), bytes: bytes.into() }];
        let (record, body) = request::prepare(&j, Limits::default()).unwrap();
        assert_eq!((record.width, record.height), (14, 10));
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["image_url"], format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(original)));
    }
}
