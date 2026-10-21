use super::*;
use futures::executor::block_on;
use serde_json::json;
use std::{cell::RefCell, collections::VecDeque, sync::Mutex};
thread_local! {static STAGES:RefCell<[usize;3]>=const{RefCell::new([0;3])};}
pub(super) fn observe(n: usize) {
    STAGES.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(v) = s.get_mut(n) {
            *v += 1;
        }
    });
}
fn stages() -> [usize; 3] {
    STAGES.with(|s| *s.borrow())
}
fn reset() {
    STAGES.with(|s| *s.borrow_mut() = [0; 3]);
}
const KEY: &str = "fal-fixture-key-not-real";
fn key() -> Secret {
    Secret::new(KEY).unwrap()
}
#[derive(Default)]
struct Transport {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Transport {
    fn new(replies: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(vec![]), replies: Mutex::new(replies.into_iter().map(Ok).collect()) })
    }
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("legacy send must not run")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn reply(v: serde_json::Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&v).unwrap() }
}
fn bytes(b: Vec<u8>) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: b }
}
pub(super) fn png(w: u32, h: u32) -> Vec<u8> {
    nodecraft_media::encode_png(w, h, &vec![120; usize::try_from(w * h * 4).unwrap()], &nodecraft_media::Limits::default()).unwrap()
}
pub(super) fn job(model: &str) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: Default::default(),
        inputs: match model {
            TRELLIS | ESRGAN => vec![crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(png(2, 2)) }],
            WAN_I2V => vec![
                crate::JobInput::Text { role: "prompt".into(), text: "A blue circle".into() },
                crate::JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: Arc::from(png(2, 2)) },
            ],
            WAN_S2V => vec![
                crate::JobInput::Text { role: "prompt".into(), text: "A blue circle".into() },
                crate::JobInput::Media { role: "portrait".into(), mime: "image/png".into(), bytes: Arc::from(png(2, 2)) },
                crate::JobInput::Media { role: "speech".into(), mime: "audio/wav".into(), bytes: Arc::from(include_bytes!("fixtures/tone.wav").as_slice()) },
            ],
            _ => vec![crate::JobInput::Text { role: "prompt".into(), text: "A blue circle".into() }],
        },
        idempotency_key: "fixture-operation".into(),
    }
}
pub(super) fn receipt(model: &str) -> serde_json::Value {
    let m = Model::read(model).unwrap();
    let root = format!("{ORIGIN}/{}/requests/r_1", m.root());
    json!({"request_id":"r_1","status":"IN_QUEUE","status_url":format!("{root}/status"),"response_url":root,"cancel_url":format!("{root}/cancel")})
}
fn handle(model: &str) -> JobHandle {
    let o = receipt(model);
    Record { model: Model::read(model).unwrap(), requested: 1, width: if model == ESRGAN { 4 } else { 0 }, height: if model == ESRGAN { 4 } else { 0 } }.handle(
        "r_1",
        &Routes {
            status: o["status_url"].as_str().unwrap().into(),
            result: o["response_url"].as_str().unwrap().into(),
            cancel: o["cancel_url"].as_str().unwrap().into(),
        },
    )
}
fn output(model: &str) -> (serde_json::Value, Vec<u8>, &'static str) {
    let file = json!({"url":"https://v3.fal.media/files/fixture","content_type":null,"file_size":null});
    match model {
        FLUX => (json!({"images":[{"url":"https://fal.media/fixture","width":2,"height":2}]}), png(2, 2), "image/png"),
        WAN => (json!({"video":file}), include_bytes!("fixtures/blue.mp4").to_vec(), "video/mp4"),
        // fal labels some clips with a generic content type; the bytes are still sniffed as MP4.
        WAN_I2V | WAN_S2V => (
            json!({"video":{"url":"https://v3.fal.media/files/fixture","content_type":"application/octet-stream","file_size":null}}),
            include_bytes!("fixtures/blue.mp4").to_vec(),
            "video/mp4",
        ),
        KOKORO => (json!({"audio":file}), include_bytes!("fixtures/tone.wav").to_vec(), "audio/wav"),
        TRELLIS => (json!({"model_glb":file}), include_bytes!("fixtures/triangle.glb").to_vec(), "model/gltf-binary"),
        ESRGAN => (json!({"image":file}), png(4, 4), "image/png"),
        _ => panic!(),
    }
}
#[test]
fn all_seven_actual_submit_resume_result_download_contracts() {
    for model in [FLUX, WAN, WAN_I2V, WAN_S2V, KOKORO, TRELLIS, ESRGAN] {
        let (o, b, mime) = output(model);
        let original = b.clone();
        let t = Transport::new(vec![reply(receipt(model)), reply(json!({"status":"IN_PROGRESS"})), reply(json!({"status":"COMPLETED"})), reply(o), bytes(b)]);
        let f = Fal::new(key(), t.clone());
        let Submission::Queued(h) = block_on(f.submit(&job(model))).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        let restored: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert!(matches!(block_on(f.poll(&restored)).unwrap(), JobProgress::Running { .. }));
        let JobProgress::Done(done) = block_on(f.poll(&restored)).unwrap() else { panic!() };
        assert_eq!(done, Completed::new(vec![Output::Bytes { mime: mime.into(), bytes: original.into() }]));
        let req = t.requests.lock().unwrap();
        assert_eq!(req.len(), 5);
        assert_eq!(req[0].method, HttpMethod::Post);
        assert_eq!(req[0].url, format!("{ORIGIN}/{model}"));
        for q in req.iter().take(4) {
            assert!(q.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Key {KEY}")));
        }
        assert!(req[4].headers.is_empty());
        assert!(req[0].headers.iter().any(|(k, v)| k == "X-Fal-No-Retry" && v == "1"));
        assert!(req[0].headers.iter().any(|(k, v)| k == "x-app-fal-disable-fallback" && v == "true"));
        assert!(!f.retry_ambiguous_submit());
    }
}
#[test]
fn independent_wire_expectations_for_every_model() {
    for model in [FLUX, WAN, WAN_I2V, WAN_S2V, KOKORO, TRELLIS, ESRGAN] {
        let t = Transport::new(vec![reply(receipt(model))]);
        let f = Fal::new(key(), t.clone());
        block_on(f.submit(&job(model))).unwrap();
        let req = t.requests.lock().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&req[0].body).unwrap();
        let expected = match model {
            FLUX => {
                json!({"prompt":"A blue circle","image_size":{"width":1024,"height":1024},"num_images":1,"num_inference_steps":4,"output_format":"png","sync_mode":false})
            }
            WAN => {
                json!({"prompt":"A blue circle","num_frames":81,"frames_per_second":16,"resolution":"480p","aspect_ratio":"16:9","interpolator_model":"none","num_interpolated_frames":0,"adjust_fps_for_interpolation":false})
            }
            WAN_I2V => {
                json!({"prompt":"A blue circle","image_url":body["image_url"],"num_frames":81,"frames_per_second":16,"resolution":"480p","aspect_ratio":"auto","interpolator_model":"none","num_interpolated_frames":0,"adjust_fps_for_interpolation":false})
            }
            WAN_S2V => {
                json!({"prompt":"A blue circle","image_url":body["image_url"],"audio_url":body["audio_url"],"num_frames":80,"frames_per_second":16,"resolution":"480p"})
            }
            KOKORO => json!({"prompt":"A blue circle","voice":"af_heart","speed":1.0}),
            TRELLIS => json!({"resolution":512,"image_url":body["image_url"]}),
            ESRGAN => json!({"model":"RealESRGAN_x4plus","scale":2,"face":false,"output_format":"png","image_url":body["image_url"]}),
            _ => panic!(),
        };
        assert_eq!(body, expected);
        if [TRELLIS, ESRGAN, WAN_I2V, WAN_S2V].contains(&model) {
            use base64::Engine;
            let uri = body["image_url"].as_str().unwrap().strip_prefix("data:image/png;base64,").unwrap();
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(uri).unwrap(), png(2, 2));
        }
    }
}
#[test]
fn exact_request_extent_and_all_precodec_refusals() {
    let j = job(TRELLIS);
    let (_, body) = request::prepare(&j, Limits::HARD).unwrap();
    for (cap, ok) in [(body.len(), true), (body.len() - 1, false)] {
        reset();
        let r = request::prepare(&j, Limits { request_bytes: cap, ..Limits::HARD });
        assert_eq!(r.is_ok(), ok);
        assert_eq!(stages(), if ok { [1, 1, 0] } else { [0, 0, 0] });
    }
    for bad in 0..7 {
        let mut j = job(TRELLIS);
        match bad {
            0 => j.params.insert("hostile".into(), json!({"deep":[]})),
            1 => {
                j.inputs.push(crate::JobInput::Text { role: "unexpected".into(), text: "x".into() });
                None
            }
            2 => {
                j.idempotency_key = "x".repeat(257);
                None
            }
            3 => {
                j.model = "unlisted".into();
                None
            }
            4 => {
                j.inputs.push(j.inputs[0].clone());
                None
            }
            5 => {
                let crate::JobInput::Media { bytes, .. } = &mut j.inputs[0] else { panic!() };
                *bytes = Arc::from(vec![0; Limits::HARD.input_bytes + 1]);
                None
            }
            _ => j.params.insert("seed".into(), json!(-1)),
        };
        reset();
        assert!(request::prepare(&j, Limits::HARD).is_err());
        assert_eq!(stages(), [0, 0, 0]);
    }
    let mut corrupt = job(TRELLIS);
    let crate::JobInput::Media { bytes, .. } = &mut corrupt.inputs[0] else { panic!() };
    *bytes = Arc::from(b"corrupt".as_slice());
    reset();
    assert!(request::prepare(&corrupt, Limits::HARD).is_err());
    assert_eq!(stages()[1], 0);
}
#[test]
fn escaped_prompt_counts_and_model_parameter_edges() {
    let mut j = job(FLUX);
    j.inputs = vec![crate::JobInput::Text { role: "prompt".into(), text: "\n\"\\😀".repeat(10) }];
    let (_, body) = request::prepare(&j, Limits::HARD).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Limits::HARD }).is_err());
    for (k, v) in [("width", json!(513)), ("num_images", json!(5)), ("seed", json!(u64::MAX)), ("num_inference_steps", json!(0))] {
        let mut x = job(FLUX);
        x.params.insert(k.into(), v);
        assert!(request::prepare(&x, Limits::HARD).is_err());
    }
    for n in [1, 4] {
        let mut x = job(FLUX);
        x.params.insert("num_images".into(), json!(n));
        assert!(request::prepare(&x, Limits::HARD).is_ok());
    }
    let mut x = job(KOKORO);
    x.params.insert("voice".into(), json!("am_adam"));
    x.params.insert("speed".into(), json!(0.5));
    assert!(request::prepare(&x, Limits::HARD).is_ok());
    x.params.insert("speed".into(), json!(2.1));
    assert!(request::prepare(&x, Limits::HARD).is_err());
}
#[test]
fn canonical_id_and_route_aliases_never_gain_authority() {
    for id in ["", ".", "..", "%2f", "a/b", "a\\b", "a?x", KEY] {
        let mut h = handle(FLUX);
        h.remote_id = id.into();
        let t = Transport::new(vec![]);
        let f = Fal::new(key(), t.clone());
        assert!(block_on(f.poll(&h)).is_err());
        assert!(block_on(f.cancel(&h)).is_err());
        assert!(t.requests.lock().unwrap().is_empty());
    }
    for url in [
        "https://queue.fal.run:443/fal-ai/flux/requests/r_1/status",
        "https://queue.fal.run/fal-ai/flux/requests/x/../r_1/status",
        "https://queue.fal.run/fal-ai/flux/requests/r_1/status?x=1",
        "https://evil.test/fal-ai/flux/requests/r_1/status",
    ] {
        let mut h = handle(FLUX);
        h.data["status_url"] = json!(url);
        let f = Fal::new(key(), Transport::new(vec![]));
        assert!(block_on(f.cancel(&h)).is_err());
    }
}
#[test]
fn only_flux_allows_coherent_documented_full_root_family() {
    let mut h = handle(FLUX);
    let root = format!("{ORIGIN}/{FLUX}/requests/r_1");
    for result in [root.clone(), format!("{root}/response")] {
        h.data["status_url"] = json!(format!("{root}/status"));
        h.data["response_url"] = json!(result);
        h.data["cancel_url"] = json!(format!("{root}/cancel"));
        assert!(Record::read(&h, &key()).is_ok());
    }
    h.data["cancel_url"] = handle(FLUX).data["cancel_url"].clone();
    assert!(Record::read(&h, &key()).is_err());
    for m in [WAN, KOKORO] {
        let mut h = handle(m);
        for (k, suffix) in [("status_url", "/status"), ("response_url", ""), ("cancel_url", "/cancel")] {
            h.data[k] = json!(format!("{ORIGIN}/{m}/requests/r_1{suffix}"));
        }
        assert!(Record::read(&h, &key()).is_err());
    }
}
#[test]
fn escaped_receipt_keys_cleanup_once_and_no_ambiguous_authority() {
    let good = serde_json::to_string(&receipt(FLUX)).unwrap();
    let escaped = good.replace("request_id", r"request_\u0069d").replace("status_url", r"status_\u0075rl");
    let t = Transport::new(vec![bytes(escaped.into_bytes())]);
    assert!(block_on(Fal::new(key(), t).submit(&job(FLUX))).is_ok());
    let mut bad = receipt(FLUX);
    bad["status"] = json!([]);
    let t = Transport::new(vec![reply(bad), reply(json!({}))]);
    let e = block_on(Fal::new(key(), t.clone()).submit(&job(FLUX))).unwrap_err();
    assert!(e.message.contains("billing"));
    let req = t.requests.lock().unwrap();
    assert_eq!(req.len(), 2);
    assert_eq!(req[1].method, HttpMethod::Put);
    drop(req);
    for s in [
        format!("{},\"request_id\":\"r_1\"}}", good.trim_end_matches('}')),
        format!("{good}x"),
        good.replace("r_1", KEY),
        good.replace("request_id", "nested_id"),
    ] {
        let t = Transport::new(vec![bytes(s.into_bytes())]);
        let f = Fal::new(key(), t.clone());
        let e = block_on(f.submit(&job(FLUX))).unwrap_err();
        assert!(!e.to_string().contains(KEY));
        assert_eq!(t.requests.lock().unwrap().len(), 1);
    }
}
#[test]
fn map_only_all_required_record_boundaries_and_bounded_json() {
    for s in [b"[]".as_slice(), br#"{"a":0,"\u0061":1}"#, br#"{"a":"\uD800"}"#, br#"{"a":1} trailing"#] {
        assert!(json::parse(s, Limits::HARD).is_err(), "{}", String::from_utf8_lossy(s));
    }
    for (s, l) in [
        (format!("{{\"x\":\"{}\"}}", "x".repeat(64)), Limits { metadata_bytes: 63, ..Limits::HARD }),
        (format!("{}0{}", "[".repeat(33), "]".repeat(33)), Limits::HARD),
        (format!("[{}]", vec!["0"; 4097].join(",")), Limits::HARD),
    ] {
        assert!(json::parse(s.as_bytes(), l).is_err());
    }
    for (o, _) in [(json!({"images":[[]]}), 0), (json!({"video":[]}), 1), (json!({"audio":null}), 2), (json!({"model_glb":[]}), 3), (json!({"image":[]}), 4)] {
        let model = if o.get("images").is_some() {
            FLUX
        } else if o.get("video").is_some() {
            WAN
        } else if o.get("audio").is_some() {
            KOKORO
        } else if o.get("model_glb").is_some() {
            TRELLIS
        } else {
            ESRGAN
        };
        let (rec, _) = Record::read(&handle(model), &key()).unwrap();
        assert!(response::outputs(&serde_json::to_vec(&o).unwrap(), rec, Limits::HARD, &key()).is_err());
    }
}
#[test]
fn four_outputs_preserved_and_late_corruption_returns_no_partial() {
    for corrupt in [false, true] {
        let mut j = job(FLUX);
        j.params.insert("num_images".into(), json!(4));
        let files = vec![json!({"url":"https://fal.media/f","width":2,"height":2}); 4];
        let mut replies = vec![reply(receipt(FLUX)), reply(json!({"status":"COMPLETED"})), reply(json!({"images":files}))];
        for i in 0..4 {
            replies.push(bytes(if corrupt && i == 3 { b"bad".to_vec() } else { png(2, 2) }));
        }
        let t = Transport::new(replies);
        let f = Fal::new(key(), t.clone());
        let Submission::Queued(h) = block_on(f.submit(&j)).unwrap() else { panic!() };
        let r = block_on(f.poll(&h));
        if corrupt {
            assert!(r.is_err());
        } else {
            let JobProgress::Done(d) = r.unwrap() else { panic!() };
            assert_eq!(d.outputs.len(), 4);
        }
        assert_eq!(t.requests.lock().unwrap().len(), 7);
    }
}
struct Clock(std::sync::atomic::AtomicU64);
impl crate::Clock for Clock {
    fn now_ms(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn sleep(&self, _: u64) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}
struct Never;
impl crate::CancelSignal for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(futures::future::pending())
    }
}
#[test]
fn shared_task_retains_accepted_handle_on_every_local_failure() {
    for stage in 0..5 {
        let mut replies = vec![reply(receipt(FLUX))];
        match stage {
            0 => replies.push(bytes(b"bad json".to_vec())),
            1 => {
                replies.push(reply(json!({"status":"COMPLETED"})));
                replies.push(bytes(b"bad result".to_vec()));
            }
            _ => {
                let (o, b, _) = output(FLUX);
                replies.push(reply(json!({"status":"COMPLETED"})));
                replies.push(reply(if stage == 2 { json!({"images":[{"url":"http://127.0.0.1/f","width":2,"height":2}]}) } else { o }));
                if stage != 2 {
                    replies.push(if stage == 3 { bytes(b"corrupt".to_vec()) } else { HttpResponse { status: 403, headers: vec![], body: b } });
                }
            }
        };
        let t = Transport::new(replies);
        let f = Fal::new(key(), t);
        let failure = block_on(crate::run_to_completion(&f, &job(FLUX), &Never, &Clock(Default::default()), crate::RunLimits::default(), |_| {})).unwrap_err();
        assert_eq!(failure.handle, Some(handle(FLUX)));
        assert!(!failure.finished);
        assert!(!failure.uncertain);
    }
}
#[test]
fn reported_terminal_failure_and_best_effort_cancel_are_distinct() {
    let t = Transport::new(vec![reply(json!({"status":"COMPLETED","error":"private provider text","error_type":"runtime_error"}))]);
    let f = Fal::new(key(), t);
    let JobProgress::Failed(e) = block_on(f.poll(&handle(WAN))).unwrap() else { panic!() };
    assert_eq!(e.kind, ErrorKind::JobFailed);
    assert!(!e.to_string().contains("private"));
    for (code, disposition) in [(202, "CANCELLATION_REQUESTED"), (400, "ALREADY_COMPLETED"), (404, "NOT_FOUND")] {
        let t = Transport::new(vec![HttpResponse { status: code, headers: vec![], body: serde_json::to_vec(&json!({"status":disposition})).unwrap() }]);
        let f = Fal::new(key(), t.clone());
        assert!(block_on(f.cancel(&handle(WAN))).is_ok());
        let req = t.requests.lock().unwrap();
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].url, format!("{ORIGIN}/fal-ai/wan/requests/r_1/cancel"));
        assert_eq!(req[0].method, HttpMethod::Put);
    }
}
#[test]
fn descriptor_admission_precedes_all_downloads_and_remaining_cap_shrinks() {
    for url in [
        "https://evilfal.media/f",
        "https://fal.media:443/f",
        "https://fal.media/f#x",
        "https://fal.media/f?key=fal-fixture-key-not-real",
        "https://user@fal.media/f",
        "http://fal.media/f",
    ] {
        let t = Transport::new(vec![
            reply(json!({"status":"COMPLETED"})),
            reply(json!({"images":[{"url":"https://fal.media/valid","width":2,"height":2},{"url":url,"width":2,"height":2}]})),
        ]);
        let f = Fal::new(key(), t.clone());
        let mut h = handle(FLUX);
        h.data["requested"] = json!(2);
        assert!(block_on(f.poll(&h)).is_err());
        assert_eq!(t.requests.lock().unwrap().len(), 2);
    }
    let b = png(2, 2);
    let cap = b.len() * 2;
    let t = Transport::new(vec![
        reply(json!({"status":"COMPLETED"})),
        reply(json!({"images":[{"url":"https://fal.media/f","width":2,"height":2},{"url":"https://fal.media/f","width":2,"height":2}]})),
        bytes(b.clone()),
        bytes(b.clone()),
    ]);
    let f = Fal::with_limits(key(), t.clone(), Limits { total_output_bytes: cap, ..Limits::HARD }).unwrap();
    let mut h = handle(FLUX);
    h.data["requested"] = json!(2);
    assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Done(_)));
    let req = t.requests.lock().unwrap();
    assert_eq!(req[2].max_response_bytes, cap);
    assert_eq!(req[3].max_response_bytes, b.len());
}
#[test]
fn actual_container_fixtures_and_truncated_extent_refusal() {
    for m in [WAN, KOKORO, TRELLIS] {
        let (_, b, _) = output(m);
        let (rec, _) = Record::read(&handle(m), &key()).unwrap();
        assert!(media::validate(&b, rec, Limits::HARD, &mut 0).is_ok());
        for data in [&b[..b.len() - 1], b"wrong bytes".as_slice()] {
            assert!(media::validate(data, rec, Limits::HARD, &mut 0).is_err());
        }
    }
}
#[test]
fn image_16_bit_and_combined_allocation_preflight() {
    use image::ImageEncoder;
    let mut b = vec![];
    let pixels = vec![0u8; 2 * 2 * 8];
    image::codecs::png::PngEncoder::new(&mut b).write_image(&pixels, 2, 2, image::ExtendedColorType::Rgba16).unwrap();
    let (rec, _) = Record::read(&handle(FLUX), &key()).unwrap();
    for (cap, ok) in [(32, true), (31, false)] {
        reset();
        let l = Limits { codec_bytes: cap, ..Limits::HARD };
        assert_eq!(media::validate(&b, rec, l, &mut 0).is_ok(), ok);
        assert_eq!(stages()[2], usize::from(ok));
    }
}

#[test]
fn oriented_input_preserves_original_encoding_and_charges_both_rasters() {
    let b = include_bytes!("fixtures/oriented.jpg");
    let meta = nodecraft_media::probe_image(b, &Limits::HARD.image(true)).unwrap();
    assert_eq!((meta.width, meta.height), (3, 2));
    let mut j = job(ESRGAN);
    j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(b.as_slice()) }];
    for (cap, ok) in [(36, true), (35, false)] {
        reset();
        let result = request::prepare(&j, Limits { codec_bytes: cap, ..Limits::HARD });
        assert_eq!(result.is_ok(), ok);
        if let Ok((r, encoded)) = result {
            assert_eq!((r.width, r.height), (6, 4));
            let body: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
            use base64::Engine;
            let uri = body["image_url"].as_str().unwrap().strip_prefix("data:image/jpeg;base64,").unwrap();
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(uri).unwrap(), b.as_slice());
        }
        assert_eq!(stages()[1], usize::from(ok));
    }
}

#[test]
fn returned_length_declared_aggregate_and_png_tail_refuse_safely() {
    let mut oversized = Vec::with_capacity(Limits::HARD.response_bytes + 1);
    oversized.extend_from_slice(b"{}");
    assert!(status(&bytes(oversized), Limits::HARD.response_bytes).is_ok());
    let (rec, _) = Record::read(&handle(FLUX), &key()).unwrap();
    let mut r = rec;
    r.requested = 4;
    let huge = json!({"images":vec![json!({"url":"https://fal.media/f","width":4096,"height":4096});4]});
    assert!(response::outputs(&serde_json::to_vec(&huge).unwrap(), r, Limits::HARD, &key()).is_err());
    let mut b = png(2, 2);
    b.extend_from_slice(b"trailing");
    assert!(media::validate(&b, rec, Limits::HARD, &mut 0).is_err());
    let mut j = job(FLUX);
    j.inputs.push(crate::JobInput::Text { role: "unexpected".into(), text: "secret".into() });
    reset();
    assert!(request::prepare(&j, Limits::HARD).is_err());
    assert_eq!(stages(), [0; 3]);
}

#[test]
fn actual_length_capacity_and_container_conflicts_are_not_partial_success() {
    let good = png(2, 2);
    let cap = good.len();
    for headers in
        [vec![("content-length".into(), (cap + 1).to_string())], vec![("content-length".into(), cap.to_string()), ("Content-Length".into(), cap.to_string())]]
    {
        let t = Transport::new(vec![
            reply(json!({"status":"COMPLETED"})),
            reply(json!({"images":[{"url":"https://fal.media/f","width":2,"height":2}]})),
            HttpResponse { status: 200, headers, body: good.clone() },
        ]);
        assert!(block_on(Fal::new(key(), t).poll(&handle(FLUX))).is_err());
    }
    let (rec, _) = Record::read(&handle(TRELLIS), &key()).unwrap();
    let mut raw = include_bytes!("fixtures/triangle.glb").to_vec();
    raw[4] = 1;
    assert!(media::validate(&raw, rec, Limits::HARD, &mut 0).is_err());
    let (rec, _) = Record::read(&handle(WAN), &key()).unwrap();
    let fake = b"\0\0\0\x18ftypisom\0\0\0\0iso2avc1\0\0\0\x10moovbad data\0\0\0\x09mdatx";
    assert!(media::validate(fake, rec, Limits::HARD, &mut 0).is_err());
}

#[test]
fn api_and_download_accept_spare_capacity_but_never_excess_length() {
    let spare = |mut r: HttpResponse, cap: usize| {
        let mut body = Vec::with_capacity(cap + 1);
        body.extend_from_slice(&r.body);
        r.body = body;
        assert!(r.body.capacity() > cap && r.body.len() <= cap);
        r
    };
    let (out, body, _) = output(FLUX);
    let original = body.clone();
    let t = Transport::new(vec![
        spare(reply(receipt(FLUX)), Limits::HARD.response_bytes),
        spare(reply(json!({"status":"COMPLETED"})), Limits::HARD.response_bytes),
        spare(reply(out), Limits::HARD.response_bytes),
        spare(bytes(body), Limits::HARD.output_bytes),
    ]);
    let f = Fal::new(key(), t.clone());
    let Submission::Queued(h) = block_on(f.submit(&job(FLUX))).unwrap() else { panic!() };
    let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: original.into() }]);
    assert_eq!(t.requests.lock().unwrap().len(), 4);
    assert!(status(&bytes(vec![0; 33]), 32).is_err());
    let (out, body, _) = output(FLUX);
    let cap = body.len() - 1;
    let t = Transport::new(vec![reply(json!({"status":"COMPLETED"})), reply(out), bytes(body)]);
    let f = Fal::with_limits(key(), t, Limits { output_bytes: cap, ..Limits::HARD }).unwrap();
    assert!(block_on(f.poll(&handle(FLUX))).is_err());
}

#[test]
fn identity_only_receipts_cleanup_local_routes_and_report_actual_outcome() {
    for (cancel_status, cancel_body, outcome) in [
        (202, json!({"status":"CANCELLATION_REQUESTED"}), "cancellation_requested"),
        (400, json!({"status":"ALREADY_COMPLETED"}), "already_completed"),
        (404, json!({"status":"NOT_FOUND"}), "not_found"),
        (401, json!({"secret":KEY}), "failed_auth"),
        (503, json!({"secret":KEY}), "failed_busy"),
        (202, json!({"status":"NOT_FOUND"}), "failed_response_validation"),
    ] {
        for refusal in 0..3 {
            let mut r = receipt(FLUX);
            match refusal {
                0 => r["status_url"] = json!("https://untrusted.test/never"),
                1 => {
                    r.as_object_mut().unwrap().remove("cancel_url");
                }
                _ => r["status"] = json!([]),
            }
            let t = Transport::new(vec![reply(r), HttpResponse { status: cancel_status, headers: vec![], body: serde_json::to_vec(&cancel_body).unwrap() }]);
            let f = Fal::new(key(), t.clone());
            let error = block_on(f.submit(&job(FLUX))).unwrap_err();
            assert!(error.message.contains("accepted request r_1"));
            assert!(error.message.contains(outcome), "{}", error.message);
            assert!(error.message.contains("billing remains uncertain"));
            assert!(!error.message.contains(KEY));
            assert!(!error.message.contains("untrusted"));
            let requests = t.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].method, HttpMethod::Post);
            assert_eq!(requests[1].method, HttpMethod::Put);
            assert_eq!(requests[1].url, format!("{ORIGIN}/fal-ai/flux/requests/r_1/cancel"));
            assert!(!f.retry_ambiguous_submit());
        }
    }
    // Complete, escaped identity alone can authorize only canonical local cleanup.
    let t = Transport::new(vec![
        bytes(br#"{"request_\u0069d":"r_\u0031"}"#.to_vec()),
        HttpResponse { status: 202, headers: vec![], body: br#"{"status":"CANCELLATION_REQUESTED"}"#.to_vec() },
    ]);
    assert!(block_on(Fal::new(key(), t.clone()).submit(&job(FLUX))).unwrap_err().message.contains("accepted request r_1"));
    assert_eq!(t.requests.lock().unwrap()[1].url, format!("{ORIGIN}/fal-ai/flux/requests/r_1/cancel"));
}

#[test]
fn cancellation_refuses_malformed_or_contradictory_status_and_preserves_http_errors() {
    for (code, body) in [
        (202, json!({})),
        (202, json!({"status":"NOT_FOUND"})),
        (400, json!({"status":"CANCELLATION_REQUESTED"})),
        (404, json!({"status":"ALREADY_COMPLETED"})),
        (202, json!({"status":"CANCELLATION_REQUESTED","error":"private"})),
        (200, json!({"status":"CANCELLATION_REQUESTED"})),
    ] {
        let t = Transport::new(vec![HttpResponse { status: code, headers: vec![], body: serde_json::to_vec(&body).unwrap() }]);
        assert_eq!(block_on(Fal::new(key(), t).cancel(&handle(FLUX))).unwrap_err().kind, ErrorKind::Malformed);
    }
    for (code, kind) in [(401, ErrorKind::Auth), (403, ErrorKind::Auth), (500, ErrorKind::Server)] {
        let t = Transport::new(vec![HttpResponse { status: code, headers: vec![], body: KEY.as_bytes().to_vec() }]);
        let e = block_on(Fal::new(key(), t).cancel(&handle(FLUX))).unwrap_err();
        assert_eq!(e.kind, kind);
        assert!(!e.message.contains(KEY));
    }
    for body in [br#"{"status":"CANCELLATION_REQUESTED","status":"CANCELLATION_REQUESTED"}"#.as_slice(), b"{", br#"{"status":"CANCELLATION_REQUESTED"}tail"#] {
        let t = Transport::new(vec![HttpResponse { status: 202, headers: vec![], body: body.to_vec() }]);
        assert!(block_on(Fal::new(key(), t).cancel(&handle(FLUX))).is_err());
    }
}

#[test]
fn retry_after_is_bounded_checked_and_optional() {
    for (value, expected) in [
        ("12", Some(12_000)),
        ("3600", Some(3_600_000)),
        ("3601", None),
        ("-1", None),
        ("bogus", None),
        ("9999999999999999", None),
        ("12345678901234567", None),
    ] {
        let r = HttpResponse { status: 429, headers: vec![("Retry-After".into(), value.into())], body: vec![] };
        let e = status(&r, Limits::HARD.response_bytes).unwrap_err();
        assert_eq!(e.kind, ErrorKind::RateLimited);
        assert_eq!(e.retry_after_ms, expected);
    }
}
/// #437: the billed units come from the authenticated result fetch only, and a bad header never
/// costs the result.
#[test]
fn billable_units_come_from_the_result_fetch_and_never_cost_the_result() {
    let with = |headers: Vec<(&str, &str)>, response: HttpResponse| HttpResponse {
        headers: headers.into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect(),
        ..response
    };
    // `result` headers go on the result fetch, `cdn` on the media download.
    let run = |result: Vec<(&str, &str)>, cdn: Vec<(&str, &str)>| {
        let (o, b, _) = output(FLUX);
        let t = Transport::new(vec![
            with(vec![("x-fal-billable-units", "9")], reply(receipt(FLUX))),
            with(vec![("x-fal-billable-units", "9")], reply(json!({"status":"COMPLETED"}))),
            with(result, reply(o)),
            with(cdn, bytes(b)),
        ]);
        let f = Fal::new(key(), t);
        let Submission::Queued(h) = block_on(f.submit(&job(FLUX))).unwrap() else { panic!() };
        let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!("the result is kept") };
        assert_eq!(done.outputs.len(), 1);
        assert_eq!(done.usage.cost, None, "units are never money");
        done.usage.billable_units.map(|u| u.to_string())
    };
    assert_eq!(run(vec![("x-fal-billable-units", "1.5")], vec![]), Some("1.5".into()));
    assert_eq!(run(vec![("X-Fal-Billable-Units", " 2 ")], vec![]), Some("2".into()));
    assert_eq!(run(vec![("x-fal-billable-units", "0")], vec![]), Some("0".into()));
    // Absent from the result fetch: the receipt's, the status's and the CDN's don't count.
    assert_eq!(run(vec![], vec![("x-fal-billable-units", "4")]), None);
    for hostile in ["", "-1", "1e3", "NaN", "0.0000001", "1,5", "10000000000", "1.5 units", &"1".repeat(64)] {
        assert_eq!(run(vec![("x-fal-billable-units", hostile)], vec![]), None, "{hostile:?}");
    }
    let duplicate = vec![("x-fal-billable-units", "1"), ("x-fal-billable-units", "2")];
    assert_eq!(run(duplicate, vec![]), None, "two values: neither is trusted");
}

#[test]
fn wan_frames_and_speech_inputs_reach_their_own_fields() {
    use base64::Engine;
    let decode =
        |v: &serde_json::Value, prefix: &str| base64::engine::general_purpose::STANDARD.decode(v.as_str().unwrap().strip_prefix(prefix).unwrap()).unwrap();
    let mut j = job(WAN_I2V);
    j.inputs.push(crate::JobInput::Text { role: "negative_prompt".into(), text: "blur".into() });
    j.inputs.push(crate::JobInput::Media { role: "last_frame".into(), mime: "image/png".into(), bytes: Arc::from(png(3, 3)) });
    j.params.insert("num_frames".into(), json!(161));
    j.params.insert("resolution".into(), json!("720p"));
    j.params.insert("aspect_ratio".into(), json!("9:16"));
    j.params.insert("seed".into(), json!(7));
    let (_, body) = request::prepare(&j, Limits::HARD).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(decode(&body["image_url"], "data:image/png;base64,"), png(2, 2));
    assert_eq!(decode(&body["end_image_url"], "data:image/png;base64,"), png(3, 3));
    assert_eq!(
        body,
        json!({"prompt":"A blue circle","negative_prompt":"blur","image_url":body["image_url"],"end_image_url":body["end_image_url"],"seed":7,"num_frames":161,"frames_per_second":16,"resolution":"720p","aspect_ratio":"9:16","interpolator_model":"none","num_interpolated_frames":0,"adjust_fps_for_interpolation":false})
    );

    let mut j = job(WAN_S2V);
    j.params.insert("num_frames".into(), json!(120));
    j.params.insert("resolution".into(), json!("580p"));
    let (_, body) = request::prepare(&j, Limits::HARD).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(decode(&body["audio_url"], "data:audio/wav;base64,"), include_bytes!("fixtures/tone.wav"));
    assert_eq!((body["num_frames"].clone(), body["resolution"].clone()), (json!(120), json!("580p")));
    assert!(body.get("aspect_ratio").is_none() && body.get("interpolator_model").is_none());
}
#[test]
fn wan_frames_and_speech_refuse_before_any_encoding() {
    let speech = |mime: &str, b: &[u8]| crate::JobInput::Media { role: "speech".into(), mime: mime.into(), bytes: Arc::from(b) };
    type Edit = Box<dyn Fn(&mut ProviderJob)>;
    let cases: Vec<(&str, Edit)> = vec![
        // The required media is missing.
        (WAN_I2V, Box::new(|j| j.inputs.truncate(1))),
        (WAN_S2V, Box::new(|j| j.inputs.truncate(2))),
        (WAN_S2V, Box::new(|j| drop(j.inputs.remove(1)))),
        // A role another model owns, or the same role twice.
        (WAN_I2V, Box::new(move |j| j.inputs.push(speech("audio/wav", include_bytes!("fixtures/tone.wav"))))),
        (WAN_S2V, Box::new(|j| j.inputs.push(crate::JobInput::Media { role: "last_frame".into(), mime: "image/png".into(), bytes: Arc::from(png(2, 2)) }))),
        (WAN, Box::new(|j| j.inputs.push(crate::JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: Arc::from(png(2, 2)) }))),
        (WAN_I2V, Box::new(|j| j.inputs.push(j.inputs[1].clone()))),
        // Speech that isn't audio, or whose bytes disagree with its declared type.
        (WAN_S2V, Box::new(move |j| j.inputs[2] = speech("audio/wav", &png(2, 2)))),
        (WAN_S2V, Box::new(move |j| j.inputs[2] = speech("audio/mpeg", include_bytes!("fixtures/tone.wav")))),
        (WAN_S2V, Box::new(move |j| j.inputs[2] = speech("audio/wav", b"RIFF"))),
        // Settings outside each model's documented range.
        (WAN_I2V, Box::new(|j| drop(j.params.insert("num_frames".into(), json!(162))))),
        (WAN_I2V, Box::new(|j| drop(j.params.insert("resolution".into(), json!("1080p"))))),
        (WAN_S2V, Box::new(|j| drop(j.params.insert("num_frames".into(), json!(42))))),
        (WAN_S2V, Box::new(|j| drop(j.params.insert("num_frames".into(), json!(124))))),
        (WAN_S2V, Box::new(|j| drop(j.params.insert("aspect_ratio".into(), json!("16:9"))))),
    ];
    for (n, (model, edit)) in cases.iter().enumerate() {
        let mut j = job(model);
        edit(&mut j);
        reset();
        assert!(request::prepare(&j, Limits::HARD).is_err(), "case {n}");
        assert_eq!(stages(), [0, 0, 0], "case {n} reached a codec or encoding");
    }
}
#[test]
fn two_full_size_inputs_fit_the_request_and_the_extent_is_exact() {
    // Noise doesn't compress, so the PNG is close to the 1 MiB input cap.
    let mut seed = 1u32;
    let noise: Vec<u8> = (0..512 * 500 * 4)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        })
        .collect();
    let frame = nodecraft_media::encode_png(512, 500, &noise, &nodecraft_media::Limits::default()).unwrap();
    assert!(frame.len() > 900_000 && frame.len() <= Limits::HARD.input_bytes, "{}", frame.len());
    let mut j = job(WAN_I2V);
    j.inputs[1] = crate::JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: Arc::from(frame.clone()) };
    j.inputs.push(crate::JobInput::Media { role: "last_frame".into(), mime: "image/png".into(), bytes: Arc::from(frame) });
    let (_, body) = request::prepare(&j, Limits::HARD).unwrap();
    assert!(body.len() > 2 * 1024 * 1024);
    assert!(request::prepare(&j, Limits { request_bytes: body.len(), ..Limits::HARD }).is_ok());
    reset();
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Limits::HARD }).is_err());
    assert_eq!(stages(), [0, 0, 0]);
}
#[test]
fn a_declared_clip_type_that_isnt_mp4_or_generic_is_refused() {
    let r = Record { model: Model::Frames, requested: 1, width: 0, height: 0 };
    for (mime, ok) in [("video/mp4", true), ("application/octet-stream", true), ("video/webm", false), ("image/png", false)] {
        let o = json!({"video":{"url":"https://v3.fal.media/files/fixture","content_type":mime,"file_size":null}});
        assert_eq!(response::outputs(&serde_json::to_vec(&o).unwrap(), r, Limits::HARD, &key()).is_ok(), ok, "{mime}");
    }
    // A generic type on a non-video output still has to match.
    let r = Record { model: Model::Audio, requested: 1, width: 0, height: 0 };
    let o = json!({"audio":{"url":"https://v3.fal.media/files/fixture","content_type":"application/octet-stream","file_size":null}});
    assert!(response::outputs(&serde_json::to_vec(&o).unwrap(), r, Limits::HARD, &key()).is_err());
}
