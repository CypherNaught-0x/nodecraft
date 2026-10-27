//! Authored interface/queue/file fixtures. These are not captured server schemas or live runs.
use super::*;
use futures::executor::block_on;
use serde_json::json;

const INFO: &str = include_str!("fixtures/mesh-object-info.json");
const OBJ: &[u8] = b"v -1 0 0\nv 1 0 0\nv 0 2 0\nf 1 2 3\n";

struct Wire {
    info: Value,
    files: Vec<(String, Vec<u8>)>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl Wire {
    fn new() -> Self {
        let mesh = nodecraft_media::decode_mesh(OBJ, &Default::default()).unwrap();
        Self {
            info: serde_json::from_str(INFO).unwrap(),
            files: vec![
                ("mesh.glb".into(), nodecraft_media::encode_glb(&mesh).unwrap()),
                ("mesh.obj".into(), OBJ.to_vec()),
                ("mesh.gltf".into(), include_bytes!("fixtures/triangle.gltf").to_vec()),
                ("poster.png".into(), nodecraft_media::encode_png(2, 2, &[200; 16], &Default::default()).unwrap()),
            ],
            requests: Mutex::default(),
        }
    }
    fn paths(&self) -> Vec<String> {
        self.requests.lock().unwrap().iter().map(|r| r.url.clone()).collect()
    }
}

impl Http for Wire {
    fn send(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = url::Url::parse(&r.url).unwrap();
        let body = match url.path() {
            "/object_info" => serde_json::to_vec(&self.info).unwrap(),
            "/queue" => br#"{"queue_running":[],"queue_pending":[]}"#.to_vec(),
            "/upload/image" => br#"{"name":"stored.png","subfolder":"in","type":"input"}"#.to_vec(),
            "/prompt" => {
                let posted: Value = serde_json::from_slice(&r.body).unwrap();
                serde_json::to_vec(&json!({"prompt_id":posted["prompt_id"],"number":1,"node_errors":{}})).unwrap()
            }
            "/view" => {
                let name = url.query_pairs().find(|(key, _)| key == "filename").unwrap().1;
                self.files.iter().find(|(file, _)| file == name.as_ref()).unwrap().1.clone()
            }
            path if path.starts_with("/history/") => {
                let id = path.trim_start_matches("/history/");
                let queued = self
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r.url.ends_with("/prompt"))
                    .any(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["prompt_id"].as_str() == Some(id));
                if queued {
                    let files: Vec<_> = self.files.iter().map(|(name, _)| json!({"filename":name,"subfolder":"mesh","type":"output"})).collect();
                    serde_json::to_vec(&json!({id:{"outputs":{"10":{"3d":files}},"status":{"status_str":"success","completed":true,"messages":[]}}})).unwrap()
                } else {
                    b"{}".to_vec()
                }
            }
            path => panic!("unexpected authored fixture request: {path}"),
        };
        let cap = r.max_response_bytes;
        self.requests.lock().unwrap().push(r);
        Box::pin(async move {
            if body.len() > cap {
                return Err(ProviderError::new(ErrorKind::Malformed, "the authored fixture response exceeds the requested cap"));
            }
            Ok(HttpResponse { status: 200, headers: vec![], body })
        })
    }
}

fn job(model: &str) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: json!({"seed":17,"steps":25,"cfg":4.5}).as_object().unwrap().clone(),
        inputs: vec![JobInput::Media {
            role: "image".into(),
            mime: "image/png".into(),
            bytes: Arc::from(nodecraft_media::encode_png(2, 2, &[100; 16], &Default::default()).unwrap()),
        }],
        idempotency_key: format!("original-comfy3d24:{model}"),
    }
}

fn provider(wire: Arc<Wire>) -> ComfyProvider {
    ComfyProvider::new("http://fixture:8188", None, "comfy3d24-fixture", wire, Arc::new(|| 0)).unwrap()
}

#[test]
fn comfy3d24_core_graphs_check_before_upload_and_durable_handles_keep_exact_meshes() {
    for model in ["hunyuan3d-2.0", "hunyuan3d-2.1"] {
        let mut wire = Wire::new();
        if model == "hunyuan3d-2.1" {
            // Current public docs also describe FILE3D on the saver. The existing Comfy
            // connection checker supports the union; do not narrow it to the legacy MESH.
            wire.info["SaveGLB"]["input"]["required"]["mesh"] = json!(["MESH,FILE3D"]);
        }
        let wire = Arc::new(wire);
        let job = job(model);
        let Submission::Queued(handle) = block_on(provider(wire.clone()).submit(&job)).unwrap() else { panic!("queued") };
        let restored = serde_json::from_slice::<JobHandle>(&serde_json::to_vec(&handle).unwrap()).unwrap();
        assert!(template::mesh::policy(&restored.data).unwrap());
        let JobProgress::Done(done) = block_on(provider(wire.clone()).poll(&restored)).unwrap() else { panic!("done") };
        let actual: Vec<_> = done
            .outputs
            .iter()
            .map(|o| match o {
                Output::Bytes { mime, bytes } => (mime.as_str(), bytes.as_ref()),
                _ => panic!("original bytes"),
            })
            .collect();
        assert_eq!(actual.len(), 3, "the poster is not a fourth result on a Mesh-only card");
        assert_eq!(actual.iter().map(|(mime, _)| *mime).collect::<Vec<_>>(), ["model/gltf-binary", "model/obj", "model/gltf+json"]);
        for ((_, bytes), (_, expected)) in actual.iter().zip(&wire.files) {
            assert_eq!(*bytes, expected.as_slice());
        }
        let Submission::Queued(same) = block_on(provider(wire.clone()).submit(&job)).unwrap() else { panic!("same queued run") };
        assert_eq!(same, handle, "lost local state does not lose the mesh contract or queue a second job");
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.iter().filter(|r| r.url.ends_with("/prompt")).count(), 1);
        assert_eq!(requests.iter().filter(|r| r.url.ends_with("/upload/image")).count(), 1);
        let sent: Value = serde_json::from_slice(&requests.iter().find(|r| r.url.ends_with("/prompt")).unwrap().body).unwrap();
        assert_eq!(sent["prompt"]["2"]["inputs"]["image"], "in/stored.png");
        assert_eq!(sent["prompt"]["7"]["inputs"]["seed"], 17);
        assert_eq!(sent["prompt"]["7"]["inputs"]["steps"], 25);
        assert_eq!(sent["prompt"]["10"]["class_type"], "SaveGLB");
        assert_eq!(sent["prompt"]["6"].is_object(), model == "hunyuan3d-2.1");
    }
}

#[test]
fn comfy3d24_refuses_missing_nodes_checkpoints_or_bad_settings_without_upload_or_prompt() {
    for case in 0..7 {
        let mut wire = Wire::new();
        let mut job = job("hunyuan3d-2.1");
        match case {
            0 => {
                wire.info.as_object_mut().unwrap().remove("SaveGLB");
            }
            1 => wire.info["ImageOnlyCheckpointLoader"]["input"]["required"]["ckpt_name"] = json!([[]]),
            2 => {
                job.params.insert("steps".into(), json!(0));
            }
            3 => {
                job.params.insert("cfg".into(), json!("bad"));
            }
            4 => {
                job.params.insert("seed".into(), json!(-1));
            }
            5 => job.inputs.clear(),
            _ => job.inputs.push(JobInput::Text { role: "prompt".into(), text: "not silently ignored".into() }),
        }
        let wire = Arc::new(wire);
        assert!(block_on(provider(wire.clone()).submit(&job)).is_err(), "case {case}");
        assert!(!wire.paths().iter().any(|url| url.contains("/upload/") || url.ends_with("/prompt")), "case{case}: {:?}", wire.paths());
    }
}

#[test]
fn comfy3d24_posters_alone_damaged_meshes_and_output_caps_fail_without_requeueing() {
    for case in 0..3 {
        let mut wire = Wire::new();
        if case == 0 {
            wire.files.retain(|(name, _)| name.ends_with(".png"));
        }
        if case == 1 {
            wire.files = vec![("mesh.glb".into(), b"glTF\x02\0\0\0\x10\0\0\0broken".to_vec())];
        }
        let wire = Arc::new(wire);
        let p = provider(wire.clone());
        let p = if case == 2 { p.with_output_limits(8, 8) } else { p };
        let Submission::Queued(handle) = block_on(p.submit(&job("hunyuan3d-2.0"))).unwrap() else { panic!("queued") };
        assert!(block_on(p.poll(&handle)).is_err());
        assert_eq!(wire.paths().iter().filter(|url| url.ends_with("/prompt")).count(), 1);
    }
}

#[test]
fn comfy3d24_generic_imports_keep_mixed_outputs_and_invalid_saved_policy_does_no_io() {
    let wire = Wire::new();
    let mixed: Vec<_> = wire
        .files
        .iter()
        .map(|(_, bytes)| Output::Bytes { mime: nodecraft_media::sniff(bytes).unwrap().mime.into(), bytes: Arc::from(bytes.as_slice()) })
        .collect();
    assert_eq!(template::mesh::outputs(&Value::Null, mixed).unwrap().len(), 4);
    let wire = Arc::new(wire);
    let mut handle = handle_for("old");
    handle.data = json!({"comfy_mesh_model_v1":"unknown"});
    assert!(block_on(provider(wire.clone()).poll(&handle)).is_err());
    assert!(wire.paths().is_empty());
}
