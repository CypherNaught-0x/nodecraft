use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, NodeId, ProjectId, Value};
use serde_json::json;
use std::collections::BTreeMap;

fn setup() -> (Session, CanvasId, NodeId) {
    let canvas = CanvasId::from_bytes([1; 16]);
    let id = NodeId::from_bytes([2; 16]);
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Editing").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let node = kinds.get("source.text").unwrap().descriptor().instantiate(id, &BTreeMap::new()).unwrap();
    assert!(node.title.is_empty());
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    (Session::new(project).unwrap(), canvas, id)
}
#[test]
fn title_and_parameter_commands_are_reversible_and_preserve_defaults() {
    let (mut session, canvas, id) = setup();
    let original = session.document().clone();
    for (command, params) in [
        ("node.title", json!({"canvas":canvas,"id":id,"title":"Custom title"})),
        ("node.param_set", json!({"canvas":canvas,"id":id,"name":"text","value":Value::Text("Prompt".into())})),
        ("node.param_set", json!({"canvas":canvas,"id":id,"name":"text","value":null})),
    ] {
        session.enabled(command, &params).unwrap();
        session.dispatch(command, &params).unwrap();
    }
    let node = session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
    assert!(!node.params.contains_key("text"));
    assert_eq!(session.kinds().get(&node.kind).unwrap().descriptor().validate_node(node, false).unwrap().get("text"), Some(&Value::Text(String::new())));
    let changed = session.document().clone();
    for _ in 0..3 {
        session.dispatch("edit.undo", &json!({})).unwrap();
    }
    assert_eq!(session.document(), &original);
    for _ in 0..3 {
        session.dispatch("edit.redo", &json!({})).unwrap();
    }
    assert_eq!(session.document(), &changed);
}
#[test]
fn malformed_or_schema_incompatible_edits_preserve_history_and_document() {
    let (mut session, canvas, id) = setup();
    let original = session.document().clone();
    for params in [
        json!({"canvas":canvas,"id":id,"name":"text"}),
        json!({"canvas":canvas,"id":id,"name":"unknown","value":null}),
        json!({"canvas":canvas,"id":id,"name":"text","value":Value::Bool(true)}),
        json!({"canvas":canvas,"id":id,"name":"text","value":Value::List(vec![])}),
        json!({"canvas":canvas,"id":id,"name":"text","value":Value::Text("x".repeat(1024*1024+1))}),
    ] {
        assert!(session.enabled("node.param_set", &params).is_err());
        assert!(session.dispatch("node.param_set", &params).is_err());
    }
    assert!(session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":"x".repeat(4097)})).is_err());
    assert_eq!(session.document(), &original);
    assert_eq!(session.revision(), 0);
    assert!(!session.can_undo());
}

#[test]
fn command_schemas_cover_typed_scalar_and_configuration_wire_values() {
    use nodecraft_types::{Integer, Number, Unsigned};
    let (session, _, _) = setup();
    let add = (session.registry().get("node.add").unwrap().input_schema)();
    assert_eq!(add["properties"]["node"]["properties"]["config"]["maxProperties"], 256);
    let parameter = (session.registry().get("node.param_set").unwrap().input_schema)();
    assert!(parameter["required"].as_array().unwrap().contains(&json!("value")));
    for value in [
        Value::Bool(true),
        Value::Number(Number::new(0.75).unwrap()),
        Value::Integer(Integer(i64::MIN)),
        Value::Unsigned(Unsigned(u64::MAX)),
        Value::Text("text".into()),
        Value::List(vec![]),
    ] {
        let wire = serde_json::to_value(value).unwrap();
        for schema in [&add, &parameter] {
            assert!(schema["$defs"]["value"]["oneOf"].as_array().unwrap().iter().any(|branch| branch["properties"]["type"]["const"] == wire["type"]));
        }
    }
}

#[test]
fn dynamic_instance_schema_drives_parameter_commands_and_retains_config() {
    use nodecraft_nodes::{JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{Error, ParamControl, ParamSpec, PortType};
    use std::borrow::Cow;
    struct Dynamic(NodeDescriptor);
    impl NodeKind for Dynamic {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn instance<'a>(&'a self, node: &nodecraft_graph::Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            let mut descriptor = self.0.clone();
            if node.config.contains_key("source.text.v1.toggle") {
                descriptor.params.params.push(ParamSpec {
                    name: "enabled".into(),
                    label: "Enabled".into(),
                    description: Some("Instance toggle".into()),
                    control: ParamControl::Toggle,
                    required: false,
                    default: Some(Value::Bool(false)),
                    visible_if: None,
                });
            }
            Ok(Cow::Owned(descriptor))
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }
    let (mut session, canvas, id) = setup();
    let mut document = session.document().clone();
    let mut node = document.canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().clone();
    node.config.insert("source.text.v1.toggle".into(), Value::Bool(true));
    Change::SetNode { canvas, node }.apply(&mut document).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Dynamic(nodecraft_nodes::text::kind().descriptor().clone()))).unwrap();
    session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let before = session.document().clone();
    session.dispatch("node.param_set", &json!({"canvas":canvas,"id":id,"name":"enabled","value":Value::Bool(true)})).unwrap();
    assert!(session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().config.contains_key("source.text.v1.toggle"));
    session.dispatch("edit.undo", &json!(null)).unwrap();
    assert_eq!(session.document(), &before);
}
