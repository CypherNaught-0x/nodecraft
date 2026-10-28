use super::*;
use futures::executor::block_on;
use std::sync::Mutex;
const INFO: &str = r#"{
 "ImageMaker":{"input":{"required":{"prompt":["STRING",{}]}},"output":["IMAGE"]},
 "SaveImage":{"input":{"required":{"images":["IMAGE",{}],"filename_prefix":["STRING",{}]}},"output":[],"output_node":true}
}"#;
const FLOW: &str = r#"{
 "1":{"class_type":"ImageMaker","inputs":{"prompt":"public fixture"}},
 "2":{"class_type":"SaveImage","inputs":{"images":["1",0],"filename_prefix":"first"}},
 "3":{"class_type":"SaveImage","inputs":{"images":["1",0],"filename_prefix":"second"}}
}"#;
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    first: Vec<u8>,
    second: Vec<u8>,
}
impl Http for Wire {
    fn send(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let path = r.url.split("8188").nth(1).unwrap_or_default();
        let body = if path == "/object_info" {
            INFO.as_bytes().to_vec()
        } else if path == "/queue" {
            br#"{"queue_running":[],"queue_pending":[]}"#.to_vec()
        } else if path == "/prompt" {
            let value: Value = serde_json::from_slice(&r.body).unwrap();
            serde_json::to_vec(&serde_json::json!({"prompt_id":value["prompt_id"],"number":1,"node_errors":{}})).unwrap()
        } else if let Some(id) = path.strip_prefix("/history/") {
            if self.requests.lock().unwrap().iter().any(|r| r.url.ends_with("/prompt")) {
                serde_json::to_vec(&serde_json::json!({id:{"outputs":{"2":{"images":[{"filename":"first.png","subfolder":"","type":"output"}]},"3":{"images":[{"filename":"second.png","subfolder":"","type":"output"}]}},"status":{"status_str":"success","completed":true,"messages":[]}}})).unwrap()
            } else {
                b"{}".to_vec()
            }
        } else if path.starts_with("/view") {
            if path.contains("second.png") { self.second.clone() } else { self.first.clone() }
        } else {
            panic!("unexpected fixture path {path}")
        };
        self.requests.lock().unwrap().push(r);
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}
#[test]
fn mapping947_actual_provider_saved_handle_keeps_selected_image_order_after_resume() {
    let first = nodecraft_media::encode_png(2, 2, &[20; 16], &Default::default()).unwrap();
    let second = nodecraft_media::encode_png(2, 2, &[180; 16], &Default::default()).unwrap();
    let http = Arc::new(Wire { requests: Mutex::default(), first: first.clone(), second: second.clone() });
    let provider = || ComfyProvider::new("http://fixture:8188", None, "fixture-salt", http.clone(), Arc::new(|| 0)).unwrap();
    let job = ProviderJob {
        model: "workflow".into(),
        params: Map::new(),
        inputs: vec![
            JobInput::Text { role: template::CARD_WORKFLOW.into(), text: FLOW.into() },
            JobInput::Text { role: template::CARD_OUTPUTS.into(), text: r#"["3","2"]"#.into() },
        ],
        idempotency_key: "mapping947-order".into(),
    };
    let Submission::Queued(handle) = block_on(provider().submit(&job)).unwrap() else { panic!("queued") };
    let handle: JobHandle = serde_json::from_slice(&serde_json::to_vec(&handle).unwrap()).unwrap();
    let JobProgress::Done(done) = block_on(provider().poll(&handle)).unwrap() else { panic!("done") };
    let actual = done
        .outputs
        .iter()
        .map(|o| match o {
            Output::Bytes { bytes, .. } => bytes.as_ref(),
            _ => panic!("bytes"),
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, [second.as_slice(), first.as_slice()]);
    assert_eq!(http.requests.lock().unwrap().iter().filter(|r| r.url.ends_with("/prompt")).count(), 1, "resuming only polls, never requeues");
}
