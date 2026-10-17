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
    #[serde(default)]
    pub outputs: Vec<Value>,
}

/// A detached node is editable data. Once stored, document edits go through changesets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pos: Position,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    /// Kind-owned, namespaced/versioned configuration; unknown keys are retained.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, Value>,
    #[serde(default)]
    pub inputs: Vec<Port>,
    #[serde(default)]
    pub outputs: Vec<Port>,
    #[serde(default)]
    pub history: Vec<Generation>,
    pub active: Option<GenerationId>,
    pub tag: Option<TagId>,
    pub field: Option<FieldId>,
}

impl Node {
    /// Validate all payload limits before a kind clones or interprets detached instance data.
    pub fn validate_payload(&self) -> Result<(), Error> {
        crate::validate::node(self)
    }

    pub fn new(id: NodeId, kind: impl Into<String>) -> Self {
        Self {
            id,
            kind: kind.into(),
            title: String::new(),
            pos: Position::default(),
            params: BTreeMap::new(),
            config: BTreeMap::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            history: Vec::new(),
            active: None,
            tag: None,
            field: None,
        }
    }
}

/// Persistent layout zone; membership is held only by `Node::field`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldRect {
    pub pos: Position,
    pub width: f32,
    pub height: f32,
}
impl Default for FieldRect {
    fn default() -> Self {
        Self { pos: Position::default(), width: 320.0, height: 240.0 }
    }
}
impl FieldRect {
    pub fn validate(&self) -> Result<(), Error> {
        crate::validate::position(self.pos)?;
        let x = f64::from(self.pos.x) + f64::from(self.width);
        let y = f64::from(self.pos.y) + f64::from(self.height);
        if !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
            || self.width > 20_000_000.0
            || self.height > 20_000_000.0
            || x.abs() > 10_000_000.0
            || y.abs() > 10_000_000.0
        {
            return Err(crate::validate::invalid("field rectangle must be positive, finite and within canvas bounds"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub id: FieldId,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub rect: FieldRect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub collapsed: bool,
}
impl Field {
    pub fn new(id: FieldId, title: impl Into<String>) -> Self {
        Self { id, title: title.into(), rect: FieldRect::default(), color: None, collapsed: false }
    }
    pub fn validate(&self) -> Result<(), Error> {
        crate::validate::text(&self.title, 4096)?;
        if let Some(color) = &self.color {
            crate::validate::text(color, 64)?;
        }
        self.rect.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    #[serde(default)]
    pub title: String,
    /// Palette identifier; unknown bounded identifiers remain round-trippable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommentReply {
    pub id: CommentId,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommentPin {
    pub id: CommentId,
    pub offset: Position,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: CommentId,
    pub text: String,
    #[serde(default)]
    pub pos: Position,
    /// None: canvas position. Some: offset from that node's body origin.
    #[serde(default)]
    pub node: Option<NodeId>,
    #[serde(default)]
    pub replies: Vec<CommentReply>,
    #[serde(default)]
    pub resolved: bool,
}

impl Comment {
    pub fn new(id: CommentId, text: impl Into<String>, pos: Position) -> Self {
        Self { id, text: text.into(), pos, node: None, replies: Vec::new(), resolved: false }
    }

    pub fn validate_payload(&self) -> Result<(), Error> {
        crate::validate::comment(self)
    }

    /// Resolve using stored node coordinates, independently of view/theme/schema geometry.
    pub fn world_position(&self, canvas: &Canvas) -> Result<Position, Error> {
        match self.node {
            None => {
                crate::validate::position(self.pos)?;
                Ok(self.pos)
            }
            Some(id) => {
                let node = canvas.nodes.get(&id).ok_or_else(|| crate::validate::invalid("comment anchor node is missing"))?;
                let x = f64::from(node.pos.x) + f64::from(self.pos.x);
                let y = f64::from(node.pos.y) + f64::from(self.pos.y);
                if !x.is_finite() || !y.is_finite() || x.abs() > 10_000_000.0 || y.abs() > 10_000_000.0 {
                    return Err(crate::validate::invalid("comment anchor exceeds canvas bounds"));
                }
                Ok(Position { x: x as f32, y: y as f32 })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    pub(crate) id: CanvasId,
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) nodes: BTreeMap<NodeId, Node>,
    #[serde(default)]
    pub(crate) edges: BTreeMap<EdgeId, Edge>,
    #[serde(default)]
    pub(crate) fields: BTreeMap<FieldId, Field>,
    #[serde(default)]
    pub(crate) comments: BTreeMap<CommentId, Comment>,
    #[serde(default)]
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
    #[serde(default)]
    title: String,
    #[serde(default)]
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
