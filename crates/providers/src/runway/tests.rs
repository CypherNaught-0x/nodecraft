use super::*;
use crate::{JobInput, Usage};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};
const KEY: &str = "runway-public-fixture-not-a-key";
const ID: &str = "17f20503-6c24-4c16-946b-35dbbce2af2f";
const URL: &str = "https://dnznrvs05pmza.cloudfront.net/output.mp4?_jwt=public-fixture";
#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Wire {
    fn new(replies: Vec<Result<HttpResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies.into()), ..Self::default() })
    }
    fn requests(&self) -> Vec<HttpRequest> {
        self.sent.lock().unwrap().clone()
    }
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-following transport forbidden")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.sent.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn key() -> Secret {
    Secret::new(KEY).unwrap()
}
fn reply(status: u16, body: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status, headers: vec![("Retry-After".into(), "7".into())], body: serde_json::to_vec(&body).unwrap() })
}
fn created() -> Result<HttpResponse, ProviderError> {
    reply(200, json!({"id":ID,"estimatedCost":{"credits":60}}))
}
fn task(status: &str) -> Result<HttpResponse, ProviderError> {
    reply(200, json!({"id":ID,"status":status,"progress":0.25}))
}
fn done() -> Result<HttpResponse, ProviderError> {
    reply(200, json!({"id":ID,"status":"SUCCEEDED","output":[URL],"cost":{"credits":60}}))
}
fn job(model: &str) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A paper boat on calm water".into() }],
        idempotency_key: "public-operation".into(),
    }
}
fn image() -> JobInput {
    let bytes = nodecraft_media::encode_png(32, 32, &vec![100; 32 * 32 * 4], &nodecraft_media::Limits::default()).unwrap();
    JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: bytes.into() }
}
fn handle() -> JobHandle {
    JobHandle { remote_id: ID.into(), data: json!({"v":1,"model":GEN4_5,"duration":5}) }
}
fn mp4() -> Vec<u8> {
    let atom = |name: &[u8; 4], data: &[u8]| [u32::try_from(data.len() + 8).unwrap().to_be_bytes().as_slice(), name, data].concat();
    let mut mvhd = vec![0; 100];
    mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&5000u32.to_be_bytes());
    let mut tkhd = vec![0; 84];
    tkhd[76..80].copy_from_slice(&(1280u32 << 16).to_be_bytes());
    tkhd[80..84].copy_from_slice(&(720u32 << 16).to_be_bytes());
    let mut hdlr = vec![0; 24];
    hdlr[8..12].copy_from_slice(b"vide");
    let trak = atom(b"trak", &[atom(b"tkhd", &tkhd), atom(b"mdia", &atom(b"hdlr", &hdlr))].concat());
    [atom(b"ftyp", b"isom\0\0\x02\0isom"), atom(b"moov", &[atom(b"mvhd", &mvhd), trak].concat()), atom(b"mdat", b"fixture")].concat()
}
#[test]
fn runway_routes_text_and_image_with_explicit_priced_duration_and_secret_headers() {
    for model in [GEN4_5, GEN4_TURBO] {
        for framed in [false, true] {
            if model == GEN4_TURBO && !framed {
                continue;
            }
            let http = Wire::new(vec![created()]);
            let provider = Runway::new(key(), http.clone());
            let mut j = job(model);
            j.params.insert("duration_seconds".into(), json!("2"));
            j.params.insert("seed".into(), json!("4294967295"));
            if framed {
                j.inputs.push(image());
            }
            let Submission::Queued(h) = block_on(provider.submit(&j)).unwrap() else { panic!("queued") };
            assert_eq!(h.remote_id, ID);
            assert_eq!(h.data["duration"], 2);
            assert!(!provider.retry_ambiguous_submit());
            let r = http.requests();
            let r = &r[0];
            assert!(r.url.ends_with(if framed { "/image_to_video" } else { "/text_to_video" }));
            assert!(r.secret_headers.contains(&"authorization".into()));
            assert!(r.headers.contains(&("X-Runway-Version".into(), VERSION.into())));
            assert_eq!(r.max_response_bytes, Limits::HARD.response_bytes);
            let body: Value = serde_json::from_slice(&r.body).unwrap();
            assert_eq!(body["duration"], 2);
            assert_eq!(body["seed"], u32::MAX);
            assert_eq!(body["model"], model);
            assert_eq!(body["ratio"], "1280:720");
            assert_eq!(body.get("promptImage").is_some(), framed);
            assert_eq!(body.get("outputFormat").is_some(), framed && model == GEN4_5);
            if framed && model == GEN4_5 {
                assert_eq!(body["outputFormat"], "mp4");
            }
            assert!(!String::from_utf8(r.body.clone()).unwrap().contains(KEY));
        }
    }
}
#[test]
fn runway_refuses_invalid_inputs_before_delivery_and_counts_utf16() {
    let mut cases = vec![job("unknown"), job(GEN4_TURBO)];
    for (name, value) in [
        ("duration_seconds", json!(1)),
        ("duration_seconds", json!(11)),
        ("duration_seconds", json!("+5")),
        ("seed", json!(4294967296u64)),
        ("ratio", json!("960:960")),
        ("outputFormat", json!("prores")),
    ] {
        let mut j = job(GEN4_5);
        j.params.insert(name.into(), value);
        cases.push(j);
    }
    let mut long = job(GEN4_5);
    long.inputs = vec![JobInput::Text { role: "prompt".into(), text: "😀".repeat(501) }];
    cases.push(long);
    let mut duplicate = job(GEN4_5);
    duplicate.inputs.push(duplicate.inputs[0].clone());
    cases.push(duplicate);
    let mut corrupt = job(GEN4_5);
    corrupt.inputs.push(JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: Arc::from(b"bad".as_slice()) });
    cases.push(corrupt);
    for j in cases {
        let http = Wire::new(vec![]);
        let p = Runway::new(key(), http.clone());
        let scope = SubmitContext::new();
        assert_eq!(block_on(p.submit_with_delivery(&j, &scope)).unwrap_err().kind, ErrorKind::UnsupportedInput);
        assert!(scope.finish_error().is_some());
        assert!(http.requests().is_empty());
    }
    let mut good = job(GEN4_5);
    good.inputs = vec![JobInput::Text { role: "prompt".into(), text: "😀".repeat(500) }];
    assert!(request::prepare(&good, Limits::HARD).is_ok());
}
#[test]
fn runway_states_and_reported_cost_are_bounded_and_downloads_never_receive_credentials() {
    let http = Wire::new(vec![task("PENDING"), task("THROTTLED"), task("RUNNING"), done(), Ok(HttpResponse { status: 200, headers: vec![], body: mp4() })]);
    let p = Runway::new(key(), http.clone());
    let h = handle();
    for _ in 0..2 {
        assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Queued { .. }));
    }
    let progress = block_on(p.poll(&h)).unwrap();
    assert!(matches!(progress, JobProgress::Running { fraction: Some(0.25), .. }));
    assert_eq!(p.poll_interval_ms(&progress), 5000);
    let JobProgress::Done(done) = block_on(p.poll(&h)).unwrap() else { panic!("done") };
    assert_eq!(done.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 600_000)));
    assert!(matches!(done.outputs.as_slice(),[Output::Bytes {mime,..}] if mime=="video/mp4"));
    let requests = http.requests();
    let download = requests.last().unwrap();
    assert_eq!(download.url, URL);
    assert!(download.secret_headers.is_empty());
    assert!(!download.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization")));
}

#[test]
fn runway_signed_credit_adjustments_preserve_successful_video_downloads() {
    for (credits, cost) in [(-9_007_199_254_740_991i64, None), (-1, None), (0, Some(0)), (60, Some(600_000)), (9_007_199_254_740_991, None)] {
        let http = Wire::new(vec![
            reply(200, json!({"id":ID,"status":"SUCCEEDED","output":[URL],"cost":{"credits":credits}})),
            Ok(HttpResponse { status: 200, headers: vec![], body: mp4() }),
        ]);
        let JobProgress::Done(done) = block_on(Runway::new(key(), http.clone()).poll(&handle())).unwrap() else { panic!("successful video") };
        assert_eq!(done.usage.cost, cost.map(|n| nodecraft_types::Money::new(nodecraft_types::Currency::USD, n)));
        assert!(matches!(done.outputs.as_slice(),[Output::Bytes {mime,..}] if mime=="video/mp4"));
        assert_eq!(http.requests().len(), 2, "billing metadata must not discard the download");
    }
}
#[test]
fn runway_failures_do_not_echo_provider_text_or_claim_unproved_nonbilling() {
    for (status, kind) in [
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (403, ErrorKind::Auth),
        (404, ErrorKind::NotFound),
        (408, ErrorKind::Timeout),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Server),
        (503, ErrorKind::Busy),
        (302, ErrorKind::Malformed),
    ] {
        let http = Wire::new(vec![reply(status, json!({"error":KEY}))]);
        let p = Runway::new(key(), http);
        let scope = SubmitContext::new();
        let e = block_on(p.submit_with_delivery(&job(GEN4_5), &scope)).unwrap_err();
        assert_eq!(e.kind, kind);
        assert!(!e.message.contains(KEY));
        assert_eq!(e.retry_after_ms, Some(7000));
        assert!(scope.finish_error().is_none());
    }
    for (code, kind) in [
        ("SAFETY.INPUT.TEXT", ErrorKind::BlockedContent),
        ("INPUT_PREPROCESSING.SAFETY.TEXT", ErrorKind::BlockedContent),
        ("ASSET.INVALID", ErrorKind::UnsupportedInput),
        ("INTERNAL", ErrorKind::JobFailed),
    ] {
        let bytes = serde_json::to_vec(&json!({"id":ID,"status":"FAILED","failureCode":code,"failure":KEY})).unwrap();
        let response::Status::Progress(JobProgress::Failed(e)) = response::status(&bytes, ID, &key()).unwrap() else { panic!("failed") };
        assert_eq!(e.kind, kind);
        assert!(!e.message.contains(KEY));
    }
    let p = Runway::new(key(), Wire::new(vec![Err(ProviderError::new(ErrorKind::Network, KEY))]));
    assert!(!block_on(p.submit(&job(GEN4_5))).unwrap_err().message.contains(KEY));
}
#[test]
fn runway_cancel_is_idempotent_and_does_not_delete_an_already_finished_output() {
    for status in [204, 404] {
        let http = Wire::new(vec![task("RUNNING"), reply(status, json!(null))]);
        let p = Runway::new(key(), http.clone());
        block_on(p.cancel(&handle())).unwrap();
        assert_eq!(http.requests()[1].method, HttpMethod::Delete);
    }
    let http = Wire::new(vec![done()]);
    assert!(block_on(Runway::new(key(), http.clone()).cancel(&handle())).is_err());
    assert_eq!(http.requests().len(), 1);
    block_on(Runway::new(key(), Wire::new(vec![reply(404, json!(null))])).cancel(&handle())).unwrap();
    block_on(Runway::new(key(), Wire::new(vec![task("CANCELLED")])).cancel(&handle())).unwrap();
}
#[test]
fn runway_rejects_hostile_receipts_handles_outputs_and_limits() {
    for id in ["../task", "17f20503-6c24-1c16-946b-35dbbce2af2f", KEY] {
        assert!(task_id(id, &key()).is_err());
    }
    assert!(response::created(format!("{{\"id\":\"{ID}\",\"id\":\"{ID}\"}}").as_bytes(), &key()).is_err());
    for body in [
        json!({"id":"other","status":"PENDING"}),
        json!({"id":ID,"status":"RUNNING","progress":2}),
        json!({"id":ID,"status":"NEW"}),
        json!({"id":ID,"status":"SUCCEEDED","output":[URL,URL]}),
        json!({"id":ID,"status":"SUCCEEDED","output":[URL],"cost":{"credits":u64::MAX}}),
    ] {
        assert!(response::status(&serde_json::to_vec(&body).unwrap(), ID, &key()).is_err());
    }
    for url in [
        "http://x.cloudfront.net/a",
        "https://cloudfront.net.evil.test/a",
        "https://127.0.0.1/a",
        "https://user@x.cloudfront.net/a",
        "https://x.cloudfront.net:8443/a",
        "https://x.cloudfront.net/a#fragment",
    ] {
        let body = json!({"id":ID,"status":"SUCCEEDED","output":[url]});
        assert!(response::status(&serde_json::to_vec(&body).unwrap(), ID, &key()).is_err());
    }
    let missing_cost = serde_json::to_vec(&json!({"id":ID,"status":"SUCCEEDED","output":[URL]})).unwrap();
    assert!(matches!(response::status(&missing_cost, ID, &key()).unwrap(), response::Status::Done { usage: Usage { cost: None, .. }, .. }));
    assert!(response::video(b"not video".to_vec(), Limits::HARD).is_err());
    assert!(response::video(mp4(), Limits { output_bytes: 1, ..Limits::HARD }).is_err());
    let mut corrupt = mp4();
    corrupt.truncate(35);
    assert!(response::video(corrupt, Limits::HARD).is_err());
    assert!(Runway::with_limits(key(), Arc::new(Wire::default()), Limits { image_bytes: Limits::HARD.image_bytes + 1, ..Limits::HARD }).is_err());
    let p = Runway::new(key(), Wire::new(vec![]));
    let mut h = handle();
    h.data["extra"] = json!(true);
    assert!(block_on(p.poll(&h)).is_err());
}
#[test]
fn runway_catalog_quotes_match_sent_durations_and_modes() {
    use nodecraft_types::{ParamValues, Unsigned, Value as Param};
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    for (id, wire, rate) in [("runway.gen4-5", GEN4_5, 120_000), ("runway.gen4-turbo", GEN4_TURBO, 50_000)] {
        let m = catalog.get(id).unwrap();
        assert_eq!(m.provider_model, wire);
        for seconds in [2, 5, 10] {
            let mut values = ParamValues::new();
            values.insert("duration_seconds".into(), Param::Unsigned(Unsigned(seconds)));
            assert_eq!(m.quote(&values).unwrap(), Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, rate * seconds)));
            let mut j = job(wire);
            if wire == GEN4_TURBO {
                j.inputs.push(image());
            }
            j.params.insert("duration_seconds".into(), json!(seconds.to_string()));
            assert_eq!(request::prepare(&j, Limits::HARD).unwrap().body.duration, seconds);
        }
        assert_eq!(m.mode("text-to-video").is_some(), wire == GEN4_5);
        assert!(m.mode("image-to-video").is_some());
    }
}
