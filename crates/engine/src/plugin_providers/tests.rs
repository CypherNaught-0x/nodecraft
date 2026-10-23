use super::*;
use crate::Session;
use crate::plugin_runtime::PluginSnapshot;
use crate::provider_setup::Readiness;
use crate::secrets::{MemoryStore, Secret};
use futures::executor::block_on;
use nodecraft_plugin::id::PackageId;
use nodecraft_plugin::json::{self, Json};
use nodecraft_plugin::signature::TrustStore;
use nodecraft_plugin_store::grants::Rights;
use nodecraft_plugin_store::{MemoryBackend, Store, archive};
use nodecraft_providers::{BoxFuture, HttpRequest, HttpResponse, JobInput, ProviderError, ProviderJob};
use nodecraft_types::{CanvasId, Currency, Money, NodeId, Value};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const CARD: NodeId = NodeId::from_bytes([10; 16]);
const PROVIDER: &str = "plugin.io.example.acme.acme";
const MODEL: &str = "plugin.io.example.acme.acme/acme/v1";
const ORIGIN: &str = "https://api.acme.example";

fn template(models: serde_json::Value) -> String {
    json!({
        "schema": 1,
        "label": "Acme Images",
        "origin": ORIGIN,
        "output_origins": ["https://cdn.acme.example"],
        "credential": "bearer",
        "idempotency": "idempotency-key",
        "models": models,
        "submit": {"method": "POST", "path": "/v1/jobs", "body": {"model": "{model}", "prompt": "{text:prompt}", "style": "{text:style}"}, "handle": "/id"},
        "poll": {
            "method": "GET",
            "path": "/v1/jobs/{job}",
            "status": "/status",
            "states": {"queued": ["queued"], "running": ["running"], "done": ["succeeded"], "failed": ["failed"]},
            "outputs": [{"at": "/output/images", "kind": "url", "mime": "image/png"}],
            "interval_ms": 2000
        },
        "limits": {"response_bytes": 65536, "timeout_ms": 30000, "concurrency": 2}
    })
    .to_string()
}

fn priced(micros: &str) -> String {
    template(json!([{"id": "acme/v1", "label": "Acme v1", "price": {"currency": "USD", "micros": micros}}]))
}

fn package(version: &str, descriptor: &str) -> Vec<u8> {
    let s = |t: &str| Json::Str(t.to_owned());
    let obj = |f: Vec<(&str, Json)>| Json::Object(f.into_iter().map(|(k, v)| (k.to_owned(), v)).collect::<BTreeMap<_, _>>());
    let manifest = json::canonical(&obj(vec![
        ("manifest_version", Json::Int(1)),
        ("package_id", s("io.example.acme")),
        ("version", s(version)),
        ("host_protocol", obj(vec![("major", Json::Int(1)), ("min_minor", Json::Int(0)), ("max_minor", Json::Int(0))])),
        ("descriptor_contract", obj(vec![("major", Json::Int(1)), ("minor", Json::Int(0))])),
        ("license", s("MIT")),
        ("runtime", obj(vec![("kind", s("provider_operations")), ("schema", Json::Int(1))])),
        ("dependencies", Json::Array(vec![])),
        ("capabilities", obj(vec![("required", Json::Array(vec![s("network"), s("secret")])), ("optional", Json::Array(vec![]))])),
        ("registrations", Json::Array(vec![obj(vec![("id", s("acme")), ("kind", s("provider")), ("descriptor", s("d/acme.json"))])])),
    ]))
    .unwrap();
    archive::build(&[("manifest.json", manifest.as_bytes()), ("LICENSE", b"MIT\n"), ("d/acme.json", descriptor.as_bytes())], None).unwrap()
}

fn id() -> PackageId {
    PackageId::parse("io.example.acme").unwrap()
}

fn rights(origins: &[&str]) -> Rights {
    use nodecraft_plugin::manifest::Capability;
    Rights { capabilities: [Capability::Network, Capability::Secret].into(), origins: origins.iter().map(|o| (*o).to_owned()).collect() }
}

/// A store holding the provider package with `descriptor`, granted `origins` and enabled.
fn granted(descriptor: &str, origins: &[&str]) -> Store<MemoryBackend> {
    let mut store = Store::open(MemoryBackend::default()).unwrap();
    let trust = TrustStore::default();
    store.import(&package("1.0.0", descriptor), &trust, false).unwrap();
    store.grant_rights(&id(), None, &trust, &rights(origins)).unwrap();
    store.enable(&id(), &trust, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    store
}

fn session(plugins: PluginSnapshot) -> Session {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.set_plugins(plugins).unwrap();
    s
}

#[test]
fn a_granted_provider_offers_its_models_on_the_card_for_their_output() {
    let mut s = session(PluginSnapshot::of(&granted(&priced("40000"), &[ORIGIN, "https://cdn.acme.example"])));
    let catalog = s.generation_catalog().unwrap();
    let card = catalog.model(nodecraft_nodes::generation::KIND, MODEL).expect("on the image card");
    assert_eq!((card.provider.as_str(), card.remote.as_str()), (PROVIDER, "acme/v1"));
    let model = catalog.run_catalog().catalog().get(MODEL).cloned().unwrap();
    assert_eq!(model.label, "Acme v1");
    assert!(catalog.model(nodecraft_nodes::generation::TEXT_KIND, MODEL).is_none(), "it makes no text");
    assert!(catalog.model(nodecraft_nodes::generation::KIND, "fal.flux-schnell").is_some(), "built-in models stay");
    let mode = model.modes.first().unwrap();
    let roles: Vec<_> = mode.inputs.iter().map(|r| (r.role.as_str(), r.min, r.inline)).collect();
    assert_eq!(roles, [("prompt", 1, true), ("style", 0, false)]);

    // A card can pick it, and its quote is the template's price.
    s.swap_resource(Some(crate::run_cmds::BudgetState { budgets: Default::default(), spent: Default::default() }));
    s.swap_resource(Some(crate::run_cmds::UsageSnapshot(Arc::new(Default::default()))));
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": CARD, "kind": nodecraft_nodes::generation::KIND, "pos": {"x": 0, "y": 0}})).unwrap();
    let preview = s.dispatch("node.model_preview", &json!({"canvas": CANVAS, "nodes": [CARD], "model": MODEL})).unwrap();
    let commit = json!({"canvas": CANVAS, "nodes": [CARD], "model": MODEL, "expected_revision": s.revision(), "preview_binding": preview["binding"]});
    s.dispatch("node.set_model", &commit).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": CARD, "name": "prompt", "value": Value::Text("a red fox".into())})).unwrap();
    // The host routes the card to the plugin's broker as it would any adapter.
    let keys = MemoryStore::default();
    keys.set(&Account::new("plugin.io.example.acme:api.acme.example").unwrap(), &Secret::new("plugin-fixture-key-not-real").unwrap()).unwrap();
    let provider = Arc::clone(s.plugins().provider(PROVIDER).unwrap());
    let mut setup = crate::provider_setup::ProviderSetup::default();
    assert_eq!(setup.ensure(PROVIDER, |a| broker(a, &provider, &keys, Arc::new(Counting::default()))), &Readiness::Ready);
    s.set_provider_routing(setup.routing([(CARD, PROVIDER)]).unwrap()).unwrap();
    let quote = s.dispatch("run.quote", &json!({"canvas": CANVAS, "mode": "selected", "nodes": [CARD]})).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], json!(40000), "{quote}");

    // A card left on a withdrawn plugin's model keeps it, and runs nothing.
    s.set_plugins(PluginSnapshot::default()).unwrap();
    let card = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&CARD).unwrap();
    assert_eq!(card.params.get("model"), Some(&Value::Text(MODEL.into())));
    match s.dispatch("run.quote", &json!({"canvas": CANVAS, "mode": "selected", "nodes": [CARD]})) {
        Ok(quote) => assert_eq!(quote["allowed"], false, "{quote}"),
        Err(error) => assert!(!error.message.is_empty()),
    }
}

#[test]
fn a_provider_reaching_past_its_grant_is_not_published() {
    // The grant covers the API origin but not where the outputs come from.
    let store = granted(&priced("40000"), &[ORIGIN]);
    let snapshot = PluginSnapshot::of(&store);
    assert!(snapshot.provider(PROVIDER).is_none());
    assert!(snapshot.models().is_empty());
}

#[test]
fn a_withdrawn_grant_takes_the_models_away_and_its_broker_sends_nothing() {
    let store = granted(&priced("40000"), &[ORIGIN, "https://cdn.acme.example"]);
    let mut s = session(PluginSnapshot::of(&store));
    let held = Arc::clone(s.plugins().provider(PROVIDER).unwrap());
    let keys = MemoryStore::default();
    let account = held.key_account().unwrap();
    assert_eq!(account, "plugin.io.example.acme:api.acme.example");
    keys.set(&Account::new(&account).unwrap(), &Secret::new("plugin-fixture-key-not-real").unwrap()).unwrap();
    let http = Arc::new(Counting::default());
    let broker = broker(PROVIDER, &held, &keys, http.clone()).unwrap();

    // The same grant again keeps the provider runs hold.
    s.set_plugins(PluginSnapshot::of(&store)).unwrap();
    assert!(Arc::ptr_eq(&held, s.plugins().provider(PROVIDER).unwrap()));
    s.set_plugins(PluginSnapshot::default()).unwrap();
    assert!(held.is_revoked());
    assert!(s.generation_catalog().unwrap().model(nodecraft_nodes::generation::KIND, MODEL).is_none(), "its models are gone");
    assert!(s.generation_catalog().unwrap().model(nodecraft_nodes::generation::KIND, "fal.flux-schnell").is_some(), "built-in models stay");

    let job = ProviderJob {
        model: "acme/v1".into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "a red fox".into() }],
        idempotency_key: "nc-abc123".into(),
    };
    let error = block_on(broker.submit(&job)).unwrap_err();
    assert!(error.message.contains("withdrawn"), "{}", error.message);
    assert_eq!(*http.0.lock().unwrap(), 0, "nothing was sent");
}

#[test]
fn a_broker_needs_the_plugins_own_key() {
    let store = granted(&priced("40000"), &[ORIGIN, "https://cdn.acme.example"]);
    let snapshot = PluginSnapshot::of(&store);
    let provider = snapshot.provider(PROVIDER).unwrap();
    let keys = MemoryStore::default();
    // A key kept for another account is not this plugin's.
    keys.set(&Account::new("plugin.io.example.other:api.acme.example").unwrap(), &Secret::new("plugin-fixture-key-not-real").unwrap()).unwrap();
    match broker(PROVIDER, provider, &keys, Arc::new(Counting::default())) {
        Err(Readiness::NeedsKey { account }) => assert_eq!(account, "plugin.io.example.acme:api.acme.example"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("built without a key"),
    }
}

#[test]
fn a_free_or_malformed_price_never_reaches_the_catalog() {
    for micros in ["0", "-1", "abc"] {
        let store = granted(&priced(micros), &[ORIGIN, "https://cdn.acme.example"]);
        assert!(PluginSnapshot::of(&store).models().is_empty(), "{micros}");
    }
    let money = Money::new(Currency::USD, 40000);
    let store = granted(&priced("40000"), &[ORIGIN, "https://cdn.acme.example"]);
    let models = PluginSnapshot::of(&store).models();
    assert!(matches!(models.first().map(|m| &m.pricing), Some(nodecraft_catalog::Pricing::PerCall { price }) if price.0 == money));
}

#[test]
fn a_package_the_catalog_cannot_hold_leaves_the_others_published() {
    let store = granted(&priced("40000"), &[ORIGIN, "https://cdn.acme.example"]);
    let s = session(PluginSnapshot::default());
    let good = PluginSnapshot::of(&store).models();
    let model = good.first().cloned().unwrap();
    let huge: Vec<_> = (0..nodecraft_catalog::MAX_MODELS)
        .map(|i| nodecraft_catalog::ModelDescriptor {
            id: format!("plugin.io.example.huge.x/m{i}"),
            provider: "plugin.io.example.huge.x".into(),
            ..model.clone()
        })
        .collect();
    let catalog = s.generation_catalog().unwrap().with_plugin_models(vec![huge, good]).unwrap();
    assert!(catalog.model(nodecraft_nodes::generation::KIND, MODEL).is_some(), "the other package stays");
    assert!(catalog.model(nodecraft_nodes::generation::KIND, "plugin.io.example.huge.x/m0").is_none());
    assert!(catalog.model(nodecraft_nodes::generation::KIND, "fal.flux-schnell").is_some(), "built-in models stay");
}

/// A transport that counts what it is asked to send and answers nothing.
#[derive(Default)]
struct Counting(Mutex<usize>);
impl nodecraft_providers::http::Http for Counting {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        *self.0.lock().unwrap() += 1;
        Box::pin(async { Err(ProviderError::new(nodecraft_providers::ErrorKind::Network, "offline")) })
    }
}
