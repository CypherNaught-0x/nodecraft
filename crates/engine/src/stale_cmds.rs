//! Staleness queries (#75): which results no longer match their inputs. Read-only: no document
//! change and no undo step. Uses the session's authoritative node-kind registry, shared with
//! editing and rendering, to resolve which parameters shape a result.
use crate::commands::{CommandSpec, Context, Effect, Phase, params};
use nodecraft_types::{CanvasId, Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StaleParams {
    canvas: CanvasId,
}

fn stale(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: StaleParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let report = nodecraft_exec::staleness::stale_report(canvas, c.kinds)?;
    let nodes: Vec<_> = report.nodes.into_iter().collect();
    let unknown = report.unknown.map_or(Value::Null, diagnostic_json);
    let mut result = serde_json::Map::new();
    result.insert("nodes".into(), json!(nodes));
    result.insert("unknown_total".into(), json!(report.unknown_total));
    result.insert("unknown".into(), unknown);
    Ok(Effect::None { result: Value::Object(result) })
}

fn diagnostic_json(diagnostic: nodecraft_exec::staleness::HashDiagnostic) -> Value {
    // The reason is already admitted and bounded. Move its String, not a borrowed json! clone.
    // Fixed UUIDs, ErrorKind and at most 512 reason bytes fit 4096 encoded bytes even at 6x escaping.
    let mut object = serde_json::Map::new();
    object.insert("node".into(), json!(diagnostic.node));
    object.insert("cause".into(), json!(diagnostic.cause));
    object.insert("kind".into(), json!(diagnostic.error.kind));
    object.insert("reason".into(), Value::String(diagnostic.error.message));
    Value::Object(object)
}

/// Cheap: the canvas and the registry exist. The stale set itself is only computed on dispatch.
fn enabled_stale(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p: StaleParams = params(value)?;
    c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    Ok(())
}

fn stale_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas"], "properties": {"canvas": {"type": "string", "format": "uuid"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "canvas.stale",
        label: "List stale results",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: stale_schema,
        params_doc: "{canvas: CanvasId} -> {nodes: [NodeId], unknown_total, unknown: null|{node, cause, kind, reason}}; stale results, plus the first locally invalid checked result and its actual failing node. Only results with recorded hashes are checked; unknown_total counts those locally invalid results, including consumers sharing one failing upstream, not unique faulty nodes. A bounded diagnostic is reserved before hashing within the existing query budget. Read-only.",
        enabled: enabled_stale,
        run: stale,
    }]
}

#[cfg(test)]
mod diagnostic_tests;

#[cfg(test)]
mod tests {
    use crate::Session;
    use nodecraft_graph::{Canvas, Change, Changeset, Generation, Node, Project};
    use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{CanvasId, Error, ErrorKind, GenerationId, NodeId, ProjectId};
    use serde_json::json;
    use std::{
        borrow::Cow,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[test]
    fn lists_stale_nodes_read_only_with_the_session_registry() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Stale").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let params = json!({"canvas": canvas});
        let mut session = Session::new(project).unwrap();
        assert!(session.enabled("canvas.stale", &params).is_ok());
        let before = session.document().clone();
        let revision = session.revision();
        assert_eq!(session.dispatch("canvas.stale", &params).unwrap(), json!({"nodes":[],"unknown_total":0,"unknown":null}));
        assert_eq!(session.revision(), revision);
        assert_eq!(session.document(), &before);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
        assert_eq!(session.enabled("canvas.stale", &json!({"canvas": CanvasId::from_bytes([9; 16])})).unwrap_err().kind, ErrorKind::NotFound);
        assert_eq!(session.dispatch("canvas.stale", &json!({"canvas": CanvasId::from_bytes([9; 16])})).unwrap_err().kind, ErrorKind::NotFound);
    }

    #[test]
    fn resolution_budget_errors_are_read_only_and_enabled_is_cheap() {
        use nodecraft_graph::{Changeset, Edge, Endpoint};
        use nodecraft_types::{EdgeId, NodeId};
        let canvas = CanvasId::from_bytes([1; 16]);
        let kinds = nodecraft_nodes::Registry::builtins().unwrap();
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Router limit").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let numbered = |prefix: u8, index: u16| {
            let mut bytes = [prefix; 16];
            bytes[..2].copy_from_slice(&index.to_le_bytes());
            bytes
        };
        let router_id = |index| NodeId::from_bytes(numbered(2, index));
        let source_id = NodeId::from_bytes([3; 16]);
        let mut changes = vec![Change::AddNode {
            canvas,
            node: kinds.get("source.text").unwrap().instantiate_instance(source_id, Default::default(), &Default::default()).unwrap(),
        }];
        let mut edge_index = 0u16;
        for index in 0..1014u16 {
            changes.push(Change::AddNode {
                canvas,
                node: kinds.get(nodecraft_nodes::router::ID).unwrap().instantiate_instance(router_id(index), Default::default(), &Default::default()).unwrap(),
            });
            let upstream = if index == 0 {
                source_id
            } else if index < 14 {
                router_id(index - 1)
            } else {
                router_id(13)
            };
            for order in 0..if index > 0 && index < 14 { 2 } else { 1 } {
                changes.push(Change::Connect {
                    canvas,
                    edge: Edge {
                        id: EdgeId::from_bytes(numbered(4, edge_index)),
                        from: Endpoint { node: upstream, port: "out".into() },
                        to: Endpoint { node: router_id(index), port: "in".into() },
                        order,
                    },
                });
                edge_index += 1;
            }
        }
        Changeset::new(changes).apply(&mut project).unwrap();
        let before = project.clone();
        assert!(nodecraft_exec::staleness::stale(project.canvases().get(&canvas).unwrap(), &kinds).is_err());
        let mut session = Session::with_registries(project, crate::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let params = json!({"canvas": canvas});
        let revision = session.revision();
        session.enabled("canvas.stale", &params).unwrap();
        let error = session.dispatch("canvas.stale", &params).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(error.message.contains("routers"));
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
        assert!(session.events_since(revision).unwrap().is_empty());
        assert!(!session.can_undo());
        assert!(!session.can_redo());
    }

    struct Custom(nodecraft_nodes::NodeDescriptor);
    impl nodecraft_nodes::NodeKind for Custom {
        fn descriptor(&self) -> &nodecraft_nodes::NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[nodecraft_types::PortType]) -> Option<nodecraft_nodes::ModeId> {
            nodecraft_nodes::ModeId::new("custom").ok()
        }
        fn plan(&self, _: &nodecraft_nodes::PlanCtx<'_>) -> Result<Vec<nodecraft_nodes::JobSpec>, nodecraft_types::Error> {
            Err(nodecraft_types::Error::new(ErrorKind::Unsupported, "test kind does not run"))
        }
    }

    #[test]
    fn custom_kinds_are_shared_by_stale_queries_and_node_edits_without_resources() {
        use nodecraft_nodes::NodeKind;
        use nodecraft_types::{GenerationId, NodeId};
        let canvas = CanvasId::from_bytes([1; 16]);
        let id = NodeId::from_bytes([2; 16]);
        let mut descriptor = nodecraft_nodes::text::kind().descriptor().clone();
        descriptor.id = "test.custom".into();
        descriptor.quick_add = None;
        let mut node = descriptor.instantiate(id, &std::collections::BTreeMap::new()).unwrap();
        node.params.clear(); // The custom registry must supply the text default when hashing.
        let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
        kinds.register(Box::new(Custom(descriptor))).unwrap();
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Custom stale").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
        let graph = project.canvases().get(&canvas).unwrap();
        let hash = nodecraft_exec::staleness::input_hash(graph, &kinds, id).unwrap();
        assert_ne!(hash, nodecraft_exec::staleness::input_hash(graph, &nodecraft_nodes::Registry::builtins().unwrap(), id).unwrap());
        let generation = GenerationId::from_bytes([3; 16]);
        Change::AddGeneration {
            canvas,
            id,
            index: 0,
            generation: nodecraft_graph::Generation {
                id: generation,
                outputs: vec![nodecraft_types::Value::Text("result".into())],
                input_hash: Some(hash),
                items: Vec::new(),
            },
        }
        .apply(&mut project)
        .unwrap();
        Change::SetActiveGeneration { canvas, id, generation: Some(generation) }.apply(&mut project).unwrap();
        let mut session = Session::with_registries(project, crate::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let params = json!({"canvas": canvas});
        assert!(session.enabled("canvas.stale", &params).is_ok());
        assert_eq!(session.dispatch("canvas.stale", &params).unwrap(), json!({"nodes":[],"unknown_total":0,"unknown":null}));
        session.dispatch("node.param_set", &json!({"canvas":canvas,"id":id,"name":"text","value":{"type":"text","value":"changed"}})).unwrap();
        let before = session.document().clone();
        let revision = session.revision();
        assert_eq!(session.dispatch("canvas.stale", &params).unwrap(), json!({"nodes":[id],"unknown_total":0,"unknown":null}));
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
        assert!(session.can_undo());
        assert!(!session.can_redo());
        session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
        assert_eq!(session.dispatch("canvas.stale", &params).unwrap(), json!({"nodes":[],"unknown_total":0,"unknown":null}));
        assert!(session.can_redo());
    }
    struct CountingKind {
        descriptor: NodeDescriptor,
        resolutions: Arc<AtomicUsize>,
    }
    impl NodeKind for CountingKind {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.descriptor
        }
        fn instance<'a>(&'a self, _: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            self.resolutions.fetch_add(1, Ordering::SeqCst);
            Ok(Cow::Borrowed(&self.descriptor))
        }
        fn mode(&self, _: &[nodecraft_types::PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }

    #[test]
    fn public_availability_skips_hashing_and_query_preserves_existing_history() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let node = NodeId::from_bytes([2; 16]);
        let generation = GenerationId::from_bytes([3; 16]);
        let resolutions = Arc::new(AtomicUsize::new(0));
        let descriptor = NodeDescriptor {
            id: "test.counting".into(),
            label: "Counting".into(),
            category: Category::Generation,
            inputs: vec![],
            outputs: vec![],
            params: Default::default(),
            quick_add: None,
        };
        let instance = descriptor.instantiate(node, &Default::default()).unwrap();
        let mut kinds = nodecraft_nodes::Registry::default();
        kinds.register(Box::new(CountingKind { descriptor, resolutions: resolutions.clone() })).unwrap();
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Stale").unwrap();
        Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }, Change::AddNode { canvas, node: instance }])
            .apply(&mut project)
            .unwrap();
        let hash = nodecraft_exec::staleness::input_hash(project.canvases().get(&canvas).unwrap(), &kinds, node).unwrap();
        Changeset::new(vec![
            Change::AddGeneration {
                canvas,
                id: node,
                index: 0,
                generation: Generation { id: generation, outputs: vec![], input_hash: Some(hash), items: Vec::new() },
            },
            Change::SetActiveGeneration { canvas, id: node, generation: Some(generation) },
        ])
        .apply(&mut project)
        .unwrap();
        // Configuration is hashed, so this produces an actual stale result.
        let mut changed = project.canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().clone();
        changed.config.insert("recipe".into(), nodecraft_types::Value::Text("changed".into()));
        Change::SetNode { canvas, node: changed }.apply(&mut project).unwrap();
        let mut session = Session::with_registries(project, crate::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let movement = |x| json!({"canvas":canvas,"id":node,"pos":{"x":x,"y":0}});
        session.dispatch("node.move", &movement(1)).unwrap();
        session.dispatch("node.move", &movement(2)).unwrap();
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert!(session.can_undo() && session.can_redo());
        let document = session.document().clone();
        let revision = session.revision();
        let events = session.events_since(0).unwrap();
        let status = session.file_status();
        let status_revision = session.status_revision();
        resolutions.store(0, Ordering::SeqCst);
        for _ in 0..40 {
            session.enabled("canvas.stale", &json!({"canvas":canvas})).unwrap();
        }
        assert_eq!(resolutions.load(Ordering::SeqCst), 0);
        for (params, kind) in [
            (json!({}), ErrorKind::InvalidInput),
            (json!({"canvas":"invalid"}), ErrorKind::InvalidInput),
            (json!({"canvas":CanvasId::from_bytes([9;16])}), ErrorKind::NotFound),
        ] {
            assert_eq!(session.enabled("canvas.stale", &params).unwrap_err().kind, kind);
            assert_eq!(session.dispatch("canvas.stale", &params).unwrap_err().kind, kind);
        }
        assert_eq!(resolutions.load(Ordering::SeqCst), 0);
        assert_eq!(session.dispatch("canvas.stale", &json!({"canvas":canvas})).unwrap(), json!({"nodes":[node],"unknown_total":0,"unknown":null}));
        // Dispatch preserves authoritative validate_instance hooks; the default hook may
        // resolve again. Availability does no resolution, but a successful query must resolve.
        assert!(resolutions.load(Ordering::SeqCst) >= 1);
        assert_eq!(session.document(), &document);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(session.file_status(), status);
        assert_eq!(session.status_revision(), status_revision);
        assert!(session.can_undo() && session.can_redo());
        session.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(session.document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos.x, 2.0);
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(session.document(), &document);
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(session.document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos.x, 0.0);
    }
}
