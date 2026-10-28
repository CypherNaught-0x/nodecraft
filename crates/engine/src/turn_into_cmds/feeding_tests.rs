use super::*;
use crate::Session;
use nodecraft_graph::{Canvas, Project};
use nodecraft_types::{Modality, ProjectId};

const C: CanvasId = CanvasId::from_bytes([150; 16]);
const TARGET: NodeId = NodeId::from_bytes([151; 16]);
const NEW: NodeId = NodeId::from_bytes([152; 16]);
const EDGE: EdgeId = EdgeId::from_bytes([153; 16]);

fn session(kind: &str) -> (Session, String) {
    let kinds = Registry::builtins().unwrap();
    let node = kinds.get(kind).unwrap().instantiate_instance(TARGET, BTreeMap::new(), &ParamValues::new()).unwrap();
    let input = node.inputs.first().unwrap().name.clone();
    let mut project = Project::new(ProjectId::from_bytes([24; 16]), "Input creation").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    (Session::new(project).unwrap(), input)
}

fn request(input: &str, kind: &str) -> Json {
    json!({"canvas":C,"id":NEW,"kind":kind,"pos":{"x":-300,"y":20},"to":{"node":TARGET,"port":input},"edge":EDGE})
}

#[test]
fn a_new_source_feeds_an_existing_input_and_one_undo_restores_the_document() {
    let (mut s, input) = session("text.replace");
    let before = s.document().clone();
    let p = request(&input, "source.text");
    s.enabled("node.create_connected", &p).unwrap();
    assert_eq!(s.document(), &before);
    assert_eq!(s.dispatch("node.create_connected", &p).unwrap(), json!({"id":NEW,"output":"out"}));
    let canvas = s.document().canvases().get(&C).unwrap();
    let source = canvas.nodes().get(&NEW).unwrap();
    assert_eq!((source.kind.as_str(), source.pos.x, source.pos.y), ("source.text", -300.0, 20.0));
    let edge = canvas.edges().get(&EDGE).unwrap();
    assert_eq!((&edge.from, &edge.to), (&Endpoint { node: NEW, port: "out".into() }, &Endpoint { node: TARGET, port: input }));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
}

#[test]
fn offered_kinds_match_real_command_admission_and_router_output_modality() {
    for kind in ["text.replace", "image.blur", "text.concat"] {
        let (s, input) = session(kind);
        let target = s.document().canvases().get(&C).unwrap().nodes().get(&TARGET).unwrap();
        let port = target.inputs.iter().find(|port| port.name == input).unwrap();
        let offered = kinds_feeding(s.kinds(), port.port_type);
        assert!(!offered.is_empty());
        assert!(offered.iter().any(|descriptor| descriptor.id == router::ID));
        for descriptor in offered {
            s.enabled("node.create_connected", &request(&input, &descriptor.id)).unwrap_or_else(|error| panic!("{} into {kind}: {error:?}", descriptor.id));
        }
    }
    let (mut s, input) = session("image.blur");
    s.dispatch("node.create_connected", &request(&input, router::ID)).unwrap();
    let router = s.document().canvases().get(&C).unwrap().nodes().get(&NEW).unwrap();
    assert_eq!(nodecraft_nodes::router::modality(router).unwrap(), Modality::Image);
    let kinds = Registry::builtins().unwrap();
    let scalar = PortType { modality: Modality::Text, many: false };
    let offered = kinds_feeding(&kinds, scalar);
    assert!(offered.iter().any(|descriptor| descriptor.id == "source.text"));
    assert!(!offered.iter().any(|descriptor| descriptor.id == "source.image"));
    assert!(offered.iter().any(|descriptor| descriptor.id == batch::ID), "Batch's item output is scalar");
    assert!(kinds_feeding(&kinds, PortType { many: true, ..scalar }).iter().any(|descriptor| descriptor.id == batch::ID));
}

#[test]
fn malformed_ends_incompatible_outputs_and_taken_ids_never_create_an_orphan() {
    let (mut s, input) = session("text.replace");
    let before = s.document().clone();
    let p = request(&input, "source.text");
    let mut both = p.clone();
    both["from"] = json!({"node":TARGET,"port":"out"});
    let mut neither = p.clone();
    neither.as_object_mut().unwrap().remove("to");
    let mut null = p.clone();
    null["from"] = Json::Null;
    let mut wrong_port = p.clone();
    wrong_port["to"]["port"] = json!("no-such-input");
    let mut missing_node = p.clone();
    missing_node["to"]["node"] = json!(NodeId::from_bytes([154; 16]));
    let mut missing_canvas = p.clone();
    missing_canvas["canvas"] = json!(CanvasId::from_bytes([155; 16]));
    let mut taken = p.clone();
    taken["id"] = json!(TARGET);
    let mut far = p.clone();
    far["pos"]["x"] = json!(1e30);
    for (p, expected) in [
        (both, ErrorKind::InvalidInput),
        (neither, ErrorKind::InvalidInput),
        (null, ErrorKind::InvalidInput),
        (wrong_port, ErrorKind::NotFound),
        (missing_node, ErrorKind::NotFound),
        (missing_canvas, ErrorKind::NotFound),
        (taken, ErrorKind::Conflict),
        (far, ErrorKind::InvalidInput),
        (request(&input, "source.image"), ErrorKind::Conflict),
        (request(&input, "unknown.kind"), ErrorKind::NotFound),
    ] {
        assert!(s.enabled("node.create_connected", &p).is_err());
        assert_eq!(s.dispatch("node.create_connected", &p).unwrap_err().kind, expected, "{p}");
        assert_eq!(s.document(), &before);
    }
    s.dispatch("node.create_connected", &request(&input, "source.text")).unwrap();
    let occupied = s.document().clone();
    let mut next = request(&input, "source.text");
    next["id"] = json!(NodeId::from_bytes([154; 16]));
    next["edge"] = json!(EdgeId::from_bytes([155; 16]));
    assert_eq!(s.dispatch("node.create_connected", &next).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(s.document(), &occupied, "an existing single input is preserved");
}

#[test]
fn list_inputs_append_in_order_refuse_edge_collisions_and_bound_the_next_order() {
    let (mut s, input) = session("text.concat");
    s.dispatch("node.create_connected", &request(&input, "source.text")).unwrap();
    let first = s.document().clone();
    let mut next = request(&input, "source.text");
    next["id"] = json!(NodeId::from_bytes([154; 16]));
    assert_eq!(s.dispatch("node.create_connected", &next).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(s.document(), &first, "an edge collision cannot leave the new source behind");
    let next_edge = EdgeId::from_bytes([155; 16]);
    next["edge"] = json!(next_edge);
    s.dispatch("node.create_connected", &next).unwrap();
    assert_eq!(s.document().canvases().get(&C).unwrap().edges().get(&next_edge).unwrap().order, 1);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &first);
    let mut project = first;
    let mut edge = project.canvases().get(&C).unwrap().edges().get(&EDGE).unwrap().clone();
    Change::Disconnect { canvas: C, id: EDGE }.apply(&mut project).unwrap();
    edge.order = u32::MAX;
    Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    let mut s = Session::new(project.clone()).unwrap();
    assert_eq!(s.dispatch("node.create_connected", &next).unwrap_err().kind, ErrorKind::Overflow);
    assert_eq!(s.document(), &project);
}
