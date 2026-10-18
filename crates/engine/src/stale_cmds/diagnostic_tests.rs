use crate::Session;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Generation, Port, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry};
use nodecraft_types::{Blake3, CanvasId, EdgeId, Error, ErrorKind, GenerationId, NodeId, PortType, ProjectId, Value};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
struct Kind {
    descriptor: NodeDescriptor,
    rejected: bool,
    reason: String,
    calls: Arc<AtomicUsize>,
}
impl NodeKind for Kind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn validate_instance(&self, node: &nodecraft_graph::Node, _: bool) -> Result<nodecraft_nodes::ResolvedParams, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.rejected { Err(Error::new(ErrorKind::Unsupported, self.reason.clone())) } else { self.descriptor.params.resolve(&node.params, false) }
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}
fn fixture(router: bool) -> (Session, Arc<AtomicUsize>) {
    fixture_reason(router, "choose a supported source configuration\n")
}
fn fixture_reason(router: bool, reason: &str) -> (Session, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut kinds = Registry::builtins().unwrap();
    let port = |name: &str| Port { name: name.into(), port_type: PortType { modality: nodecraft_types::Modality::Text, many: false } };
    for (name, source) in [("test.rejected", true), ("test.consumer", false)] {
        kinds
            .register(Box::new(Kind {
                descriptor: NodeDescriptor {
                    id: name.into(),
                    label: "Diagnostic fixture".into(),
                    category: if source { Category::Source } else { Category::Generation },
                    inputs: if source { vec![] } else { vec![port("in")] },
                    outputs: vec![port("out")],
                    params: Default::default(),
                    quick_add: None,
                },
                rejected: source,
                reason: reason.into(),
                calls: calls.clone(),
            }))
            .unwrap();
    }
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Diagnostics").unwrap();
    let mut changes = vec![Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }];
    for (source, target) in [(1, 20), (2, 21)] {
        let mut upstream = kinds
            .get(if router { "structure.router" } else { "test.rejected" })
            .unwrap()
            .instantiate_instance(id(source), Default::default(), &Default::default())
            .unwrap();
        if router {
            upstream.config.insert("modality".into(), Value::Text("invalid fixture modality".into()));
        }
        let mut consumer = kinds.get("test.consumer").unwrap().instantiate_instance(id(target), Default::default(), &Default::default()).unwrap();
        consumer.config.insert("private".into(), Value::Text("unrelated-private-fixture".into()));
        let g = GenerationId::from_bytes([target; 16]);
        consumer.history.push(Generation { id: g, outputs: vec![Value::Text("result".into())], input_hash: Some(Blake3::from_bytes([0; 32])) });
        consumer.active = Some(g);
        changes.extend([
            Change::AddNode { canvas: C, node: upstream },
            Change::AddNode { canvas: C, node: consumer },
            Change::Connect {
                canvas: C,
                edge: Edge {
                    id: EdgeId::from_bytes([source; 16]),
                    from: Endpoint { node: id(source), port: "out".into() },
                    to: Endpoint { node: id(target), port: "in".into() },
                    order: 0,
                },
            },
        ]);
    }
    Changeset::new(changes).apply(&mut project).unwrap();
    (Session::with_registries(project, crate::commands::Registry::builtins().unwrap(), kinds).unwrap(), calls)
}
#[test]
fn production_stale_reports_actual_source_and_bounded_reason() {
    let (mut s, calls) = fixture(false);
    calls.store(0, Ordering::SeqCst);
    let document = s.document().clone();
    let revision = s.revision();
    s.enabled("canvas.stale", &json!({"canvas":C})).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = s.dispatch("canvas.stale", &json!({"canvas":C})).unwrap();
    assert_eq!(result["unknown_total"], 2);
    assert_eq!(result["unknown"]["node"], json!(id(20)));
    assert_eq!(result["unknown"]["cause"], json!(id(1)));
    assert_eq!(result["unknown"]["kind"], "unsupported");
    let reason = result["unknown"]["reason"].as_str().unwrap();
    assert!(reason.contains(&id(1).to_string()));
    assert!(reason.contains("choose a supported source configuration "));
    assert!(!reason.contains('\n'));
    assert_eq!(calls.load(Ordering::SeqCst), 4, "two consumers and two sources; formatting never revalidates");
    assert_eq!(result["nodes"], json!([]));
    assert_eq!(s.document(), &document);
    assert_eq!(s.revision(), revision);
    assert!(!s.can_undo());
    assert!(!s.can_redo());
}
#[test]
fn production_stale_reports_actual_router_not_a_different_router() {
    let (mut s, _) = fixture(true);
    let result = s.dispatch("canvas.stale", &json!({"canvas":C})).unwrap();
    assert_eq!(result["unknown_total"], 2);
    assert_eq!(result["unknown"]["cause"], json!(id(1)));
    let reason = result["unknown"]["reason"].as_str().unwrap();
    assert!(reason.contains(&id(1).to_string()));
    assert!(!reason.contains(&id(2).to_string()));
    assert!(reason.contains("configuration and ports"));
}

#[test]
fn production_reason_is_bounded_and_normalized_without_reflecting_unrelated_values() {
    let native = format!("{}TAIL_MARKER", "bad\n\t\u{202e}🙂".repeat(500));
    let (mut session, _) = fixture_reason(false, &native);
    let result = session.dispatch("canvas.stale", &json!({"canvas":C})).unwrap();
    let reason = result["unknown"]["reason"].as_str().unwrap();
    assert!(reason.len() <= 512);
    assert!(reason.ends_with('…'));
    assert!(!reason.contains("TAIL_MARKER"));
    assert!(!reason.contains("unrelated-private-fixture"));
    assert!(!reason.chars().any(|c| c.is_control() || c == '\u{202e}'));
    assert!(serde_json::to_vec(&result["unknown"]).unwrap().len() <= 4096);
}

#[test]
fn diagnostic_presentation_moves_reason_and_has_finite_encoded_size() {
    let reason = "\\\"".repeat(256);
    let original = reason.as_ptr();
    let json = super::diagnostic_json(nodecraft_exec::staleness::HashDiagnostic {
        node: id(20),
        cause: id(1),
        error: Error::new(ErrorKind::CurrencyMismatch, reason),
    });
    assert_eq!(json["reason"].as_str().unwrap().as_ptr(), original, "no second reason copy at JSON boundary");
    assert!(serde_json::to_vec(&json).unwrap().len() <= 4096);
}
