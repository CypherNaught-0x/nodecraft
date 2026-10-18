use super::*;
use crate::{HttpResponse, JobInput, ProviderJob};
use serde_json::json;
fn job() -> ProviderJob {
    ProviderJob {
        model: LITE.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "original public-safe fixture: a blue square".into() }],
        idempotency_key: "local-only".into(),
    }
}
#[test]
fn request_contract_and_lite_spelling() {
    let l = Limits::default();
    let j = job();
    let p = request::prepare(&j, &l).unwrap();
    let q = p.encode(&crate::secrets::Secret::new("fixture-key").unwrap(), &l).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&q.body).unwrap();
    assert_eq!(body["model"], LITE);
    assert_eq!(body["response_format"]["image_size"], "1K");
    assert_eq!(body["response_format"]["mime_type"], "image/jpeg");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], false);
    assert_eq!(body["background"], false);
    assert!(body.get("idempotency_key").is_none());
    for size in ["1024px", "1k", "2K", "4K"] {
        let mut j = job();
        j.params.insert("image_size".into(), json!(size));
        assert!(request::prepare(&j, &l).is_err());
    }
}
#[test]
fn malformed_optional_usage_does_not_erase_paid_outputs() {
    let bytes = jpeg();
    let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    for usage in [json!(null), json!("bad"), json!({"total_input_tokens":-1}), json!({"total_input_tokens":{}})] {
        let body =
            json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"image","mime_type":"image/jpeg","data":data}]}],"usage":usage});
        let got = response::parse(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&body).unwrap() }, &Limits::default()).unwrap();
        assert_eq!(got.outputs.len(), 1);
        assert_eq!(got.usage, crate::Usage::default());
    }
}
fn jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(2, 2, image::Rgb([30, 60, 90]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

fn reply(v: serde_json::Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&v).unwrap() }
}
fn image_value() -> serde_json::Value {
    json!({"type":"image","mime_type":"image/jpeg","data":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,jpeg())})
}
fn success() -> serde_json::Value {
    json!({"status":"completed","steps":[{"type":"model_output","content":[image_value()]}]})
}
#[test]
fn complete_ordered_outputs_ignore_only_known_thoughts() {
    let mut v = success();
    v["steps"] = json!([{"type":"thought","summary":[image_value()]},{"type":"model_output","content":[{"type":"text","text":"a"},image_value()]},{"type":"model_output","content":[image_value(),{"type":"text","text":"b"}]}]);
    let got = response::parse(reply(v), &Limits::default()).unwrap();
    assert_eq!(got.outputs.len(), 4);
    assert_eq!(got.outputs.first(), Some(&crate::Output::Text("a".into())));
    assert_eq!(got.outputs.last(), Some(&crate::Output::Text("b".into())));
}
#[test]
fn required_output_errors_never_publish_a_partial_collection() {
    for replacement in [
        json!({"type":"image","mime_type":"image/jpeg","data":"Zh=="}),
        json!({"type":"image","mime_type":"image/jpeg","data":"AAAA"}),
        json!({"type":"image","mime_type":"image/png","data":"AAAA"}),
        json!({"type":"image","uri":"https://untrusted.invalid/output"}),
        json!({"type":"unknown"}),
        json!({"type":"text","data":"secret"}),
    ] {
        let mut v = success();
        v["steps"][0]["content"].as_array_mut().unwrap().push(replacement);
        assert!(response::parse(reply(v), &Limits::default()).is_err());
    }
    for status in ["failed", "incomplete", "cancelled", "queued", "in_progress", "requires_action", "future"] {
        let mut v = success();
        v["status"] = json!(status);
        assert!(response::parse(reply(v), &Limits::default()).is_err());
    }
    let mut v = success();
    v["steps"][0]["type"] = json!("unknown");
    assert!(response::parse(reply(v), &Limits::default()).is_err());
    let mut v = success();
    v["steps"][0]["content"] = json!([{"type":"text","text":"safety might apply but no documented reason"}]);
    assert_eq!(response::parse(reply(v), &Limits::default()).unwrap_err().kind, ErrorKind::Malformed);
}
#[test]
fn usage_is_all_or_none_and_never_invents_currency() {
    for usage in [
        json!({"total_input_tokens":4,"total_output_tokens":"bad"}),
        json!({"total_input_tokens":1,"input_tokens_by_modality":[{"modality":"text","tokens":2}]}),
        json!({"total_input_tokens":1,"input_tokens_by_modality":[{"modality":"text","tokens":18446744073709551615u64},{"modality":"image","tokens":1}]}),
        json!({"total_input_tokens":18446744073709551615u64,"total_output_tokens":1}),
        json!({"total_input_tokens":7,"output_tokens_by_modality":{}}),
    ] {
        let mut v = success();
        v["usage"] = usage;
        let got = response::parse(reply(v), &Limits::default()).unwrap();
        assert_eq!(got.usage, crate::Usage::default());
    }
    let mut v = success();
    v["usage"] = json!({"total_input_tokens":7,"total_output_tokens":11,"total_tokens":22,"total_thought_tokens":4});
    let got = response::parse(reply(v), &Limits::default()).unwrap();
    assert_eq!(got.usage.input_tokens, Some(7));
    assert_eq!(got.usage.output_tokens, Some(11));
    assert_eq!(got.usage.cost, None);
}
#[test]
fn lowered_request_limits_precede_any_body_or_pixel_copies() {
    let mut j = job();
    let bytes = jpeg();
    j.inputs.push(JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(bytes.clone()) });
    let mut l = Limits { total_input_bytes: bytes.len(), ..Limits::default() };
    assert!(request::prepare(&j, &l).is_ok());
    l.total_input_bytes = bytes.len() - 1;
    COPIES.with(|c| c.set((0, 0)));
    assert!(request::prepare(&j, &l).is_err());
    assert_eq!(COPIES.with(|c| c.get()), (0, 0));
    let l = Limits { prompt_bytes: 5, ..Limits::default() };
    j.inputs[0] = JobInput::Text { role: "prompt".into(), text: "12345".into() };
    assert!(request::prepare(&j, &l).is_ok());
    j.inputs[0] = JobInput::Text { role: "prompt".into(), text: "123456".into() };
    COPIES.with(|c| c.set((0, 0)));
    assert!(request::prepare(&j, &l).is_err());
    assert_eq!(COPIES.with(|c| c.get()), (0, 0));
    j = job();
    let p = request::prepare(&j, &Limits::default()).unwrap();
    let key = crate::secrets::Secret::new("fixture-key").unwrap();
    let exact = p.encode(&key, &Limits::default()).unwrap().body.len();
    let l = Limits { request_bytes: exact, ..Limits::default() };
    assert!(request::prepare(&j, &l).is_ok());
    assert!(request::prepare(&j, &Limits { request_bytes: exact - 1, ..l }).is_err());
}
#[test]
fn model_roles_options_and_reference_order_are_revalidated() {
    for model in [NANO, LITE, PRO] {
        let mut j = job();
        j.model = model.into();
        assert!(request::prepare(&j, &Limits::default()).is_ok());
    }
    let mut j = job();
    j.inputs.push(JobInput::Media { role: "reference".into(), mime: "image/jpeg".into(), bytes: Arc::from(jpeg()) });
    j.inputs.swap(0, 1);
    let q = request::prepare(&j, &Limits::default()).unwrap().encode(&crate::secrets::Secret::new("fixture-key").unwrap(), &Limits::default()).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&q.body).unwrap();
    assert_eq!(v["input"][0]["type"], "image");
    assert_eq!(v["input"][1]["type"], "text");
    assert!(q.secret_headers.iter().any(|n| n == "x-goog-api-key"));
    assert!(!format!("{q:?}").contains("fixture-key"));
    for role in ["mask", "system", "assistant", "negative_prompt", &"hostile".repeat(10000)] {
        let mut j = job();
        j.inputs[0] = JobInput::Text { role: role.into(), text: "x".into() };
        let e = request::prepare(&j, &Limits::default()).err().unwrap();
        assert!(e.message.len() < 160);
    }
    let mut j = job();
    j.params.insert("unknown".into(), json!({"nested":["hostile"]}));
    assert!(request::prepare(&j, &Limits::default()).is_err());
    let mut j = job();
    j.inputs.push(j.inputs[0].clone());
    assert!(request::prepare(&j, &Limits::default()).is_err());
}
#[test]
fn json_and_output_admission_have_inclusive_boundaries() {
    let v = success();
    let r = reply(v.clone());
    let n = r.body.len();
    assert!(response::parse(r, &Limits { response_bytes: n, ..Limits::default() }).is_ok());
    assert!(response::parse(reply(v.clone()), &Limits { response_bytes: n - 1, ..Limits::default() }).is_err());
    let size = jpeg().len();
    assert!(response::parse(reply(v.clone()), &Limits { output_bytes: size, total_output_bytes: size, ..Limits::default() }).is_ok());
    assert!(response::parse(reply(v.clone()), &Limits { output_bytes: size - 1, ..Limits::default() }).is_err());
    for l in [
        Limits { images: 0, ..Limits::default() },
        Limits { contents: 0, ..Limits::default() },
        Limits { steps: 0, ..Limits::default() },
        Limits { pixels: 3, ..Limits::default() },
        Limits { total_pixels: 3, ..Limits::default() },
        Limits { width: 1, ..Limits::default() },
        Limits { metadata_bytes: 1, ..Limits::default() },
        Limits { encoded_bytes: 1, ..Limits::default() },
        Limits { text_bytes: 0, ..Limits::default() },
        Limits { depth: 1, ..Limits::default() },
    ] {
        let mut v = v.clone();
        v["steps"][0]["content"].as_array_mut().unwrap().push(json!({"type":"text","text":"x"}));
        assert!(response::parse(reply(v), &l).is_err());
    }
    for raw in [
        r#"{"status":"completed","status":"failed","steps":[]}"#,
        r#"{"steps":[],"\u0073teps":[],"status":"completed"}"#,
        r#"{"status":"completed","steps":[]} trailing"#,
    ] {
        assert!(response::parse(HttpResponse { status: 200, headers: vec![], body: raw.as_bytes().to_vec() }, &Limits::default()).is_err());
    }
    let deep = format!("{}0{}", "[".repeat(17), "]".repeat(17));
    assert!(wire::admit(deep.as_bytes(), &Limits::default()).is_err());
}
#[test]
fn non_success_diagnostics_are_static_and_retry_hint_is_bounded() {
    for (status, kind) in [
        (302, ErrorKind::Malformed),
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (403, ErrorKind::Auth),
        (404, ErrorKind::NotFound),
        (408, ErrorKind::Timeout),
        (429, ErrorKind::RateLimited),
        (503, ErrorKind::Busy),
        (500, ErrorKind::Server),
    ] {
        let e = response::parse(
            HttpResponse { status, headers: vec![("retry-after".into(), "2".into())], body: b"secret prompt key".to_vec() },
            &Limits::default(),
        )
        .unwrap_err();
        assert_eq!(e.kind, kind);
        assert_eq!(e.retry_after_ms, Some(2000));
        assert!(!e.message.contains("secret"));
    }
    let e = response::parse(
        HttpResponse { status: 429, headers: vec![("retry-after".into(), "999999999999999999999999".into())], body: vec![] },
        &Limits::default(),
    )
    .unwrap_err();
    assert_eq!(e.retry_after_ms, None);
}
#[test]
fn lower_only_configuration_and_corrupt_last_reference() {
    struct Refuse;
    impl crate::Http for Refuse {
        fn send(&self, _: crate::HttpRequest) -> crate::BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            panic!("no request")
        }
    }
    let key = crate::secrets::Secret::new("fixture").unwrap();
    assert!(GeminiImages::with_limits(key, Arc::new(Refuse), Limits { images: 17, ..Limits::default() }).is_err());
    let mut j = job();
    j.inputs.push(JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(jpeg()) });
    j.inputs.push(JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(b"bad".as_slice()) });
    COPIES.with(|c| c.set((0, 0)));
    assert!(request::prepare(&j, &Limits::default()).is_err());
    assert_eq!(COPIES.with(|c| c.get()), (0, 0));
}

#[test]
fn duplicate_optional_counter_is_unknown_without_erasing_valid_image() {
    let mut v = success();
    v["usage"] = json!({"total_input_tokens":7});
    let raw = serde_json::to_string(&v).unwrap().replace("\"total_input_tokens\":7", "\"total_input_tokens\":7,\"total_input_tokens\":8");
    let got = response::parse(HttpResponse { status: 200, headers: vec![], body: raw.into_bytes() }, &Limits::default()).unwrap();
    assert_eq!(got.outputs.len(), 1);
    assert_eq!(got.usage, crate::Usage::default());
}

#[test]
fn structural_counters_and_reference_counts_have_exact_inclusive_controls() {
    let l = Limits::default();
    for (raw, depth, members, values, strings) in [("{}", 1, 0, 1, 0), ("{\"a\":[\"x\"]}", 3, 1, 3, 2)] {
        let exact = Limits { depth, members, values, strings, ..l };
        assert!(wire::admit(raw.as_bytes(), &exact).is_ok());
        for field in 0..4 {
            let mut low = exact;
            match field {
                0 => low.depth = depth.saturating_sub(1),
                1 => {
                    if members == 0 {
                        continue;
                    }
                    low.members = members - 1;
                }
                2 => low.values = values - 1,
                _ => {
                    if strings == 0 {
                        continue;
                    }
                    low.strings = strings - 1;
                }
            }
            assert!(wire::admit(raw.as_bytes(), &low).is_err());
        }
    }
    let mut j = job();
    let image = JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(jpeg()) };
    for _ in 0..14 {
        j.inputs.push(image.clone());
    }
    assert!(request::prepare(&j, &l).is_ok());
    j.inputs.push(image);
    assert!(request::prepare(&j, &l).is_err());
    let mut j = job();
    j.inputs[0] = JobInput::Text { role: "prompt".into(), text: "éé".into() };
    let exact = Limits { prompt_chars: 2, prompt_bytes: 4, ..l };
    assert!(request::prepare(&j, &exact).is_ok());
    assert!(request::prepare(&j, &Limits { prompt_chars: 1, ..exact }).is_err());
    assert!(request::prepare(&j, &Limits { prompt_bytes: 3, ..exact }).is_err());
}

#[test]
fn delivery_scope_taints_before_transport_constructor_and_local_refusal_is_positive() {
    use crate::Provider;
    use futures::executor::block_on;
    struct Construct(std::sync::atomic::AtomicUsize);
    impl crate::Http for Construct {
        fn send(&self, _: crate::HttpRequest) -> crate::BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            panic!("redirect-capable fallback forbidden")
        }
        fn send_no_redirect(&self, _: crate::HttpRequest) -> crate::BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(futures::future::pending())
        }
    }
    let http = Arc::new(Construct(std::sync::atomic::AtomicUsize::new(0)));
    let p = GeminiImages::new(crate::secrets::Secret::new("fixture").unwrap(), http.clone());
    let mut bad = job();
    bad.model = "retired-imagen".into();
    let scope = crate::SubmitContext::new();
    assert!(block_on(p.submit_with_delivery(&bad, &scope)).is_err());
    assert!(scope.finish_error().is_some());
    assert_eq!(http.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    let scope = crate::SubmitContext::new();
    let j = job();
    let mut work = p.submit_with_delivery(&j, &scope);
    let w = futures::task::noop_waker();
    assert!(work.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_pending());
    drop(work);
    assert!(scope.finish_error().is_none());
    assert_eq!(http.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!p.retry_ambiguous_submit());
    let handle = crate::JobHandle { remote_id: "unused".into(), data: serde_json::Value::Null };
    assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(block_on(p.cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
}

#[test]
fn numeric_metadata_is_admitted_before_json_number_parsing() {
    let l = Limits { metadata_bytes: 8, ..Limits::default() };
    assert!(wire::admit(b"12345678", &l).is_ok());
    assert!(wire::admit(b"123456789", &l).is_err());
}
