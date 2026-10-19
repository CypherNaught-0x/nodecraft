use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Node, Port, Project};
use nodecraft_types::{CanvasId, EdgeId, Modality, NodeId, PortType, ProjectId};

pub fn canvas() -> CanvasId {
    CanvasId::from_bytes([1; 16])
}
pub fn node_id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
pub fn node(n: u8) -> Node {
    let mut node = Node::new(node_id(n), "test.text");
    node.inputs.push(Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } });
    node.outputs.push(Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } });
    node
}
pub fn edge() -> Edge {
    Edge {
        id: EdgeId::from_bytes([3; 16]),
        from: Endpoint { node: node_id(1), port: "out".into() },
        to: Endpoint { node: node_id(2), port: "in".into() },
        order: 0,
    }
}
pub fn project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Commands").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut project).unwrap();
    for n in [1, 2] {
        Change::AddNode { canvas: canvas(), node: node(n) }.apply(&mut project).unwrap();
    }
    Change::Connect { canvas: canvas(), edge: edge() }.apply(&mut project).unwrap();
    project
}

#[allow(dead_code)]
pub fn layout_context(session: &nodecraft_engine::Session) -> nodecraft_engine::layout_cmds::LayoutContext {
    use nodecraft_graph::layout::Extent;
    let selection = vec![node_id(1), node_id(2)];
    nodecraft_engine::layout_cmds::LayoutContext {
        project: session.document().id(),
        canvas: canvas(),
        revision: session.revision(),
        extents: selection.iter().map(|id| Extent { id: *id, width: 320.0, height: 220.0, left: 0.0, top: 22.5, right: 10.0, bottom: 10.0 }).collect(),
        selection,
        gap: 64.0,
        blocked: false,
    }
}
