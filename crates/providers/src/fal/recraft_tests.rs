//! Authored Recraft fixtures: strict paid inputs, saved tasks and safe original SVG delivery.
use super::tests::{handle, job, png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "recraft35-fixture-not-a-key";
const MODELS: [&str; 2] = [RECRAFT_IMAGE, RECRAFT_VECTOR];
const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><rect x="32" y="32" width="960" height="960" fill="#285ac8"/></svg>"##;
const GCS: &str = "https://storage.googleapis.com/falserverless/example_outputs/recraft-v4/standard-output.webp";
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
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("unexpected HTTP");
        Box::pin(async { reply })
    }
}
fn reply(value: Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() }
}
fn fal(wire: &Arc<Wire>) -> Fal {
    Fal::new(Secret::new(KEY).unwrap(), wire.clone())
}
fn output(url: &str, mime: &str, bytes: &[u8]) -> Value {
    json!({"images":[{"url":url,"content_type":mime,"file_size":bytes.len(),"file_name":"public-fixture"}]})
}

#[test]
fn recraft35_both_routes_pin_single_output_safety_and_only_the_formal_fal_controls() {
    for model in MODELS {
        let wire = Wire::new(vec![reply(receipt(model))]);
        let provider = fal(&wire);
        let Submission::Queued(h) = block_on(provider.submit(&job(model))).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        assert_eq!(h.data["response_url"], "https://queue.fal.run/fal-ai/recraft/requests/r_1");
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.url, format!("{ORIGIN}/{model}"));
        assert_eq!(request.method, HttpMethod::Post);
        assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Key {KEY}")));
        assert!(request.headers.iter().any(|(k, v)| k == "X-Fal-No-Retry" && v == "1"));
        assert!(request.headers.iter().any(|(k, v)| k == "x-app-fal-disable-fallback" && v == "true"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body, json!({"prompt":"A blue circle","image_size":"square_hd","colors":[],"enable_safety_checker":true}));
        assert_eq!(h.data["requested"], 1);
        assert!(!provider.retry_ambiguous_submit());
    }
}
#[test]
fn recraft35_presets_and_bounded_rgb_preferences_encode_without_native_only_parameters() {
    for model in MODELS {
        for preset in ["square_hd", "square", "portrait_4_3", "portrait_16_9", "landscape_4_3", "landscape_16_9"] {
            let mut j = job(model);
            j.params = json!({"image_size":preset,"colors":"#000000,#112233,#445566,#778899,#aAbBcC,#ffffff","background_color":"#123456"})
                .as_object()
                .unwrap()
                .clone();
            let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["image_size"], preset);
            assert_eq!(
                body["colors"],
                json!([{"r":0,"g":0,"b":0},{"r":17,"g":34,"b":51},{"r":68,"g":85,"b":102},{"r":119,"g":136,"b":153},{"r":170,"g":187,"b":204},{"r":255,"g":255,"b":255}])
            );
            assert_eq!(body["background_color"], json!({"r":18,"g":52,"b":86}));
            assert_eq!(body.as_object().unwrap().len(), 5);
        }
        let mut j = job(model);
        j.params = json!({"colors":"","background_color":""}).as_object().unwrap().clone();
        let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
        assert!(serde_json::from_slice::<Value>(&bytes).unwrap().get("background_color").is_none());
    }
}
#[test]
fn recraft35_invalid_roles_colors_types_counts_and_unknown_controls_never_send_paid_http() {
    let mut jobs = vec![];
    for model in MODELS {
        for (key, value) in [
            ("seed", json!(1)),
            ("num_images", json!(2)),
            ("n", json!(1)),
            ("sync_mode", json!(true)),
            ("enable_safety_checker", json!(false)),
            ("style", json!("realistic_image")),
            ("model", json!("recraftv4_1_pro")),
            ("image_size", json!({"width":1024,"height":1024})),
            ("image_size", json!("auto")),
            ("image_size", Value::Null),
            ("colors", json!([{"r":1,"g":2,"b":3}])),
            ("colors", json!(1)),
            ("colors", json!("#ffffffff")),
            ("colors", json!("#000000,")),
            ("colors", json!("#000000,#000000,#000000,#000000,#000000,#000000,#000000")),
            ("background_color", json!("red")),
            ("background_color", json!("#12345678")),
            ("background_color", json!("#zzzzzz")),
        ] {
            let mut j = job(model);
            j.params.insert(key.into(), value);
            jobs.push(j);
        }
        for text in ["".to_string(), " \n\t".into(), "🦋".repeat(10_001)] {
            let mut j = job(model);
            j.inputs = vec![crate::JobInput::Text { role: "prompt".into(), text }];
            jobs.push(j);
        }
        let mut j = job(model);
        j.inputs.clear();
        jobs.push(j);
        let mut j = job(model);
        j.inputs.push(j.inputs[0].clone());
        jobs.push(j);
        let mut j = job(model);
        j.inputs = vec![crate::JobInput::Text { role: "negative_prompt".into(), text: "unsupported".into() }];
        jobs.push(j);
        let mut j = job(model);
        j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: nodecraft_media::svg::MIME.into(), bytes: SVG.into() }];
        jobs.push(j);
    }
    let wire = Arc::new(Wire::default());
    for j in jobs {
        let context = crate::SubmitContext::new();
        assert!(block_on(fal(&wire).submit_with_delivery(&j, &context)).is_err());
        assert!(context.finish_error().is_some(), "local refusal must prove no submission");
    }
    assert!(wire.requests.lock().unwrap().is_empty());
}
#[test]
fn recraft35_unicode_prompt_complete_escaped_extent_and_lowered_geometry_are_admitted_before_payment() {
    for model in MODELS {
        let mut j = job(model);
        j.inputs = vec![crate::JobInput::Text { role: "prompt".into(), text: "🦋".repeat(10_000) }];
        assert!(request::prepare(&j, Limits::default()).is_ok());
        j.inputs = vec![crate::JobInput::Text { role: "prompt".into(), text: "\"\\\n".repeat(1000) }];
        let (_, body) = request::prepare(&j, Limits::default()).unwrap();
        assert!(request::prepare(&j, Limits { request_bytes: body.len(), ..Default::default() }).is_ok());
        assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Default::default() }).is_err());
        assert!(request::prepare(&j, Limits { text_bytes: 16, ..Default::default() }).is_err());
        assert!(request::prepare(&j, Limits { output_width: 1023, ..Default::default() }).is_err());
        assert!(request::prepare(&j, Limits { output_height: 1023, ..Default::default() }).is_err());
        assert!(request::prepare(&j, Limits { total_pixels: 1024 * 1024 - 1, ..Default::default() }).is_err());
        let limits = Limits::default().for_model(Model::read(model).unwrap());
        assert_eq!(limits.text_bytes, 64 * 1024);
        assert_eq!(limits.output_bytes, 16 * 1024 * 1024);
        assert_eq!(limits.total_output_bytes, 16 * 1024 * 1024);
        assert_eq!((limits.output_width, limits.output_height, limits.total_pixels), (4096, 4096, 16_777_216));
    }
    assert_eq!(Limits::default().for_model(Model::Image).text_bytes, 16 * 1024);
    assert_eq!(Limits::default().for_model(Model::Image).total_output_bytes, 48 * 1024 * 1024);
}
#[test]
fn recraft35_saved_handles_resume_both_formats_without_credentials_on_fal_or_model_scoped_gcs_storage() {
    for model in MODELS {
        for url in ["https://v3b.fal.media/public-recraft35", GCS] {
            let (mime, bytes) = if model == RECRAFT_VECTOR { (nodecraft_media::svg::MIME, SVG.to_vec()) } else { ("image/png", png(4, 4)) };
            let wire = Wire::new(vec![
                reply(receipt(model)),
                reply(json!({"status":"IN_PROGRESS"})),
                reply(json!({"status":"COMPLETED"})),
                reply(output(url, mime, &bytes)),
                HttpResponse { status: 200, headers: vec![], body: bytes.clone() },
                reply(json!({"status":"OK"})),
            ]);
            let provider = fal(&wire);
            let Submission::Queued(h) = block_on(provider.submit(&job(model))).unwrap() else { panic!() };
            let saved: JobHandle = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
            assert!(matches!(block_on(provider.poll(&saved)).unwrap(), JobProgress::Running { .. }));
            let JobProgress::Done(done) = block_on(provider.poll(&saved)).unwrap() else { panic!() };
            assert_eq!(done.outputs, vec![Output::Bytes { mime: mime.into(), bytes: bytes.clone().into() }]);
            assert!(done.usage.cost.is_none());
            block_on(provider.cancel(&saved)).unwrap();
            let requests = wire.requests.lock().unwrap();
            assert_eq!(requests.len(), 6);
            assert!(requests[4].headers.is_empty());
            assert_eq!(requests[4].max_response_bytes, 16 * 1024 * 1024);
            assert_eq!(requests[5].method, HttpMethod::Put);
            assert_eq!(requests[5].url, "https://queue.fal.run/fal-ai/recraft/requests/r_1/cancel");
            let mut tampered = saved.clone();
            tampered.data["requested"] = json!(2);
            assert!(Record::read(&tampered, &Secret::new(KEY).unwrap()).is_err());
            let mut tampered = saved;
            tampered.data["status_url"] = json!(format!("{ORIGIN}/{model}/requests/r_1/status"));
            assert!(Record::read(&tampered, &Secret::new(KEY).unwrap()).is_err());
        }
    }
}
#[test]
fn recraft35_gcs_exception_is_exact_canonical_and_does_not_widen_style_or_old_aliases() {
    let key = Secret::new(KEY).unwrap();
    for model in MODELS {
        let record = Record::read(&handle(model), &key).unwrap().0;
        for url in [
            "https://storage.googleapis.com/another-bucket/output.svg",
            "https://storage.googleapis.com/falserverless-evil/output.svg",
            "https://storage.googleapis.com/falserverless/../another-bucket/x",
            "https://storage.googleapis.com/falserverless/a/../../x",
            "https://storage.googleapis.com/falserverless/%2e%2e/x",
            "https://storage.googleapis.com:443/falserverless/x",
            "https://user@storage.googleapis.com/falserverless/x",
            "https://storage.googleapis.com/falserverless/x?download=1",
            "https://storage.googleapis.com/falserverless/x#secret",
            "https://storage.googleapis.com/falserverless/",
            "https://storage.googleapis.com/falserverless//x",
            "https://storage.googleapis.com.evil.example/falserverless/x",
            "http://storage.googleapis.com/falserverless/x",
        ] {
            assert!(
                response::outputs(
                    &serde_json::to_vec(&output(url, if model == RECRAFT_VECTOR { nodecraft_media::svg::MIME } else { "image/png" }, SVG)).unwrap(),
                    record,
                    Limits::default().for_model(record.model),
                    &key
                )
                .is_err(),
                "{url}"
            );
        }
        assert!(
            response::outputs(
                &serde_json::to_vec(&output(&format!("https://storage.googleapis.com/falserverless/{KEY}"), "image/svg+xml", SVG)).unwrap(),
                record,
                Limits::default(),
                &key
            )
            .is_err()
        );
    }
    assert!(response::storage(GCS).is_err(), "style links keep the original fal.media policy");
    let record = Record::read(&handle(FLUX), &key).unwrap().0;
    assert!(response::outputs(&serde_json::to_vec(&json!({"images":[{"url":GCS,"width":4,"height":4}]})).unwrap(), record, Limits::default(), &key).is_err());
}
#[test]
fn recraft35_declared_wrong_types_counts_and_oversize_outputs_refuse_before_cdn_fetch() {
    for model in MODELS {
        for file in [
            json!({"images":[]}),
            json!({"images":[{"url":"https://fal.media/x"},{"url":"https://fal.media/y"}]}),
            json!({"images":[{"url":"https://fal.media/x","content_type":"application/zip"}]}),
            json!({"images":[{"url":"https://fal.media/x","file_size":16777217}]}),
            json!({"images":[{"url":"https://fal.media/x","width":4097,"height":1}]}),
            json!({"images":[{"url":"https://fal.media/x","file_size":"10"}]}),
            json!({"images":[{"url":"https://fal.media/x","content_type":false}]}),
            json!({"images":[{"url":"https://fal.media/x"}],"video":null}),
        ] {
            let wire = Wire::new(vec![reply(json!({"status":"COMPLETED"})), reply(file)]);
            assert!(block_on(fal(&wire).poll(&handle(model))).is_err());
            assert_eq!(wire.requests.lock().unwrap().len(), 2, "metadata refusal must happen before storage HTTP");
        }
    }
}
#[test]
fn recraft35_safe_large_svg_and_unsupported_static_preview_features_retain_original_typed_bytes() {
    let unsupported = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024"><text x="8" y="20">Original editable title</text></svg>"#.to_vec();
    assert!(nodecraft_media::svg::probe(&unsupported, &Limits::default().image(false)).unwrap().preview_unavailable.is_some());
    let mut large = b"<!--".to_vec();
    large.resize(1_800_000, b'a');
    large.extend(b"-->");
    large.extend(SVG);
    for bytes in [SVG.to_vec(), unsupported, large] {
        let wire = Wire::new(vec![
            reply(json!({"status":"COMPLETED"})),
            reply(output("https://fal.media/public-recraft35.svg", nodecraft_media::svg::MIME, &bytes)),
            HttpResponse { status: 200, headers: vec![], body: bytes.clone() },
        ]);
        let JobProgress::Done(done) = block_on(fal(&wire).poll(&handle(RECRAFT_VECTOR))).unwrap() else { panic!() };
        assert_eq!(done.outputs, vec![Output::Bytes { mime: nodecraft_media::svg::MIME.into(), bytes: bytes.into() }]);
    }
}
#[test]
fn recraft35_svg_safety_actual_geometry_and_raster_vector_type_boundaries_are_enforced() {
    let record = Record::read(&handle(RECRAFT_VECTOR), &Secret::new(KEY).unwrap()).unwrap().0;
    for bytes in [
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><script>bad()</script></svg>"#.as_slice(),
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><image href="https://other.example/x"/></svg>"#.as_slice(),
        br#"<html><svg xmlns="http://www.w3.org/2000/svg"/></html>"#.as_slice(),
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="4097" height="1"/>"#.as_slice(),
        b"malformed SVG".as_slice(),
    ] {
        assert!(media::validate(bytes, record, Limits::default(), &mut 0).is_err());
    }
    assert!(media::validate(&png(4, 4), record, Limits::default(), &mut 0).is_err());
    let raster = Record::read(&handle(RECRAFT_IMAGE), &Secret::new(KEY).unwrap()).unwrap().0;
    assert!(media::validate(SVG, raster, Limits::default(), &mut 0).is_err());
    assert!(media::validate(SVG, record, Limits { total_pixels: 1024 * 1024 - 1, ..Default::default() }, &mut 0).is_err());
    assert!(media::validate(SVG, record, Limits { output_bytes: SVG.len() - 1, ..Default::default() }, &mut 0).is_err());
    let wire = Wire::new(vec![
        reply(json!({"status":"COMPLETED"})),
        reply(json!({"images":[{"url":"https://fal.media/x","width":100,"height":100}]})),
        HttpResponse { status: 200, headers: vec![], body: SVG.to_vec() },
    ]);
    assert!(block_on(fal(&wire).poll(&handle(RECRAFT_VECTOR))).is_err());
}
#[test]
fn recraft35_remote_refusals_lost_and_malformed_acceptance_remain_uncertain_without_replay() {
    for model in MODELS {
        for status in [400, 401, 402, 403, 422, 429, 500, 503] {
            let wire = Wire::new(vec![HttpResponse { status, headers: vec![("Retry-After".into(), "2".into())], body: KEY.as_bytes().to_vec() }]);
            let context = crate::SubmitContext::new();
            let e = block_on(fal(&wire).submit_with_delivery(&job(model), &context)).unwrap_err();
            assert!(context.finish_error().is_none());
            assert!(!e.message.contains(KEY));
            assert!(!fal(&wire).retry_ambiguous_submit());
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
            if status == 429 {
                assert_eq!(e.retry_after_ms, Some(2000));
            }
        }
        for response in [
            Err(ProviderError::new(ErrorKind::Network, KEY)),
            Ok(HttpResponse { status: 200, headers: vec![], body: br#"{"request_id":"r_1","request_id":"r_2","status":"IN_QUEUE"}"#.to_vec() }),
        ] {
            let wire = Arc::new(Wire { replies: Mutex::new(vec![response].into()), ..Default::default() });
            let context = crate::SubmitContext::new();
            let e = block_on(fal(&wire).submit_with_delivery(&job(model), &context)).unwrap_err();
            assert!(context.finish_error().is_none());
            assert!(!e.message.contains(KEY));
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn recraft35_raster_webp_and_jpeg_use_actual_codecs_and_oversize_svg_downloads_are_refused() {
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    for (format, mime) in [(ImageFormat::Jpeg, "image/jpeg"), (ImageFormat::WebP, "image/webp")] {
        let mut bytes = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([40, 70, 120]))).write_to(&mut bytes, format).unwrap();
        let bytes = bytes.into_inner();
        let wire = Wire::new(vec![
            reply(json!({"status":"COMPLETED"})),
            reply(output(GCS, mime, &bytes)),
            HttpResponse { status: 200, headers: vec![], body: bytes.clone() },
        ]);
        let JobProgress::Done(done) = block_on(fal(&wire).poll(&handle(RECRAFT_IMAGE))).unwrap() else { panic!() };
        assert_eq!(done.outputs, vec![Output::Bytes { mime: mime.into(), bytes: bytes.into() }]);
    }
    let wire = Wire::new(vec![
        reply(json!({"status":"COMPLETED"})),
        reply(json!({"images":[{"url":"https://fal.media/x","content_type":"image/svg+xml"}]})),
        HttpResponse { status: 200, headers: vec![], body: vec![0; 16 * 1024 * 1024 + 1] },
    ]);
    assert!(block_on(fal(&wire).poll(&handle(RECRAFT_VECTOR))).is_err());
    assert_eq!(wire.requests.lock().unwrap()[2].max_response_bytes, 16 * 1024 * 1024);
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid smoke: NODECRAFT_LIVE_RECRAFT=1 + NODECRAFT_LIVE_RECRAFT_ACKNOWLEDGE_COST=1 + FAL_KEY; one raster costs $0.035"]
fn recraft35_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_RECRAFT").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_RECRAFT_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let provider = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let Submission::Queued(h) = provider.submit(&job(RECRAFT_IMAGE)).await.unwrap() else { panic!() };
        for _ in 0..180 {
            match provider.poll(&h).await.unwrap() {
                JobProgress::Done(done) => {
                    let [Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!("one raster expected") };
                    assert!(matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp"));
                    nodecraft_media::decode_image(bytes, &Default::default()).unwrap();
                    return;
                }
                JobProgress::Failed(error) => panic!("{}", error.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            }
        }
        let _ = provider.cancel(&h).await;
        panic!("Recraft live task timed out");
    });
}

#[test]
fn recraft35_svg_reference_requires_explicit_rasterization_before_a_raster_fal_job() {
    let wire = Arc::new(Wire::default());
    let mut j = job(ESRGAN);
    j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: nodecraft_media::svg::MIME.into(), bytes: SVG.into() }];
    let scope = crate::SubmitContext::new();
    assert!(block_on(fal(&wire).submit_with_delivery(&j, &scope)).is_err());
    assert!(wire.requests.lock().unwrap().is_empty());
    assert!(scope.finish_error().is_some());
}
