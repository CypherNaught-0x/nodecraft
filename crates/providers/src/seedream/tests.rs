//! Authored media and synthetic official-shape JSON; no paid provider recordings.
use super::*;
use crate::{HttpRequest, HttpResponse, JobInput, Output, Usage};
use base64::Engine;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::sync::Mutex;
struct Canned {
    reply: Mutex<Option<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Canned {
    fn new(status: u16, body: Vec<u8>) -> Arc<Self> {
        Arc::new(Self { reply: Mutex::new(Some(HttpResponse { status, body, headers: vec![] })), requests: Mutex::default() })
    }
}
impl Http for Canned {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-following fallback forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.reply.lock().unwrap().take().expect("exactly one request");
        Box::pin(async move { Ok(reply) })
    }
}
fn job() -> ProviderJob {
    ProviderJob {
        model: MODEL.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "Blue circle\n雪".into() }],
        idempotency_key: "fixture-key-no-server-dedup".into(),
    }
}
pub(super) fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let im = image::RgbImage::from_pixel(w, h, image::Rgb([20, 30, 40]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(im).write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}
pub(super) fn body(bytes: &[u8], w: u32, h: u32) -> Vec<u8> {
    serde_json::to_vec(&json!({"model":MODEL,"data":[{"b64_json":base64::engine::general_purpose::STANDARD.encode(bytes),"size":format!("{w}x{h}")}],"usage":{"generated_images":1,"output_tokens":7,"total_tokens":7}})).unwrap()
}
fn run(j: &ProviderJob, b: Vec<u8>, l: Limits) -> (Result<Submission, ProviderError>, Arc<Canned>) {
    let h = Canned::new(200, b);
    let p = Seedream::with_limits(Secret::new("fixture-secret-not-real").unwrap(), h.clone(), l).unwrap();
    (block_on(p.submit(j)), h)
}
fn done(r: Result<Submission, ProviderError>) -> Completed {
    let Submission::Finished(c) = r.unwrap() else { panic!("synchronous") };
    c
}
fn media(bytes: Arc<[u8]>) -> JobInput {
    JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes }
}
fn reset() {
    COUNTERS.with(|v| v.set([0; 4]));
}
fn counts() -> [usize; 4] {
    COUNTERS.with(|v| v.get())
}
#[test]
fn generation_and_ordered_one_two_fourteen_references_use_exact_wire() {
    let jpg = jpeg(16, 16);
    let first: Arc<[u8]> = Arc::from(jpeg(16, 17));
    let second: Arc<[u8]> = Arc::from(jpeg(17, 16));
    for n in [0, 1, 2, 14] {
        let mut j = job();
        for i in 0..n {
            j.inputs.push(media(if i % 2 == 0 { first.clone() } else { second.clone() }));
        }
        let (c, h) = run(&j, body(&jpg, 16, 16), Limits::HARD);
        let c = done(c);
        assert_eq!(c.outputs.len(), 1);
        assert_eq!(c.usage.output_tokens, Some(7));
        assert_eq!(c.usage.cost, None);
        let reqs = h.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        let r = &reqs[0];
        assert_eq!(r.url, API);
        assert!(r.secret_headers.iter().any(|k| k == "authorization"));
        assert!(!format!("{r:?}").contains("fixture-secret"));
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["model"], MODEL);
        assert_eq!(v["stream"], false);
        assert_eq!(v["watermark"], true);
        assert_eq!(v["response_format"], "b64_json");
        assert_eq!(v["sequential_image_generation"], "disabled");
        assert!(v.get("output_format").is_none());
        assert!(v.get("idempotency_key").is_none());
        if n == 0 {
            assert!(v.get("image").is_none());
        } else {
            let files = v["image"].as_array().unwrap();
            assert_eq!(files.len(), n);
            for (i, f) in files.iter().enumerate() {
                let raw = f.as_str().unwrap().strip_prefix("data:image/jpeg;base64,").unwrap();
                assert_eq!(
                    base64::engine::general_purpose::STANDARD.decode(raw).unwrap().as_slice(),
                    if i % 2 == 0 { first.as_ref() } else { second.as_ref() }
                );
            }
        }
    }
}
#[test]
fn invalid_last_input_and_metadata_refuse_before_encode_or_send() {
    let good: Arc<[u8]> = Arc::from(jpeg(16, 16));
    let mut cases = vec![];
    let mut j = job();
    for _ in 0..15 {
        j.inputs.push(media(good.clone()));
    }
    cases.push(j);
    let mut j = job();
    j.inputs.push(media(good.clone()));
    j.inputs.push(media(Arc::from(&b"not jpeg"[..])));
    cases.push(j);
    for key in ["prompt", "output_format", "model", "stream", "size"] {
        let mut j = job();
        j.params.insert(key.into(), json!("hostile"));
        cases.push(j);
    }
    let mut j = job();
    j.inputs.push(JobInput::Text { role: "prompt".into(), text: "duplicate".into() });
    cases.push(j);
    let mut j = job();
    j.inputs.push(media(Arc::from(jpeg(14, 16))));
    cases.push(j);
    for j in cases {
        reset();
        let (r, h) = run(&j, body(&jpeg(16, 16), 16, 16), Limits::HARD);
        assert!(r.is_err());
        assert!(h.requests.lock().unwrap().is_empty());
        assert_eq!(counts()[0], 0);
    }
}
#[test]
fn lower_only_limits_wire_metadata_and_input_boundaries() {
    assert!(Seedream::with_limits(Secret::new("fixture").unwrap(), Canned::new(200, vec![]), Limits { axis: 16385, ..Limits::HARD }).is_err());
    let j = job();
    let p = request::prepare(&j, Limits::HARD).unwrap();
    let length = p.encode(&Secret::new("fixture").unwrap(), Limits::HARD).unwrap().body.len();
    for cap in [length - 1, length, length + 1] {
        reset();
        let (r, h) = run(&j, body(&jpeg(16, 16), 16, 16), Limits { request_bytes: cap, ..Limits::HARD });
        assert_eq!(r.is_ok(), cap >= length);
        assert_eq!(h.requests.lock().unwrap().len(), usize::from(cap >= length));
        assert_eq!(counts()[0], usize::from(cap >= length));
    }
    let mut huge = String::with_capacity(1024 * 1024);
    huge.push_str("2K");
    let mut j = job();
    j.params.insert("size".into(), Value::String(huge));
    assert!(run(&j, body(&jpeg(16, 16), 16, 16), Limits::HARD).0.is_ok(), "borrowed caller spare capacity is not retained");
    let mut j = job();
    j.inputs.push(media(Arc::from(jpeg(16, 16))));
    let len = match &j.inputs[1] {
        JobInput::Media { bytes, .. } => bytes.len(),
        _ => panic!("fixture media"),
    };
    for cap in [len - 1, len] {
        let (r, h) = run(&j, body(&jpeg(16, 16), 16, 16), Limits { input_bytes: cap, ..Limits::HARD });
        assert_eq!(r.is_ok(), cap == len);
        assert_eq!(h.requests.lock().unwrap().len(), usize::from(cap == len));
    }
}
#[test]
fn wide_valid_four_k_geometry_is_not_clamped_to4096_after_payment() {
    for (w, h) in [(8192, 2048), (16384, 1024)] {
        let jpg = jpeg(w, h);
        let mut j = job();
        j.params.insert("size".into(), json!("4K"));
        let c = done(run(&j, body(&jpg, w, h), Limits::HARD).0);
        let Output::Bytes { bytes, .. } = &c.outputs[0] else { panic!() };
        assert_eq!(bytes.as_ref(), jpg);
    }
    for (w, h) in [(16385, 1024), (16384, 1025), (16384, 1023), (0, 16), (u32::MAX, u32::MAX)] {
        reset();
        let (r, _) = run(&job(), body(&jpeg(16, 16), w, h), Limits::HARD);
        assert!(r.is_err());
        assert_eq!(counts()[2], 0, "declared impossible geometry refuses before output allocation");
    }
}
#[test]
fn every_required_record_is_map_only_and_all_required_fields_unique() {
    let good: Value = serde_json::from_slice(&body(&jpeg(16, 16), 16, 16)).unwrap();
    let mut cases = vec![json!([MODEL, good["data"]]), Value::Null];
    for key in ["model", "data"] {
        let mut v = good.clone();
        v.as_object_mut().unwrap().remove(key);
        cases.push(v);
        let mut v = good.clone();
        v[key] = Value::Null;
        cases.push(v);
    }
    let mut v = good.clone();
    v["data"] = json!([[good["data"][0]["b64_json"], "16x16"]]);
    cases.push(v);
    for key in ["size", "b64_json"] {
        let mut v = good.clone();
        v["data"][0].as_object_mut().unwrap().remove(key);
        cases.push(v);
    }
    for size in ["+16x16", "16x0", "16x16x1", "16X16", " 16x16", "4294967296x16", "17x16"] {
        let mut v = good.clone();
        v["data"][0]["size"] = json!(size);
        cases.push(v);
    }
    for key in ["error", "url"] {
        let mut v = good.clone();
        v["data"][0][key] = json!(null);
        cases.push(v);
    }
    let mut v = good.clone();
    v["model"] = json!("another-model");
    cases.push(v);
    let mut v = good.clone();
    v["data"].as_array_mut().unwrap().push(good["data"][0].clone());
    cases.push(v);
    for v in cases {
        assert!(run(&job(), serde_json::to_vec(&v).unwrap(), Limits::HARD).0.is_err());
    }
    let raw = String::from_utf8(serde_json::to_vec(&good).unwrap()).unwrap();
    for (s, t) in [
        ("\"model\":", format!("\"model\":\"{MODEL}\",\"model\":")),
        ("\"size\":", "\"size\":\"16x16\",\"size\":".into()),
        ("\"b64_json\":", "\"b64_json\":\"abcd\",\"b64_json\":".into()),
    ] {
        assert!(run(&job(), raw.replacen(s, &t, 1).into_bytes(), Limits::HARD).0.is_err());
    }
}
#[test]
fn invalid_optional_usage_preserves_paid_jpeg_as_unknown() {
    let good: Value = serde_json::from_slice(&body(&jpeg(16, 16), 16, 16)).unwrap();
    for u in [
        Value::Null,
        json!([]),
        json!([1, 7, 7]),
        json!({"output_tokens":"7"}),
        json!({"output_tokens":-1}),
        json!({"output_tokens":7,"total_tokens":8}),
        json!({"generated_images":2}),
        json!({"output_tokens":1e100}),
    ] {
        let mut v = good.clone();
        v["usage"] = u;
        let c = done(run(&job(), serde_json::to_vec(&v).unwrap(), Limits::HARD).0);
        assert_eq!(c.usage, Usage::default());
        assert_eq!(c.outputs.len(), 1);
    }
    let mut no = good.clone();
    no.as_object_mut().unwrap().remove("usage");
    assert_eq!(done(run(&job(), serde_json::to_vec(&no).unwrap(), Limits::HARD).0).usage, Usage::default());
    let raw = String::from_utf8(serde_json::to_vec(&good).unwrap()).unwrap();
    for raw in [raw.replacen("\"usage\":", "\"usage\":{},\"usage\":", 1), raw.replacen("\"output_tokens\":7", "\"output_tokens\":8,\"output_tokens\":7", 1)] {
        assert_eq!(done(run(&job(), raw.into_bytes(), Limits::HARD).0).usage, Usage::default());
    }
    let mut v = good;
    v["usage"] = json!({"output_tokens":u64::MAX,"total_tokens":u64::MAX});
    assert_eq!(done(run(&job(), serde_json::to_vec(&v).unwrap(), Limits::HARD).0).usage.output_tokens, Some(u64::MAX));
}
#[test]
fn body_codec_and_lexical_limits_do_not_publish_partial_or_allocate_early() {
    let good = body(&jpeg(16, 16), 16, 16);
    for cap in [good.len() - 1, good.len()] {
        let (r, _) = run(&job(), good.clone(), Limits { response_bytes: cap, ..Limits::HARD });
        assert_eq!(r.is_ok(), cap == good.len());
    }
    let v: Value = serde_json::from_slice(&good).unwrap();
    let raw = String::from_utf8(good.clone()).unwrap();
    let blob = v["data"][0]["b64_json"].as_str().unwrap();
    let escaped = raw.replace(blob, &blob.replacen('/', "\\/", 1));
    assert!(escaped != raw);
    assert!(run(&job(), escaped.into_bytes(), Limits::HARD).0.is_err());
    for b in ["a", "====", "AAAA=", "AB==", "AA A", "data:image/jpeg;base64,AAAA"] {
        let mut v = v.clone();
        v["data"][0]["b64_json"] = json!(b);
        assert!(run(&job(), serde_json::to_vec(&v).unwrap(), Limits::HARD).0.is_err());
    }
    let mut corrupt = jpeg(16, 16);
    corrupt.truncate(corrupt.len() / 2);
    assert!(run(&job(), body(&corrupt, 16, 16), Limits::HARD).0.is_err());
    let png = nodecraft_media::encode_png(16, 16, &[0; 16 * 16 * 4], &nodecraft_media::Limits::default()).unwrap();
    assert!(run(&job(), body(&png, 16, 16), Limits::HARD).0.is_err());
    let mut v = v;
    v["unknown"] = json!("x".repeat(100));
    reset();
    assert!(run(&job(), serde_json::to_vec(&v).unwrap(), Limits { metadata_bytes: 10, ..Limits::HARD }).0.is_err());
    assert_eq!(counts()[2], 0);
    let deep = format!("{{\"unknown\":{}0{}}}", "[".repeat(17), "]".repeat(17));
    assert!(run(&job(), deep.into_bytes(), Limits::HARD).0.is_err());
}
#[test]
fn http_errors_are_static_and_never_leak_payload_or_invent_safety_codes() {
    for (status, kind) in [
        (302, ErrorKind::Malformed),
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (403, ErrorKind::Auth),
        (408, ErrorKind::Timeout),
        (429, ErrorKind::RateLimited),
        (503, ErrorKind::Busy),
        (500, ErrorKind::Server),
    ] {
        let h = Canned::new(status, b"secret prompt fixture-secret".to_vec());
        let p = Seedream::new(Secret::new("fixture-secret").unwrap(), h);
        let e = block_on(p.submit(&job())).unwrap_err();
        assert_eq!(e.kind, kind);
        assert!(!e.message.contains("secret"));
    }
    let mut v: Value = serde_json::from_slice(&body(&jpeg(16, 16), 16, 16)).unwrap();
    v["error"] = json!({"code":"undocumented_safety_code","message":"secret"});
    assert_eq!(run(&job(), serde_json::to_vec(&v).unwrap(), Limits::HARD).0.unwrap_err().kind, ErrorKind::JobFailed);
}
#[test]
fn catalog_contains_both_modes_and_exact_single_output_estimate() {
    let c = nodecraft_catalog::Catalog::builtin().unwrap();
    let m = c.get("seedream.4_5").unwrap();
    assert_eq!(m.provider_model, MODEL);
    assert_eq!(m.modes.len(), 2);
    assert_eq!(m.quote(&Default::default()).unwrap().unwrap().micros, 40000);
}

#[test]
fn escaped_usage_keys_stay_bounded_and_duplicate_optional_members_become_unknown() {
    let raw = String::from_utf8(body(&jpeg(16, 16), 16, 16)).unwrap();
    let escaped = raw.replacen("output_tokens", r"output_\u0074okens", 1);
    assert_eq!(done(run(&job(), escaped.into_bytes(), Limits::HARD).0).usage.output_tokens, Some(7));
    let duplicate = raw.replacen("\"output_tokens\":7", r#""output_tokens":7,"output_\u0074okens":8"#, 1);
    assert_eq!(done(run(&job(), duplicate.into_bytes(), Limits::HARD).0).usage, Usage::default());
}

#[test]
fn jpeg_trailing_payload_is_not_silently_filtered_to_a_valid_prefix() {
    let mut bytes = jpeg(16, 16);
    bytes.extend_from_slice(b"trailing-private-body");
    assert!(run(&job(), body(&bytes, 16, 16), Limits::HARD).0.is_err());
    bytes.extend_from_slice(&[255, 217]);
    assert!(run(&job(), body(&bytes, 16, 16), Limits::HARD).0.is_err());
}
