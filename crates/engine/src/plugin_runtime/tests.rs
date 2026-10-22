use super::*;
use crate::Session;
use crate::live_actions::{apply, compute, pending};
use nodecraft_plugin::json::{self, Json};
use nodecraft_plugin::signature::TrustStore;
use nodecraft_plugin_store::{MemoryBackend, archive};
use nodecraft_types::{CanvasId, EdgeId, NodeId};
use serde_json::json;
use std::collections::BTreeSet;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const TEXT: NodeId = NodeId::from_bytes([10; 16]);
const SHOUT_NODE: NodeId = NodeId::from_bytes([11; 16]);
const KIND: &str = "plugin.io.example.text.warm";
const SHOUT: &str = r#"{"id":"user.shout","inputs":[{"kind":"text","name":"words"}],"label":"Shout","outputs":[{"expr":"upper(words)","kind":"text","name":"out"}],"schema":1,"version":1}"#;

fn package(version: &str, definition: &str) -> Vec<u8> {
    let s = |t: &str| Json::Str(t.to_owned());
    let obj = |f: Vec<(&str, Json)>| Json::Object(f.into_iter().map(|(k, v)| (k.to_owned(), v)).collect::<BTreeMap<_, _>>());
    let manifest = json::canonical(&obj(vec![
        ("manifest_version", Json::Int(1)),
        ("package_id", s("io.example.text")),
        ("version", s(version)),
        ("host_protocol", obj(vec![("major", Json::Int(1)), ("min_minor", Json::Int(0)), ("max_minor", Json::Int(0))])),
        ("descriptor_contract", obj(vec![("major", Json::Int(1)), ("minor", Json::Int(0))])),
        ("license", s("MIT")),
        ("runtime", obj(vec![("kind", s("text_actions")), ("schema", Json::Int(1))])),
        ("dependencies", Json::Array(vec![])),
        ("capabilities", obj(vec![("required", Json::Array(vec![])), ("optional", Json::Array(vec![]))])),
        ("registrations", Json::Array(vec![obj(vec![("id", s("warm")), ("kind", s("action")), ("descriptor", s("d/warm.json"))])])),
    ]))
    .unwrap();
    archive::build(&[("manifest.json", manifest.as_bytes()), ("LICENSE", b"MIT\n"), ("d/warm.json", definition.as_bytes())], None).unwrap()
}

fn id() -> PackageId {
    PackageId::parse("io.example.text").unwrap()
}

/// A store holding `definition`'s package, granted and enabled.
fn granted(definition: &str) -> Store<MemoryBackend> {
    let mut store = Store::open(MemoryBackend::default()).unwrap();
    let trust = TrustStore::default();
    store.import(&package("1.0.0", definition), &trust, false).unwrap();
    store.grant(&id(), &trust).unwrap();
    store.enable(&id(), &trust, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    store
}

/// A canvas with a text source feeding the plugin's card.
fn session(plugins: PluginSnapshot) -> Session {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.set_plugins(plugins).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": TEXT, "kind": "source.text", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": TEXT, "name": "text", "value": Value::Text("hello".into())})).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": SHOUT_NODE, "kind": KIND, "pos": {"x": 400, "y": 0}})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([12; 16]), "from": {"node": TEXT, "port": "out"}, "to": {"node": SHOUT_NODE, "port": "words"}, "order": 0});
    s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    s
}

fn shouted(s: &Session) -> Option<Vec<Value>> {
    let node = s.document().canvases().get(&CANVAS)?.nodes().get(&SHOUT_NODE)?;
    node.history.iter().find(|g| Some(g.id) == node.active).map(|g| g.outputs.clone())
}

#[test]
fn a_granted_package_registers_its_card_and_runs_live() {
    let mut s = session(PluginSnapshot::of(&granted(SHOUT)));
    let kind = s.kinds().get(KIND).unwrap().descriptor();
    assert_eq!((kind.label.as_str(), kind.inputs.len(), kind.outputs.len()), ("Shout", 1, 1));
    let job = pending(&s, CANVAS, &BTreeMap::new()).unwrap().unwrap();
    let outcome = compute(job).unwrap();
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert!(apply(&mut s, outcome).unwrap());
    assert_eq!(shouted(&s), Some(vec![Value::Text("HELLO".into())]));
    assert!(!crate::cache::enabled(s.document().canvases().get(&CANVAS).unwrap().nodes().get(&SHOUT_NODE).unwrap(), s.kinds()), "never reused");
}

#[test]
fn only_enabled_granted_packages_are_published() {
    let mut store = Store::open(MemoryBackend::default()).unwrap();
    let trust = TrustStore::default();
    store.import(&package("1.0.0", SHOUT), &trust, false).unwrap();
    assert!(PluginSnapshot::of(&store).is_empty(), "imported only");
    store.grant(&id(), &trust).unwrap();
    assert!(PluginSnapshot::of(&store).is_empty(), "granted, not enabled");
    store.enable(&id(), &trust, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    let published = PluginSnapshot::of(&store);
    assert_eq!(published.get(KIND).map(|a| (a.stamp.grant_id.as_str(), a.stamp.epoch)), Some(("g1", 1)));
    store.disable(&id()).unwrap();
    assert!(PluginSnapshot::of(&store).is_empty(), "disabled");
}

#[test]
fn a_result_whose_grant_went_while_it_ran_is_dropped() {
    let store = granted(SHOUT);
    let mut s = session(PluginSnapshot::of(&store));
    let outcome = compute(pending(&s, CANVAS, &BTreeMap::new()).unwrap().unwrap()).unwrap();
    let ran = Arc::clone(s.plugins().get(KIND).unwrap());
    s.set_plugins(PluginSnapshot::default()).unwrap();
    assert!(ran.is_revoked());
    assert!(!apply(&mut s, outcome).unwrap(), "not recorded");
    assert_eq!(shouted(&s), None);
    assert!(s.kinds().get(KIND).is_err(), "the card's kind is gone");
    assert!(s.kinds().get("text.concat").is_ok(), "built-in kinds stay");
    // A job that starts on the revoked action stops at once.
    let routed = Routed {
        local: Arc::new(crate::action_exec::LocalActions::new(Default::default())),
        plugins: Some(Arc::new(PluginSnapshot { actions: BTreeMap::from([(KIND.to_owned(), ran)]) })),
    };
    let error = routed.execute(KIND, &ParamValues::new(), &[Value::Text("hi".into())]).unwrap_err();
    assert!(error.contains("revoked"), "{error}");
}

#[test]
fn an_unchanged_grant_keeps_the_actions_running_jobs_hold() {
    let store = granted(SHOUT);
    let mut s = session(PluginSnapshot::of(&store));
    let held = Arc::clone(s.plugins().get(KIND).unwrap());
    s.set_plugins(PluginSnapshot::of(&store)).unwrap();
    assert!(Arc::ptr_eq(&held, s.plugins().get(KIND).unwrap()));
    // An update revokes the grant: a fresh one is another stamp, and the old action is revoked.
    let mut store = store;
    let trust = TrustStore::default();
    store.disable(&id()).unwrap();
    store.update(&package("1.0.1", SHOUT), &trust, false).unwrap();
    store.grant(&id(), &trust).unwrap();
    store.enable(&id(), &trust, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    s.set_plugins(PluginSnapshot::of(&store)).unwrap();
    assert!(held.is_revoked());
    assert_eq!(s.plugins().get(KIND).map(|a| (a.stamp.grant_id.as_str(), a.is_revoked())), Some(("g2", false)));
}

#[test]
fn list_inputs_and_parameters_reach_the_definition() {
    let definition = r#"{"id":"user.glue","inputs":[{"kind":"text","name":"head"},{"kind":"text_list","name":"items"}],"label":"Glue",
        "outputs":[{"expr":"if loud then upper(head + join(items, sep)) else head + join(items, sep)","kind":"text","name":"out"}],
        "params":[{"default":"-","kind":"text","name":"sep"},{"default":false,"kind":"bool","name":"loud"}],"schema":1,"version":1}"#;
    let plugins = PluginSnapshot::of(&granted(definition));
    let kinds = plugins.kinds();
    let descriptor = kinds.first().unwrap().descriptor();
    assert_eq!(descriptor.inputs.iter().map(|p| p.port_type.many).collect::<Vec<_>>(), [false, true]);
    assert_eq!(descriptor.params.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["sep", "loud"]);
    let routed = Routed { local: Arc::new(crate::action_exec::LocalActions::new(Default::default())), plugins: Some(Arc::new(plugins)) };
    let inputs = [Value::Text(">".into()), Value::Text("a".into()), Value::List(vec![Value::Text("b".into()), Value::Text("c".into())])];
    let params = ParamValues::from([("sep".to_owned(), Value::Text("+".into())), ("loud".to_owned(), Value::Bool(true))]);
    assert_eq!(routed.execute(KIND, &params, &inputs), Ok(Value::Text(">A+B+C".into())));
    assert!(routed.execute(KIND, &params, &[Value::Integer(Integer(1)), Value::Text("a".into())]).is_err(), "only text");
    assert!(routed.long_running(KIND));
    assert_eq!(routed.execute("plugin.io.example.other.x", &params, &inputs), Err("that plugin action isn't granted to run".into()));
    let pass = ParamValues::from([("find".to_owned(), Value::Text(String::new()))]);
    assert_eq!(routed.execute("text.replace", &pass, &inputs[1..2]), Ok(Value::Text("a".into())), "the host's own actions still run");
}

#[test]
fn a_list_input_before_the_last_is_refused_when_enabled() {
    let definition = r#"{"id":"user.bad","inputs":[{"kind":"text_list","name":"items"},{"kind":"text","name":"head"}],"label":"Bad",
        "outputs":[{"expr":"head","kind":"text","name":"out"}],"schema":1,"version":1}"#;
    let mut store = Store::open(MemoryBackend::default()).unwrap();
    store.import(&package("1.0.0", definition), &TrustStore::default(), false).unwrap();
    let error = crate::plugin_text::compile(&store.load(&id()).unwrap()).unwrap_err();
    assert!(error.message.contains("last input"), "{}", error.message);
}

#[test]
fn every_compiled_definition_makes_a_card_that_runs() {
    let compiled = |definition: &str| {
        let mut store = Store::open(MemoryBackend::default()).unwrap();
        store.import(&package("1.0.0", definition), &TrustStore::default(), false).unwrap();
        crate::plugin_text::compile(&store.load(&id()).unwrap())
    };
    let pad = |label: &str, default: &str| {
        format!(
            r#"{{"id":"user.pad","inputs":[{{"kind":"text","name":"words"}}],"label":"{label}","outputs":[{{"expr":"words + pad","kind":"text","name":"out"}}],"params":[{{"default":"{default}","kind":"text","name":"pad"}}],"schema":1,"version":1}}"#
        )
    };
    // A long text default is its definition's to bound, not the card's: it registers.
    let s = session(PluginSnapshot::of(&granted(&pad("Pad", &"x".repeat(4000)))));
    assert!(s.kinds().get(KIND).is_ok(), "the card registered");
    // An action with no inputs could never run on a card.
    let none = r#"{"id":"user.none","inputs":[],"label":"None","outputs":[{"expr":"\"hi\"","kind":"text","name":"out"}],"schema":1,"version":1}"#;
    let error = compiled(none).unwrap_err();
    assert!(error.message.contains("at least one input"), "{}", error.message);
}
