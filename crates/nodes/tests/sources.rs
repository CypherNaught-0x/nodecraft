use nodecraft_graph::{Canvas, Change, Changeset, Project};
use nodecraft_nodes::{InputValue, JobSpec, NodeDescriptor, NodeKind, ParamValues, PlanCtx, Registry};
use nodecraft_types::{CanvasId, Error, ErrorKind, Modality, NodeId, PortType, ProjectId, Value};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    kind: String,
    value: Value,
}

#[test]
fn golden_source_graph_plans_existing_values_and_round_trips_through_undo() {
    let registry = Registry::builtins().unwrap();
    let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/sources.json")).unwrap();
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Source fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Sources") }.apply(&mut project).unwrap();
    let empty = project.clone();
    let mut changes = Vec::new();
    let mut expected = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let id = NodeId::from_bytes((index as u128).to_be_bytes());
        let kind = registry.get(&case.kind).unwrap();
        let parameter = if case.kind == "source.text" { "text" } else { "asset" };
        let params = ParamValues::from([(parameter.into(), case.value.clone())]);
        let node = kind.descriptor().instantiate(id, &params).unwrap();
        assert_eq!(kind.mode(&[]).unwrap().as_str(), "source");
        expected.push(JobSpec::Source { node: id, output: "out".into(), value: case.value.clone() });
        changes.push(Change::AddNode { canvas, node });
    }
    let undo = Changeset::new(changes).apply(&mut project).unwrap();
    let json = serde_json::to_string(&project).unwrap();
    let mut restored: Project = serde_json::from_str(&json).unwrap();
    let nodes = restored.canvases().get(&canvas).unwrap().nodes();
    let mut actual = Vec::new();
    for node in nodes.values() {
        let kind = registry.get(&node.kind).unwrap();
        actual.extend(kind.plan(&PlanCtx { node, inputs: &[] }).unwrap());
    }
    assert_eq!(actual, expected);
    assert_eq!(serde_json::from_str::<Vec<JobSpec>>(&serde_json::to_string(&actual).unwrap()).unwrap(), expected);
    let redo = undo.apply(&mut restored).unwrap();
    assert_eq!(restored, empty);
    redo.apply(&mut restored).unwrap();
    assert_eq!(restored, project);
}

#[test]
fn sources_declare_ports_shortcuts_and_no_connected_input_modes() {
    let registry = Registry::builtins().unwrap();
    assert_eq!(registry.descriptors().filter(|d| d.category == nodecraft_nodes::Category::Source).count(), 4);
    assert_eq!(registry.compatible(&[]).filter(|d| d.category == nodecraft_nodes::Category::Source).count(), 4);
    for (id, modality, key) in [
        ("source.text", Modality::Text, "T"),
        ("source.image", Modality::Image, "I"),
        ("source.video", Modality::Video, "V"),
        ("source.audio", Modality::Audio, "A"),
    ] {
        let kind = registry.get(id).unwrap();
        let descriptor = kind.descriptor();
        descriptor.validate().unwrap();
        assert!(descriptor.inputs.is_empty());
        assert_eq!(descriptor.outputs.len(), 1);
        assert_eq!(descriptor.outputs.first().unwrap().port_type, PortType { modality, many: false });
        assert_eq!(descriptor.quick_add.as_deref(), Some(key));
        assert_eq!(serde_json::from_str::<NodeDescriptor>(&serde_json::to_string(descriptor).unwrap()).unwrap(), *descriptor);
        for input in [Modality::Text, Modality::Image, Modality::Video, Modality::Audio, Modality::Mesh, Modality::Document] {
            let port = PortType { modality: input, many: false };
            assert!(kind.mode(&[port]).is_none());
            // Only the router takes a wire of any modality (it is created with the wire's).
            assert_eq!(registry.compatible(&[port]).map(|d| d.id.as_str()).collect::<Vec<_>>(), vec![nodecraft_nodes::router::ID]);
            let node = descriptor.instantiate(NodeId::from_bytes([0; 16]), &ParamValues::new()).unwrap();
            let inputs = [InputValue { port: "in".into(), port_type: port, value: Value::Text("ignored".into()) }];
            assert!(kind.plan(&PlanCtx { node: &node, inputs: &inputs }).is_err());
        }
    }
}

#[test]
fn empty_text_is_valid_but_media_sources_remain_unconfigured_until_an_asset_is_selected() {
    let registry = Registry::builtins().unwrap();
    for descriptor in registry.descriptors() {
        let kind = registry.get(&descriptor.id).unwrap();
        let node = descriptor.instantiate(NodeId::from_bytes([1; 16]), &ParamValues::new()).unwrap();
        let result = kind.plan(&PlanCtx { node: &node, inputs: &[] });
        match descriptor.id.as_str() {
            "source.text" => assert_eq!(result.unwrap(), vec![JobSpec::Source { node: node.id, output: "out".into(), value: Value::Text(String::new()) }]),
            "structure.note" => {
                assert_eq!(node.params, ParamValues::from([("text".into(), Value::Text(String::new()))]));
                assert!(node.inputs.is_empty() && node.outputs.is_empty());
                assert!(result.unwrap().is_empty());
            }
            "structure.router" => assert_eq!(result.unwrap_err().kind, ErrorKind::InvalidInput),
            "source.image" | "source.video" | "source.audio" => {
                assert!(node.params.is_empty());
                assert_eq!(result.unwrap_err().kind, ErrorKind::InvalidInput);
            }
            id => panic!("add an explicit default-planning contract for builtin {id}"),
        }
    }
}

#[test]
fn tampered_ports_kinds_and_parameters_cannot_be_planned() {
    let kind = nodecraft_nodes::text::kind();
    let valid = kind.descriptor().instantiate(NodeId::from_bytes([1; 16]), &ParamValues::new()).unwrap();
    let mut wrong_kind = valid.clone();
    wrong_kind.kind = "source.image".into();
    let mut wrong_port = valid.clone();
    wrong_port.outputs.get_mut(0).unwrap().port_type.modality = Modality::Image;
    let mut unknown_param = valid.clone();
    unknown_param.params.insert("secret".into(), Value::Text("never echo".into()));
    let mut wrong_type = valid.clone();
    wrong_type.params.insert("text".into(), Value::Bool(true));
    let mut oversized = valid;
    oversized.params.insert("text".into(), Value::Text("x".repeat(1024 * 1024 + 1)));
    for node in [wrong_kind, wrong_port, unknown_param, wrong_type, oversized] {
        let error = kind.plan(&PlanCtx { node: &node, inputs: &[] }).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(!error.message.contains("never echo"));
    }
}

struct Fake(NodeDescriptor);
impl NodeKind for Fake {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<nodecraft_nodes::ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Err(Error::new(ErrorKind::Unsupported, "test kind"))
    }
}

#[test]
fn registry_rejects_duplicates_conflicting_shortcuts_and_bad_descriptors() {
    let mut registry = Registry::builtins().unwrap();
    let builtin_count = registry.descriptors().count();
    assert_eq!(registry.register(Box::new(nodecraft_nodes::text::kind())).unwrap_err().kind, ErrorKind::Conflict);
    let mut descriptor = nodecraft_nodes::text::kind().descriptor().clone();
    descriptor.id = "other.kind".into();
    descriptor.quick_add = Some("t".into());
    assert_eq!(registry.register(Box::new(Fake(descriptor.clone()))).unwrap_err().kind, ErrorKind::Conflict);
    descriptor.quick_add = None;
    descriptor.outputs.push(descriptor.outputs.first().unwrap().clone());
    assert_eq!(registry.register(Box::new(Fake(descriptor))).unwrap_err().kind, ErrorKind::InvalidInput);
    assert!(registry.get("unknown").is_err());
    assert_eq!(registry.descriptors().count(), builtin_count);
}

#[test]
fn shortcuts_normalize_unicode_lowercase() {
    let mut registry = Registry::default();
    let mut first = nodecraft_nodes::text::kind().descriptor().clone();
    first.quick_add = Some("Á".into());
    registry.register(Box::new(Fake(first.clone()))).unwrap();
    first.id = "other.kind".into();
    first.quick_add = Some("á".into());
    assert_eq!(registry.register(Box::new(Fake(first))).unwrap_err().kind, ErrorKind::Conflict);
}
