//! Staleness queries (#75): which results no longer match their inputs. Read-only: no document
//! change and no undo step. Needs the node-kind registry as a host resource (to resolve which
//! parameters shape a result); without it the command is unsupported.
use crate::commands::{CommandSpec, Context, Effect, params};
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
    let kinds = c.resources.require::<nodecraft_nodes::Registry>()?;
    let nodes: Vec<_> = nodecraft_exec::staleness::stale(canvas, kinds)?.into_iter().collect();
    Ok(Effect::None { result: json!({ "nodes": nodes }) })
}

/// Cheap: the canvas and the registry exist. The stale set itself is only computed on dispatch.
fn enabled_stale(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p: StaleParams = params(value)?;
    c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    c.resources.require::<nodecraft_nodes::Registry>().map(|_| ())
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
        params_doc: "{canvas: CanvasId} -> {nodes: [NodeId]}; nodes whose active result no longer matches its inputs, or was built on a stale upstream result. Read-only.",
        enabled: enabled_stale,
        run: stale,
    }]
}

#[cfg(test)]
mod tests {
    use crate::Session;
    use nodecraft_graph::{Canvas, Change, Project};
    use nodecraft_types::{CanvasId, ErrorKind, ProjectId};
    use serde_json::json;

    #[test]
    fn lists_stale_nodes_read_only_and_needs_the_registry() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Stale").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let params = json!({"canvas": canvas});
        let mut bare = Session::new(project.clone()).unwrap();
        assert_eq!(bare.dispatch("canvas.stale", &params).unwrap_err().kind, ErrorKind::Unsupported);
        let mut resources = crate::commands::Resources::default();
        resources.insert(nodecraft_nodes::Registry::builtins().unwrap());
        let mut session = Session::new(project).unwrap().with_resources(resources);
        let revision = session.revision();
        assert_eq!(session.dispatch("canvas.stale", &params).unwrap(), json!({"nodes": []}));
        assert_eq!(session.revision(), revision);
        assert!(!session.can_undo());
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
        let mut resources = crate::commands::Resources::default();
        resources.insert(kinds);
        let mut session = Session::new(project).unwrap().with_resources(resources);
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
}
