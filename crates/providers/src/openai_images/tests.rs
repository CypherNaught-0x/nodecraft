//! Synthetic official-shape replies and authored tiny media, not authenticated API recordings.
use super::*;
use crate::{HttpRequest, HttpResponse, JobInput, Output, Usage};
use base64::Engine;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::sync::Mutex;

struct Canned {
    replies: Mutex<Vec<Result<HttpResponse, ProviderError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Canned {
    fn response(body: Vec<u8>) -> Arc<Self> {
        Self::status(200, body)
    }
    fn status(status: u16, body: Vec<u8>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(vec![Ok(HttpResponse { status, headers: vec![], body })]), requests: Mutex::default() })
    }
    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}
impl Http for Canned {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("Images must use the opt-in no-redirect operation")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let result = self.replies.lock().unwrap().pop().expect("no automatic second request");
        Box::pin(async move { result })
    }
}
fn image(format: &str, alpha: bool, color: u8) -> Vec<u8> {
    let alpha = alpha && format != "jpeg";
    let rgba = image::RgbaImage::from_pixel(2, 3, image::Rgba([color, 30, 40, if alpha { 100 } else { 255 }]));
    let im = if alpha { image::DynamicImage::ImageRgba8(rgba) } else { image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(rgba).to_rgb8()) };
    let f = match format {
        "jpeg" => image::ImageFormat::Jpeg,
        "webp" => image::ImageFormat::WebP,
        _ => image::ImageFormat::Png,
    };
    let mut out = std::io::Cursor::new(Vec::new());
    im.write_to(&mut out, f).unwrap();
    out.into_inner()
}
fn png() -> Vec<u8> {
    image("png", true, 10)
}
fn job() -> ProviderJob {
    ProviderJob {
        model: SUNBURST.into(),
        params: serde_json::Map::new(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "Draw a blue circle.\n雪".into() }],
        idempotency_key: "local-key-not-a-server-dedup-contract".into(),
    }
}
fn media(role: &str, format: &str, color: u8) -> JobInput {
    JobInput::Media { role: role.into(), mime: format!("image/{format}"), bytes: Arc::from(image(format, true, color)) }
}
fn body(images: Vec<Vec<u8>>) -> Vec<u8> {
    serde_json::to_vec(&json!({"data":images.into_iter().map(|bytes|json!({"b64_json":base64::engine::general_purpose::STANDARD.encode(bytes)})).collect::<Vec<_>>(),"usage":{"input_tokens":7,"output_tokens":11,"total_tokens":18,"input_tokens_details":{"text_tokens":3,"image_tokens":4}}})).unwrap()
}
fn client(http: Arc<dyn Http>, limits: Limits) -> OpenAiImages {
    OpenAiImages::with_limits(Secret::new("fixture-key-not-real").unwrap(), http, limits).unwrap()
}
fn counters() -> (usize, usize, usize, usize) {
    COUNTERS.with(|c| c.get())
}
fn reset() {
    COUNTERS.with(|c| c.set((0, 0, 0, 0)))
}
fn run(j: &ProviderJob, reply: Vec<u8>, limits: Limits) -> (Result<Submission, ProviderError>, Arc<Canned>) {
    let h = Canned::response(reply);
    let result = block_on(client(h.clone(), limits).submit(j));
    (result, h)
}
fn completed(value: Submission) -> Completed {
    match value {
        Submission::Finished(c) => c,
        Submission::Queued(_) => panic!("Images has no queue handle"),
    }
}

#[test]
fn generation_models_defaults_and_complete_option_matrix() {
    for model in [SUNBURST, FLARE] {
        for size in ["auto", "1024x1024", "1536x1024", "1024x1536"] {
            for quality in ["auto", "low", "medium", "high", "xhigh", "max"] {
                let mut j = job();
                j.model = model.into();
                j.params.insert("size".into(), json!(size));
                j.params.insert("quality".into(), json!(quality));
                let (done, h) = run(&j, body(vec![png()]), Limits::default());
                let c = completed(done.unwrap());
                assert_eq!(c.outputs.len(), 1);
                assert_eq!(c.usage.cost, None);
                assert_eq!((c.usage.input_tokens, c.usage.output_tokens), (Some(7), Some(11)));
                let req = h.requests().pop().unwrap();
                let v: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(req.url, format!("{ORIGIN}/images/generations"));
                assert_eq!(req.timeout_ms, 180_000);
                assert_eq!(v["model"], model);
                assert_eq!(v["size"], size);
                assert_eq!(v["quality"], quality);
                assert_eq!(v["n"], 1);
                assert_eq!(v["background"], "auto");
                assert_eq!(v["output_format"], "png");
                assert!(v.get("response_format").is_none());
                assert!(!req.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("Idempotency-Key")));
            }
        }
    }
}
#[test]
fn formats_transparency_compression_and_ten_outputs() {
    for format in ["png", "jpeg", "webp"] {
        for background in ["auto", "opaque", "transparent"] {
            if format == "jpeg" && background == "transparent" {
                continue;
            }
            let mut j = job();
            j.params.insert("output_format".into(), json!(format));
            j.params.insert("background".into(), json!(background));
            j.params.insert("n".into(), json!(10));
            if format != "png" {
                j.params.insert("output_compression".into(), json!(0));
            }
            let (done, h) = run(&j, body((0..10).map(|i| image(format, format != "jpeg", i)).collect()), Limits::default());
            let c = completed(done.unwrap());
            assert_eq!(c.outputs.len(), 10);
            assert_eq!(h.requests().len(), 1);
            for o in c.outputs {
                let Output::Bytes { mime, bytes } = o else { panic!("inline bytes") };
                assert_eq!(mime, format!("image/{format}"));
                assert_eq!(nodecraft_media::probe_image(&bytes, &nodecraft_media::Limits::default()).unwrap().mime, mime);
            }
        }
    }
    for compression in [1, 100] {
        let mut j = job();
        j.params.extend([("output_format".into(), json!("jpeg")), ("output_compression".into(), json!(compression))]);
        assert!(run(&j, body(vec![image("jpeg", false, 1)]), Limits::default()).0.is_ok());
    }
}
#[test]
fn edit_sixteen_references_stay_in_order_and_mask_follows_first_image() {
    let mut j = job();
    for i in 0..16 {
        j.inputs.push(media(if i % 2 == 0 { "image" } else { "reference" }, "png", i));
    }
    j.inputs.push(media("mask", "png", 99));
    let (_, h) = run(&j, body(vec![png()]), Limits::default());
    let req = h.requests().pop().unwrap();
    assert!(req.url.ends_with("/images/edits"));
    let ct = req.headers.iter().find(|(n, _)| n == "Content-Type").unwrap().1.clone();
    let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
    assert!(req.body.ends_with(format!("--{boundary}--\r\n").as_bytes()));
    let mut offset = 0;
    for i in 0..16 {
        let marker = format!("name=\"image[]\"; filename=\"image{i:03}.png\"");
        let at = req.body.windows(marker.len()).position(|w| w == marker.as_bytes()).unwrap();
        assert!(at >= offset);
        offset = at + marker.len();
        let bytes = image("png", true, i);
        assert!(req.body.get(offset..).unwrap().windows(bytes.len()).any(|w| w == bytes));
    }
    let mask = b"name=\"mask\"; filename=\"image016.png\"";
    let at = req.body.windows(mask.len()).position(|w| w == mask).unwrap();
    assert!(at > offset);
    assert_eq!(h.requests().len(), 1);
}
#[test]
fn request_refusals_precede_network_and_payload_encoding() {
    let mut invalid_jobs = Vec::new();
    for (key, value) in [
        ("n", json!(0)),
        ("n", json!(11)),
        ("n", json!(1.5)),
        ("n", json!("1")),
        ("size", json!("3840x0")),
        ("size", json!("256x256")),
        ("quality", json!("hd")),
        ("quality", json!(true)),
        ("input_fidelity", json!("high")),
        ("unknown", json!({"deep":[1,2]})),
        ("output_compression", json!(101)),
        ("output_compression", json!(0)),
    ] {
        let mut j = job();
        j.params.insert(key.into(), value);
        invalid_jobs.push(j);
    }
    let mut j = job();
    j.model = "gpt-image-2.5-sunburst".into();
    invalid_jobs.push(j);
    let mut j = job();
    j.params.extend([("background".into(), json!("transparent")), ("output_format".into(), json!("jpeg"))]);
    invalid_jobs.push(j);
    let mut j = job();
    j.inputs.push(j.inputs[0].clone());
    invalid_jobs.push(j);
    let mut j = job();
    j.inputs.clear();
    invalid_jobs.push(j);
    let mut j = job();
    j.inputs.push(media("mask", "png", 1));
    invalid_jobs.push(j);
    let mut j = job();
    j.inputs.push(media("other", "png", 1));
    invalid_jobs.push(j);
    let mut j = job();
    for _ in 0..17 {
        j.inputs.push(media("image", "png", 1));
    }
    invalid_jobs.push(j);
    let mut j = job();
    j.inputs.extend([media("image", "png", 1), media("mask", "png", 2), media("mask", "png", 3)]);
    invalid_jobs.push(j);
    let mut j = job();
    j.inputs.push(JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(b"not PNG".as_slice()) });
    invalid_jobs.push(j);
    let mut j = job();
    j.inputs.extend([media("image", "jpeg", 1), media("mask", "png", 2)]);
    invalid_jobs.push(j);
    let mut j = job();
    j.inputs.extend([media("image", "png", 1), JobInput::Media { role: "mask".into(), mime: "image/png".into(), bytes: Arc::from(image("png", false, 2)) }]);
    invalid_jobs.push(j);
    for j in invalid_jobs {
        reset();
        let (result, h) = run(&j, body(vec![png()]), Limits::default());
        assert!(result.is_err());
        assert!(h.requests().is_empty());
        assert_eq!(counters().0, 0);
    }
}
#[test]
fn lower_only_input_wire_metadata_pixel_and_codec_limits() {
    let mut j = job();
    j.inputs.push(media("image", "png", 1));
    let valid = Limits::default();
    let mut cases = Vec::new();
    for field in 0..8 {
        let mut l = valid;
        match field {
            0 => l.input_bytes = 1,
            1 => l.total_input_bytes = 1,
            2 => l.request_bytes = 1,
            3 => l.metadata_bytes = 1,
            4 => l.width = 1,
            5 => l.height = 1,
            6 => l.total_pixels = 1,
            _ => l.codec_bytes = 1,
        };
        cases.push(l);
    }
    for l in cases {
        reset();
        let (result, h) = run(&j, body(vec![png()]), l);
        assert!(result.is_err());
        assert!(h.requests().is_empty());
        assert_eq!(counters().0, 0);
    }
    let mut l = valid;
    l.response_bytes += 1;
    assert!(OpenAiImages::with_limits(Secret::new("fixture").unwrap(), Canned::response(vec![]), l).is_err());
    let mut j = job();
    j.inputs = vec![JobInput::Text { role: "prompt".into(), text: "雪雪".into() }];
    let mut l = valid;
    l.prompt_chars = 2;
    l.prompt_bytes = 6;
    assert!(run(&j, body(vec![png()]), l).0.is_ok());
    l.prompt_bytes = 5;
    assert!(run(&j, body(vec![png()]), l).0.is_err());
    l.prompt_bytes = 6;
    l.prompt_chars = 1;
    assert!(run(&j, body(vec![png()]), l).0.is_err());
}
#[test]
fn partial_n_is_retained_once_without_filling_and_usage_is_optional() {
    let mut j = job();
    j.params.insert("n".into(), json!(10));
    let (done, h) =
        run(&j, serde_json::to_vec(&json!({"data":[{"b64_json":base64::engine::general_purpose::STANDARD.encode(png())}]})).unwrap(), Limits::default());
    let c = completed(done.unwrap());
    assert_eq!(c.outputs.len(), 1);
    assert_eq!(c.usage, Usage::default());
    assert_eq!(h.requests().len(), 1);
}
#[test]
fn response_cheap_refusals_allocate_no_decoded_payload_and_late_bad_images_publish_nothing() {
    let good = base64::engine::general_purpose::STANDARD.encode(png());
    for value in [
        json!({"data":[]}),
        json!({"data":[{"url":"https://do-not-fetch/private"}]}),
        json!({"data":[{"b64_json":"A==="}]}),
        json!({"data":[{"b64_json":"AB=="}]}),
        json!({"data":[{"b64_json":"AAAA="}]}),
        json!({"data":[{"b64_json":"%%%%"}]}),
        json!({"data":[{"b64_json":good},{"b64_json":good}]}),
    ] {
        reset();
        assert!(run(&job(), serde_json::to_vec(&value).unwrap(), Limits::default()).0.is_err());
        assert_eq!(counters().1, 0);
        assert_eq!(counters().2, 0);
    }
    let l = Limits { total_output_bytes: png().len() - 1, ..Limits::default() };
    reset();
    assert!(run(&job(), body(vec![png()]), l).0.is_err());
    assert_eq!(counters().1, 0);
    let mut j = job();
    j.params.insert("n".into(), json!(2));
    reset();
    assert!(run(&j, body(vec![png(), b"bad-image".to_vec()]), Limits::default()).0.is_err());
    assert_eq!(counters().1, 2);
    assert_eq!(counters().2, 0);
}
#[test]
fn lexical_limits_apply_before_serde_unescaping_including_unknown_fields() {
    let l = Limits { metadata_bytes: 512, ..Limits::default() };
    for raw in [
        format!(r#"{{"data":[],"unknown":"{}"}}"#, r"\u0061".repeat(513)),
        format!("{}0{}", "[".repeat(18), "]".repeat(18)),
        format!(r#"{{"data":[],{}}}"#, (0..65).map(|i| format!("\"k{i}\":0")).collect::<Vec<_>>().join(",")),
        r#"{"data":[],"unknown":"\uD800"}"#.into(),
        r#"{"d\u0061ta":[]}"#.into(),
    ] {
        reset();
        assert!(run(&job(), raw.into_bytes(), l).0.is_err());
        assert_eq!(counters(), (1, 0, 0, 0), "only request encoding occurred; serde was never entered");
    }
    let base = body(vec![png()]);
    let raw = String::from_utf8(base).unwrap();
    let at = raw.find("iVBOR").unwrap();
    let mut escaped = raw.clone();
    escaped.replace_range(at..at + 1, r"\u0069");
    reset();
    assert!(run(&job(), escaped.into_bytes(), Limits::default()).0.is_ok());
    assert_eq!(counters().3, 1);
}
#[test]
fn duplicate_critical_fields_usage_overflows_and_mime_alpha_mismatch_are_safe_errors() {
    let good = base64::engine::general_purpose::STANDARD.encode(png());
    let duplicate = format!(r#"{{"data":[{{"b64_json":"{good}","b64_json":"{good}"}}]}}"#);
    assert!(run(&job(), duplicate.into_bytes(), Limits::default()).0.is_err());
    let mut b: Value = serde_json::from_slice(&body(vec![png()])).unwrap();
    b["usage"]["input_tokens_details"]["text_tokens"] = json!(u64::MAX);
    assert!(run(&job(), serde_json::to_vec(&b).unwrap(), Limits::default()).0.is_err());
    assert!(run(&job(), body(vec![image("jpeg", false, 1)]), Limits::default()).0.is_err());
    let mut j = job();
    j.params.insert("background".into(), json!("transparent"));
    assert!(run(&j, body(vec![image("png", false, 1)]), Limits::default()).0.is_err());
}
#[test]
fn provider_errors_are_classified_without_echoing_hostile_details() {
    for (status, code, ty, kind) in [
        (400, "moderation_blocked", "invalid_request_error", ErrorKind::BlockedContent),
        (500, "bad_input", "image_generation_user_error", ErrorKind::UnsupportedInput),
        (429, "credit_balance_exhausted", "insufficient_quota", ErrorKind::Budget),
        (429, "project_spend_limit_exceeded", "insufficient_quota", ErrorKind::Budget),
        (429, "slow_down", "rate_limit_error", ErrorKind::RateLimited),
        (401, "bad_key", "auth", ErrorKind::Auth),
        (403, "model_access", "auth", ErrorKind::Auth),
        (410, "retired", "invalid", ErrorKind::NotFound),
        (503, "server_is_overloaded", "service_unavailable_error", ErrorKind::Busy),
        (500, "unknown", "error", ErrorKind::Server),
        (408, "timeout", "error", ErrorKind::Timeout),
        (307, "redirect", "error", ErrorKind::Malformed),
    ] {
        let body =
            serde_json::to_vec(&json!({"error":{"code":code,"type":ty,"message":"fixture-key-not-real private prompt https://signed/?token=secret"}})).unwrap();
        let h = Canned::status(status, body);
        let e = block_on(client(h.clone(), Limits::default()).submit(&job())).unwrap_err();
        assert_eq!(e.kind, kind);
        let text = e.to_string();
        assert!(!text.contains("fixture-key") && !text.contains("private prompt") && !text.contains("signed") && !text.contains("token"));
        assert_eq!(h.requests().len(), 1);
    }
    let h = Canned::status(429, b"not JSON and contains fixture-key-not-real".to_vec());
    let e = block_on(client(h, Limits::default()).submit(&job())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::RateLimited);
    assert!(!e.to_string().contains("fixture-key"));
}
#[test]
fn image_submit_uncertainty_and_retry_policy_are_explicit() {
    assert!(!client(Canned::response(body(vec![png()])), Limits::default()).retry_ambiguous_submit());
    for kind in [ErrorKind::Network, ErrorKind::Timeout, ErrorKind::Server] {
        let h = Arc::new(Canned { replies: Mutex::new(vec![Err(ProviderError::new(kind, "lost after possible acceptance"))]), requests: Mutex::default() });
        let p = client(h.clone(), Limits::default());
        let e = block_on(p.submit(&job())).unwrap_err();
        assert_eq!(e.kind, kind);
        assert_eq!(h.requests().len(), 1);
    }
}

#[test]
fn non_mask_edits_admit_jpeg_and_webp_without_reordering_or_transcoding() {
    for format in ["jpeg", "webp"] {
        let mut j = job();
        j.inputs.extend([media("image", format, 1), media("reference", format, 2)]);
        let (done, h) = run(&j, body(vec![png()]), Limits::default());
        assert!(done.is_ok());
        let req = h.requests().pop().unwrap();
        let first = image(format, true, 1);
        let second = image(format, true, 2);
        let a = req.body.windows(first.len()).position(|w| w == first).unwrap();
        let b = req.body.windows(second.len()).position(|w| w == second).unwrap();
        assert!(a < b);
        assert!(!format!("{req:?}").contains("fixture-key"));
    }
}
