//! A saved edited mapping through the actual host, uploads, prompt and result recording.
use super::*;
struct Paused {
    server: Arc<Server>,
    gate: Mutex<Option<futures::channel::oneshot::Receiver<()>>>,
}
impl Http for Paused {
    fn send(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let paused = r.url.ends_with("/prompt");
        let reply = self.server.send(r);
        let gate = if paused { self.gate.lock().unwrap().take() } else { None };
        Box::pin(async move {
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            reply.await
        })
    }
}
fn fields(s: &mut Session, id: NodeId) -> serde_json::Value {
    s.dispatch("comfy.workflow.mapping", &json!({"canvas":C,"id":id})).unwrap()
}
#[test]
fn mapping947_saved_selection_runs_with_hidden_values_and_blocks_only_the_inflight_card() {
    let (mut s, mut host, server) = placed(card());
    let catalog = Arc::new(nodecraft_comfy::NodeCatalog::parse(OBJECT_INFO.as_bytes()).unwrap().0);
    nodecraft_engine::comfy_workflow_cmds::finish(&mut s, 1, Ok(catalog.clone().into()));
    s.dispatch("node.param_set", &json!({"canvas":C,"id":CARD,"name":"seed","value":Value::Unsigned(Unsigned(19))})).unwrap();
    let listed = fields(&mut s, CARD);
    let edit = json!({"canvas":C,"id":CARD,"binding":listed["binding"],"inputs":[{"node":"3","input":"image","name":"camera"},{"node":"1","input":"seed","name":"Seed value"}],"outputs":[{"node":"2","label":"Chosen image"}],"output_names":[{"modality":"image","name":"images"}]});
    s.dispatch("comfy.workflow.mapping_set", &edit).unwrap();
    let bytes = nodecraft_format::encode(&nodecraft_format::Snapshot { project: s.document().clone(), assets: s.assets().clone(), epoch: 0 }).unwrap();
    let snapshot = nodecraft_format::decode(&bytes).unwrap();
    let (release, gate) = futures::channel::oneshot::channel();
    let http = Arc::new(Paused { server: server.clone(), gate: Mutex::new(Some(gate)) });
    let comfy = ComfyProvider::new("http://gpu-box:8188", None, "fixture-salt", http, Arc::new(|| 0)).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(comfy)).unwrap();
    routing.select(CARD, comfy_workflow::PROVIDER, binding.target()).unwrap();
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut s = Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap().with_services(services);
    s.set_provider_routing(routing).unwrap();
    host.install(&mut s);
    nodecraft_engine::comfy_workflow_cmds::finish(&mut s, 2, Ok(catalog.into()));
    let listed = fields(&mut s, CARD);
    let mut edit = edit;
    edit["binding"] = listed["binding"].clone();
    let request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = s.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    let mut start = request;
    start["accept"] = json!({"token":quote["token"],"partial":false,"unpriced_jobs":0,"amounts":{}});
    s.dispatch("run.start", &start).unwrap();
    assert_eq!(
        s.dispatch("comfy.workflow.mapping_set", &edit).unwrap_err().kind,
        nodecraft_types::ErrorKind::Busy,
        "accepted but not yet started run guards its canvas"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        host.pump(&mut s);
        if server.requests.lock().unwrap().iter().any(|r| r.url.ends_with("/prompt")) {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        s.dispatch("comfy.workflow.mapping_set", &edit).unwrap_err().kind,
        nodecraft_types::ErrorKind::Busy,
        "a submitted job guards its card until settled"
    );
    let other = NodeId::from_bytes([77; 16]);
    let mut another = s.document().canvases()[&C].nodes()[&CARD].clone();
    another.id = other;
    s.dispatch("node.add", &json!({"canvas":C,"node":another})).unwrap();
    let listed = fields(&mut s, other);
    let mut other_edit = edit.clone();
    other_edit["id"] = json!(other);
    other_edit["binding"] = listed["binding"].clone();
    other_edit["inputs"][1]["name"] = json!("Another seed label");
    s.dispatch("comfy.workflow.mapping_set", &other_edit).unwrap();
    release.send(()).unwrap();
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(&mut s));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    assert_eq!(
        s.dispatch("comfy.workflow.mapping_set", &edit).unwrap()["changed"],
        false,
        "completed run releases the edit guard; an unchanged Apply keeps its result"
    );
    let node = &s.document().canvases()[&C].nodes()[&CARD];
    assert_eq!(node.outputs[0].name, "images");
    assert!(node.active.is_some());
    assert!(
        matches!(node.history.last().unwrap().outputs.as_slice(),[Value::List(v)] if matches!(v.as_slice(),[Value::Media(a)] if a.mime.essence()=="image/png"))
    );
    let requests = server.requests.lock().unwrap();
    let prompt = requests.iter().find(|r| r.url.ends_with("/prompt")).unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&prompt.body).unwrap();
    assert_eq!(sent["prompt"]["1"]["inputs"]["text"], "a blue whale", "hidden prompt value was baked before saving");
    assert_eq!(sent["prompt"]["1"]["inputs"]["seed"], 19);
    assert_eq!(sent["prompt"]["3"]["inputs"]["image"], "in/stored.png", "renamed input retained its wire and upload");
    assert!(sent["prompt"].get("4").is_none(), "unselected saver does not run");
}
