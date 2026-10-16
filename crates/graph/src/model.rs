use std::collections::BTreeMap;

use nodecraft_types::{CanvasId, CommentId, EdgeId, Error, FieldId, GenerationId, NodeId, PortType, ProjectId, TagId, Value};
use serde::{Deserialize, Serialize};

/// World coordinates, independent of the UI toolkit. Validation rejects non-finite values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    pub name: String,
    pub port_type: PortType,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub node: NodeId,
    pub port: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub from: Endpoint,
    pub to: Endpoint,
    /// Stable ordering within one input. Gaps are allowed; single inputs use zero.
    pub order: u32,
}

/// A history entry; provider/run-specific provenance is added by the generation milestone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub id: GenerationId,
    pub outputs: Vec<Value>,
}

/// A detached node is editable data. Once stored, document edits go through changesets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: String,
    pub title: String,
    pub pos: Position,
    pub params: BTreeMap<String, Value>,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub history: Vec<Generation>,
    pub active: Option<GenerationId>,
    pub tag: Option<TagId>,
    pub field: Option<FieldId>,
}

impl Node {
    pub fn new(id: NodeId, kind: impl Into<String>) -> Self {
        Self {
            id,
            kind: kind.into(),
            title: String::new(),
            pos: Position::default(),
            params: BTreeMap::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            history: Vec::new(),
            active: None,
            tag: None,
            field: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub id: FieldId,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: CommentId,
    pub text: String,
    pub pos: Position,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    pub(crate) id: CanvasId,
    pub(crate) title: String,
    pub(crate) nodes: BTreeMap<NodeId, Node>,
    pub(crate) edges: BTreeMap<EdgeId, Edge>,
    pub(crate) fields: BTreeMap<FieldId, Field>,
    pub(crate) comments: BTreeMap<CommentId, Comment>,
    pub(crate) tags: BTreeMap<TagId, Tag>,
}

impl Canvas {
    pub fn new(id: CanvasId, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
            fields: BTreeMap::new(),
            comments: BTreeMap::new(),
            tags: BTreeMap::new(),
        }
    }

    pub fn id(&self) -> CanvasId {
        self.id
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn nodes(&self) -> &BTreeMap<NodeId, Node> {
        &self.nodes
    }
    pub fn edges(&self) -> &BTreeMap<EdgeId, Edge> {
        &self.edges
    }
    pub fn fields(&self) -> &BTreeMap<FieldId, Field> {
        &self.fields
    }
    pub fn comments(&self) -> &BTreeMap<CommentId, Comment> {
        &self.comments
    }
    pub fn tags(&self) -> &BTreeMap<TagId, Tag> {
        &self.tags
    }
}

/// A validated document. Maps are private so there is one mutation path: `Changeset::apply`.
/// File/control readers must bound input bytes and retain their decoder's recursion limit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ProjectData")]
pub struct Project {
    pub(crate) id: ProjectId,
    pub(crate) title: String,
    pub(crate) canvases: BTreeMap<CanvasId, Canvas>,
}

#[derive(Deserialize)]
struct ProjectData {
    id: ProjectId,
    title: String,
    canvases: BTreeMap<CanvasId, Canvas>,
}

impl TryFrom<ProjectData> for Project {
    type Error = Error;
    fn try_from(data: ProjectData) -> Result<Self, Self::Error> {
        let project = Self { id: data.id, title: data.title, canvases: data.canvases };
        project.validate()?;
        Ok(project)
    }
}

impl Project {
    pub fn new(id: ProjectId, title: impl Into<String>) -> Result<Self, Error> {
        let project = Self { id, title: title.into(), canvases: BTreeMap::new() };
        project.validate()?;
        Ok(project)
    }

    pub fn id(&self) -> ProjectId {
        self.id
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn canvases(&self) -> &BTreeMap<CanvasId, Canvas> {
        &self.canvases
    }
}
