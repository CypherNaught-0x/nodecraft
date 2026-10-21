use super::*;
use crate::Session;
use nodecraft_graph::{Canvas, Project};
use nodecraft_types::{Modality, ProjectId};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const TEXT: NodeId = NodeId::from_bytes([2; 16]);
const IMAGE: NodeId = NodeId::from_bytes([3; 16]);
const NEW: NodeId = NodeId::from_bytes([7; 16]);
const EDGE: EdgeId = EdgeId::from_bytes([8; 16]);

fn session() -> Session {
    let kinds = Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Turn into").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, kind) in [(TEXT, "source.text"), (IMAGE, "source.image")] {
        let node = kinds.get(kind).unwrap().instantiate_instance(id, BTreeMap::new(), &ParamValues::new()).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    Session::new(project).unwrap()
}

fn params(from: NodeId, kind: &str) -> Json {
    json!({"canvas": C, "id": NEW, "kind": kind, "pos": {"x": 300, "y": 20}, "from": {"node": from, "port": "out"}, "edge": EDGE})
}

fn node(s: &Session, id: NodeId) -> &Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap()
}

#[test]
fn creates_the_node_and_its_connection_as_one_undo_step() {
    let mut s = session();
    let before = s.document().clone();
    assert_eq!(s.dispatch("node.create_connected", &params(TEXT, router::ID)).unwrap(), json!({"id": NEW, "input": "in"}));
    let created = node(&s, NEW);
    assert_eq!((created.kind.as_str(), created.pos.x, created.pos.y), (router::ID, 300.0, 20.0));
    let edge = s.document().canvases().get(&C).unwrap().edges().get(&EDGE).unwrap().clone();
    assert_eq!((edge.from, edge.to), (Endpoint { node: TEXT, port: "out".into() }, Endpoint { node: NEW, port: "in".into() }));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before, "one undo removes both");
}

#[test]
fn a_router_takes_the_noodles_modality() {
    let mut s = session();
    s.dispatch("node.create_connected", &params(IMAGE, router::ID)).unwrap();
    assert_eq!(router::modality(node(&s, NEW)).unwrap(), Modality::Image);
    assert_eq!(node(&s, NEW).outputs.first().map(|p| p.port_type.modality), Some(Modality::Image));
}

#[test]
fn refuses_what_it_cannot_connect_and_leaves_the_document_alone() {
    let mut s = session();
    let before = s.document().clone();
    let mut taken = params(TEXT, router::ID);
    taken["id"] = json!(IMAGE);
    let mut edge_taken = params(TEXT, router::ID);
    s.dispatch("node.create_connected", &params(IMAGE, router::ID)).unwrap();
    edge_taken["id"] = json!(NodeId::from_bytes([9; 16]));
    let after_first = s.document().clone();
    for (p, kind) in [
        (params(TEXT, "source.image"), ErrorKind::Conflict),
        (params(TEXT, "no.such"), ErrorKind::NotFound),
        (params(NodeId::from_bytes([5; 16]), router::ID), ErrorKind::NotFound),
        (
            json!({"canvas": C, "id": NodeId::from_bytes([9; 16]), "kind": router::ID, "pos": {"x": 0, "y": 0}, "from": {"node": TEXT, "port": "nope"}, "edge": EdgeId::from_bytes([9; 16])}),
            ErrorKind::NotFound,
        ),
        (taken, ErrorKind::Conflict),
        (edge_taken, ErrorKind::Conflict),
    ] {
        let error = s.dispatch("node.create_connected", &p).unwrap_err();
        assert_eq!(error.kind, kind, "{p}: {}", error.message);
        assert_eq!(s.document(), &after_first);
    }
    let mut far = params(TEXT, router::ID);
    far["pos"] = json!({"x": 1e30, "y": 0});
    assert_eq!(s.dispatch("node.create_connected", &far).unwrap_err().kind, ErrorKind::InvalidInput);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
}

#[test]
fn the_picker_offers_kinds_with_an_input_for_the_noodle_and_never_sources() {
    let kinds = Registry::builtins().unwrap();
    let ids = |output| kinds_accepting(&kinds, output).into_iter().map(|d| d.id.as_str()).collect::<Vec<_>>();
    // Action kinds (#410) take one item; a list goes into a router, a batch (#92) or a list
    // input (Join). Routers and batches take any modality, one item or a list.
    let image_actions = [
        "image.aspect",
        "image.blur",
        "image.crop",
        "image.duplicate",
        "image.expand",
        "image.filter",
        "image.flip",
        "image.grade",
        "image.key",
        "image.layers",
        "image.mask",
        "image.overlay",
        "image.replace_color",
        "image.resize",
        "image.rotate",
        "image.shader",
        "image.tint",
    ];
    let mut image_one: Vec<&str> = image_actions.to_vec();
    // A picture can be a still on a timeline (#576) or panned and zoomed into a video (#108).
    image_one.extend([
        nodecraft_nodes::batch::ID,
        router::ID,
        nodecraft_nodes::element::ID,
        nodecraft_nodes::timeline::ID,
        nodecraft_nodes::video_frames::KEN_BURNS,
    ]);
    image_one.sort();
    let mut got = ids(PortType { modality: Modality::Image, many: false });
    got.sort();
    assert_eq!(got, image_one);
    let mut text_one = ids(PortType { modality: Modality::Text, many: false });
    text_one.sort();
    assert_eq!(text_one, [nodecraft_nodes::batch::ID, router::ID, "text.concat", "text.replace", "text.split", "text.template"]);
    let mut text_many = ids(PortType { modality: Modality::Text, many: true });
    text_many.sort();
    assert_eq!(text_many, [nodecraft_nodes::batch::ID, router::ID, "text.concat", "text.template"], "only list inputs take a list");
    assert_eq!(
        ids(PortType { modality: Modality::Image, many: true }),
        [nodecraft_nodes::element::ID, nodecraft_nodes::layers::ID, nodecraft_nodes::batch::ID, router::ID, nodecraft_nodes::timeline::ID],
        "an Element, the Layer Editor and a Timeline take a list of images"
    );
    use nodecraft_nodes::video_edit::{REPLACE_AUDIO, SPEED, STITCH, TRIM};
    use nodecraft_nodes::video_frames::{CONTACT_SHEET, FRAMES, LONG_EXPOSURE, REVERSE};
    let (batch, timeline) = (nodecraft_nodes::batch::ID, nodecraft_nodes::timeline::ID);
    assert_eq!(
        ids(PortType { modality: Modality::Video, many: false }),
        [batch, router::ID, CONTACT_SHEET, FRAMES, LONG_EXPOSURE, REPLACE_AUDIO, REVERSE, SPEED, STITCH, timeline, TRIM],
        "a Timeline and the video actions take a video"
    );
    assert_eq!(
        ids(PortType { modality: Modality::Video, many: true }),
        [batch, router::ID, STITCH, timeline],
        "only Stitch and a Timeline take a list of videos"
    );
    assert_eq!(
        ids(PortType { modality: Modality::Audio, many: false }),
        [batch, router::ID, REPLACE_AUDIO, timeline],
        "a Timeline and Replace sound take a sound"
    );
    assert_eq!(ids(PortType { modality: Modality::Audio, many: true }), [batch, router::ID, timeline], "a Timeline takes sounds");
    assert!(compatible(PortType { modality: Modality::Text, many: false }, PortType { modality: Modality::Text, many: true }));
    assert!(!compatible(PortType { modality: Modality::Text, many: true }, PortType { modality: Modality::Text, many: false }));
    assert!(!compatible(PortType { modality: Modality::Text, many: false }, PortType { modality: Modality::Image, many: false }));
}
