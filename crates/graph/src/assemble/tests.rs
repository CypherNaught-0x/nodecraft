use super::*;
use crate::{Endpoint, Port, Position};
use nodecraft_types::{Modality, PortType};
use proptest::prelude::*;

fn pid() -> ProjectId {
    ProjectId::from_bytes([0; 16])
}
fn cid(n: u8) -> CanvasId {
    CanvasId::from_bytes([n; 16])
}
fn nid(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn eid(n: u8) -> EdgeId {
    EdgeId::from_bytes([n; 16])
}

/// A node with a list input `many`, a single input `one` and an output `out`.
fn node(n: u8) -> Node {
    let mut node = Node::new(nid(n), "test.text");
    node.inputs.push(Port { name: "many".into(), port_type: PortType { modality: Modality::Text, many: true } });
    node.inputs.push(Port { name: "one".into(), port_type: PortType { modality: Modality::Text, many: false } });
    node.outputs.push(Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } });
    node
}

fn edge(n: u8, from: u8, to: u8, port: &str, order: u32) -> Edge {
    Edge { id: eid(n), from: Endpoint { node: nid(from), port: "out".into() }, to: Endpoint { node: nid(to), port: port.into() }, order }
}

fn header(n: u8) -> CanvasPart {
    CanvasPart::Header { id: cid(n), title: format!("Canvas {n}"), kind: CanvasKind::Canvas }
}

fn parts(nodes: &[u8], edges: Vec<Edge>) -> Parts {
    Parts {
        title: "Shared".into(),
        canvases: vec![header(1)],
        nodes: nodes.iter().map(|n| (cid(1), node(*n))).collect(),
        edges: edges.into_iter().map(|e| (cid(1), e)).collect(),
        ..Parts::default()
    }
}

fn edges_of(assembled: &Assembled) -> Vec<EdgeId> {
    assembled.project.canvases.get(&cid(1)).map(|c| c.edges.keys().copied().collect()).unwrap_or_default()
}

#[test]
fn a_valid_project_comes_back_as_it_was() {
    let mut project = Project::new(pid(), "Round trip").unwrap();
    let mut canvas = Canvas::new(cid(1), "One");
    let mut field = Field::new(FieldId::from_bytes([7; 16]), "Moodboard");
    field.rect.width = 100.0;
    field.rect.height = 100.0;
    let tag = Tag { id: TagId::from_bytes([8; 16]), title: "Hero".into(), color: None };
    let mut a = node(1);
    a.field = Some(field.id);
    a.tag = Some(tag.id);
    canvas.fields.insert(field.id, field);
    canvas.tags.insert(tag.id, tag);
    canvas.nodes.insert(a.id, a);
    canvas.nodes.insert(nid(2), node(2));
    canvas.edges.insert(eid(1), edge(1, 1, 2, "one", 0));
    let mut comment = Comment::new(CommentId::from_bytes([9; 16]), "Nice", Position { x: 4.0, y: 4.0 });
    comment.node = Some(nid(2));
    canvas.comments.insert(comment.id, comment);
    project.canvases.insert(canvas.id, canvas);
    project.canvases.insert(cid(2), Canvas::sheet(cid(2), "Rows"));
    project.validate().unwrap();

    let assembled = assemble(pid(), Parts::of(&project)).unwrap();
    assert_eq!(assembled.project, project);
    assert!(assembled.dropped.is_empty() && assembled.repaired.is_empty());
}

#[test]
fn of_two_edges_into_one_single_input_the_lower_id_stays() {
    let assembled = assemble(pid(), parts(&[1, 2, 3], vec![edge(9, 2, 3, "one", 0), edge(4, 1, 3, "one", 0)])).unwrap();
    assert_eq!(edges_of(&assembled), vec![eid(4)]);
    assert_eq!(assembled.dropped, BTreeSet::from([Part::Edge(cid(1), eid(9))]));
    // A list input takes both, at different places, but not two at one place.
    let assembled = assemble(pid(), parts(&[1, 2, 3, 4], vec![edge(1, 1, 4, "many", 0), edge(2, 2, 4, "many", 1), edge(3, 3, 4, "many", 1)])).unwrap();
    assert_eq!(edges_of(&assembled), vec![eid(1), eid(2)]);
}

#[test]
fn edges_that_close_a_cycle_lose_their_newest_edge() {
    // Each peer connected one way round; together they made a loop.
    let assembled = assemble(pid(), parts(&[1, 2, 3], vec![edge(1, 1, 2, "many", 0), edge(2, 2, 3, "many", 0), edge(3, 3, 1, "many", 0)])).unwrap();
    assert_eq!(edges_of(&assembled), vec![eid(1), eid(2)]);
    assert_eq!(assembled.dropped, BTreeSet::from([Part::Edge(cid(1), eid(3))]));
}

#[test]
fn many_cycles_end_acyclic_past_the_pass_limit() {
    // Every node feeds every other: far more cycles than the passes break one by one.
    let ids: Vec<u8> = (1..=20).collect();
    let mut edges = Vec::new();
    let mut n = 0u8;
    for from in &ids {
        for to in &ids {
            if from != to {
                n = n.wrapping_add(1);
                let mut e = edge(n, *from, *to, "many", u32::from(*from));
                e.id = EdgeId::from_bytes([n, *from, *to, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
                edges.push(e);
            }
        }
    }
    let assembled = assemble(pid(), parts(&ids, edges)).unwrap();
    assembled.project.validate().unwrap();
}

#[test]
fn a_node_whose_field_or_tag_went_stays_out_of_it() {
    let mut parts = parts(&[1], Vec::new());
    if let Some((_, node)) = parts.nodes.first_mut() {
        node.field = Some(FieldId::from_bytes([5; 16]));
        node.tag = Some(TagId::from_bytes([6; 16]));
    }
    let assembled = assemble(pid(), parts).unwrap();
    let node = &assembled.project.canvases[&cid(1)].nodes[&nid(1)];
    assert_eq!((node.field, node.tag), (None, None));
    assert_eq!(assembled.repaired, BTreeSet::from([Part::Node(cid(1), nid(1))]));
}

#[test]
fn a_comment_on_a_removed_card_stays_unpinned() {
    let mut parts = parts(&[], Vec::new());
    let mut comment = Comment::new(CommentId::from_bytes([3; 16]), "Here", Position { x: 10.0, y: 20.0 });
    comment.node = Some(nid(1));
    parts.comments.push((cid(1), comment));
    let assembled = assemble(pid(), parts).unwrap();
    let comment = &assembled.project.canvases[&cid(1)].comments[&CommentId::from_bytes([3; 16])];
    assert_eq!(comment.node, None);
    assert_eq!(assembled.repaired, BTreeSet::from([Part::Comment(cid(1), comment.id)]));
}

#[test]
fn parts_without_a_canvas_or_with_a_taken_id_are_left_out() {
    let mut parts = parts(&[1], Vec::new());
    parts.canvases.push(header(2));
    // The same node on another canvas, a node on a canvas nobody has, and a technique canvas
    // that came as a header rather than whole.
    parts.nodes.push((cid(2), node(1)));
    parts.nodes.push((cid(3), node(2)));
    parts.canvases.push(CanvasPart::Header { id: cid(4), title: "T".into(), kind: CanvasKind::Technique });
    let assembled = assemble(pid(), parts).unwrap();
    assert_eq!(assembled.dropped, BTreeSet::from([Part::Canvas(cid(4)), Part::Node(cid(2), nid(1)), Part::Node(cid(3), nid(2))]));
    assert!(assembled.project.canvases[&cid(1)].nodes.contains_key(&nid(1)));
}

#[test]
fn of_two_skills_with_one_name_the_lower_id_stays() {
    let skill = |n: u8| Skill {
        id: SkillId::from_bytes([n; 16]),
        name: "Polish".into(),
        description: String::new(),
        instructions: "Make it shine.".into(),
        version: 1,
        requires: crate::Requires::default(),
        source: crate::SkillSource::default(),
        enabled: true,
    };
    let mut parts = parts(&[], Vec::new());
    parts.skills = vec![skill(9), skill(2)];
    let assembled = assemble(pid(), parts).unwrap();
    assert_eq!(assembled.project.skills.keys().copied().collect::<Vec<_>>(), vec![SkillId::from_bytes([2; 16])]);
    assert_eq!(assembled.dropped, BTreeSet::from([Part::Skill(SkillId::from_bytes([9; 16]))]));
}

proptest! {
    #[test]
    fn any_parts_assemble_valid_and_the_same_in_any_order(
        wires in proptest::collection::vec((1u8..=8, 1u8..=8, any::<bool>(), 0u32..3), 0..40),
        rotate in 0usize..40,
    ) {
        let edges: Vec<Edge> = wires
            .iter()
            .enumerate()
            .map(|(i, (from, to, many, order))| edge(u8::try_from(i).unwrap_or(0).wrapping_add(1), *from, *to, if *many { "many" } else { "one" }, *order))
            .collect();
        let ids: Vec<u8> = (1..=8).collect();
        let first = assemble(pid(), parts(&ids, edges.clone())).unwrap();
        first.project.validate().unwrap();
        let mut shuffled = edges;
        let len = shuffled.len().max(1);
        shuffled.rotate_left(rotate % len);
        shuffled.reverse();
        let mut nodes = ids;
        nodes.reverse();
        let second = assemble(pid(), parts(&nodes, shuffled)).unwrap();
        prop_assert_eq!(first.project, second.project);
        prop_assert_eq!(first.dropped, second.dropped);
    }
}
