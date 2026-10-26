//! The shipping Azure card uses the ordinary generation planner and unknown-price admission.
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources},
    generation_catalog::GenerationCatalog,
    provider_routing::ProviderRouting,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_providers::llm::{ApiKey, Endpoint, LlmClient};
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, ProviderError};
use nodecraft_types::{CanvasId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct NoHttp;
impl Http for NoHttp {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("quote and admission never send provider requests")
    }
}

fn session(deployment: Option<&str>) -> (Session, Arc<AtomicUsize>, CanvasId, NodeId) {
    let canvas = CanvasId::from_bytes([45; 16]);
    let node = NodeId::from_bytes([46; 16]);
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut values: nodecraft_types::ParamValues =
        [("model".into(), Value::Text("azure-openai.deployment".into())), ("prompt".into(), Value::Text("A harbour".into()))].into();
    if let Some(deployment) = deployment {
        values.insert("deployment".into(), Value::Text(deployment.into()));
    }
    let card = kinds.get(nodecraft_nodes::generation::TEXT_KIND).unwrap().instantiate_instance(node, Default::default(), &values).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([47; 16]), "Azure public fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(nodecraft_engine::run_cmds::BudgetState { budgets: Default::default(), spent: Default::default() });
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    let mut routing = ProviderRouting::new();
    let provider = LlmClient::new(Endpoint::azure_openai("https://demo.openai.azure.com", ApiKey::new("public-fixture")).unwrap(), Arc::new(NoHttp));
    let binding = routing.bind_default(Arc::new(provider)).unwrap();
    routing.select(node, "azure-openai", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let calls = starts.clone();
    session
        .register_run_host(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"run": "fixture"}))
        })
        .unwrap();
    (session, starts, canvas, node)
}

#[test]
fn azure_card_requires_explicit_unknown_price_acceptance() {
    let (mut session, starts, canvas, node) = session(Some("public-chat"));
    let mut request = json!({"canvas": canvas, "mode": "selected", "nodes": [node]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], false, "not a free quote: {quote}");
    assert!(quote["steps"][0]["price"].is_null(), "{quote}");
    assert!(quote["steps"][0]["unpriced"].as_str().unwrap().contains("usage"));
    request["accept"] = json!({"token": quote["token"], "amounts": {}});
    assert!(session.dispatch("run.start", &request).is_err());
    request["accept"]["partial"] = json!(true);
    assert!(session.dispatch("run.start", &request).is_err());
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    request["accept"]["unpriced_jobs"] = json!(1);
    session.dispatch("run.start", &request).unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
}

#[test]
fn azure_missing_or_invalid_deployments_fail_before_price_consent() {
    for deployment in [None, Some(""), Some(" "), Some("path/name"), Some("two words"), Some("雪")] {
        let (mut session, starts, canvas, node) = session(deployment);
        let request = json!({"canvas":canvas,"mode":"selected","nodes":[node]});
        let error = session.dispatch("run.quote", &request).unwrap_err();
        assert_eq!(error.kind, nodecraft_types::ErrorKind::InvalidInput);
        assert!(error.message.contains("set an Azure deployment name"), "{error:?}");
        assert_eq!(starts.load(Ordering::SeqCst), 0, "invalid setup must not become a failed paid job");
    }
    let valid = "a".repeat(64);
    let (mut session, _, canvas, node) = session(Some(&valid));
    assert_eq!(session.dispatch("run.quote", &json!({"canvas":canvas,"mode":"selected","nodes":[node]})).unwrap()["allowed"], true);
}
