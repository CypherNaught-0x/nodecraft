//! Public Hunyuan3D V3 schemas through the real fal queue; authored offline fixtures only.
use super::tests::{handle, job, png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};
const KEY: &str = "hunyuan42-fixture-not-a-key";
const MODELS: [&str; 3] = [HUNYUAN_TEXT, HUNYUAN_IMAGE, HUNYUAN_SKETCH];
const TRIANGLE: &[u8] = include_bytes!("fixtures/triangle.glb");
#[derive(Default)]
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Wire {
    fn new(replies: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies.into_iter().map(Ok).collect()), ..Default::default() })
    }
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects refused")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn reply(value: Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() }
}
fn fal(wire: &Arc<Wire>) -> Fal {
    Fal::new(Secret::new(KEY).unwrap(), wire.clone())
}
fn image(role: &str) -> crate::JobInput {
    crate::JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: png(2, 2).into() }
}
fn hunyuan(model: &str) -> ProviderJob {
    let mut j = job(model);
    if model == HUNYUAN_IMAGE {
        j.inputs = vec![image("image")];
    }
    if model == HUNYUAN_SKETCH {
        j.inputs.push(crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png(128, 128).into() });
    }
    j
}

#[test]
fn hunyuan42_three_modes_send_only_the_fields_in_their_schema() {
    for model in MODELS {
        let wire = Wire::new(vec![reply(receipt(model))]);
        let f = fal(&wire);
        let Submission::Queued(h) = block_on(f.submit(&hunyuan(model))).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        let requests = wire.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.url, format!("{ORIGIN}/{model}"));
        assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Key {KEY}")));
        assert!(request.headers.iter().any(|(k, v)| k == "X-Fal-No-Retry" && v == "1"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["enable_safety_checker"], true);
        assert_eq!(body["enable_pbr"], false);
        assert_eq!(body.get("prompt").is_some(), model != HUNYUAN_IMAGE);
        assert_eq!(body.get("input_image_url").is_some(), model != HUNYUAN_TEXT);
        if model == HUNYUAN_SKETCH {
            assert!(body.get("generate_type").is_none());
        } else {
            assert_eq!(body["generate_type"], "Normal");
        }
        for absent in ["face_count", "polygon_type", "seed", "back_image_url", "left_image_url", "right_image_url"] {
            assert!(body.get(absent).is_none(), "{absent}");
        }
        assert!(!f.retry_ambiguous_submit());
    }
}

#[test]
fn hunyuan42_every_priced_preset_encodes_its_type_material_and_polygon_contract() {
    for model in MODELS {
        for (preset, kind, polygon, pbr) in [
            ("textured", "Normal", None, false),
            ("pbr", "Normal", None, true),
            ("low-poly-triangles", "LowPoly", Some("triangle"), false),
            ("low-poly-quads", "LowPoly", Some("quadrilateral"), false),
            ("low-poly-triangles-pbr", "LowPoly", Some("triangle"), true),
            ("low-poly-quads-pbr", "LowPoly", Some("quadrilateral"), true),
            ("geometry", "Geometry", None, false),
        ] {
            let mut j = hunyuan(model);
            j.params.insert("mesh_preset".into(), json!(preset));
            let prepared = request::prepare(&j, Limits::default());
            if model == HUNYUAN_SKETCH && !matches!(preset, "textured" | "pbr") {
                assert!(prepared.is_err());
                continue;
            }
            let (record, bytes) = prepared.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(record.requested, 1);
            assert_eq!(body["enable_pbr"], pbr);
            assert_eq!(body.get("generate_type").and_then(Value::as_str), if model == HUNYUAN_SKETCH { None } else { Some(kind) });
            assert_eq!(body.get("polygon_type").and_then(Value::as_str), polygon);
            assert!(body.get("face_count").is_none());
        }
    }
}

#[test]
fn hunyuan42_bad_roles_paid_extras_and_image_payloads_refuse_before_delivery() {
    let mut jobs = vec![job(HUNYUAN_IMAGE), job(HUNYUAN_SKETCH)];
    for model in MODELS {
        for (key, value) in [
            ("mesh_preset", json!("Normal")),
            ("mesh_preset", json!(false)),
            ("face_count", json!(40000)),
            ("enable_pbr", json!(true)),
            ("generate_type", json!("LowPoly")),
            ("enable_safety_checker", json!(false)),
            ("seed", json!(0)),
        ] {
            let mut j = hunyuan(model);
            j.params.insert(key.into(), value);
            jobs.push(j);
        }
        let mut j = hunyuan(model);
        j.inputs.push(j.inputs[0].clone());
        jobs.push(j);
        let mut j = hunyuan(model);
        j.inputs.push(image("back"));
        jobs.push(j);
    }
    for model in [HUNYUAN_TEXT, HUNYUAN_SKETCH] {
        for text in [" ".into(), "a".repeat(1025)] {
            let mut j = hunyuan(model);
            j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text };
            jobs.push(j);
        }
    }
    for (mime, bytes) in [("image/png", b"bad".to_vec()), ("image/jpeg", png(2, 2)), ("image/png", png(1025, 1))] {
        let mut j = hunyuan(HUNYUAN_IMAGE);
        j.inputs[0] = crate::JobInput::Media { role: "image".into(), mime: mime.into(), bytes: bytes.into() };
        jobs.push(j);
    }
    let wire = Arc::new(Wire::default());
    for j in jobs {
        let context = crate::SubmitContext::new();
        assert!(block_on(fal(&wire).submit_with_delivery(&j, &context)).is_err());
        assert!(context.finish_error().is_some(), "a local refusal never creates a paid request");
    }
    assert!(wire.requests.lock().unwrap().is_empty());
    let mut j = hunyuan(HUNYUAN_SKETCH);
    j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "\"\\\n".repeat(100) };
    let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: bytes.len(), ..Limits::default() }).is_ok());
    assert!(request::prepare(&j, Limits { request_bytes: bytes.len() - 1, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { input_bytes: 1, ..Limits::default() }).is_err());
}

#[test]
fn hunyuan42_saved_tasks_resume_cancel_and_download_only_the_glb_without_credentials() {
    for model in MODELS {
        let wire = Wire::new(vec![
            reply(receipt(model)),
            reply(json!({"status":"IN_QUEUE","queue_position":1})),
            reply(json!({"status":"IN_PROGRESS"})),
            reply(json!({"status":"COMPLETED"})),
            reply(
                json!({"model_glb":{"url":"https://v3.fal.media/hunyuan.glb","content_type":"model/gltf-binary","file_size":TRIANGLE.len()},"thumbnail":{"url":"https://unused.example/thumb"},"model_urls":{"obj":{"url":"https://unused.example/object"}}}),
            ),
            HttpResponse { status: 200, headers: vec![], body: TRIANGLE.to_vec() },
            HttpResponse { status: 202, headers: vec![], body: serde_json::to_vec(&json!({"status":"CANCELLATION_REQUESTED"})).unwrap() },
        ]);
        let f = fal(&wire);
        let Submission::Queued(h) = block_on(f.submit(&hunyuan(model))).unwrap() else { panic!() };
        let h: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Queued { position: Some(1) }));
        assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Running { .. }));
        let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
        assert_eq!(done.outputs, vec![Output::Bytes { mime: "model/gltf-binary".into(), bytes: TRIANGLE.into() }]);
        block_on(f.cancel(&h)).unwrap();
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 7);
        assert!(requests[5].headers.is_empty());
        assert_eq!(requests[6].method, HttpMethod::Put);
        assert_eq!(requests[6].url, format!("{ORIGIN}/fal-ai/hunyuan3d-v3/requests/r_1/cancel"));
        assert!(done.usage.cost.is_none(), "billable units are not a USD invoice");
    }
}

fn large_glb() -> Vec<u8> {
    let json_len = usize::try_from(u32::from_le_bytes(TRIANGLE[12..16].try_into().unwrap())).unwrap();
    let mut value: Value = serde_json::from_slice(&TRIANGLE[20..20 + json_len]).unwrap();
    let bin_start = 20 + json_len + 8;
    let mut bin = TRIANGLE[bin_start..].to_vec();
    bin.resize(17 * 1024 * 1024, 0);
    value["buffers"][0]["byteLength"] = json!(bin.len());
    let mut text = serde_json::to_vec(&value).unwrap();
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let len = 20 + text.len() + 8 + bin.len();
    let mut glb = b"glTF".to_vec();
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
    glb.extend_from_slice(&u32::try_from(text.len()).unwrap().to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&text);
    glb.extend_from_slice(&u32::try_from(bin.len()).unwrap().to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin);
    glb
}

#[test]
fn hunyuan42_large_mesh_quota_is_resumable_and_does_not_widen_other_model_limits() {
    let glb = large_glb();
    assert!(glb.len() > Limits::HARD.output_bytes);
    let mesh = nodecraft_media::decode_mesh(&glb, &Default::default()).unwrap();
    assert_eq!(mesh.positions.len(), 3);
    assert_eq!(mesh.indices.len(), 3);
    let wire = Wire::new(vec![
        reply(json!({"status":"COMPLETED"})),
        reply(json!({"model_glb":{"url":"https://fal.media/large.glb","file_size":glb.len()}})),
        HttpResponse { status: 200, headers: vec![], body: glb },
    ]);
    assert!(matches!(block_on(fal(&wire).poll(&handle(HUNYUAN_IMAGE))).unwrap(), JobProgress::Done(_)));
    for model in [Model::HunyuanText, Model::HunyuanImage, Model::HunyuanSketch] {
        let l = Limits::default().for_model(model);
        assert_eq!(l.output_bytes, 256 * 1024 * 1024);
        assert_eq!(l.total_output_bytes, l.output_bytes);
        assert_eq!(l.request_bytes, Limits::HARD.request_bytes);
    }
    assert_eq!(Limits::default().for_model(Model::Mesh).output_bytes, Limits::HARD.output_bytes);
    let record = Record::read(&handle(HUNYUAN_IMAGE), &Secret::new(KEY).unwrap()).unwrap().0;
    for value in [
        json!({"model_glb":{"url":"https://evil.example/mesh"}}),
        json!({"model_glb":{"url":"https://fal.media/mesh","file_size":268435457u64}}),
        json!({"model_glb":{"url":"https://fal.media/mesh"},"images":[]}),
    ] {
        assert!(
            response::outputs(&serde_json::to_vec(&value).unwrap(), record, Limits::default().for_model(record.model), &Secret::new(KEY).unwrap()).is_err()
        );
    }
    assert!(media::validate(b"bad glb", record, Limits::default(), &mut 0).is_err());
    let mut h = handle(HUNYUAN_IMAGE);
    h.data["status_url"] = json!("https://queue.fal.run/fal-ai/flux/requests/r_1/status");
    assert!(Record::read(&h, &Secret::new(KEY).unwrap()).is_err());
}

#[test]
fn hunyuan42_remote_failures_are_sanitized_and_do_not_prove_a_paid_job_was_refused() {
    for (status, kind) in [
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (403, ErrorKind::Auth),
        (404, ErrorKind::NotFound),
        (422, ErrorKind::UnsupportedInput),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Server),
        (503, ErrorKind::Busy),
        (504, ErrorKind::Timeout),
    ] {
        let wire = Wire::new(vec![HttpResponse { status, headers: vec![("Retry-After".into(), "2".into())], body: KEY.as_bytes().to_vec() }]);
        let context = crate::SubmitContext::new();
        let e = block_on(fal(&wire).submit_with_delivery(&hunyuan(HUNYUAN_TEXT), &context)).unwrap_err();
        assert_eq!(e.kind, kind);
        assert!(!e.message.contains(KEY));
        assert!(context.finish_error().is_none());
        if status == 429 {
            assert_eq!(e.retry_after_ms, Some(2000));
        }
    }
    let wire = Arc::new(Wire { replies: Mutex::new(vec![Err(ProviderError::new(ErrorKind::Network, KEY))].into()), ..Default::default() });
    let context = crate::SubmitContext::new();
    assert!(block_on(fal(&wire).submit_with_delivery(&hunyuan(HUNYUAN_TEXT), &context)).is_err());
    assert!(context.finish_error().is_none());
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid live smoke: NODECRAFT_LIVE_HUNYUAN=1 + NODECRAFT_LIVE_HUNYUAN_ACKNOWLEDGE_COST=1 + FAL_KEY; one Geometry GLB costs $0.225"]
fn hunyuan42_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_HUNYUAN").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_HUNYUAN_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let f = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let mut j = hunyuan(HUNYUAN_TEXT);
        j.params.insert("mesh_preset".into(), json!("geometry"));
        let Submission::Queued(h) = f.submit(&j).await.unwrap() else { panic!() };
        for _ in 0..180 {
            match f.poll(&h).await.unwrap() {
                JobProgress::Done(done) => {
                    assert_eq!(done.outputs.len(), 1);
                    return;
                }
                JobProgress::Failed(e) => panic!("{}", e.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(3)).await,
            }
        }
        let _ = f.cancel(&h).await;
        panic!("Hunyuan3D live task timed out");
    });
}

#[test]
fn hunyuan42_sketch_minimum_is_checked_before_paid_delivery() {
    for (width, height) in [(127, 128), (128, 127), (1, 1)] {
        let mut j = job(HUNYUAN_SKETCH);
        j.inputs.push(crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png(width, height).into() });
        let wire = Wire::new(vec![reply(receipt(HUNYUAN_SKETCH))]);
        let context = crate::SubmitContext::new();
        let error = block_on(fal(&wire).submit_with_delivery(&j, &context)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput);
        assert!(context.finish_error().is_some());
        assert!(wire.requests.lock().unwrap().is_empty());
    }
}
