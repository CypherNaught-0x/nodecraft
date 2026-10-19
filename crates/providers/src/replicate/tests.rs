use super::*;
use crate::{HttpResponse, JobInput};
use futures::executor::block_on;
use std::collections::VecDeque;
use std::sync::Mutex;
struct Mock {
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
    seen: Mutex<Vec<HttpRequest>>,
}
impl Http for Mock {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { panic!("redirect-following send must never run") })
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(r.clone());
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        let cap = r.max_response_bytes;
        Box::pin(async move {
            let reply = reply?;
            if reply.body.len() > cap { Err(malformed("fixture enforced transport body cap")) } else { Ok(reply) }
        })
    }
}
fn prediction(model: &str, status: &str, output: serde_json::Value) -> HttpResponse {
    let (m, v) = model.split_once(':').unwrap();
    HttpResponse {
        status: 200,
        headers: vec![],
        body: serde_json::to_vec(&serde_json::json!({"id":"public-prediction","model":m,"version":v,"status":status,"data_removed":false,"output":output}))
            .unwrap(),
    }
}
fn fixture(replies: Vec<HttpResponse>) -> (Replicate, Arc<Mock>) {
    let m = Arc::new(Mock { replies: Mutex::new(replies.into_iter().map(Ok).collect()), seen: Mutex::new(vec![]) });
    (Replicate::new(Secret::new("public-fixture-key").unwrap(), m.clone()), m)
}
fn job(model: &str) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A public landscape".into() }],
        idempotency_key: "public-attempt".into(),
    }
}
fn png(w: u32, h: u32) -> Vec<u8> {
    let pixels = image::RgbaImage::from_pixel(w, h, image::Rgba([30, 50, 70, 255]));
    nodecraft_media::encode_png(w, h, pixels.as_raw(), &nodecraft_media::Limits::default()).unwrap()
}
fn reset() {
    OBS.with(|m| *m.borrow_mut() = [0; 7]);
}
fn counts() -> [usize; 7] {
    OBS.with(|m| *m.borrow())
}
#[test]
fn text_lifecycle_initial_finished_still_publishes_handle_before_literal_output() {
    let (p, m) = fixture(vec![
        prediction(HELLO, "succeeded", serde_json::json!("https://literal.example/text")),
        prediction(HELLO, "succeeded", serde_json::json!("https://literal.example/text")),
    ]);
    let Submission::Queued(h) = block_on(p.submit(&job(HELLO))).unwrap() else { panic!("handle first") };
    let h: JobHandle = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
    let JobProgress::Done(done) = block_on(p.poll(&h)).unwrap() else { panic!("done") };
    assert_eq!(done.outputs, [Output::Text("https://literal.example/text".into())]);
    assert!(done.usage.cost.is_none());
    assert_eq!(m.seen.lock().unwrap().len(), 2);
    assert!(!p.retry_ambiguous_submit());
}

fn bytes(body: Vec<u8>) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body }
}
fn image_input(j: &mut ProviderJob, role: &str, body: Vec<u8>) {
    j.inputs.push(JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: Arc::from(body) });
}
fn handle(p: &Replicate, j: &ProviderJob) -> JobHandle {
    let Submission::Queued(h) = block_on(p.submit(j)).unwrap() else { panic!("queued") };
    serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap()
}
fn full_prediction() -> serde_json::Value {
    let (m, v) = SDXL.split_once(':').unwrap();
    serde_json::json!({"id":"public-prediction","model":m,"version":v,"input":{"prompt":"public","negative_prompt":"","image":null,"mask":null,"width":1024,"height":1024,"num_outputs":1,"scheduler":"K_EULER","num_inference_steps":50,"guidance_scale":7.5,"prompt_strength":0.8,"seed":1,"refine":"no_refiner","high_noise_frac":0.8,"refine_steps":50,"apply_watermark":true,"disable_safety_checker":false},"output":null,"data_removed":false,"error":null,"status":"processing","created_at":"2026-10-18T00:00:00Z","started_at":"2026-10-18T00:00:01Z","completed_at":null,"metrics":{"predict_time":1.0,"total_time":2.0,"public_extra":0},"urls":{"get":"https://untrusted.invalid/ignored","cancel":"https://untrusted.invalid/ignored","stream":"https://untrusted.invalid/ignored","web":"https://replicate.com/p/public"},"source":"api"})
}
fn fields(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Object(o) => o.len() + o.values().map(fields).sum::<usize>(),
        serde_json::Value::Array(a) => a.iter().map(fields).sum(),
        _ => 0,
    }
}
#[test]
fn realistic_prediction_and_exact_non_input_field_boundary_precede_typed_parse() {
    let original = full_prediction();
    assert_eq!(fields(&original), 38);
    for extra in [0, 43, 44] {
        let mut v = original.clone();
        for n in 0..extra {
            v.as_object_mut().unwrap().insert(format!("extra{n}"), serde_json::Value::Null);
        }
        reset();
        let reply = bytes(serde_json::to_vec(&v).unwrap());
        let result = response::parse(&reply, Record { model: Model::Image, requested: 1 }, Limits::HARD);
        if extra <= 43 {
            assert!(result.is_ok());
            assert_eq!(counts()[5], 1);
        } else {
            assert!(result.is_err());
            assert_eq!(counts()[5], 0);
        }
    }
}
#[test]
fn roles_parameters_and_all_input_geometry_are_admitted_before_codec_or_encoding() {
    let good = png(2, 1);
    let mut j = job(SDXL);
    image_input(&mut j, "image", good.clone());
    image_input(&mut j, "mask", good.clone());
    for variant in 0..6 {
        let mut l = Limits::HARD;
        let mut bad = j.clone();
        match variant {
            0 => l.input_bytes = good.len() - 1,
            1 => l.total_input_bytes = good.len() * 2 - 1,
            2 => l.width = 1,
            3 => l.total_pixels = 3,
            4 => image_input(&mut bad, "mask", good.clone()),
            _ => {
                bad.params.insert("width".into(), serde_json::json!(513));
            }
        }
        reset();
        assert!(request::prepare(&bad, l).is_err(), "variant{variant}");
        assert_eq!(counts()[1], 0);
        assert_eq!(counts()[2], 0);
        assert_eq!(counts()[3], 0);
    }
    let l = Limits { input_bytes: good.len(), total_input_bytes: good.len() * 2, width: 2, height: 1, pixels: 2, total_pixels: 4, ..Limits::HARD };
    reset();
    let prepared = request::prepare(&j, l).unwrap();
    let body = prepared.encode(l).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["input"]["num_outputs"], 1);
    assert_eq!(v["input"]["scheduler"], "K_EULER");
    assert_eq!(counts()[0], 2);
    assert_eq!(counts()[1], 2);
    assert_eq!(counts()[2], 2);
    assert_eq!(counts()[3], 1);
    use base64::Engine;
    let uri = v["input"]["image"].as_str().unwrap();
    assert_eq!(base64::engine::general_purpose::STANDARD.decode(uri.strip_prefix("data:image/png;base64,").unwrap()).unwrap(), good);
    reset();
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..l }).is_err());
    assert_eq!(counts()[1], 0, "cheap body cap before decoder");
    assert_eq!(counts()[2], 0);
    assert_eq!(counts()[3], 0);
}
#[test]
fn every_supported_scalar_branch_and_mask_mode_has_exact_wire_values() {
    for size in [512, 768, 1024] {
        for n in [1, 4] {
            let mut j = job(SDXL);
            j.params=serde_json::from_value(serde_json::json!({"width":size,"height":size,"num_outputs":n,"num_inference_steps":100,"guidance_scale":50.0,"prompt_strength":0.0,"seed":i64::MIN})).unwrap();
            for mode in 0..3 {
                let mut j = j.clone();
                if mode >= 1 {
                    image_input(&mut j, "image", png(1, 1));
                }
                if mode == 2 {
                    image_input(&mut j, "mask", png(1, 1));
                }
                let prepared = request::prepare(&j, Limits::HARD).unwrap();
                let v: serde_json::Value = serde_json::from_slice(&prepared.encode(Limits::HARD).unwrap()).unwrap();
                assert_eq!(v["input"]["width"], size);
                assert_eq!(v["input"]["num_outputs"], n);
                assert_eq!(v["input"]["seed"], i64::MIN);
                assert_eq!(v["input"]["mask"].is_string(), mode == 2);
                assert_eq!(v["version"], SDXL);
            }
        }
    }
    for (key, value) in [
        ("num_outputs", serde_json::json!(1.5)),
        ("seed", serde_json::json!(u64::MAX)),
        ("guidance_scale", serde_json::json!(51)),
        ("unknown", serde_json::json!(true)),
    ] {
        let mut j = job(SDXL);
        j.params.insert(key.into(), value);
        assert!(request::prepare(&j, Limits::HARD).is_err());
    }
}
#[test]
fn serialized_handle_retains_requested_count_and_partial_success_materializes_all_outputs() {
    for (requested, returned, ok) in [(1, 2, false), (4, 2, true)] {
        let output = serde_json::Value::Array((0..returned).map(|n| serde_json::json!(format!("https://replicate.delivery/public/{n}.png"))).collect());
        let mut replies = vec![prediction(SDXL, "starting", serde_json::Value::Null), prediction(SDXL, "succeeded", output)];
        for _ in 0..returned {
            replies.push(bytes(png(1, 1)));
        }
        let (p, m) = fixture(replies);
        let mut j = job(SDXL);
        j.params.insert("num_outputs".into(), serde_json::json!(requested));
        let h = handle(&p, &j);
        reset();
        let result = block_on(p.poll(&h));
        if ok {
            let JobProgress::Done(done) = result.unwrap() else { panic!("done") };
            assert_eq!(done.outputs.len(), 2);
            assert!(done.outputs.iter().all(|o| matches!(o,Output::Bytes{mime,..}if mime=="image/png")));
            assert_eq!(counts()[6], 1);
        } else {
            assert!(result.is_err());
            assert_eq!(counts()[4], 0);
            assert_eq!(counts()[6], 0);
        }
        let seen = m.seen.lock().unwrap();
        assert_eq!(seen.len(), if ok { 4 } else { 2 });
        for r in seen.iter() {
            let api = r.url.starts_with(API);
            assert_eq!(r.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer public-fixture-key"), api);
            assert!(!r.url.contains("untrusted"));
        }
    }
}
#[test]
fn remaining_aggregate_budget_is_the_actual_http_cap_and_zero_stops_next_request() {
    let data = png(1, 1);
    for delta in [0, 1] {
        let mut l = Limits::HARD;
        l.output_bytes = data.len();
        l.total_output_bytes = 2 * data.len() - delta;
        let output = serde_json::json!(["https://replicate.delivery/a", "https://replicate.delivery/b"]);
        let (mut p, m) = fixture(vec![
            prediction(SDXL, "starting", serde_json::Value::Null),
            prediction(SDXL, "succeeded", output),
            bytes(data.clone()),
            bytes(data.clone()),
        ]);
        p.limits = l;
        let mut j = job(SDXL);
        j.params.insert("num_outputs".into(), serde_json::json!(2));
        let h = handle(&p, &j);
        reset();
        let result = block_on(p.poll(&h));
        assert_eq!(result.is_ok(), delta == 0);
        assert_eq!(m.seen.lock().unwrap().last().unwrap().max_response_bytes, data.len() - delta);
        assert_eq!(counts()[6], usize::from(delta == 0));
    }
    let (mut p, m) = fixture(vec![
        prediction(SDXL, "starting", serde_json::Value::Null),
        prediction(SDXL, "succeeded", serde_json::json!(["https://replicate.delivery/a"])),
    ]);
    p.limits.total_output_bytes = 0;
    let h = handle(&p, &job(SDXL));
    reset();
    assert!(block_on(p.poll(&h)).is_err());
    assert_eq!(counts()[4], 0);
    assert_eq!(m.seen.lock().unwrap().len(), 2);
}
#[test]
fn complete_url_admission_rejects_hostile_late_url_before_any_download() {
    for bad in [
        "https://replicate.delivery.evil.invalid/x",
        "http://replicate.delivery/x",
        "https://user@replicate.delivery/x",
        "https://replicate.delivery:444/x",
        "https://replicate.delivery/x#secret",
        "https://127.0.0.1/x",
    ] {
        let (p, m) = fixture(vec![
            prediction(SDXL, "starting", serde_json::Value::Null),
            prediction(SDXL, "succeeded", serde_json::json!(["https://replicate.delivery/a", bad])),
        ]);
        let mut j = job(SDXL);
        j.params.insert("num_outputs".into(), serde_json::json!(2));
        let h = handle(&p, &j);
        reset();
        assert!(block_on(p.poll(&h)).is_err());
        assert_eq!(counts()[4], 0);
        assert_eq!(counts()[6], 0);
        assert_eq!(m.seen.lock().unwrap().len(), 2);
    }
    for good in ["https://replicate.delivery/x?q=public", "https://sub.replicate.delivery/x", "https://replicate.delivery:443/x"] {
        assert!(response::delivery_url(good).is_ok());
    }
}
#[test]
fn wrong_identity_hostile_handle_and_late_corrupt_output_never_publish_completed() {
    let (p, m) = fixture(vec![]);
    for (id, data) in [
        ("../escape", serde_json::json!({"v":1,"model":SDXL,"shape":"images","requested":1})),
        ("ok", serde_json::json!({"v":1,"model":SDXL,"shape":"images","requested":5})),
        ("ok", serde_json::json!({"v":2,"model":SDXL,"shape":"images","requested":1})),
    ] {
        let h = JobHandle { remote_id: id.into(), data };
        assert!(block_on(p.poll(&h)).is_err());
        assert!(block_on(p.cancel(&h)).is_err());
    }
    assert!(m.seen.lock().unwrap().is_empty());
    let (p, _) = fixture(vec![prediction(SDXL, "starting", serde_json::Value::Null), prediction(HELLO, "processing", serde_json::Value::Null)]);
    let h = handle(&p, &job(SDXL));
    assert!(block_on(p.poll(&h)).is_err());
    let (p, _) = fixture(vec![
        prediction(SDXL, "starting", serde_json::Value::Null),
        prediction(SDXL, "succeeded", serde_json::json!(["https://replicate.delivery/a", "https://replicate.delivery/b"])),
        bytes(png(1, 1)),
        bytes(b"corrupt image".to_vec()),
    ]);
    let mut j = job(SDXL);
    j.params.insert("num_outputs".into(), serde_json::json!(2));
    let h = handle(&p, &j);
    reset();
    assert!(block_on(p.poll(&h)).is_err());
    assert_eq!(counts()[4], 2);
    assert_eq!(counts()[1], 1);
    assert_eq!(counts()[6], 0);
}
#[test]
fn structural_and_encoded_string_bounds_apply_before_typed_unescape() {
    let r = Record { model: Model::Text, requested: 1 };
    let mut v = full_prediction();
    let (m, version) = HELLO.split_once(':').unwrap();
    v["model"] = serde_json::json!(m);
    v["version"] = serde_json::json!(version);
    v["logs"] = serde_json::Value::Null;
    let base = serde_json::to_string(&v).unwrap();
    let raw = base.replace("\"logs\":null", &format!("\"logs\":\"{}\"", "\\u0061".repeat(500)));
    reset();
    assert!(response::parse(&bytes(raw.into_bytes()), r, Limits { metadata_bytes: 1000, ..Limits::HARD }).is_err());
    assert_eq!(counts()[5], 0);
    let mut invalid = serde_json::to_vec(&v).unwrap();
    invalid.push(255);
    reset();
    assert!(response::parse(&bytes(invalid), r, Limits::HARD).is_err());
    assert_eq!(counts()[5], 0);
    let raw = serde_json::to_string(&v).unwrap().replacen("\"id\"", "\"\\u0069d\"", 1);
    reset();
    assert!(response::parse(&bytes(raw.into_bytes()), r, Limits::HARD).is_err());
    assert_eq!(counts()[5], 0);
    for n in [14, 15] {
        let mut v = v.clone();
        let raw = format!("{}null{}", "[".repeat(n), "]".repeat(n));
        v["extra"] = serde_json::from_str(&raw).unwrap();
        reset();
        let reply = bytes(serde_json::to_vec(&v).unwrap());
        let result = response::parse(&reply, r, Limits::HARD);
        assert_eq!(result.is_ok(), n == 14);
        assert_eq!(counts()[5], usize::from(n == 14));
    }
}
#[test]
fn progress_terminal_states_expiry_and_cancel_use_fixed_id_paths() {
    for status in ["starting", "processing", "failed", "canceled", "aborted"] {
        let (p, m) = fixture(vec![
            prediction(SDXL, "starting", serde_json::Value::Null),
            prediction(SDXL, status, serde_json::Value::Null),
            prediction(SDXL, "canceled", serde_json::Value::Null),
        ]);
        let h = handle(&p, &job(SDXL));
        let progress = block_on(p.poll(&h)).unwrap();
        assert!(match status {
            "starting" => matches!(progress, JobProgress::Queued { .. }),
            "processing" => matches!(progress, JobProgress::Running { .. }),
            "failed" => matches!(progress,JobProgress::Failed(e)if e.kind==ErrorKind::JobFailed),
            _ => matches!(progress,JobProgress::Failed(e)if e.kind==ErrorKind::Cancelled),
        });
        block_on(p.cancel(&h)).unwrap();
        let seen = m.seen.lock().unwrap();
        assert_eq!(seen.last().unwrap().method, HttpMethod::Post);
        assert_eq!(seen.last().unwrap().url, "https://api.replicate.com/v1/predictions/public-prediction/cancel");
    }
    let mut reply = prediction(SDXL, "succeeded", serde_json::Value::Null);
    let mut v: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    v["data_removed"] = serde_json::json!(true);
    reply.body = serde_json::to_vec(&v).unwrap();
    let (p, _) = fixture(vec![prediction(SDXL, "starting", serde_json::Value::Null), reply]);
    let h = handle(&p, &job(SDXL));
    assert_eq!(block_on(p.poll(&h)).unwrap_err().kind, ErrorKind::Expired);
}
#[test]
fn http_errors_are_safe_classified_and_do_not_echo_body_or_transport_url() {
    for (status, kind) in [
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (404, ErrorKind::NotFound),
        (429, ErrorKind::RateLimited),
        (503, ErrorKind::Busy),
        (504, ErrorKind::Timeout),
        (500, ErrorKind::Server),
        (307, ErrorKind::Malformed),
    ] {
        let (p, _) =
            fixture(vec![HttpResponse { status, headers: vec![("retry-after".into(), "2".into())], body: b"private-prompt secret-key signed-url".to_vec() }]);
        let e = block_on(p.submit(&job(SDXL))).unwrap_err();
        assert_eq!(e.kind, kind);
        assert_eq!(e.retry_after_ms, Some(2000));
        assert!(!e.to_string().contains("private-prompt"));
    }
    let e = transport(ProviderError::new(ErrorKind::Network, "https://replicate.delivery/private-secret-path?token=secret"));
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(!e.to_string().contains("secret"));
}

#[test]
fn catalog_defaults_and_public_scalar_admission_agree() {
    use nodecraft_types::{ParamValues, Unsigned, Value};
    let c = nodecraft_catalog::Catalog::builtin().unwrap();
    for (id, wire) in [("replicate.sdxl", SDXL), ("replicate.hello", HELLO)] {
        let model = c.get(id).unwrap();
        assert_eq!(model.provider_model, wire);
        assert_eq!(model.quote(&ParamValues::new()).unwrap(), None);
        let mut j = job(wire);
        let resolved = model.params.resolve(&ParamValues::new(), false).unwrap();
        for (name, value) in resolved.visible_values() {
            let v = match value {
                Value::Unsigned(n) => serde_json::json!(n.0),
                Value::Integer(n) => serde_json::json!(n.0),
                Value::Number(n) => serde_json::json!(n.get()),
                _ => panic!("scalar model values"),
            };
            j.params.insert(name.clone(), v);
        }
        let p = request::prepare(&j, Limits::HARD).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&p.encode(Limits::HARD).unwrap()).unwrap();
        assert_eq!(v["version"], wire);
    }
    let model = c.get("replicate.sdxl").unwrap();
    assert_eq!(model.modes.len(), 3);
    for width in [512, 768, 1024] {
        let mut values = ParamValues::new();
        values.insert("width".into(), Value::Unsigned(Unsigned(width)));
        assert!(model.params.resolve(&values, false).is_ok());
    }
    let mut values = ParamValues::new();
    values.insert("width".into(), Value::Unsigned(Unsigned(513)));
    assert!(model.params.resolve(&values, false).is_err());
}
#[test]
fn codec_and_output_geometry_refuse_before_decoder_boundary() {
    let data = png(2, 1);
    let mut j = job(SDXL);
    image_input(&mut j, "image", data.clone());
    reset();
    assert!(request::prepare(&j, Limits { codec_bytes: 1, ..Limits::HARD }).is_err());
    assert_eq!(counts()[1], 0);
    let (mut p, _) = fixture(vec![
        prediction(SDXL, "starting", serde_json::Value::Null),
        prediction(SDXL, "succeeded", serde_json::json!(["https://replicate.delivery/a"])),
        bytes(data.clone()),
    ]);
    p.limits.width = 1;
    let h = handle(&p, &job(SDXL));
    reset();
    assert!(block_on(p.poll(&h)).is_err());
    assert_eq!(counts()[1], 0);
    assert_eq!(counts()[6], 0);
}

// Synthetic service echoes the complete submitted input and normalizes every known pinned default.
struct EchoHttp {
    seen: Mutex<Vec<HttpRequest>>,
    input: Mutex<serde_json::Value>,
}
impl Http for EchoHttp {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("no legacy transport")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        if r.method == HttpMethod::Post {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            let mut input = body["input"].clone();
            let object = input.as_object_mut().unwrap();
            for (key, value) in [
                ("seed", serde_json::json!(1)),
                ("image", serde_json::Value::Null),
                ("mask", serde_json::Value::Null),
                ("negative_prompt", serde_json::json!("")),
                ("high_noise_frac", serde_json::json!(0.8)),
                ("refine_steps", serde_json::json!(50)),
                ("lora_scale", serde_json::json!(0.6)),
                ("replicate_weights", serde_json::Value::Null),
            ] {
                object.entry(key).or_insert(value);
            }
            *self.input.lock().unwrap() = input;
        }
        let mut reply = full_prediction();
        reply["input"] = self.input.lock().unwrap().clone();
        reply["status"] = serde_json::json!(if r.method == HttpMethod::Post { "starting" } else { "processing" });
        let bytes = serde_json::to_vec(&reply).unwrap();
        let cap = r.max_response_bytes;
        self.seen.lock().unwrap().push(r);
        Box::pin(async move {
            if bytes.len() > cap { Err(malformed("fixture actual response cap")) } else { Ok(HttpResponse { status: 200, headers: vec![], body: bytes }) }
        })
    }
}
fn random_png(w: u32, h: u32) -> Vec<u8> {
    let mut seed = 0x12345678u32;
    let rgba: Vec<u8> = (0..w * h * 4)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    nodecraft_media::encode_png(w, h, &rgba, &nodecraft_media::Limits::default()).unwrap()
}
fn echo_fixture(l: Limits) -> (Replicate, Arc<EchoHttp>) {
    let t = Arc::new(EchoHttp { seen: Mutex::new(vec![]), input: Mutex::new(serde_json::Value::Null) });
    (Replicate::with_limits(Secret::new("public-fixture-not-real").unwrap(), t.clone(), l).unwrap(), t)
}
#[test]
fn full_large_input_echo_and_normalized_defaults_retain_the_prediction_handle() {
    for (w, h) in [(8, 8), (256, 200)] {
        let png = random_png(w, h);
        assert_eq!(png.len(), if w == 8 { 332 } else { 205083 });
        let mut j = job(SDXL);
        image_input(&mut j, "image", png);
        let (p, t) = echo_fixture(Limits::HARD);
        reset();
        let h = handle(&p, &j);
        assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Running { .. }));
        let c = counts();
        assert_eq!(c[1], 1);
        assert_eq!(c[2], 1);
        assert_eq!(c[3], 1);
        assert_eq!(c[5], 2);
        let seen = t.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].max_response_bytes, 2 * 1024 * 1024);
        if w == 256 {
            assert!(seen[0].body.len() > 256 * 1024);
        }
    }
}
#[test]
fn normalized_echo_exact_admission_boundary_and_one_byte_less_before_work() {
    let mut j = job(SDXL);
    j.inputs = vec![JobInput::Text { role: "prompt".into(), text: "p".repeat(130900) }];
    image_input(&mut j, "image", random_png(256, 200));
    image_input(&mut j, "mask", random_png(256, 200));
    j.inputs.push(JobInput::Text { role: "negative_prompt".into(), text: "n".repeat(130900) });
    j.params.insert("num_outputs".into(), serde_json::json!(4));
    let prepared = request::prepare(&j, Limits::HARD).unwrap();
    let bound = prepared.echo_bound().unwrap();
    let (_, o) = response::regions(prepared.record, Limits::HARD).unwrap();
    let response_bytes = bound + Limits::HARD.metadata_bytes + 64 * 1024 + o;
    assert!(response_bytes < Limits::HARD.response_bytes);
    let (p, t) = echo_fixture(Limits { response_bytes, ..Limits::HARD });
    reset();
    let h = handle(&p, &j);
    assert!(block_on(p.poll(&h)).is_ok());
    assert_eq!(t.seen.lock().unwrap().len(), 2);
    assert_eq!(counts()[1], 2);
    assert_eq!(counts()[2], 2);
    let (p, t) = echo_fixture(Limits { response_bytes: response_bytes - 1, ..Limits::HARD });
    reset();
    assert!(block_on(p.submit(&j)).is_err());
    assert!(t.seen.lock().unwrap().is_empty());
    assert_eq!(counts()[1], 0);
    assert_eq!(counts()[2], 0);
    assert_eq!(counts()[3], 0);
}
#[test]
fn impossible_escaped_echo_combination_refuses_before_decode_encode_or_post() {
    let data = random_png(256, 200);
    let mut j = job(SDXL);
    j.inputs = vec![
        JobInput::Text { role: "prompt".into(), text: "\u{1}".repeat(130900) },
        JobInput::Text { role: "negative_prompt".into(), text: "\u{1}".repeat(130900) },
    ];
    image_input(&mut j, "image", data.clone());
    image_input(&mut j, "mask", data);
    let (p, t) = echo_fixture(Limits::HARD);
    reset();
    assert!(block_on(p.submit(&j)).unwrap_err().to_string().contains("response echo"));
    assert!(t.seen.lock().unwrap().is_empty());
    assert_eq!(counts()[1], 0);
    assert_eq!(counts()[2], 0);
    assert_eq!(counts()[3], 0);
    let mut j = job(HELLO);
    j.inputs = vec![JobInput::Text { role: "text".into(), text: "\u{1}".repeat(128 * 1024) }];
    reset();
    assert!(block_on(p.submit(&j)).is_err());
    assert!(t.seen.lock().unwrap().is_empty());
    assert_eq!(counts()[3], 0);
}
#[test]
fn lowered_text_output_admits_nonterminal_null_without_publishing_invalid_success() {
    for text_bytes in [0, 1] {
        let (mut p, _) = fixture(vec![
            prediction(HELLO, "starting", serde_json::Value::Null),
            prediction(HELLO, "processing", serde_json::Value::Null),
            prediction(HELLO, "succeeded", serde_json::json!("xx")),
        ]);
        p.limits.text_bytes = text_bytes;
        reset();
        let h = handle(&p, &job(HELLO));
        assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Running { .. }));
        assert!(block_on(p.poll(&h)).is_err());
        assert_eq!(counts()[6], 0);
    }
}
#[test]
fn echo_regions_are_exclusive_and_duplicate_root_regions_do_not_bypass_limits() {
    let r = Record { model: Model::Text, requested: 1 };
    let reply = prediction(HELLO, "starting", serde_json::Value::Null);
    let raw = String::from_utf8(reply.body).unwrap();
    for field in ["input", "output"] {
        let raw = if field == "output" { raw.replacen("{", "{\"output\":null,", 1) } else { raw.replacen("{", "{\"input\":{},\"input\":{},", 1) };
        reset();
        assert!(response::parse(&bytes(raw.into_bytes()), r, Limits::HARD).is_err());
        assert_eq!(counts()[5], 0);
    }
    // Root input has its own region; an equally named nested field stays ordinary metadata.
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    v["extra"] = serde_json::json!({"input":"x".repeat(300000)});
    reset();
    assert!(response::parse(&bytes(serde_json::to_vec(&v).unwrap()), r, Limits::HARD).is_err());
    assert_eq!(counts()[5], 0);
    let (i, o) = response::regions(r, Limits::HARD).unwrap();
    assert_eq!(i + o + Limits::HARD.metadata_bytes + 65536, Limits::HARD.response_bytes);
    let raw = raw.replacen("{", "{\"input\":{\"a\":0},", 1);
    let l = Limits { response_bytes: 7 + o + Limits::HARD.metadata_bytes + 65536, ..Limits::HARD };
    assert!(response::parse(&bytes(raw.clone().into_bytes()), r, l).is_ok());
    reset();
    assert!(response::parse(&bytes(raw.replace("{\"a\":0}", "{ \"a\":0}").into_bytes()), r, l).is_err());
    assert_eq!(counts()[5], 0);
}

#[test]
fn metadata_framing_and_actual_reply_caps_each_have_exact_boundaries() {
    let record = Record { model: Model::Text, requested: 1 };
    let mut reply = prediction(HELLO, "starting", serde_json::Value::Null);
    let mut v: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    v["logs"] = serde_json::json!("a".repeat(32));
    reply.body = serde_json::to_vec(&v).unwrap();
    let metadata = v.as_object().unwrap().iter().map(|(k, v)| k.len() + v.as_str().map_or(0, str::len)).sum::<usize>();
    let l = Limits { metadata_bytes: metadata, text_bytes: 0, ..Limits::HARD };
    reset();
    assert!(response::parse(&reply, record, l).is_ok());
    assert_eq!(counts()[5], 1);
    reset();
    assert!(response::parse(&reply, record, Limits { metadata_bytes: metadata - 1, ..l }).is_err());
    assert_eq!(counts()[5], 0);
    let framing = reply.body.len() - metadata - 4;
    let mut exact = vec![b' '; 65536 - framing];
    exact.extend_from_slice(&reply.body);
    reply.body = exact;
    reset();
    assert!(response::parse(&reply, record, l).is_ok());
    assert_eq!(counts()[5], 1);
    let mut over = reply.body.clone();
    over.push(b' ');
    reset();
    assert!(response::parse(&bytes(over), record, l).is_err());
    assert_eq!(counts()[5], 0);
    let exact = Limits { response_bytes: reply.body.len(), ..l };
    assert!(response::parse(&reply, record, exact).is_ok());
    reset();
    assert!(response::parse(&reply, record, Limits { response_bytes: exact.response_bytes - 1, ..exact }).is_err());
    assert_eq!(counts()[5], 0);
}

#[test]
fn conservative_echo_counts_del_like_an_ascii_escaping_response_serializer() {
    let mut j = job(HELLO);
    j.inputs = vec![JobInput::Text { role: "text".into(), text: "\u{7f}".repeat(100) }];
    let prepared = request::prepare(&j, Limits::HARD).unwrap();
    assert_eq!(prepared.echo_bound().unwrap(), 611); // {"text":""} plus 100 six-byte escapes.
}

mod followups;
