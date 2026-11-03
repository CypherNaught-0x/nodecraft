//! Soul IDs as provider-bound handles (#180): a trained Soul ID is a style descriptor of family
//! `soul-2` that only the Higgsfield account that trained it can use. The Library says so and
//! checks it against the account last fetched, exports never write it out as a file, a SOUL 2
//! card takes it as its Soul ID, and a project saved and opened on another account (or with no
//! Higgsfield list at all) keeps every setting and refuses the run, saying why, before anything is
//! paid. The adapter runs over a scripted transport; nothing reaches the network.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
};
use nodecraft_graph::{Canvas, Change, Generation, Node, Project};
use nodecraft_nodes::generation::{KIND, MODEL, PROMPT, STYLE_KIND};
use nodecraft_providers::{BoxFuture, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, higgsfield::Higgsfield, secrets::Secret};
use nodecraft_types::{AssetRef, CanvasId, Choice, ErrorKind, GenerationId, NodeId, OptionKey, OptionLists, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([18; 16]);
/// A Train style card holding a trained Soul ID, one holding a FLUX style, and a SOUL 2 card.
const MARA: NodeId = NodeId::from_bytes([1; 16]);
const INK: NodeId = NodeId::from_bytes([2; 16]);
const SOUL: NodeId = NodeId::from_bytes([3; 16]);
const FLUX: NodeId = NodeId::from_bytes([4; 16]);
const SOUL_ID: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
const OTHER_ID: &str = "0b5d7c2a-8f1e-4c3b-9a6d-2e4f8a1c3b5d";

fn soul_descriptor() -> Vec<u8> {
    nodecraft_media::style::encode(&nodecraft_media::style::Style {
        provider: "higgsfield".into(),
        trainer: "higgsfield/soul-id/v2".into(),
        family: "soul-2".into(),
        trigger: String::new(),
        weights: format!("https://api.higgsfield.ai/v1/custom-references/{SOUL_ID}"),
    })
    .unwrap()
}

fn flux_descriptor() -> Vec<u8> {
    nodecraft_media::style::encode(&nodecraft_media::style::Style {
        provider: "fal".into(),
        trainer: "fal-ai/flux-lora-fast-training".into(),
        family: "flux-1".into(),
        trigger: "inkwash".into(),
        weights: "https://v3.fal.media/files/ink/weights.safetensors".into(),
    })
    .unwrap()
}

fn trained(kinds: &nodecraft_nodes::Registry, id: NodeId, title: &str, trainer: &str, asset: AssetRef) -> Node {
    let values = [(MODEL.into(), Value::Text(trainer.into()))].into();
    let mut node = kinds.get(STYLE_KIND).unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
    node.title = title.into();
    let made = GenerationId::from_bytes([id.as_bytes()[0].wrapping_add(100); 16]);
    node.history.push(Generation::new(made, vec![Value::Media(asset)]));
    node.active = Some(made);
    node
}

fn image_card(kinds: &nodecraft_nodes::Registry, id: NodeId, model: &str, soul_id: Option<&str>) -> Node {
    let mut values: nodecraft_types::ParamValues =
        [(MODEL.into(), Value::Text(model.into())), (PROMPT.into(), Value::Text("a portrait in the rain".into()))].into();
    if let Some(soul_id) = soul_id {
        values.insert("custom_reference_id".into(), Value::Text(soul_id.into()));
    }
    let mut node = kinds.get(KIND).unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
    node.title = model.into();
    node
}

/// A project with both trained styles, a SOUL 2 card naming `soul_id`, and a FLUX LoRA card.
fn session(soul_id: Option<&str>) -> Session {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut scratch = Session::new(Project::new(ProjectId::from_bytes([9; 16]), "Scratch").unwrap()).unwrap();
    let soul = scratch.import_asset(&soul_descriptor()).unwrap();
    let ink = scratch.import_asset(&flux_descriptor()).unwrap();
    let style = |hash, family: &str| AssetRef {
        hash,
        mime: format!("application/vnd.nodecraft.style+json; family={family}").parse().unwrap(),
        meta: Default::default(),
    };
    let mut project = Project::new(ProjectId::from_bytes([18; 16]), "Soul IDs").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Portraits") }.apply(&mut project).unwrap();
    let nodes = [
        trained(&kinds, MARA, "Mara", "higgsfield.soul-id-trainer", style(soul, "soul-2")),
        trained(&kinds, INK, "Ink wash", "fal.flux-lora-trainer", style(ink, "flux-1")),
        image_card(&kinds, SOUL, "higgsfield.soul-2", soul_id),
        image_card(&kinds, FLUX, "fal.flux-lora", None),
    ];
    for node in nodes {
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    for bytes in [soul_descriptor(), flux_descriptor()] {
        session.import_asset(&bytes).unwrap();
    }
    session
}

/// Installs the Soul IDs an account lists, as `options.refresh` does.
fn account(session: &mut Session, ids: &[&str]) {
    let mut lists = session.swap_resource::<OptionLists>(None).unwrap_or_default();
    let key = OptionKey { owner: "higgsfield.soul-2", source: "higgsfield.soul_ids", scope: None };
    lists.set(key, ids.iter().map(|id| Choice { value: (*id).into(), label: "Mara".into() }).collect()).unwrap();
    session.swap_resource(Some(lists));
}

fn styles(session: &mut Session) -> Json {
    session.dispatch("library.styles", &json!({})).unwrap()
}

fn entry(listed: &Json, id: NodeId) -> Json {
    listed["styles"].as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap().clone()
}

fn soul_param(session: &Session) -> Option<Value> {
    session.document().canvases().get(&C).unwrap().nodes().get(&SOUL).unwrap().params.get("custom_reference_id").cloned()
}

#[test]
fn the_library_tells_a_soul_id_from_a_portable_style_and_checks_it_against_the_account() {
    let mut s = session(None);
    let listed = styles(&mut s);
    assert_eq!(listed["account_bound"], 1, "{listed}");
    let mara = entry(&listed, MARA);
    assert_eq!((mara["identity"].clone(), mara["portable"].clone(), mara["soul_id"].clone()), (json!(true), json!(false), json!(SOUL_ID)), "{mara}");
    assert_eq!(mara["available"], "unchecked", "no list fetched yet: {mara}");
    let ink = entry(&listed, INK);
    assert_eq!((ink["portable"].clone(), ink["soul_id"].clone(), ink["available"].clone()), (json!(true), Json::Null, Json::Null), "{ink}");

    account(&mut s, &[OTHER_ID]);
    assert_eq!(entry(&styles(&mut s), MARA)["available"], "unlisted", "another account's list doesn't have it");
    account(&mut s, &[OTHER_ID, SOUL_ID]);
    assert_eq!(entry(&styles(&mut s), MARA)["available"], "listed");
    // The count covers the project whatever the search.
    assert_eq!(s.dispatch("library.styles", &json!({"query": "ink"})).unwrap()["account_bound"], 1);
    assert_eq!(nodecraft_engine::soul_ids::handles(s.document()), nodecraft_engine::soul_ids::Handles { identities: 1, uses: 0 });
}

#[test]
fn a_soul_id_is_never_exported_as_a_portable_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(None);
    let generation = |id: NodeId| GenerationId::from_bytes([id.as_bytes()[0].wrapping_add(100); 16]);
    let path = |name: &str| dir.path().join(name).display().to_string();
    let refused = s.dispatch("history.export", &json!({"canvas": C, "id": MARA, "generation": generation(MARA), "path": path("mara.json")})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
    assert!(refused.message.contains("only on the Higgsfield account that trained it"), "{}", refused.message);
    assert!(!dir.path().join("mara.json").exists(), "nothing was written");

    s.swap_resource(Some(nodecraft_engine::local_export::ExportHost::authorize(dir.path()).unwrap()));
    let destination = dir.path().display().to_string();
    let plan = s
        .dispatch(
            "export.local_plan",
            &json!({"canvas": C, "sources": [{"node": MARA, "port": "out"}, {"node": INK, "port": "out"}], "destination": destination}),
        )
        .unwrap();
    let entries = plan["entries"].as_array().unwrap();
    assert!(entries[0]["reason"].as_str().is_some_and(|r| r.contains("not a portable file")), "{plan}");
    assert!(!entries[1]["reason"].as_str().is_some_and(|r| r.contains("Soul ID")), "a portable style isn't refused as one: {plan}");
}

#[test]
fn a_soul_id_is_used_on_a_soul_2_card_as_its_soul_id_and_never_on_a_lora_model() {
    let mut s = session(None);
    let edge = nodecraft_types::EdgeId::from_bytes([40; 16]);
    let using = |style: NodeId, target: NodeId| json!({"canvas": C, "style": style, "target": target, "edge": edge});
    // style.list's verdicts agree with style.use.
    let listed = s.dispatch("style.list", &json!({"canvas": C, "for": SOUL})).unwrap();
    let verdict = |id: NodeId| listed["styles"].as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap()["compatible"].clone();
    assert_eq!((verdict(MARA), verdict(INK)), (json!(true), json!(false)), "{listed}");

    let used = s.dispatch("style.use", &using(MARA, SOUL)).unwrap();
    assert_eq!((used["param"].clone(), used["soul_id"].clone(), used["edge"].clone()), (json!("custom_reference_id"), json!(SOUL_ID), Json::Null), "{used}");
    assert_eq!(soul_param(&s), Some(Value::Text(SOUL_ID.into())));
    assert!(s.document().canvases().get(&C).unwrap().edges().is_empty(), "no wire: SOUL 2 takes a Soul ID as a parameter");
    assert_eq!(s.dispatch("style.use", &using(MARA, SOUL)).unwrap_err().kind, ErrorKind::Conflict);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(soul_param(&s), None, "one undo step");

    // Family checks keep a Soul ID off a LoRA model's style input and a LoRA off SOUL 2.
    let lora = s.dispatch("style.use", &using(MARA, FLUX)).unwrap_err();
    assert!(lora.message.contains("soul-2"), "{}", lora.message);
    assert!(s.dispatch("style.use", &using(INK, SOUL)).is_err());

    // An account whose list doesn't have it can't be given it.
    account(&mut s, &[OTHER_ID]);
    let elsewhere = s.dispatch("style.use", &using(MARA, SOUL)).unwrap_err();
    assert!(elsewhere.message.contains("Only the account that trained it can use it"), "{}", elsewhere.message);
    assert_eq!(soul_param(&s), None);
}

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms.min(5)));
            let _ = tx.send(());
        });
        Box::pin(async move {
            let _ = rx.await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// Another Higgsfield account, as the adapter sees it: what it says about the Soul ID.
struct Account {
    /// The reply to `GET /v1/custom-references/{id}`: its status and body.
    reference: (u16, Json),
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Account {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let (status, reply) = if r.url.contains("/estimate/") {
            (200, json!({"credits": "2", "usd": "0.10"}))
        } else if r.method == HttpMethod::Get && r.url.ends_with(&format!("/v1/custom-references/{SOUL_ID}")) {
            self.reference.clone()
        } else {
            panic!("nothing else may be sent before the Soul ID is confirmed: {:?} {}", r.method, r.url)
        };
        self.requests.lock().unwrap().push(r);
        let body = serde_json::to_vec(&reply).unwrap();
        Box::pin(async move { Ok(HttpResponse { status, headers: vec![], body }) })
    }
}

/// Saves `session`'s project as a `.ncraft` archive and opens it in a fresh session, as another
/// person (or the same person on another account) would.
fn reopen(session: &Session) -> Session {
    let bytes =
        nodecraft_format::encode(&nodecraft_format::Snapshot { project: session.document().clone(), assets: session.assets().clone(), epoch: 0 }).unwrap();
    let snapshot = nodecraft_format::decode(&bytes).unwrap();
    let catalog = GenerationCatalog::builtin().unwrap();
    let mut resources = Resources::default();
    resources.insert(catalog.clone());
    resources.insert(catalog.run_catalog());
    let mut reopened =
        Session::with_registries(snapshot.project, Registry::builtins().unwrap(), catalog.registry().unwrap()).unwrap().with_resources(resources);
    // The descriptors come back from the file, byte for byte.
    let mut scratch = Session::new(Project::new(ProjectId::from_bytes([9; 16]), "Scratch").unwrap()).unwrap();
    for bytes in [soul_descriptor(), flux_descriptor()] {
        let hash = scratch.import_asset(&bytes).unwrap();
        reopened.import_asset(snapshot.assets.get(&hash).expect("the descriptor is in the file")).unwrap();
    }
    reopened
}

/// Installs a run host and routes the SOUL 2 card to Higgsfield over `account`.
fn hosted(mut session: Session, account: Arc<Account>) -> (Session, RunHost) {
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "soul".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    session = session.with_services(services);
    let mut routing = ProviderRouting::new();
    let key = Secret::new("fixture-key-id-0002:fixture-secret-not-real-0002").unwrap();
    let binding = routing.bind_default(Arc::new(Higgsfield::new(key, account))).unwrap();
    routing.select(SOUL, "higgsfield", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    (session, host)
}

/// Runs the SOUL 2 card on `account`, accepting whatever the quote shows, and returns why it
/// failed and what was sent.
fn run_on(session: Session, account: Arc<Account>) -> (Session, Vec<RunNotice>) {
    let (mut session, mut host) = hosted(session, account);
    let request = json!({"canvas": C, "mode": "selected", "nodes": [SOUL]});
    let q = session.dispatch("run.quote", &request).unwrap();
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": q["complete"] != json!(true), "unpriced_jobs": if q["complete"] == json!(true) { 0 } else { 1 }, "amounts": amounts(&q)});
    session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    (session, notices)
}

fn amounts(q: &Json) -> Json {
    let mut amounts = serde_json::Map::new();
    for (currency, total) in q["totals"].as_object().into_iter().flatten() {
        amounts.insert(currency.clone(), total["micros"].clone());
    }
    Json::Object(amounts)
}

fn failure(notices: &[RunNotice]) -> String {
    notices
        .iter()
        .find_map(|n| match n {
            RunNotice::Failed { node, reason, .. } if *node == SOUL => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the SOUL 2 card should fail: {notices:?}"))
}

#[test]
fn a_project_opened_on_another_account_keeps_its_soul_id_and_says_it_is_unavailable_there() {
    let saved = session(Some(SOUL_ID));
    let mut s = reopen(&saved);
    assert_eq!(soul_param(&s), Some(Value::Text(SOUL_ID.into())), "the card's Soul ID survives the file");
    assert_eq!(entry(&styles(&mut s), MARA)["soul_id"], SOUL_ID, "and so does the trained identity");
    assert_eq!(nodecraft_engine::soul_ids::handles(s.document()), nodecraft_engine::soul_ids::Handles { identities: 1, uses: 1 });

    // The other account's list doesn't have it: the identities view says so, and the run is
    // refused before anything is quoted or sent, with the card's setting left as it was.
    account(&mut s, &[OTHER_ID]);
    assert_eq!(entry(&styles(&mut s), MARA)["available"], "unlisted");
    // An account that is never asked anything: the block comes before any request.
    let silent = Arc::new(Account { reference: (500, json!({})), requests: Mutex::default() });
    let (mut s, _host) = hosted(s, silent.clone());
    let quote = s.dispatch("run.quote", &json!({"canvas": C, "mode": "selected", "nodes": [SOUL]}));
    let refused = match quote {
        Ok(q) => q.to_string(),
        Err(e) => e.message,
    };
    assert!(refused.contains("\"allowed\":false") && refused.contains("not_an_option"), "{refused}");
    assert!(silent.requests.lock().unwrap().is_empty(), "nothing was sent to the other account");
    assert_eq!(soul_param(&s), Some(Value::Text(SOUL_ID.into())), "a refusal never clears the setting");
}

#[test]
fn without_a_fetched_list_the_adapter_refuses_a_soul_id_the_account_lacks_before_paying() {
    let cases = [
        ((404, json!({"detail": "Not found"})), "no such Soul ID; Soul IDs belong to the account that trained them"),
        ((200, json!({"id": SOUL_ID, "name": "Mara", "model_version": "v2", "status": "in_progress"})), "still training"),
        (
            (200, json!({"id": SOUL_ID, "name": "Mara", "model_version": "v2", "status": "failed", "fail_reason": "no face found"})),
            "training failed: no face found",
        ),
    ];
    for (reference, expected) in cases {
        // Opened with no Higgsfield list: nothing is known about this account yet.
        let mut s = reopen(&session(Some(SOUL_ID)));
        assert_eq!(entry(&styles(&mut s), MARA)["available"], "unchecked");
        let account = Arc::new(Account { reference, requests: Mutex::default() });
        let (s, notices) = run_on(s, account.clone());
        let reason = failure(&notices);
        assert!(reason.contains(expected) && reason.contains("Nothing was billed"), "{reason}");
        let sent = account.requests.lock().unwrap();
        assert!(sent.iter().all(|r| !r.url.ends_with("/higgsfield-ai/soul/v2/standard")), "the paid submit was never sent");
        assert!(sent.iter().any(|r| r.url.ends_with(&format!("/v1/custom-references/{SOUL_ID}"))), "the free check was");
        assert_eq!(soul_param(&s), Some(Value::Text(SOUL_ID.into())), "the card keeps its Soul ID");
        assert!(s.document().canvases().get(&C).unwrap().nodes().get(&MARA).unwrap().active.is_some(), "and the identity its card holds");
    }
}
