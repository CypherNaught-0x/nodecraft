use super::*;
use crate::commands::{Phase, Services};
use crate::run_cmds::{BudgetState, PreparedRun};
use nodecraft_exec::cost::{Budgets, Spent};
use nodecraft_graph::{Change, Project};
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use serde_json::{Value as Json, json};
use std::sync::{Arc, Mutex};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const CARD: NodeId = NodeId::from_bytes([2; 16]);
const WORKFLOW: &str =
    r#"{"1":{"class_type":"PartnerImage","inputs":{"prompt":"A public-safe fox"}},"2":{"class_type":"SaveImage","inputs":{"images":["1",0]}}}"#;

fn catalog(paid: bool) -> NodeCatalog {
    NodeCatalog::parse(
        serde_json::to_string(&json!({
            "PartnerImage":{"input":{"required":{"prompt":["STRING",{}]}},"output":["IMAGE"],"api_node":paid},
            "SaveImage":{"input":{"required":{"images":["IMAGE",{}]}},"output":[],"output_node":true}
        }))
        .unwrap()
        .as_bytes(),
    )
    .unwrap()
    .0
}

struct Offline;
impl nodecraft_providers::Provider for Offline {
    fn id(&self) -> &str {
        "comfyui"
    }
    fn submit<'a>(
        &'a self,
        _: &'a nodecraft_providers::ProviderJob,
    ) -> nodecraft_providers::BoxFuture<'a, Result<nodecraft_providers::Submission, nodecraft_providers::ProviderError>> {
        Box::pin(async { panic!("quote tests never submit") })
    }
    fn poll<'a>(
        &'a self,
        _: &'a nodecraft_providers::JobHandle,
    ) -> nodecraft_providers::BoxFuture<'a, Result<nodecraft_providers::JobProgress, nodecraft_providers::ProviderError>> {
        Box::pin(async { panic!("quote tests never poll") })
    }
    fn cancel<'a>(&'a self, _: &'a nodecraft_providers::JobHandle) -> nodecraft_providers::BoxFuture<'a, Result<(), nodecraft_providers::ProviderError>> {
        Box::pin(async { panic!("quote tests never cancel") })
    }
}

type Started = Arc<Mutex<Vec<PreparedRun>>>;
fn session(server: Option<&str>) -> (Session, Started) {
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Legacy workflow").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    // A pre-partner mapping: the saved workflow uses the class, but its mapping has no partners.
    crate::comfy_workflow_cmds::finish(&mut session, 1, Ok(Arc::new(catalog(false)).into()));
    session.dispatch("comfy.workflow.import", &json!({"canvas":CANVAS,"id":CARD,"pos":{"x":0,"y":0},"workflow":WORKFLOW})).unwrap();
    let node = session.document().canvases().get(&CANVAS).unwrap().nodes().get(&CARD).unwrap();
    assert!(comfy_workflow::mapping(node).unwrap().partners.is_empty());
    session.swap_resource(Some(BudgetState { budgets: Budgets::default(), spent: Spent::default() }));
    let mut routing = crate::provider_routing::ProviderRouting::new();
    let binding = match server {
        Some(name) => routing.bind_named(Arc::new(Offline), crate::provider_setup::comfy_instance(name).unwrap()).unwrap(),
        None => routing.bind_default(Arc::new(Offline)).unwrap(),
    };
    routing.select(CARD, "comfyui", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    let started: Started = Arc::default();
    let received = started.clone();
    let mut services = Services::default();
    services
        .register_run_host(move |prepared| {
            received.lock().unwrap().push(prepared);
            Ok(json!({"accepted":true}))
        })
        .unwrap();
    (session.with_services(services), started)
}

fn request() -> Json {
    json!({"canvas":CANVAS,"mode":"selected","nodes":[CARD]})
}
fn quote(session: &mut Session) -> Json {
    session.dispatch("run.quote", &request()).unwrap()
}
fn start(quote: &Json, partial: bool) -> Json {
    let mut request = request();
    request["accept"] = json!({"token":quote["token"],"amounts":{},"partial":partial,"unpriced_jobs":1});
    request
}

#[test]
fn legacy_partner_quotes_refresh_without_edits_and_admission_requires_partial_consent() {
    let (mut session, started) = session(None);
    let before = quote(&mut session);
    assert_eq!(before["complete"], true, "{before}");
    let document = session.document().clone();
    let revision = session.revision();
    crate::comfy_workflow_cmds::finish(&mut session, 2, Ok(Arc::new(catalog(true)).into()));
    let after = quote(&mut session);
    assert_eq!(after["complete"], false, "{after}");
    assert!(after["steps"][0]["unpriced"].as_str().unwrap().contains("comfy.org credits"));
    assert_ne!(before["token"], after["token"]);
    assert_eq!(session.document(), &document);
    assert_eq!(session.revision(), revision);
    assert!(session.dispatch("run.start", &start(&before, true)).unwrap_err().message.contains("changed since quotation"));
    assert!(session.dispatch("run.start", &start(&after, false)).is_err(), "the admitted quote must also be unpriced");
    session.dispatch("run.start", &start(&after, true)).unwrap();
    assert_eq!(started.lock().unwrap().len(), 1);
    // Evidence changing between preparation and authorization also invalidates the work.
    let prepared = started.lock().unwrap().pop().unwrap();
    crate::comfy_workflow_cmds::forget(&mut session);
    let options = nodecraft_exec::run::RunOptions { id: [4; 16], first_job: 1, limits: Default::default(), retry: Default::default() };
    assert!(session.authorize_prepared_run(prepared, options).is_err());
}

#[test]
fn named_server_evidence_never_leaks_to_another_server_and_failed_fetch_retains_it() {
    let (mut session, _) = session(Some("studio"));
    let initial = quote(&mut session);
    crate::comfy_server_cmds::finish(&mut session, 2, "laptop", true, Ok(Arc::new(catalog(true)).into()));
    assert_eq!(quote(&mut session)["complete"], true, "default/laptop evidence cannot classify studio's classes");
    assert_eq!(quote(&mut session)["token"], initial["token"], "an unrelated profile must not invalidate open consent");
    crate::comfy_server_cmds::finish(&mut session, 3, "studio", false, Ok(Arc::new(catalog(true)).into()));
    let paid = quote(&mut session);
    assert_eq!(paid["complete"], false);
    crate::comfy_server_cmds::finish(&mut session, 4, "studio", false, Err(Error::new(ErrorKind::Network, "offline")));
    assert_eq!(quote(&mut session)["token"], paid["token"], "failed fetch leaves last evidence installed");
    crate::comfy_server_cmds::forget(&mut session, "laptop", true);
    assert_eq!(quote(&mut session)["complete"], false, "forgetting another server preserves studio's evidence");
    assert_eq!(quote(&mut session)["token"], paid["token"], "unrelated forget also preserves consent");
    crate::comfy_server_cmds::forget(&mut session, "studio", false);
    assert_eq!(quote(&mut session)["complete"], true);
    crate::comfy_server_cmds::finish(&mut session, 5, "studio", false, Ok(Arc::new(catalog(false)).into()));
    assert_eq!(quote(&mut session)["complete"], true, "known local-only nodes remain free");
}

#[test]
fn identical_and_unrelated_refreshes_preserve_consent_and_prepared_authority() {
    for selected in [None, Some("studio")] {
        let (mut session, started) = session(selected);
        let default = selected.is_none();
        // A default-server check installs both its named and unnamed alias.
        crate::comfy_server_cmds::finish(&mut session, 2, "studio", default, Ok(Arc::new(catalog(true)).into()));
        let accepted = quote(&mut session);
        assert_eq!(accepted["complete"], false);
        for request in 3..6 {
            crate::comfy_server_cmds::finish(&mut session, request, "studio", default, Ok(Arc::new(catalog(true)).into()));
            crate::comfy_server_cmds::finish(&mut session, request + 10, "laptop", false, Ok(Arc::new(catalog(request % 2 == 0)).into()));
            assert_eq!(quote(&mut session)["token"], accepted["token"], "same evidence and unrelated server changes preserve consent");
        }
        session.dispatch("run.start", &start(&accepted, true)).unwrap();
        let prepared = started.lock().unwrap().pop().unwrap();
        crate::comfy_server_cmds::finish(&mut session, 6, "studio", default, Ok(Arc::new(catalog(true)).into()));
        crate::comfy_server_cmds::forget(&mut session, "laptop", false);
        let options = nodecraft_exec::run::RunOptions { id: [4; 16], first_job: 1, limits: Default::default(), retry: Default::default() };
        assert!(session.authorize_prepared_run(prepared, options).is_ok(), "harmless refreshes after preparation preserve authority");
    }
}

#[test]
fn evidence_binding_depends_on_current_contents_instead_of_refresh_history() {
    let (mut session, _) = session(None);
    let free = quote(&mut session);
    crate::comfy_workflow_cmds::finish(&mut session, 2, Ok(Arc::new(catalog(false)).into()));
    assert_eq!(quote(&mut session)["token"], free["token"], "a repeated empty partner list is stable too");
    crate::comfy_workflow_cmds::finish(&mut session, 3, Ok(Arc::new(catalog(true)).into()));
    let paid = quote(&mut session);
    assert_ne!(paid["token"], free["token"]);
    crate::comfy_workflow_cmds::forget(&mut session);
    assert_ne!(quote(&mut session)["token"], paid["token"], "forgetting selected evidence still invalidates consent");
    crate::comfy_workflow_cmds::finish(&mut session, 4, Ok(Arc::new(catalog(true)).into()));
    assert_eq!(quote(&mut session)["token"], paid["token"], "reinstalling the same evidence restores its binding");
}

#[test]
fn a_budget_cap_refuses_legacy_partner_work_before_the_host_receives_it() {
    let (mut session, started) = session(None);
    crate::comfy_workflow_cmds::finish(&mut session, 2, Ok(Arc::new(catalog(true)).into()));
    session.swap_resource(Some(BudgetState {
        budgets: Budgets { per_call: Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1_000_000)), ..Budgets::default() },
        spent: Spent::default(),
    }));
    let quote = quote(&mut session);
    assert_eq!(quote["allowed"], false);
    assert!(session.dispatch("run.start", &start(&quote, true)).is_err());
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn partner_evidence_has_bounded_retention_and_saturates_conservatively() {
    let mut json = serde_json::Map::new();
    for n in 0..MAX_CLASSES + 1 {
        json.insert(format!("Partner{n}"), json!({"input":{},"output":[],"api_node":true}));
    }
    let catalog = NodeCatalog::parse(serde_json::to_string(&json).unwrap().as_bytes()).unwrap().0;
    let classes = Classes::read(&catalog);
    assert_eq!(classes.hashes.len(), MAX_CLASSES);
    assert!(classes.incomplete);
    assert!(classes.contains("anything"), "overflow must never classify a paid workflow as free");
    let (mut session, _) = session(None);
    for n in 0..crate::settings::MAX_COMFY_SERVERS + 5 {
        keep(&mut session, crate::provider_setup::comfy_instance(&format!("server{n}")), &catalog);
    }
    let context = session.context(Phase::Preview);
    assert_eq!(context.resources.get::<Known>().unwrap().servers.len(), crate::settings::MAX_COMFY_SERVERS + 1);
}
