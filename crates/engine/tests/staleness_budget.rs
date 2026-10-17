//! A query budget failure is an atomic read-only command failure.
use nodecraft_engine::{Session, commands::Registry};
use nodecraft_graph::{Canvas, Change, Changeset, Generation, Node, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{CanvasId, Error, ErrorKind, GenerationId, NodeId, ParamControl, ParamSchema, ParamSpec, PortType, ProjectId, Value};
use serde_json::json;

struct LargeDefault(NodeDescriptor);
impl NodeKind for LargeDefault {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}
#[test]
fn effective_default_budget_failure_preserves_document_history_revision_and_events() {
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds
        .register(Box::new(LargeDefault(NodeDescriptor {
            id: "test.large_default".into(),
            label: "Large default".into(),
            category: Category::Generation,
            inputs: vec![],
            outputs: vec![],
            quick_add: None,
            params: ParamSchema {
                params: vec![ParamSpec {
                    name: "prompt".into(),
                    label: "Prompt".into(),
                    description: None,
                    required: false,
                    visible_if: None,
                    control: ParamControl::Text { multiline: true, max_bytes: 1_000_000 },
                    default: Some(Value::Text("x".repeat(600_000))),
                }],
            },
        })))
        .unwrap();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Budget").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let first = NodeId::from_bytes(0u128.to_be_bytes());
    Change::AddNode { canvas, node: Node::new(first, "test.large_default") }.apply(&mut project).unwrap();
    let hash = nodecraft_exec::staleness::input_hash(project.canvases().get(&canvas).unwrap(), &kinds, first).unwrap();
    let mut changes = vec![];
    for index in 0..450u128 {
        let id = NodeId::from_bytes(index.to_be_bytes());
        let generation = GenerationId::from_bytes(index.to_be_bytes());
        if index != 0 {
            changes.push(Change::AddNode { canvas, node: Node::new(id, "test.large_default") });
        }
        changes.push(Change::AddGeneration { canvas, id, index: 0, generation: Generation { id: generation, outputs: vec![], input_hash: Some(hash) } });
        changes.push(Change::SetActiveGeneration { canvas, id, generation: Some(generation) });
    }
    Changeset::new(changes).apply(&mut project).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    session.dispatch("node.title", &json!({"canvas":canvas,"id":first,"title":"Unrelated edit"})).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    let before = session.document().clone();
    let revision = session.revision();
    let history = (session.can_undo(), session.can_redo());
    assert!(session.enabled("canvas.stale", &json!({"canvas":canvas})).is_ok(), "preview stays cheap");
    let error = session.dispatch("canvas.stale", &json!({"canvas":canvas})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("too much data"));
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert_eq!((session.can_undo(), session.can_redo()), history);
    assert!(session.events_since(revision).unwrap().is_empty());
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(session.document().canvases().get(&canvas).unwrap().nodes().get(&first).unwrap().title, "Unrelated edit");
}
