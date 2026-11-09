//! Exercise the actual import/mapping/planning conversion before the provider boundary.
use super::*;
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, JobInput, Provider, ProviderError, Submission};
use std::sync::Mutex;
const INFO: &str = r#"{
 "ImageMaker":{"input":{"required":{"prompt":["STRING",{}]}},"output":["IMAGE"]},
 "SaveImage":{"input":{"required":{"images":["IMAGE",{}],"filename_prefix":["STRING",{}]}},"output_node":true},
 "LoadImage":{"input":{"required":{"image":[["fixture.png"],{"image_upload":true}]}},"output":["IMAGE"]}
}"#;
const FLOW: &str = r#"{
 "1":{"class_type":"ImageMaker","inputs":{"prompt":"selected prompt"}},
 "2":{"class_type":"SaveImage","inputs":{"images":["1",0],"filename_prefix":"NodeCraft"}},
 "3":{"class_type":"ImageMaker","inputs":{"prompt":"unused mapped prompt"}},
 "4":{"class_type":"LoadImage","inputs":{"image":"fixture.png"}}
}"#;
#[derive(Default)]
struct Wire(Mutex<Vec<HttpRequest>>);
impl Http for Wire {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let body = if request.url.ends_with("/object_info") {
            INFO.as_bytes().to_vec()
        } else if request.url.ends_with("/queue") {
            br#"{"queue_running":[],"queue_pending":[]}"#.to_vec()
        } else if request.url.contains("/history/") {
            b"{}".to_vec()
        } else if request.url.ends_with("/prompt") {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            serde_json::to_vec(&json!({"prompt_id":body["prompt_id"],"number":1,"node_errors":{}})).unwrap()
        } else {
            panic!("a pruned media branch must never upload: {}", request.url)
        };
        self.0.lock().unwrap().push(request);
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}
#[test]
fn templates948_imported_planned_card_ignores_only_pruned_values_and_uploads() {
    let mut s = session();
    finish(&mut s, 1, Ok(Arc::new(NodeCatalog::parse(INFO.as_bytes()).unwrap().0).into()));
    s.dispatch("comfy.workflow.import", &import_params(FLOW)).unwrap();
    let mapped = comfy_workflow::card(card(&s)).unwrap();
    assert!(mapped.mapping.params.iter().any(|parameter| parameter.node == "3" && parameter.input == "prompt"));
    assert_eq!(mapped.mapping.inputs.len(), 1);
    assert_eq!(mapped.mapping.inputs[0].node, "4");
    let png = nodecraft_media::encode_png(2, 2, &[255; 16], &Default::default()).unwrap();
    let hash = s.import_asset(&png).unwrap();
    let node = card(&s);
    let file = &mapped.mapping.inputs[0];
    let port = node.inputs.iter().find(|port| port.name == file.port).unwrap();
    let inputs = vec![nodecraft_nodes::InputValue {
        port: port.name.clone(),
        port_type: port.port_type,
        value: nodecraft_types::Value::Media(nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() }),
        origins: vec![],
    }];
    let mut targets = nodecraft_types::TargetSelection::empty();
    targets.insert(CARD, &nodecraft_types::ProviderTarget::default_instance(comfy_workflow::PROVIDER).unwrap()).unwrap();
    let spec = s.kinds().get(comfy_workflow::ID).unwrap().plan(&nodecraft_nodes::PlanCtx { node, inputs: &inputs, targets: &targets }).unwrap().pop().unwrap();
    for case in [
        "valid",
        "unknown-value",
        "unknown-pruned-value",
        "invalid-pruned-value",
        "unknown-media",
        "unknown-pruned-media",
        "pruned-not-upload",
        "invalid-required",
        "missing-ancestor",
        "retained-not-upload",
    ] {
        let (_, mut job) = crate::generate::provider_job_with_media(&spec, "original-offline-pruned-card", s.assets()).unwrap();
        assert!(job.inputs.iter().any(|input| matches!(input,JobInput::Media{role,..}if role=="4.image")));
        for input in &mut job.inputs {
            match input {
                JobInput::Text { role, text } if role == comfy_workflow::VALUES_ROLE => {
                    let mut values: Value = serde_json::from_str(text).unwrap();
                    assert_eq!(values["3.prompt"], "unused mapped prompt");
                    if case == "unknown-value" {
                        values["99.prompt"] = json!("not an original target");
                    }
                    if case == "unknown-pruned-value" {
                        values["3.not_declared"] = json!("not a declared original input");
                    }
                    if case == "invalid-pruned-value" {
                        values["3.prompt"] = json!(99);
                    }
                    if case == "invalid-required" {
                        values["1.prompt"] = json!(99);
                    }
                    *text = values.to_string();
                }
                JobInput::Text { role, text } if role == comfy_workflow::WORKFLOW_ROLE && case == "missing-ancestor" => {
                    let mut workflow: Value = serde_json::from_str(text).unwrap();
                    workflow["2"]["inputs"]["images"] = json!(["99", 0]);
                    *text = workflow.to_string();
                }
                JobInput::Media { role, .. } if case == "unknown-media" => *role = "99.image".into(),
                JobInput::Media { role, .. } if case == "unknown-pruned-media" => *role = "4.not_declared".into(),
                JobInput::Media { role, .. } if case == "pruned-not-upload" => *role = "3.prompt".into(),
                JobInput::Media { role, .. } if case == "retained-not-upload" => *role = "1.prompt".into(),
                _ => {}
            }
        }
        let http = Arc::new(Wire::default());
        let provider = nodecraft_providers::comfyui::ComfyProvider::new("http://fixture:8188", None, "fixture-salt", http.clone(), Arc::new(|| 0)).unwrap();
        let result = futures::executor::block_on(provider.submit(&job));
        let requests = http.0.lock().unwrap();
        let post = requests.iter().find(|request| request.url.ends_with("/prompt"));
        assert!(requests.iter().all(|request| !request.url.contains("/upload/")));
        if case == "valid" {
            assert!(matches!(result, Ok(Submission::Queued(_))), "{result:?}");
            let request: Value = serde_json::from_slice(&post.unwrap().body).unwrap();
            assert_eq!(request["prompt"].as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(), ["1", "2"]);
            assert_eq!(request["prompt"]["1"]["inputs"]["prompt"], "selected prompt");
        } else {
            assert!(result.is_err(), "{case}");
            assert!(post.is_none(), "{case}");
        }
    }
}
