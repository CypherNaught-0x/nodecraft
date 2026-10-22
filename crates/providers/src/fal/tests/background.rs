//! Original offline contracts for the image-only, fixed-price Bria mode (#749).
use super::*;

fn accepted() -> HttpResponse {
    reply(json!({"request_id":"r_1","status":"IN_QUEUE",
        "status_url":"https://queue.fal.run/fal-ai/bria/requests/r_1/status",
        "response_url":"https://queue.fal.run/fal-ai/bria/requests/r_1",
        "cancel_url":"https://queue.fal.run/fal-ai/bria/requests/r_1/cancel"}))
}
fn cutout() -> (Vec<u8>, Vec<u8>) {
    let rgba = vec![255, 0, 0, 0, 0, 255, 0, 127, 0, 0, 255, 255, 255, 255, 255, 255];
    (nodecraft_media::encode_png(2, 2, &rgba, &nodecraft_media::Limits::default()).unwrap(), rgba)
}
fn descriptor(size: usize) -> HttpResponse {
    reply(json!({"image":{"url":"https://v3.fal.media/files/public/cutout.png",
        "content_type":"image/png","file_size":size,"file_name":null,"width":2,"height":2}}))
}

#[test]
fn catalog_exposes_exactly_one_image_role_and_the_image_only_price() {
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    let model = catalog.get("fal.bria-rmbg").unwrap();
    assert_eq!((model.provider.as_str(), model.provider_model.as_str()), ("fal", BRIA_REMOVE));
    assert_eq!(model.quote(&Default::default()).unwrap().unwrap().micros, 18_000);
    let [mode] = model.modes.as_slice() else { panic!("one image-only mode") };
    assert_eq!(mode.id, "remove-background");
    assert_eq!(mode.output, nodecraft_types::Modality::Image);
    let [role] = mode.inputs.as_slice() else { panic!("one required image") };
    assert_eq!((role.role.as_str(), role.modality, role.min, role.max, role.inline), ("image", nodecraft_types::Modality::Image, 1, 1, false));
    assert!(model.params.resolve(&Default::default(), false).unwrap().visible_values().next().is_none());
}

#[test]
fn original_image_wire_resume_and_transparent_png_are_preserved() {
    use base64::Engine;
    let (png, rgba) = cutout();
    let transport = Transport::new(vec![
        accepted(),
        reply(json!({"status":"IN_PROGRESS"})),
        reply(json!({"status":"COMPLETED"})),
        descriptor(png.len()),
        bytes(png.clone()),
    ]);
    let provider = Fal::new(key(), transport.clone());
    let Submission::Queued(handle) = block_on(provider.submit(&job(BRIA_REMOVE))).unwrap() else { panic!("a resumable queue receipt") };
    let restored: JobHandle = serde_json::from_str(&serde_json::to_string(&handle).unwrap()).unwrap();
    assert_eq!(handle.data["status_url"], "https://queue.fal.run/fal-ai/bria/requests/r_1/status");
    assert!(matches!(block_on(provider.poll(&restored)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(provider.poll(&restored)).unwrap() else { panic!("one completed cutout") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: png.clone().into() }]);
    let (_, decoded) = nodecraft_media::decode_image(&png, &Limits::HARD.image(false)).unwrap();
    assert_eq!(decoded.to_rgba8().into_raw(), rgba, "the original alpha channel survives");
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0].url, "https://queue.fal.run/fal-ai/bria/background/remove");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(super::png(2, 2));
    assert_eq!(body, json!({"image_url":format!("data:image/png;base64,{encoded}"),"sync_mode":false}));
    for request in requests.iter().take(4) {
        assert!(request.headers.iter().any(|(name, value)| name == "Authorization" && value == &format!("Key {KEY}")));
    }
    assert!(requests[4].headers.is_empty(), "CDN download has no credential");
    assert!(!provider.retry_ambiguous_submit());
}

#[test]
fn prompt_extra_inputs_and_parameters_refuse_before_codecs_encoding_or_http() {
    let mut cases = Vec::new();
    let mut missing = job(BRIA_REMOVE);
    missing.inputs.clear();
    cases.push(missing);
    let mut duplicate = job(BRIA_REMOVE);
    duplicate.inputs.push(duplicate.inputs[0].clone());
    cases.push(duplicate);
    let mut prompt = job(BRIA_REMOVE);
    prompt.inputs.push(crate::JobInput::Text { role: "prompt".into(), text: "guided removal costs more".into() });
    cases.push(prompt);
    for (name, value) in [("prompt", json!("guided")), ("seed", json!(1)), ("sync_mode", json!(true)), ("output_format", json!("jpeg"))] {
        let mut invalid = job(BRIA_REMOVE);
        invalid.params.insert(name.into(), value);
        cases.push(invalid);
    }
    for invalid in cases {
        reset();
        let transport = Transport::new(vec![]);
        assert!(block_on(Fal::new(key(), transport.clone()).submit(&invalid)).is_err());
        assert!(transport.requests.lock().unwrap().is_empty());
        assert_eq!(stages(), [0, 0, 0]);
    }
}

#[test]
fn exact_request_and_lower_output_geometry_refuse_before_paid_dispatch() {
    let job = job(BRIA_REMOVE);
    let (_, body) = request::prepare(&job, Limits::HARD).unwrap();
    assert!(request::prepare(&job, Limits { request_bytes: body.len(), ..Limits::HARD }).is_ok());
    for limits in [
        Limits { request_bytes: body.len() - 1, ..Limits::HARD },
        Limits { output_width: 1, ..Limits::HARD },
        Limits { output_height: 1, ..Limits::HARD },
        Limits { total_pixels: 3, ..Limits::HARD },
    ] {
        reset();
        let transport = Transport::new(vec![]);
        let provider = Fal::with_limits(key(), transport.clone(), limits).unwrap();
        assert!(block_on(provider.submit(&job)).is_err());
        assert!(transport.requests.lock().unwrap().is_empty());
        assert_eq!(stages(), [0, 0, 0]);
    }
}

#[test]
fn wrong_envelopes_types_origins_and_actual_geometry_never_publish_a_cutout() {
    for value in [
        json!({"image":[]}),
        json!({"image":{"url":"https://evil.test/image.png"}}),
        json!({"image":{"url":"https://fal.media/image.png","content_type":"image/jpeg"}}),
        json!({"image":{"url":"https://fal.media/image.png","width":4097}}),
        json!({"image":{"url":"https://fal.media/image.png"},"images":[]}),
    ] {
        let transport = Transport::new(vec![reply(json!({"status":"COMPLETED"})), reply(value)]);
        assert!(block_on(Fal::new(key(), transport.clone()).poll(&handle(BRIA_REMOVE))).is_err());
        assert_eq!(transport.requests.lock().unwrap().len(), 2, "descriptor refusal precedes CDN fetch");
    }
    for png in [super::png(3, 2), {
        let mut png = cutout().0;
        png.push(0);
        png
    }] {
        let transport = Transport::new(vec![reply(json!({"status":"COMPLETED"})), descriptor(png.len()), bytes(png)]);
        assert!(block_on(Fal::new(key(), transport).poll(&handle(BRIA_REMOVE))).is_err());
    }
}

#[test]
fn cancel_and_hostile_restored_handles_use_only_the_allowlisted_bria_root() {
    let transport = Transport::new(vec![HttpResponse { status: 202, ..reply(json!({"status":"CANCELLATION_REQUESTED"})) }]);
    assert!(block_on(Fal::new(key(), transport.clone()).cancel(&handle(BRIA_REMOVE))).is_ok());
    let requests = transport.requests.lock().unwrap();
    assert_eq!((requests[0].method, requests[0].url.as_str()), (HttpMethod::Put, "https://queue.fal.run/fal-ai/bria/requests/r_1/cancel"));
    drop(requests);
    for (name, value) in [
        ("width", json!(0)),
        ("height", json!(1025)),
        ("requested", json!(2)),
        ("cancel_url", json!("https://queue.fal.run/fal-ai/bria/background/remove/requests/r_1/cancel")),
    ] {
        let mut invalid = handle(BRIA_REMOVE);
        invalid.data[name] = value;
        let transport = Transport::new(vec![]);
        assert!(block_on(Fal::new(key(), transport.clone()).cancel(&invalid)).is_err());
        assert!(transport.requests.lock().unwrap().is_empty());
    }
}
