//! Public Ideogram V3-on-fal contract, exercised without provider calls.
use super::tests::{handle, job, png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::json;
use std::{collections::VecDeque, sync::Mutex};

const MODEL: &str = "fal-ai/ideogram/v3";

#[test]
fn ideogram34_balanced_request_has_one_known_price_and_no_hidden_overrides() {
    let mut job = job(MODEL);
    job.params.insert("seed".into(), json!(42));
    let (_, bytes) = request::prepare(&job, Limits::default()).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        json!({
            "prompt":"A blue circle", "image_size":"square_hd", "rendering_speed":"BALANCED",
            "num_images":1, "expand_prompt":true, "sync_mode":false, "style":"AUTO", "seed":42
        })
    );
}

const KEY: &str = "ideogram34-public-fixture-not-a-key";
#[derive(Default)]
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<HttpResponse>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable transport refused")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let response = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { Ok(response) })
    }
}
fn reply(value: serde_json::Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() }
}
fn provider(wire: &Arc<Wire>) -> Fal {
    Fal::new(Secret::new(KEY).unwrap(), wire.clone())
}

#[test]
fn ideogram34_bad_roles_parameters_and_escaped_extent_refuse_before_delivery() {
    let mut invalid_jobs = Vec::new();
    for (key, value) in [
        ("rendering_speed", json!("QUALITY")),
        ("num_images", json!(4)),
        ("sync_mode", json!(true)),
        ("image_size", json!({"width":8192})),
        ("style", json!("CUSTOM")),
        ("expand_prompt", json!("true")),
        ("seed", json!(-1)),
        ("seed", json!(2147483648u64)),
        ("seed", json!(1.5)),
        ("seed", json!({"nested":[]})),
    ] {
        let mut j = job(MODEL);
        j.params.insert(key.into(), value);
        invalid_jobs.push(j);
    }
    let mut j = job(MODEL);
    j.inputs.push(j.inputs[0].clone());
    invalid_jobs.push(j);
    let mut j = job(MODEL);
    j.inputs.clear();
    invalid_jobs.push(j);
    let mut j = job(MODEL);
    j.inputs = vec![crate::JobInput::Text { role: "prompt".into(), text: " ".into() }];
    invalid_jobs.push(j);
    let mut j = job(MODEL);
    j.inputs.push(crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png(2, 2).into() });
    invalid_jobs.push(j);
    let mut j = job(MODEL);
    j.inputs.push(crate::JobInput::Text { role: "unknown".into(), text: "ignored?".into() });
    invalid_jobs.push(j);
    let mut j = job(MODEL);
    j.inputs = vec![crate::JobInput::Text { role: "prompt".into(), text: "x".repeat(Limits::HARD.text_bytes) }];
    invalid_jobs.push(j);
    let wire = Arc::new(Wire::default());
    for j in invalid_jobs {
        assert_eq!(block_on(provider(&wire).submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    }
    assert!(wire.requests.lock().unwrap().is_empty());
    let mut j = job(MODEL);
    j.inputs.push(crate::JobInput::Text { role: "negative_prompt".into(), text: "\"\n\\".repeat(64) });
    let (_, body) = request::prepare(&j, Limits::HARD).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: body.len(), ..Limits::HARD }).is_ok());
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Limits::HARD }).is_err());
}

#[test]
fn ideogram34_presets_and_controls_follow_the_documented_schema() {
    for size in ["square_hd", "square", "portrait_4_3", "portrait_16_9", "landscape_4_3", "landscape_16_9"] {
        for style in ["AUTO", "GENERAL", "REALISTIC", "DESIGN"] {
            let mut j = job(MODEL);
            j.params = serde_json::from_value(json!({"image_size":size,"style":style,"expand_prompt":false,"seed":2147483647})).unwrap();
            j.inputs.push(crate::JobInput::Text { role: "negative_prompt".into(), text: "No border".into() });
            let (_, body) = request::prepare(&j, Limits::HARD).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["image_size"], size);
            assert_eq!(body["style"], style);
            assert_eq!(body["negative_prompt"], "No border");
            assert_eq!(body["expand_prompt"], false);
            assert_eq!(body["rendering_speed"], "BALANCED");
            assert_eq!(body["num_images"], 1);
        }
    }
}

#[test]
fn ideogram34_queue_resume_result_and_cancel_reuse_fal_without_leaking_auth() {
    let image = png(2, 2);
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            reply(receipt(MODEL)),
            reply(json!({"status":"IN_PROGRESS"})),
            reply(json!({"status":"COMPLETED"})),
            reply(json!({"images":[{"url":"https://v3.fal.media/files/public/ideogram.png","file_size":image.len()}],"seed":42})),
            HttpResponse { status: 200, headers: vec![], body: image.clone() },
            HttpResponse { status: 202, headers: vec![], body: serde_json::to_vec(&json!({"status":"CANCELLATION_REQUESTED"})).unwrap() },
        ])),
        ..Default::default()
    });
    let f = provider(&wire);
    let Submission::Queued(h) = block_on(f.submit(&job(MODEL))).unwrap() else { panic!() };
    assert_eq!(h, handle(MODEL));
    let restored: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
    assert!(matches!(block_on(f.poll(&restored)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(f.poll(&restored)).unwrap() else { panic!() };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: image.into() }]);
    assert!(done.usage.cost.is_none(), "catalog estimate is not an observed invoice");
    block_on(f.cancel(&restored)).unwrap();
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests[0].url, format!("{ORIGIN}/{MODEL}"));
    assert_eq!(requests[1].url, format!("{ORIGIN}/fal-ai/ideogram/requests/r_1/status"));
    assert_eq!(requests[5].url, format!("{ORIGIN}/fal-ai/ideogram/requests/r_1/cancel"));
    assert_eq!(requests[5].method, HttpMethod::Put);
    assert!(requests[4].headers.is_empty());
    for request in requests.iter().take(4) {
        assert!(request.headers.iter().any(|(name, value)| name == "Authorization" && value == &format!("Key {KEY}")));
    }
    assert!(requests[0].headers.iter().any(|(name, value)| name == "X-Fal-No-Retry" && value == "1"));
    assert!(!f.retry_ambiguous_submit());
}

#[test]
fn ideogram34_malformed_results_and_forged_receipts_never_authorize_downloads() {
    let key = Secret::new(KEY).unwrap();
    let h = handle(MODEL);
    let record = Record::read(&h, &key).unwrap().0;
    for result in [
        json!({"images":[]}),
        json!({"images":[{"url":"https://evil.example/file"}]}),
        json!({"images":[{"url":"https://fal.media/a"},{"url":"https://fal.media/b"}]}),
        json!({"images":[{"url":"https://fal.media/a","width":5000}]}),
        json!({"images":[{"url":"https://fal.media/a","content_type":"video/mp4"}]}),
        json!({"images":[{"url":"https://fal.media/a"}],"video":{}}),
    ] {
        assert!(response::outputs(&serde_json::to_vec(&result).unwrap(), record, Limits::HARD, &key).is_err());
    }
    let mut forged = h.clone();
    forged.data["requested"] = json!(2);
    assert!(Record::read(&forged, &key).is_err());
    let mut forged = h;
    forged.data["status_url"] = json!("https://queue.fal.run/fal-ai/flux/requests/r_1/status");
    assert!(Record::read(&forged, &key).is_err());
    assert!(media::validate(b"not an image", record, Limits::HARD, &mut 0).is_err());
    assert!(media::validate(&png(2, 2), record, Limits { output_bytes: 1, ..Limits::HARD }, &mut 0).is_err());
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid live smoke: NODECRAFT_LIVE_IDEOGRAM=1 and FAL_KEY; one Balanced image (dated 0.06 USD)"]
fn ideogram34_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_IDEOGRAM").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("FAL_KEY").expect("FAL_KEY required for explicitly enabled smoke")).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let f = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let Submission::Queued(h) = f.submit(&job(MODEL)).await.unwrap() else { panic!() };
        for _ in 0..120 {
            match f.poll(&h).await.unwrap() {
                JobProgress::Done(done) => {
                    assert_eq!(done.outputs.len(), 1);
                    return;
                }
                JobProgress::Failed(error) => panic!("{}", error.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            }
        }
        let _ = f.cancel(&h).await;
        panic!("live Ideogram request did not finish before the smoke deadline");
    });
}
