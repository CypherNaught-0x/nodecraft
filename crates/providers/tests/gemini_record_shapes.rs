#![cfg(feature = "gemini-images")]
use nodecraft_providers::{
    BoxFuture, Http, HttpRequest, HttpResponse, JobInput, Provider, ProviderError, ProviderJob, Submission, Usage,
    gemini_images::{GeminiImages, LITE},
    secrets::Secret,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Canned {
    body: Vec<u8>,
    calls: AtomicUsize,
}
impl Http for Canned {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect fallback forbidden")
    }
    fn send_no_redirect(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(HttpResponse { status: 200, headers: vec![], body: self.body.clone() }) })
    }
}
fn fixture() -> Value {
    serde_json::from_slice(include_bytes!("fixtures/gemini_images/success.json")).unwrap()
}
fn run(v: Value) -> Result<nodecraft_providers::Completed, ProviderError> {
    run_raw(serde_json::to_vec(&v).unwrap())
}
fn run_raw(body: Vec<u8>) -> Result<nodecraft_providers::Completed, ProviderError> {
    let h = Arc::new(Canned { body, calls: AtomicUsize::new(0) });
    let p = GeminiImages::new(Secret::new("public-fixture-key").unwrap(), h.clone());
    let j = ProviderJob {
        model: LITE.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "public shape probe".into() }],
        idempotency_key: "review-shape".into(),
    };
    let got = futures::executor::block_on(p.submit(&j));
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    got.map(|s| match s {
        Submission::Finished(c) => c,
        _ => panic!("synchronous"),
    })
}
#[test]
fn review_object_control() {
    let c = run(fixture()).unwrap();
    assert_eq!(c.usage.input_tokens, Some(7));
    assert_eq!(c.usage.output_tokens, Some(11));
    assert_eq!(c.outputs.len(), 1);
}
#[test]
fn review_usage_sequence_must_be_unknown() {
    let mut v = fixture();
    v["usage"] = json!([7, 11, 18, null, null, null, null, null]);
    let c = run(v).unwrap();
    assert_eq!(c.outputs.len(), 1);
    assert_eq!(c.usage, Usage::default());
}
#[test]
fn review_modality_sequence_must_be_unknown() {
    let mut v = fixture();
    v["usage"] = json!({"total_input_tokens":7,"total_output_tokens":11,"input_tokens_by_modality":[["text",7]]});
    let c = run(v).unwrap();
    assert_eq!(c.outputs.len(), 1);
    assert_eq!(c.usage, Usage::default());
}
#[test]
fn review_root_sequence_must_refuse() {
    let v = fixture();
    let got = run(json!(["completed", v["steps"], null]));
    assert!(got.is_err(), "positional root accepted: {got:?}");
}
#[test]
fn review_step_sequence_must_refuse() {
    let mut v = fixture();
    v["steps"] = json!([["model_output", v["steps"][0]["content"], []]]);
    let got = run(v);
    assert!(got.is_err(), "positional step accepted: {got:?}");
}
#[test]
fn review_content_sequence_must_refuse() {
    let mut v = fixture();
    let data = v["steps"][0]["content"][0]["data"].clone();
    v["steps"][0]["content"] = json!([["image", data, "image/jpeg", null, null]]);
    let got = run(v);
    assert!(got.is_err(), "positional content accepted: {got:?}");
}

#[test]
fn required_records_reject_null_and_missing_fields() {
    for v in [Value::Null, json!({}), json!({"steps":fixture()["steps"]}), json!({"status":"completed"})] {
        assert_eq!(run(v).unwrap_err().kind, nodecraft_providers::ErrorKind::Malformed);
    }
    for part in [Value::Null, json!({}), json!({"content":fixture()["steps"][0]["content"]})] {
        let mut v = fixture();
        v["steps"] = json!([part]);
        assert_eq!(run(v).unwrap_err().kind, nodecraft_providers::ErrorKind::Malformed);
    }
    for part in [Value::Null, json!({}), json!({"data":fixture()["steps"][0]["content"][0]["data"],"mime_type":"image/jpeg"})] {
        let mut v = fixture();
        v["steps"][0]["content"] = json!([part]);
        assert_eq!(run(v).unwrap_err().kind, nodecraft_providers::ErrorKind::Malformed);
    }
}
#[test]
fn optional_object_records_preserve_identical_paid_images_or_all_unknown() {
    let expected = run(fixture()).unwrap().outputs;
    for usage in [
        Value::Null,
        json!({}),
        json!({"total_input_tokens":7,"total_output_tokens":11,"input_tokens_by_modality":[null]}),
        json!({"total_input_tokens":7,"total_output_tokens":11,"input_tokens_by_modality":[{}]}),
        json!({"total_input_tokens":7,"total_output_tokens":11,"output_tokens_by_modality":[["image",11]]}),
    ] {
        let mut v = fixture();
        v["usage"] = usage;
        let c = run(v).unwrap();
        assert_eq!(c.outputs, expected);
        assert_eq!(c.usage, Usage::default());
    }
    let mut v = fixture();
    v.as_object_mut().unwrap().remove("usage");
    let c = run(v).unwrap();
    assert_eq!(c.outputs, expected);
    assert_eq!(c.usage, Usage::default());
    let mut v = fixture();
    v["usage"]["input_tokens_by_modality"] = json!([{"modality":"text","tokens":7}]);
    v["usage"]["output_tokens_by_modality"] = json!([{"modality":"image","tokens":11}]);
    let c = run(v).unwrap();
    assert_eq!(c.outputs, expected);
    assert_eq!(c.usage.input_tokens, Some(7));
    assert_eq!(c.usage.output_tokens, Some(11));
}
#[test]
fn duplicate_required_fields_refuse_but_duplicate_optional_fields_are_unknown() {
    let raw = serde_json::to_string(&fixture()).unwrap();
    for s in [
        raw.replace("\"status\":\"completed\"", "\"status\":\"completed\",\"status\":\"failed\""),
        raw.replace("\"type\":\"model_output\"", "\"type\":\"model_output\",\"type\":\"model_output\""),
        raw.replace("\"type\":\"image\"", "\"type\":\"image\",\"type\":\"image\""),
    ] {
        assert_eq!(run_raw(s.into_bytes()).unwrap_err().kind, nodecraft_providers::ErrorKind::Malformed);
    }
    let expected = run(fixture()).unwrap().outputs;
    let duplicate = raw.replace("\"total_input_tokens\":7", "\"total_input_tokens\":7,\"total_input_tokens\":8");
    let c = run_raw(duplicate.into_bytes()).unwrap();
    assert_eq!(c.outputs, expected);
    assert_eq!(c.usage, Usage::default());
    let mut v = fixture();
    v["usage"]["input_tokens_by_modality"] = json!([{"modality":"text","tokens":7}]);
    let duplicate = serde_json::to_string(&v).unwrap().replace("\"tokens\":7", "\"tokens\":7,\"tokens\":8");
    let c = run_raw(duplicate.into_bytes()).unwrap();
    assert_eq!(c.outputs, expected);
    assert_eq!(c.usage, Usage::default());
}
#[test]
fn thought_summary_records_are_map_only_too() {
    let mut v = fixture();
    v["steps"].as_array_mut().unwrap().insert(0, json!({"type":"thought","summary":[["text",null,null,"private",null]]}));
    assert_eq!(run(v).unwrap_err().kind, nodecraft_providers::ErrorKind::Malformed);
}

#[test]
fn non_utf8_reply_refuses_without_a_completion() {
    let error = run_raw(vec![0xff]).unwrap_err();
    assert_eq!(error.kind, nodecraft_providers::ErrorKind::Malformed);
    assert!(!error.message.contains("public-fixture-key"));
}
