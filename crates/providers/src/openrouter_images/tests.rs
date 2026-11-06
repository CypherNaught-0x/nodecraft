use super::*;
use crate::{HttpRequest, HttpResponse, JobInput, Output};
use base64::Engine;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Mutex;

const KEY: &str = "public-openrouter-fixture";
const MODEL: &str = "tencent/hy-image-v3.5-preview";

struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<HttpResponse>>,
}

impl Wire {
    fn new(replies: impl IntoIterator<Item = HttpResponse>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::default(), replies: Mutex::new(replies.into_iter().collect()) })
    }
    fn sent(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("the adapter must not follow redirects")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("unexpected request");
        Box::pin(async move { Ok(reply) })
    }
}

fn png(side: u32) -> Vec<u8> {
    let n = (side * side * 4) as usize;
    nodecraft_media::encode_png(side, side, &vec![200; n], &nodecraft_media::Limits::default()).unwrap()
}

fn reply(status: u16, value: Value) -> HttpResponse {
    HttpResponse { status, headers: vec![], body: serde_json::to_vec(&value).unwrap() }
}

fn image_reply(bytes: &[u8], cost: Value) -> HttpResponse {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    reply(200, json!({"created": 1, "data": [{"b64_json": data, "media_type": "image/png"}], "usage": {"cost": cost}}))
}

fn job(params: Value, inputs: Vec<JobInput>) -> ProviderJob {
    ProviderJob { model: MODEL.into(), params: params.as_object().unwrap().clone(), inputs, idempotency_key: "public-key".into() }
}

fn prompt(text: &str) -> JobInput {
    JobInput::Text { role: "prompt".into(), text: text.into() }
}

fn provider(wire: Arc<Wire>) -> OpenRouterImages {
    OpenRouterImages::new(Secret::new(KEY).unwrap(), wire)
}

fn body(request: &HttpRequest) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

#[test]
fn sends_the_prompt_and_settings_and_returns_the_image_with_its_cost() {
    let image = png(8);
    let wire = Wire::new([image_reply(&image, json!(0.0016))]);
    let adapter = provider(wire.clone());
    let job = job(json!({"resolution": "1K", "aspect_ratio": "16:9", "seed": 7}), vec![prompt("a lighthouse at dusk")]);
    let Submission::Finished(done) = block_on(adapter.submit(&job)).unwrap() else { panic!("synchronous") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(image) }]);
    assert_eq!(done.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1600)));
    let sent = wire.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, API);
    assert_eq!(body(&sent[0]), json!({"model": MODEL, "prompt": "a lighthouse at dusk", "resolution": "1K", "aspect_ratio": "16:9", "seed": 7}));
    assert!(format!("{:?}", sent[0]).find(KEY).is_none(), "the key never shows in debug output");
    assert!(!adapter.retry_ambiguous_submit(), "no idempotency: an uncertain submit is terminal");
}

#[test]
fn reference_images_go_as_data_urls() {
    let reference = png(4);
    let wire = Wire::new([image_reply(&png(8), json!(null))]);
    let job =
        job(json!({}), vec![prompt("make it blue"), JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(reference.clone()) }]);
    let Submission::Finished(done) = block_on(provider(wire.clone()).submit(&job)).unwrap() else { panic!("synchronous") };
    assert_eq!(done.usage.cost, None, "no reported cost stays unknown, never zero");
    let sent = body(&wire.sent()[0]);
    let url = sent["input_references"][0]["image_url"]["url"].as_str().unwrap();
    assert_eq!(url, format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&reference)));
    assert_eq!(sent["input_references"][0]["type"], "image_url");
}

#[test]
fn refuses_bad_jobs_before_sending_anything() {
    let wire = Wire::new([]);
    let adapter = provider(wire.clone());
    let cases = [
        job(json!({}), vec![]),
        job(json!({"steps": 4}), vec![prompt("x")]),
        job(json!({"resolution": "1K\" ,\"n\":9"}), vec![prompt("x")]),
        job(json!({"seed": -1}), vec![prompt("x")]),
        job(json!({"seed": 1e30}), vec![prompt("x")]),
        job(json!({}), vec![prompt("x"), JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(&b"not a png"[..]) }]),
        job(json!({}), vec![prompt("x"), JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(png(4)) }]),
        job(json!({}), vec![prompt("x"), JobInput::Media { role: "mask".into(), mime: "image/png".into(), bytes: Arc::from(png(4)) }]),
        ProviderJob { model: "bad model\"".into(), ..job(json!({}), vec![prompt("x")]) },
    ];
    for case in cases {
        let error = block_on(adapter.submit(&case)).expect_err("refused");
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "{}", error.message);
    }
    assert!(wire.sent().is_empty(), "nothing was sent");
}

#[test]
fn hostile_replies_are_errors_not_panics() {
    let svg = reply(200, json!({"data": [{"b64_json": "PHN2Zz48L3N2Zz4=", "media_type": "image/svg+xml"}]}));
    let replies = [
        reply(502, json!({"error": "upstream"})),
        reply(401, json!({"error": {"message": "bad key"}})),
        reply(200, json!({"data": []})),
        reply(200, json!({"data": [{"b64_json": "!!!"}]})),
        reply(200, json!({"data": [{"b64_json": "aGVsbG8="}]})),
        reply(200, json!({"data": [{}]})),
        reply(200, json!("not an object")),
        HttpResponse { status: 200, headers: vec![], body: b"{\"data\":[".to_vec() },
        svg,
    ];
    let count = replies.len();
    let wire = Wire::new(replies);
    let adapter = provider(wire.clone());
    for _ in 0..count {
        let error = block_on(adapter.submit(&job(json!({}), vec![prompt("x")]))).expect_err("an error");
        assert!(!error.message.contains(KEY));
    }
    assert_eq!(wire.sent().len(), count);
}

#[test]
fn a_502_says_it_was_not_billed_and_a_cost_is_rounded_up() {
    let error = response::parse(&reply(502, json!({})), Limits::HARD).err().unwrap();
    assert!(error.message.contains("not billed"));
    assert_eq!(response::usd(0.0000001), Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1)));
    assert_eq!(response::usd(f64::NAN), None);
    assert_eq!(response::usd(-1.0), None);
    assert_eq!(response::usd(f64::INFINITY), None);
}

#[test]
fn an_oversized_output_is_refused_before_decoding() {
    let limits = Limits { output_bytes: 64, ..Limits::HARD };
    let big = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 4096]);
    let error = response::parse(&reply(200, json!({"data": [{"b64_json": big}]})), limits).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Malformed);
}

#[test]
fn limits_can_only_be_lowered() {
    let http = Wire::new([]);
    assert!(OpenRouterImages::with_limits(Secret::new(KEY).unwrap(), http.clone(), Limits { output_bytes: 1, ..Limits::HARD }).is_ok());
    assert!(OpenRouterImages::with_limits(Secret::new(KEY).unwrap(), http, Limits { output_bytes: usize::MAX, ..Limits::HARD }).is_err());
}
