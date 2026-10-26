//! Offline contracts from the public Luma-on-fal schemas, with procedural media.
use super::tests::{handle, job, png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "luma26-public-fixture-not-a-key";
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
        panic!("redirect-capable transport refused")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let result = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { result })
    }
}
fn reply(value: Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() }
}
fn bytes(body: Vec<u8>) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body }
}
fn fal(wire: &Arc<Wire>) -> Fal {
    Fal::new(Secret::new(KEY).unwrap(), wire.clone())
}
fn frame(role: &str) -> crate::JobInput {
    crate::JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: png(2, 2).into() }
}
fn luma(model: &str) -> ProviderJob {
    let mut j = job(model);
    if model == LUMA_RAY_I2V {
        j.inputs.push(frame("first_frame"));
    }
    j
}

#[test]
fn luma26_request_routes_and_bodies_follow_each_documented_schema() {
    for model in [LUMA_RAY, LUMA_RAY_I2V, LUMA_PHOTON] {
        let wire = Wire::new(vec![reply(receipt(model))]);
        let mut j = luma(model);
        if model == LUMA_RAY {
            j.params.insert("duration_seconds".into(), json!(10));
        }
        if model == LUMA_RAY_I2V {
            j.inputs.push(frame("last_frame"));
        }
        let Submission::Queued(h) = block_on(fal(&wire).submit(&j)).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        let requests = wire.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.url, format!("https://queue.fal.run/{model}"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let mut expected = if model == LUMA_PHOTON {
            json!({"prompt":"A blue circle","aspect_ratio":"1:1"})
        } else {
            json!({"prompt":"A blue circle","aspect_ratio":"16:9","resolution":"540p","duration":if model == LUMA_RAY {"10s"} else {"5s"},"loop":false,"hdr":false,"exr_export":false})
        };
        if model == LUMA_RAY_I2V {
            use base64::Engine;
            let uri = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png(2, 2)));
            expected["image_url"] = json!(uri);
            expected["end_image_url"] = expected["image_url"].clone();
        }
        assert_eq!(body, expected);
        assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Key {KEY}")));
        assert!(request.headers.iter().any(|(k, v)| k == "X-Fal-No-Retry" && v == "1"));
        assert!(!fal(&wire).retry_ambiguous_submit());
    }
}

#[test]
fn luma26_refuses_unsupported_modes_roles_and_parameters_before_delivery() {
    let mut invalid_jobs = Vec::new();
    for (model, key, value) in [
        (LUMA_RAY, "duration_seconds", json!(6)),
        (LUMA_RAY_I2V, "duration_seconds", json!(10)),
        (LUMA_RAY, "resolution", json!("4k")),
        (LUMA_RAY, "hdr", json!(true)),
        (LUMA_RAY, "exr_export", json!(true)),
        (LUMA_PHOTON, "seed", json!(1)),
        (LUMA_PHOTON, "num_images", json!(4)),
        (LUMA_RAY, "loop", json!("yes")),
        (LUMA_RAY, "aspect_ratio", json!({"nested":[]})),
    ] {
        let mut j = luma(model);
        j.params.insert(key.into(), value);
        invalid_jobs.push(j);
    }
    let mut ten_loop = luma(LUMA_RAY);
    ten_loop.params = serde_json::from_value(json!({"duration_seconds":10,"loop":true})).unwrap();
    invalid_jobs.push(ten_loop);
    let mut end_loop = luma(LUMA_RAY_I2V);
    end_loop.inputs.push(frame("last_frame"));
    end_loop.params.insert("loop".into(), json!(true));
    invalid_jobs.push(end_loop);
    let mut duplicate = luma(LUMA_RAY_I2V);
    duplicate.inputs.push(frame("first_frame"));
    invalid_jobs.push(duplicate);
    invalid_jobs.push(job(LUMA_RAY_I2V));
    let mut photon_frame = luma(LUMA_PHOTON);
    photon_frame.inputs.push(frame("first_frame"));
    invalid_jobs.push(photon_frame);
    for (model, prompt) in [(LUMA_PHOTON, "aa".into()), (LUMA_PHOTON, "x".repeat(5001)), (LUMA_RAY, "x".repeat(6001)), (LUMA_RAY, " ".into())] {
        let mut j = luma(model);
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: prompt };
        invalid_jobs.push(j);
    }
    let wire = Arc::new(Wire::default());
    for j in invalid_jobs {
        assert_eq!(block_on(fal(&wire).submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    }
    assert!(wire.requests.lock().unwrap().is_empty());
    let j = luma(LUMA_RAY_I2V);
    let (_, body) = request::prepare(&j, Limits::HARD).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: body.len(), ..Limits::HARD }).is_ok());
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Limits::HARD }).is_err());
    let mut bad_frame = j;
    bad_frame.inputs[1] = crate::JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: Arc::from(&b"broken"[..]) };
    assert!(request::prepare(&bad_frame, Limits::HARD).is_err());
}

#[test]
fn luma26_accepted_tasks_resume_and_keep_credential_free_bounded_results() {
    for model in [LUMA_RAY, LUMA_RAY_I2V, LUMA_PHOTON] {
        let media = if model == LUMA_PHOTON { png(2, 2) } else { include_bytes!("fixtures/blue.mp4").to_vec() };
        let file = json!({"url":"https://v3.fal.media/files/luma-fixture","file_size":media.len()});
        let result = if model == LUMA_PHOTON { json!({"images":[file]}) } else { json!({"video":file,"exr_file":null}) };
        let mut result = reply(result);
        result.headers.push(("X-Fal-Billable-Units".into(), "0.25".into()));
        let wire = Wire::new(vec![
            reply(receipt(model)),
            reply(json!({"status":"IN_QUEUE","queue_position":2})),
            reply(json!({"status":"IN_PROGRESS"})),
            reply(json!({"status":"COMPLETED"})),
            result,
            bytes(media.clone()),
        ]);
        let f = fal(&wire);
        let Submission::Queued(h) = block_on(f.submit(&luma(model))).unwrap() else { panic!() };
        let h: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Queued { position: Some(2) }));
        assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Running { .. }));
        let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
        assert_eq!(done.outputs, vec![Output::Bytes { mime: if model == LUMA_PHOTON { "image/png" } else { "video/mp4" }.into(), bytes: media.into() }]);
        assert!(done.usage.cost.is_none());
        assert_eq!(done.usage.billable_units, Some(BillableUnits::parse("0.25").unwrap()));
        let requests = wire.requests.lock().unwrap();
        assert!(requests.last().unwrap().headers.is_empty());
        let root = if model == LUMA_PHOTON { LUMA_PHOTON } else { "luma/agent" };
        assert_eq!(requests[1].url, format!("https://queue.fal.run/{root}/requests/r_1/status"));
    }
}

#[test]
fn luma26_photon_generic_files_allow_real_jpeg_without_invented_dimensions() {
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg).encode(&[120; 12], 2, 2, image::ExtendedColorType::Rgb8).unwrap();
    let wire = Wire::new(vec![
        reply(json!({"status":"COMPLETED"})),
        reply(json!({"images":[{"url":"https://fal.media/photon","content_type":"image/jpeg"}]})),
        bytes(jpeg),
    ]);
    let JobProgress::Done(done) = block_on(fal(&wire).poll(&handle(LUMA_PHOTON))).unwrap() else { panic!() };
    assert!(matches!(&done.outputs[0],Output::Bytes{mime,..} if mime == "image/jpeg"));
    let key = Secret::new(KEY).unwrap();
    let record = Record::read(&handle(LUMA_PHOTON), &key).unwrap().0;
    let mut extra = handle(LUMA_PHOTON);
    extra.data["requested"] = json!(2);
    assert!(Record::read(&extra, &key).is_err(), "Photon's saved contract admits exactly one image");
    for bad in [
        json!({"images":[]}),
        json!({"images":[{"url":"https://evil.example/a"}]}),
        json!({"images":[{"url":"https://fal.media/a","width":5000}]}),
        json!({"images":[{"url":"https://fal.media/a","content_type":"video/mp4"}]}),
        json!({"images":[{"url":"https://fal.media/a"},{"url":"https://fal.media/b"}]}),
    ] {
        assert!(response::outputs(&serde_json::to_vec(&bad).unwrap(), record, Limits::HARD, &key).is_err());
    }
    let record = Record::read(&handle(LUMA_RAY), &key).unwrap().0;
    assert!(
        response::outputs(
            &serde_json::to_vec(&json!({"video":{"url":"https://fal.media/a"},"exr_file":{"url":"https://fal.media/b"}})).unwrap(),
            record,
            Limits::HARD,
            &key
        )
        .is_err()
    );
    assert!(media::validate(b"broken", record, Limits::HARD, &mut 0).is_err());
}

#[test]
fn luma26_cancel_and_error_contracts_use_only_the_selected_queue_root() {
    for model in [LUMA_RAY, LUMA_RAY_I2V, LUMA_PHOTON] {
        for (code, status_name) in [(202, "CANCELLATION_REQUESTED"), (400, "ALREADY_COMPLETED"), (404, "NOT_FOUND")] {
            let mut response = reply(json!({"status":status_name}));
            response.status = code;
            let wire = Wire::new(vec![response]);
            block_on(fal(&wire).cancel(&handle(model))).unwrap();
            assert_eq!(wire.requests.lock().unwrap()[0].method, HttpMethod::Put);
        }
        let mut forged = handle(model);
        forged.data["status_url"] = json!("https://evil.example/status");
        let wire = Arc::new(Wire::default());
        assert!(block_on(fal(&wire).poll(&forged)).is_err());
        assert!(wire.requests.lock().unwrap().is_empty());
    }
    for (status_code, kind) in [(401, ErrorKind::Auth), (429, ErrorKind::RateLimited), (500, ErrorKind::Server)] {
        let mut response = reply(json!({"secret":KEY}));
        response.status = status_code;
        let wire = Wire::new(vec![response]);
        let error = block_on(fal(&wire).submit(&luma(LUMA_RAY))).unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(!error.message.contains(KEY));
    }
}

#[test]
fn luma26_catalog_quotes_match_sent_duration_and_selected_resolution() {
    use nodecraft_types::{Currency, Money, ParamValues, Unsigned, Value};
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    for id in ["fal.luma-ray32", "fal.luma-ray32-i2v"] {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider, "fal");
        for (resolution, rate) in [("540p", 100_000), ("720p", 200_000), ("1080p", 400_000)] {
            let mut params = ParamValues::new();
            params.insert("resolution".into(), Value::Text(resolution.into()));
            assert_eq!(model.quote(&params).unwrap(), Some(Money::new(Currency::USD, rate * 5)));
            if id == "fal.luma-ray32" {
                params.insert("duration_seconds".into(), Value::Unsigned(Unsigned(10)));
                assert_eq!(model.quote(&params).unwrap(), Some(Money::new(Currency::USD, rate * 10)));
            }
        }
    }
    assert!(catalog.get("fal.luma-photon").unwrap().quote(&ParamValues::new()).unwrap().is_none());
}

#[test]
fn luma26_resumed_ray_downloads_above_image_cap_and_keeps_lowered_limits() {
    let mut media = include_bytes!("fixtures/blue.mp4").to_vec();
    let padding = 17 * 1024 * 1024;
    media.extend_from_slice(&u32::try_from(padding + 8).unwrap().to_be_bytes());
    media.extend_from_slice(b"free");
    media.resize(media.len() + padding, 0);
    assert!(media.len() > Limits::HARD.output_bytes);
    for model in [LUMA_RAY, LUMA_RAY_I2V] {
        for declared in [true, false] {
            let mut file = json!({"url":"https://fal.media/ray-large","content_type":"video/mp4"});
            if declared {
                file["file_size"] = json!(media.len());
            }
            let wire = Wire::new(vec![reply(json!({"status":"COMPLETED"})), reply(json!({"video":file})), bytes(media.clone())]);
            let restored: JobHandle = serde_json::from_str(&serde_json::to_string(&handle(model)).unwrap()).unwrap();
            let JobProgress::Done(done) = block_on(fal(&wire).poll(&restored)).unwrap() else {
                panic!("complete Ray result");
            };
            assert!(matches!(done.outputs.as_slice(), [Output::Bytes { mime, bytes }] if mime == "video/mp4" && bytes.as_ref() == media.as_slice()));
            let requests = wire.requests.lock().unwrap();
            assert_eq!(requests.last().unwrap().max_response_bytes, 256 * 1024 * 1024);
            assert!(requests.last().unwrap().headers.is_empty());
        }
        for (limits, declared_size) in [
            (Limits { output_bytes: 8 * 1024 * 1024, total_output_bytes: 8 * 1024 * 1024, ..Limits::default() }, media.len()),
            (Limits::default(), Limits::VIDEO_HARD.output_bytes + 1),
        ] {
            let wire =
                Wire::new(vec![reply(json!({"status":"COMPLETED"})), reply(json!({"video":{"url":"https://fal.media/ray-large","file_size":declared_size}}))]);
            let f = Fal::with_limits(Secret::new(KEY).unwrap(), wire.clone(), limits).unwrap();
            assert_eq!(block_on(f.poll(&handle(model))).unwrap_err().kind, ErrorKind::Malformed);
            assert_eq!(wire.requests.lock().unwrap().len(), 2, "declared excess refuses before media download");
        }
    }
    for model in [Model::Image, Model::Photon, Model::Video, Model::Frames, Model::Speech, Model::Audio, Model::Mesh] {
        let limits = Limits::default().for_model(model);
        assert_eq!(limits.output_bytes, Limits::HARD.output_bytes);
        assert_eq!(limits.total_output_bytes, Limits::HARD.total_output_bytes);
    }
    assert!(Limits { output_bytes: Limits::VIDEO_HARD.output_bytes + 1, ..Limits::default() }.validate().is_err());
    assert!(Limits { total_output_bytes: Limits::VIDEO_HARD.total_output_bytes + 1, ..Limits::default() }.validate().is_err());
}
