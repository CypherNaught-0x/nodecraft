//! Hosted profile through the same placed Workflow, real maker, binding, quote and RunHost.
use super::*;
use nodecraft_engine::{
    provider_setup::{self, HostEnv, ProviderSetup},
    secrets::{Account, MemoryStore, Secret, SecretStore},
    settings::{ComfyServer, ProviderSettings},
};
const KEY: &str = "comfyui-public-fixture-not-a-key";
const JOB: &str = "cloud-job-16";
struct Cloud {
    requests: Mutex<Vec<HttpRequest>>,
    png: Vec<u8>,
    lose_create: bool,
}
impl Http for Cloud {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("cloud may not use redirect-capable transport")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let path = request.url.strip_prefix(nodecraft_providers::comfy_cloud::ORIGIN).unwrap();
        let create = path == "/api/v2/jobs";
        let body = match path {
            "/api/object_info" => OBJECT_INFO.as_bytes().to_vec(),
            "/api/v2/assets" => br#"{"id":"input-16"}"#.to_vec(),
            "/api/v2/jobs" => serde_json::to_vec(&json!({"id":JOB,"status":"queued"})).unwrap(),
            "/api/v2/jobs/cloud-job-16" => {
                serde_json::to_vec(&json!({"id":JOB,"status":"succeeded","outputs":[{"id":"output-16","node_id":"2","type":"image","name":"result.png"}]}))
                    .unwrap()
            }
            "/api/v2/assets/output-16/content" => self.png.clone(),
            _ => panic!("unexpected cloud path {path}"),
        };
        self.requests.lock().unwrap().push(request);
        let lost = create && self.lose_create;
        Box::pin(async move {
            if lost {
                Err(ProviderError::new(nodecraft_providers::ErrorKind::Network, "paid reply lost"))
            } else {
                Ok(HttpResponse { status: 200, headers: vec![], body })
            }
        })
    }
}
fn cloud_session(lost: bool) -> (Session, RunHost, Arc<Cloud>) {
    let (mut session, host, local) = placed(card());
    let host = host.with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let cloud = Arc::new(Cloud { requests: Mutex::default(), png: local.png.clone(), lose_create: lost });
    let keys = MemoryStore::default();
    keys.set(&Account::new("comfy-cloud").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    let settings = ProviderSettings { comfyui: vec![ComfyServer::cloud("Cloud")], comfyui_default: Some("Cloud".into()), ..Default::default() };
    let env = HostEnv { keys: &keys, http: cloud.clone(), settings: &settings, now_ms: Arc::new(|| 0), progress: None };
    let slot = provider_setup::comfy_slot("Cloud");
    let mut setup = ProviderSetup::default();
    assert_eq!(setup.ensure(&slot, |slot| provider_setup::make(slot, &env)), &provider_setup::Readiness::Ready);
    session.set_provider_routing(setup.routing_for([(CARD, slot.as_str())], &settings).unwrap()).unwrap();
    assert!(cloud.requests.lock().unwrap().is_empty(), "construction and routing send nothing");
    (session, host, cloud)
}
fn request() -> serde_json::Value {
    json!({"canvas":C,"mode":"selected","nodes":[CARD]})
}
fn start(session: &mut Session) {
    let mut request = request();
    let q = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["complete"], false, "{q}");
    assert!(q["steps"][0]["unpriced"].as_str().unwrap().contains("GPU time"));
    request["accept"] = json!({"token":q["token"],"amounts":{},"partial":false,"unpriced_jobs":0});
    assert!(session.dispatch("run.start", &request).is_err());
    request["accept"] = json!({"token":q["token"],"amounts":{},"partial":true,"unpriced_jobs":1});
    session.dispatch("run.start", &request).unwrap();
}
fn pump(session: &mut Session, host: &mut RunHost) -> Vec<RunNotice> {
    let mut notices = vec![];
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        notices.extend(host.pump(session));
        if !host.is_busy() {
            return notices;
        }
        assert!(Instant::now() < deadline, "{notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn hosted_workflow16_runs_with_real_card_values_upload_and_selected_output() {
    let (mut session, mut host, cloud) = cloud_session(false);
    start(&mut session);
    let notices = pump(&mut session, &mut host);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
    let node = session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    assert!(
        matches!(node.history.last().unwrap().outputs.as_slice(),[Value::List(v)] if matches!(v.as_slice(),[Value::Media(a)] if a.mime.essence()=="image/png"))
    );
    let requests = cloud.requests.lock().unwrap();
    let create: Vec<_> = requests.iter().filter(|r| r.url.ends_with("/api/v2/jobs")).collect();
    assert_eq!(create.len(), 1);
    let sent: serde_json::Value = serde_json::from_slice(&create[0].body).unwrap();
    assert_eq!(sent["workflow"]["1"]["inputs"]["text"], "a blue whale");
    assert_eq!(sent["workflow"]["1"]["inputs"]["seed"], 7);
    assert_eq!(sent["workflow"]["3"]["inputs"]["image"]["info"]["id"], "input-16");
    assert!(sent["workflow"].get("4").is_none(), "unselected output must not execute");
    assert!(create[0].headers.iter().any(|(k, v)| k == "Idempotency-Key" && v.len() == 36));
    for request in requests.iter() {
        assert!(request.secret_headers.contains(&if request.url.ends_with("object_info") { "x-api-key" } else { "authorization" }.into()));
        assert!(!String::from_utf8_lossy(&request.body).contains(KEY));
    }
}
#[test]
fn hosted_workflow16_unknown_price_blocks_finite_budget_before_network() {
    let (mut session, _host, cloud) = cloud_session(false);
    session.swap_resource(Some(nodecraft_engine::run_cmds::BudgetState {
        budgets: Budgets { per_call: Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1_000_000)), ..Default::default() },
        spent: Default::default(),
    }));
    let q = session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false, "{q}");
    let mut request = request();
    request["accept"] = json!({"token":q["token"],"amounts":{},"partial":true,"unpriced_jobs":1});
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(cloud.requests.lock().unwrap().is_empty());
}
#[test]
fn hosted_workflow16_lost_create_does_not_replay_with_three_attempts_enabled() {
    let (mut session, mut host, cloud) = cloud_session(true);
    start(&mut session);
    let notices = pump(&mut session, &mut host);
    assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { .. })), "{notices:?}");
    assert_eq!(cloud.requests.lock().unwrap().iter().filter(|r| r.url.ends_with("/api/v2/jobs")).count(), 1);
}
