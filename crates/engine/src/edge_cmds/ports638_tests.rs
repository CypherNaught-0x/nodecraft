use crate::Session;
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId};
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const OLD: NodeId = NodeId::from_bytes([3; 16]);
const OTHER: NodeId = NodeId::from_bytes([4; 16]);
const MOVING: EdgeId = EdgeId::from_bytes([5; 16]);

fn edge(to: NodeId, port: &str) -> Edge {
    Edge { id: MOVING, from: Endpoint { node: SOURCE, port: "out".into() }, to: Endpoint { node: to, port: port.into() }, order: 0 }
}
fn session(kind: &str, moving: bool, port: &str) -> Session {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Legacy inputs").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, kind) in [(SOURCE, kind), (OLD, nodecraft_nodes::timeline::ID), (OTHER, nodecraft_nodes::timeline::ID)] {
        let mut node = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &Default::default()).unwrap();
        if id == OLD {
            node.inputs.truncate(1);
            node.title = "Saved before audio and stills".into();
        }
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    if moving {
        Change::Connect { canvas: C, edge: edge(OTHER, port) }.apply(&mut project).unwrap();
    }
    // A real .ncraft ZIP with the old input list opens without silently upgrading it.
    let snapshot = nodecraft_format::Snapshot { project, assets: Default::default(), epoch: 0 };
    let loaded = nodecraft_format::decode(&nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    Session::new(loaded.project).unwrap()
}

#[test]
fn connecting_and_rewiring_upgrade_legacy_inputs_as_one_undoable_edit() {
    for (kind, port) in [("source.audio", "audio"), ("source.image", "stills")] {
        for moving in [false, true] {
            let mut s = session(kind, moving, port);
            let before = s.document().clone();
            let command = if moving { "edge.rewire" } else { "edge.connect" };
            let mut params = json!({"canvas": C, "edge": edge(OLD, port)});
            if moving {
                params["replace"] = json!([MOVING]);
            }
            s.enabled(command, &params).unwrap();
            assert_eq!(s.document(), &before, "enabled is pure");
            s.dispatch(command, &params).unwrap();
            let canvas = s.document().canvases().get(&C).unwrap();
            assert_eq!(canvas.nodes().get(&OLD).unwrap().inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["clips", "audio", "stills"]);
            assert_eq!(canvas.edges().get(&MOVING).unwrap().to.node, OLD);
            let after = s.document().clone();
            s.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(s.document(), &before, "old card and original wire are restored");
            assert!(!s.can_undo(), "migration and wire share one undo step");
            s.dispatch("edit.redo", &json!({})).unwrap();
            assert_eq!(s.document(), &after);
        }
    }
}

#[test]
fn a_refused_rewire_neither_upgrades_nor_disconnects_the_original() {
    let mut s = session("source.image", true, "stills");
    let before = s.document().clone();
    for (port, replace) in [("audio", MOVING), ("stills", EdgeId::from_bytes([99; 16]))] {
        let params = json!({"canvas": C, "edge": edge(OLD, port), "replace": [replace]});
        assert!(s.enabled("edge.rewire", &params).is_err());
        assert!(s.dispatch("edge.rewire", &params).is_err());
        assert_eq!(s.document(), &before);
        assert!(!s.can_undo());
    }
}
