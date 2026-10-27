use super::*;
use futures::executor::block_on;
use std::sync::Mutex;
const INFO: &str = r#"{
 "ImageMaker":{"input":{"required":{"prompt":["STRING",{}]}},"output":["IMAGE"]},
 "Dynamic":{"input":{"required":{"model":["COMFY_DYNAMICCOMBO_V3",{"options":[{"key":"small","inputs":{"required":{"resolution":[["64","128"],{}]}}}]}]}},"output":["IMAGE"]},
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

#[test]
fn templates948_provider_sends_dotted_dynamic_values_and_refuses_invalid_choice_before_prompt_post() {
    for (value, accepted) in [("128", true), ("invalid", false)] {
        let http = Arc::new(Wire { requests: Mutex::default(), first: Vec::new(), second: Vec::new() });
        let provider = ComfyProvider::new("http://fixture:8188", None, "fixture-salt", http.clone(), Arc::new(|| 0)).unwrap();
        let job=ProviderJob{model:"workflow".into(),params:Map::new(),inputs:vec![
            JobInput::Text{role:template::CARD_WORKFLOW.into(),text:r#"{"1":{"class_type":"Dynamic","inputs":{"model":"small","model.resolution":"64"}},"2":{"class_type":"SaveImage","inputs":{"images":["1",0],"filename_prefix":"NodeCraft"}}}"#.into()},
            JobInput::Text{role:template::CARD_VALUES.into(),text:serde_json::json!({"1.model.resolution":value}).to_string()},
            JobInput::Text{role:template::CARD_OUTPUTS.into(),text:r#"["2"]"#.into()}
        ],idempotency_key:"mapping948-dynamic".into()};
        let result = block_on(provider.submit(&job));
        assert_eq!(result.is_ok(), accepted);
        let requests = http.requests.lock().unwrap();
        let post = requests.iter().find(|r| r.url.ends_with("/prompt"));
        if accepted {
            let sent: Value = serde_json::from_slice(&post.unwrap().body).unwrap();
            assert_eq!(sent["prompt"]["1"]["inputs"]["model.resolution"], "128");
            assert_eq!(sent["prompt"]["1"]["inputs"]["model"], "small");
        } else {
            assert!(post.is_none(), "invalid dynamic value must not reach generation POST");
        }
    }
}

#[test]
fn templates948_provider_prunes_unselected_helpers_but_checks_required_ancestors_before_io() {
    for invalid_ancestor in [false, true] {
        let http = Arc::new(Wire { requests: Mutex::default(), first: Vec::new(), second: Vec::new() });
        let provider = ComfyProvider::new("http://fixture:8188", None, "fixture-salt", http.clone(), Arc::new(|| 0)).unwrap();
        let mut flow: Value = serde_json::from_str(FLOW).unwrap();
        flow["9"] = serde_json::json!({"class_type":"UninstalledUnselectedHelper","inputs":{"image":["10",0]}});
        if invalid_ancestor {
            flow["1"]["inputs"] = serde_json::json!({});
        }
        let job = ProviderJob {
            model: "workflow".into(),
            params: Map::new(),
            inputs: vec![
                JobInput::Text { role: template::CARD_WORKFLOW.into(), text: flow.to_string() },
                JobInput::Text { role: template::CARD_OUTPUTS.into(), text: r#"["2"]"#.into() },
            ],
            idempotency_key: "templates948-selected-closure".into(),
        };
        let result = block_on(provider.submit(&job));
        assert_eq!(result.is_ok(), !invalid_ancestor);
        let requests = http.requests.lock().unwrap();
        let post = requests.iter().find(|r| r.url.ends_with("/prompt"));
        if invalid_ancestor {
            assert!(post.is_none());
            assert!(requests.iter().all(|r| !r.url.contains("/upload/")));
        } else {
            let request: Value = serde_json::from_slice(&post.unwrap().body).unwrap();
            assert_eq!(request["prompt"].as_object().unwrap().keys().collect::<Vec<_>>(), ["1", "2"]);
            assert_eq!(request["prompt"]["1"]["inputs"]["prompt"], "public fixture");
        }
    }
}
