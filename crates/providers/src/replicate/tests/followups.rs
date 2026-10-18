use super::*;

#[test]
fn before_delivery_has_no_authorization() {
    let (p, m) = fixture(vec![
        prediction(SDXL, "starting", serde_json::Value::Null),
        prediction(SDXL, "succeeded", serde_json::json!(["https://sub.replicate.delivery/a"])),
        bytes(png(1, 1)),
    ]);
    let h = handle(&p, &job(SDXL));
    assert!(block_on(p.poll(&h)).is_ok());
    let seen = m.seen.lock().unwrap();
    let download = seen.last().unwrap();
    assert!(!download.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization")), "delivery must not carry API Authorization");
}
#[test]
fn before_rejected_accepted_create_requests_one_cancel_and_retains_id() {
    let mut reply = prediction(SDXL, "processing", serde_json::Value::Null);
    let mut v: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    v["version"] = serde_json::json!("hidden");
    reply.body = serde_json::to_vec(&v).unwrap();
    let (p, m) = fixture(vec![reply, HttpResponse { status: 503, headers: vec![], body: b"private ignored".to_vec() }]);
    let e = block_on(p.submit(&job(SDXL))).unwrap_err();
    let seen = m.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "accepted rejection must attempt bounded cancel");
    assert!(seen[1].url.ends_with("/public-prediction/cancel"));
    assert!(e.message.contains("public-prediction"));
    assert!(e.message.contains("unconfirmed"));
}
#[test]
fn before_echo_fields_do_not_spend_other_field_budget() {
    let mut v = full_prediction();
    for i in 0..30 {
        v.as_object_mut().unwrap().insert(format!("extra{i}"), serde_json::Value::Null);
    }
    let (p, _) = fixture(vec![bytes(serde_json::to_vec(&v).unwrap())]);
    assert!(block_on(p.submit(&job(SDXL))).is_ok(), "known echo plus bounded other fields must fit separately");
}
#[test]
fn before_download_auth_error_names_output_link() {
    let (p, _) = fixture(vec![
        prediction(SDXL, "starting", serde_json::Value::Null),
        prediction(SDXL, "succeeded", serde_json::json!(["https://replicate.delivery/a"])),
        HttpResponse { status: 403, headers: vec![], body: vec![] },
    ]);
    let h = handle(&p, &job(SDXL));
    let e = block_on(p.poll(&h)).unwrap_err();
    assert!(e.message.contains("output"));
    assert!(!e.to_string().contains("API key"));
}

#[test]
fn field_overflow_receipt_still_cancels_without_application_publication() {
    let mut v = full_prediction();
    for i in 0..44 {
        v.as_object_mut().unwrap().insert(format!("extra{i}"), serde_json::Value::Null);
    }
    let (p, m) = fixture(vec![bytes(serde_json::to_vec(&v).unwrap()), prediction(SDXL, "canceled", serde_json::Value::Null)]);
    reset();
    let e = block_on(p.submit(&job(SDXL))).unwrap_err();
    assert!(e.message.contains("public-prediction"));
    assert!(e.message.contains("terminal status observed"));
    assert_eq!(counts()[5], 0);
    assert_eq!(counts()[6], 0);
    let seen = m.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].timeout_ms, 15_000);
    assert_eq!(seen[1].max_response_bytes, Limits::HARD.response_bytes);
}
#[test]
fn input_field_budget_has_its_own_exact_boundary() {
    for (n, ok) in [(32, true), (33, false)] {
        let mut v = full_prediction();
        v["input"] = serde_json::json!({});
        for i in 0..n {
            v["input"].as_object_mut().unwrap().insert(format!("input{i}"), serde_json::Value::Null);
        }
        let reply = bytes(serde_json::to_vec(&v).unwrap());
        reset();
        assert_eq!(response::parse(&reply, Record { model: Model::Image, requested: 1 }, Limits::HARD).is_ok(), ok);
        assert_eq!(counts()[5], usize::from(ok));
    }
}
#[test]
fn invalid_receipts_do_not_cancel_nested_duplicate_or_malformed_ids() {
    for raw in [
        r#"{"id":"public-prediction","id":"other"}"#,
        r#"{"x":{"id":"other"}}"#,
        r#"{"id":"public-prediction","x":"\uD800"}"#,
        r#"{"id":"public-prediction"} garbage"#,
        r#"{"id":"../escape"}"#,
    ] {
        let (p, m) = fixture(vec![bytes(raw.as_bytes().to_vec())]);
        let e = block_on(p.submit(&job(SDXL))).unwrap_err();
        assert!(!e.message.contains("public-prediction"));
        assert_eq!(m.seen.lock().unwrap().len(), 1);
    }
}
#[test]
fn secret_bearing_created_and_restored_ids_never_enter_urls_or_diagnostics() {
    for id in ["public-fixture-key", "prefix-public-fixture-key-suffix"] {
        for version_bad in [false, true] {
            let mut v = full_prediction();
            v["id"] = serde_json::json!(id);
            if version_bad {
                v["version"] = serde_json::json!("hidden");
            }
            let (p, m) = fixture(vec![bytes(serde_json::to_vec(&v).unwrap())]);
            let e = block_on(p.submit(&job(SDXL))).unwrap_err();
            assert!(!e.to_string().contains("public-fixture-key"));
            assert!(e.message.contains("redacted"));
            let seen = m.seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert!(!seen[0].url.contains("public-fixture-key"));
        }
        let (p, m) = fixture(vec![]);
        let h = Record { model: Model::Image, requested: 1 }.handle(id);
        assert!(block_on(p.poll(&h)).is_err());
        assert!(block_on(p.cancel(&h)).is_err());
        assert!(m.seen.lock().unwrap().is_empty());
    }
}
#[test]
fn hostile_cancel_echo_is_unconfirmed_without_reflection_or_more_requests() {
    let mut bad = full_prediction();
    bad["version"] = serde_json::json!("hidden");
    for reply in [
        serde_json::json!({"id":"public-fixture-key","status":"canceled","logs":"private"}),
        serde_json::json!({"id":"public-prediction","status":"processing"}),
        serde_json::json!({"id":"public-prediction","status":"unknown"}),
    ] {
        let (p, m) = fixture(vec![bytes(serde_json::to_vec(&bad).unwrap()), bytes(serde_json::to_vec(&reply).unwrap())]);
        let e = block_on(p.submit(&job(SDXL))).unwrap_err();
        assert!(e.message.contains("public-prediction"));
        assert!(e.message.contains("unconfirmed"));
        assert!(!e.message.contains("public-fixture-key"));
        assert!(!e.message.contains("private"));
        assert_eq!(m.seen.lock().unwrap().len(), 2);
    }
}
#[test]
fn download_errors_are_sanitized_and_polling_is_two_seconds() {
    let (p, _) = fixture(vec![]);
    assert_eq!(p.poll_interval_ms(&JobProgress::Queued { position: None }), 2000);
    assert_eq!(p.poll_interval_ms(&JobProgress::Running { fraction: None, message: None, partial: None }), 2000);
    for status in [301, 302, 303, 307, 308, 401, 403, 404, 410, 504] {
        let reply = HttpResponse { status, headers: vec![("retry-after".into(), "2".into())], body: b"private signed url".to_vec() };
        let e = response::download_status(&reply, Limits::HARD, 100).unwrap_err();
        assert!(e.message.contains("output"));
        assert!(!e.to_string().contains("API key"));
        assert!(!e.message.contains("private"));
        assert_eq!(e.retry_after_ms, Some(2000));
    }
}
