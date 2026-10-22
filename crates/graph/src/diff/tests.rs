use super::*;
use crate::assemble::{CanvasPart, Parts, assemble};
use crate::{Comment, Edge, Endpoint, Field, Node, Port, Position, Skill, Tag};
use nodecraft_types::{CanvasId, CommentId, EdgeId, FieldId, Modality, NodeId, PortType, ProjectId, SkillId, TagId};
use proptest::prelude::*;

fn pid() -> ProjectId {
    ProjectId::from_bytes([0; 16])
}

/// The ids and contents a random project picks from, so two of them overlap.
/// A card: canvas, id, kind, a flag, and two optional params.
type NodeShape = (u8, u8, u8, bool, Option<u8>, Option<u8>);

#[derive(Clone, Debug)]
struct Shape {
    title: u8,
    canvases: Vec<(u8, u8, bool)>,
    nodes: Vec<NodeShape>,
    edges: Vec<(u8, u8, u8, bool, u32)>,
    fields: Vec<(u8, u8, u8)>,
    tags: Vec<(u8, u8, u8)>,
    comments: Vec<(u8, u8, Option<u8>, u8)>,
    skills: Vec<(u8, u8)>,
}

fn shape() -> impl Strategy<Value = Shape> {
    (
        0u8..3,
        proptest::collection::vec((1u8..=3, 0u8..2, any::<bool>()), 0..4),
        proptest::collection::vec((1u8..=3, 1u8..=12, 0u8..3, any::<bool>(), proptest::option::of(1u8..=4), proptest::option::of(1u8..=4)), 0..16),
        proptest::collection::vec((1u8..=3, 1u8..=12, 1u8..=12, any::<bool>(), 0u32..2), 0..24),
        proptest::collection::vec((1u8..=3, 1u8..=4, 0u8..2), 0..5),
        proptest::collection::vec((1u8..=3, 1u8..=4, 0u8..2), 0..5),
        proptest::collection::vec((1u8..=3, 1u8..=6, proptest::option::of(1u8..=12), 0u8..2), 0..6),
        proptest::collection::vec((1u8..=4, 0u8..3), 0..4),
    )
        .prop_map(|(title, canvases, nodes, edges, fields, tags, comments, skills)| Shape {
            title,
            canvases,
            nodes,
            edges,
            fields,
            tags,
            comments,
            skills,
        })
}

fn project(shape: &Shape) -> Project {
    let c = |n: u8| CanvasId::from_bytes([n; 16]);
    let n = |n: u8| NodeId::from_bytes([n; 16]);
    let mut parts = Parts { title: format!("Title {}", shape.title), ..Parts::default() };
    for (id, title, sheet) in &shape.canvases {
        let kind = if *sheet { CanvasKind::Sheet } else { CanvasKind::Canvas };
        parts.canvases.push(CanvasPart::Header { id: c(*id), title: format!("Canvas {title}"), kind });
    }
    for (canvas, id, title, list, field, tag) in &shape.nodes {
        let mut node = Node::new(n(*id), "test.text");
        node.title = format!("Card {title}");
        node.pos = Position { x: f32::from(*title) * 10.0, y: 0.0 };
        node.inputs.push(Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: *list } });
        node.outputs.push(Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } });
        node.field = field.map(|f| FieldId::from_bytes([f; 16]));
        node.tag = tag.map(|t| TagId::from_bytes([t; 16]));
        parts.nodes.push((c(*canvas), node));
    }
    for (i, (canvas, from, to, _, order)) in shape.edges.iter().enumerate() {
        let id = EdgeId::from_bytes([u8::try_from(i).unwrap_or(0), *from, *to, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let edge = Edge { id, from: Endpoint { node: n(*from), port: "out".into() }, to: Endpoint { node: n(*to), port: "in".into() }, order: *order };
        parts.edges.push((c(*canvas), edge));
    }
    for (canvas, id, title) in &shape.fields {
        let mut field = Field::new(FieldId::from_bytes([*id; 16]), format!("Field {title}"));
        field.rect.width = 50.0;
        field.rect.height = 50.0;
        parts.fields.push((c(*canvas), field));
    }
    for (canvas, id, title) in &shape.tags {
        parts.tags.push((c(*canvas), Tag { id: TagId::from_bytes([*id; 16]), title: format!("Tag {title}"), color: None }));
    }
    for (canvas, id, node, text) in &shape.comments {
        let mut comment = Comment::new(CommentId::from_bytes([*id; 16]), format!("Note {text}"), Position { x: 1.0, y: 1.0 });
        comment.node = node.map(n);
        parts.comments.push((c(*canvas), comment));
    }
    for (id, name) in &shape.skills {
        parts.skills.push(Skill {
            id: SkillId::from_bytes([*id; 16]),
            name: format!("Skill {name}"),
            description: String::new(),
            instructions: "Do it.".into(),
            version: 1,
            requires: crate::Requires::default(),
            source: crate::SkillSource::default(),
            enabled: true,
        });
    }
    assemble(pid(), parts).map(|a| a.project).unwrap_or_else(|_| Project::new(pid(), "").unwrap())
}

fn reach(from: &Project, to: &Project) {
    let mut project = from.clone();
    for set in diff(from, to).unwrap() {
        set.apply(&mut project).unwrap();
        project.validate().unwrap();
    }
    assert_eq!(&project, to);
}

#[test]
fn the_same_project_needs_no_changes() {
    let shape = Shape {
        title: 1,
        canvases: vec![(1, 0, false)],
        nodes: vec![(1, 1, 0, true, None, None)],
        edges: Vec::new(),
        fields: Vec::new(),
        tags: Vec::new(),
        comments: Vec::new(),
        skills: Vec::new(),
    };
    let project = project(&shape);
    assert!(diff(&project, &project).unwrap().is_empty());
}

#[test]
fn two_skills_can_trade_names() {
    let mut from = Shape {
        title: 0,
        canvases: Vec::new(),
        nodes: Vec::new(),
        edges: Vec::new(),
        fields: Vec::new(),
        tags: Vec::new(),
        comments: Vec::new(),
        skills: vec![(1, 1), (2, 2)],
    };
    let a = project(&from);
    from.skills = vec![(1, 2), (2, 1)];
    reach(&a, &project(&from));
}

#[test]
fn a_card_whose_ports_change_keeps_its_wire() {
    let mut shape = Shape {
        title: 0,
        canvases: vec![(1, 0, false)],
        nodes: vec![(1, 1, 0, false, None, None), (1, 2, 0, false, None, None)],
        edges: vec![(1, 1, 2, false, 0)],
        fields: Vec::new(),
        tags: Vec::new(),
        comments: Vec::new(),
        skills: Vec::new(),
    };
    let a = project(&shape);
    shape.nodes[1].3 = true;
    let b = project(&shape);
    assert_eq!(b.canvases.values().map(|c| c.edges.len()).sum::<usize>(), 1);
    reach(&a, &b);
}

#[test]
fn a_big_canvas_comes_in_several_changesets() {
    let mut to = Project::new(pid(), "").unwrap();
    let mut canvas = Canvas::new(CanvasId::from_bytes([1; 16]), "Big");
    // As many cards as a project may hold, and some fields: more changes than one changeset.
    for i in 0..10u8 {
        let mut field = Field::new(FieldId::from_bytes([i; 16]), "F");
        field.rect.width = 5.0;
        field.rect.height = 5.0;
        canvas.fields.insert(field.id, field);
    }
    for i in 0..(crate::MAX_NODES as u32) {
        let mut bytes = [0u8; 16];
        bytes[..4].copy_from_slice(&i.to_be_bytes());
        bytes[15] = 1;
        let node = Node::new(NodeId::from_bytes(bytes), "test.text");
        canvas.nodes.insert(node.id, node);
    }
    to.canvases.insert(canvas.id, canvas);
    let from = Project::new(pid(), "").unwrap();
    let sets = diff(&from, &to).unwrap();
    assert!(sets.len() >= 2);
    assert!(sets.iter().all(|s| s.changes.len() <= MAX_CHANGES));
    reach(&from, &to);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn any_project_becomes_any_other_through_valid_steps(a in shape(), b in shape()) {
        let (from, to) = (project(&a), project(&b));
        reach(&from, &to);
        reach(&to, &from);
    }
}
