use std::collections::BTreeMap;

use nodecraft_types::{
    BillableUnits, Blake3, CanvasId, CommentId, EdgeId, Error, FieldId, GenerationId, ItemId, NodeId, ParamValues, PortType, ProjectId, ProviderInstanceId,
    SkillId, TagId, Value,
};

use crate::Skill;
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
    /// Hash of everything the result was made from (kind, parameters, input values), recorded
    /// when it is produced; a node whose inputs now hash differently is stale (#75). Absent for
    /// results recorded before hashing, which are never reported stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<Blake3>,
    /// A result fanned out by batches (#92): one tag per item, and every output is a list with
    /// one value per item, in this order. Empty for an ordinary result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ItemTag>,
    /// What each of its provider jobs was asked to do (#162), in job order: the provider,
    /// model, parameters and inputs it was submitted with. The catalog version of the model is
    /// not recorded yet, so a model that changes behind the same id can't be told apart. Empty for a result made without a provider, one
    /// recorded before settings were kept, or one whose settings were too large to keep.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<JobSettings>,
    /// A fanned result some of whose items have no value (#480): which, and why each is
    /// missing. `None` for a complete result. A partial result never stands in for a
    /// complete one: it is stale, and nothing downstream reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<Incomplete>,
}

impl Generation {
    /// A result with no recorded input hash.
    pub fn new(id: GenerationId, outputs: Vec<Value>) -> Self {
        Self { id, outputs, input_hash: None, items: Vec::new(), settings: Vec::new(), incomplete: None }
    }

    /// Whether some of its items have no value.
    pub fn is_partial(&self) -> bool {
        self.incomplete.is_some()
    }
}

/// The items of a fanned result that have no value (#480). Each output holds an empty list in
/// their place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incomplete {
    /// By index into the result's `items`, ascending; never all of them.
    pub missing: Vec<MissingItem>,
}

/// One item of a partial result that has no value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingItem {
    pub item: u32,
    pub why: Missing,
    /// For a failed item, the jobs that may have been billed although they didn't finish (#517):
    /// a reply lost on submit, or a job the provider accepted whose outputs then failed here.
    /// Running the item again sends them under these keys, so a provider that keeps a job by
    /// its key answers with it instead of charging again. Empty when none may have been billed,
    /// or the provider reported them failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<SentKey>,
}

impl MissingItem {
    /// An item with no job that may have been billed.
    pub fn new(item: u32, why: Missing) -> Self {
        Self { item, why, keys: Vec::new() }
    }
}

/// The idempotency key one job of an item was last sent under (#517).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentKey {
    /// Which of the item's jobs, by position among them.
    pub job: u32,
    /// Opaque ASCII, at most [`MAX_SENT_KEY`] long: nothing about the account or the provider.
    pub key: String,
}

/// The longest idempotency key a job is sent under.
pub const MAX_SENT_KEY: usize = 64;

/// Why an item of a partial result has no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Missing {
    /// Its job failed.
    Failed,
    /// It was stopped, or never started, before it finished.
    Cancelled,
}

/// The effective settings one provider job was submitted with: never a credential, only the
/// provider, which configured instance of it, the provider's model id, the parameters and the
/// inputs it was given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSettings {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<ProviderInstanceId>,
    pub model: String,
    #[serde(default)]
    pub params: ParamValues,
    /// The inputs the job was given, in submission order. Media are recorded by reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<JobInput>,
    /// For a fanned result, the index into its `items` of the item this job was for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<u32>,
    /// Unlike the fields above, what the provider reported after the job ran (#437): how many of
    /// its own units it billed, when it says. A unit's price depends on the provider's pricing
    /// for `model`, so this is never read as money.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billable_units: Option<BillableUnits>,
}

/// One input a provider job was given, in a role such as `prompt` or `image`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobInput {
    pub role: String,
    pub value: Value,
}

/// One batch item a fanned result was made from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    pub batch: NodeId,
    pub item: ItemId,
}

/// Which items of which batches one value of a fanned result comes from: one key per batch,
/// ordered by batch id.
pub type ItemTag = Vec<ItemKey>;

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
            crate::validate::palette_color(color)?;
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
impl Tag {
    pub fn validate(&self) -> Result<(), Error> {
        crate::validate::text(&self.title, 4096)?;
        if let Some(color) = &self.color {
            crate::validate::palette_color(color)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommentReply {
    pub id: CommentId,
    pub text: String,
    /// Who wrote it, as shown in a shared session (#121); none for a local note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

impl CommentReply {
    pub fn new(id: CommentId, text: impl Into<String>) -> Self {
        Self { id, text: text.into(), author: None }
    }
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
    /// Who started the discussion, as shown in a shared session (#121); none for a local note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

impl Comment {
    pub fn new(id: CommentId, text: impl Into<String>, pos: Position) -> Self {
        Self { id, text: text.into(), pos, node: None, replies: Vec::new(), resolved: false, author: None }
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

/// What a canvas is for (#163). A sheet is a Batch Generate table: its Generate nodes are its
/// rows, in position order, and it opens as a table rather than as a node graph. A technique is
/// a published Technique version (#93): frozen, and described by its canvas's `Technique`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasKind {
    #[default]
    Canvas,
    Sheet,
    Technique,
}

impl CanvasKind {
    pub fn is_canvas(&self) -> bool {
        *self == Self::Canvas
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    pub(crate) id: CanvasId,
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "CanvasKind::is_canvas")]
    pub(crate) kind: CanvasKind,
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
    /// What a technique canvas publishes; present exactly when the kind is technique.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) technique: Option<crate::Technique>,
}

impl Canvas {
    pub fn new(id: CanvasId, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            kind: CanvasKind::Canvas,
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
            fields: BTreeMap::new(),
            comments: BTreeMap::new(),
            tags: BTreeMap::new(),
            technique: None,
        }
    }

    /// Whether `self` and `other` differ at most in where things are and how they are grouped:
    /// node positions and fields, and the canvas's fields and comments. Nothing a run plans or
    /// reads differs, so work derived from one canvas holds for the other (#549). Every other
    /// field counts; a new one does too until it is listed here as layout.
    pub fn same_but_layout(&self, other: &Canvas) -> bool {
        let Canvas { id, title, kind, nodes, edges, fields: _, comments: _, tags, technique } = self;
        let same_node = |a: &Node, b: &Node| {
            let Node { id, kind, title, pos: _, params, config, inputs, outputs, history, active, tag, field: _ } = a;
            *id == b.id
                && *kind == b.kind
                && *title == b.title
                && *params == b.params
                && *config == b.config
                && *inputs == b.inputs
                && *outputs == b.outputs
                && *history == b.history
                && *active == b.active
                && *tag == b.tag
        };
        *id == other.id
            && *title == other.title
            && *kind == other.kind
            && *edges == other.edges
            && *tags == other.tags
            && *technique == other.technique
            && nodes.len() == other.nodes.len()
            && nodes.iter().zip(&other.nodes).all(|((a_id, a), (b_id, b))| a_id == b_id && same_node(a, b))
    }

    /// An empty Batch Generate sheet (#163).
    pub fn sheet(id: CanvasId, title: impl Into<String>) -> Self {
        Self { kind: CanvasKind::Sheet, ..Self::new(id, title) }
    }

    pub fn id(&self) -> CanvasId {
        self.id
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn kind(&self) -> CanvasKind {
        self.kind
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
    /// The project's own agent skills (#168), which travel with the file.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) skills: BTreeMap<SkillId, Skill>,
}

#[derive(Deserialize)]
struct ProjectData {
    id: ProjectId,
    #[serde(default)]
    title: String,
    #[serde(default)]
    canvases: BTreeMap<CanvasId, Canvas>,
    #[serde(default)]
    skills: BTreeMap<SkillId, Skill>,
}

impl TryFrom<ProjectData> for Project {
    type Error = Error;
    fn try_from(data: ProjectData) -> Result<Self, Self::Error> {
        let project = Self { id: data.id, title: data.title, canvases: data.canvases, skills: data.skills };
        project.validate()?;
        Ok(project)
    }
}

impl Project {
    pub fn new(id: ProjectId, title: impl Into<String>) -> Result<Self, Error> {
        let project = Self { id, title: title.into(), canvases: BTreeMap::new(), skills: BTreeMap::new() };
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
    pub fn skills(&self) -> &BTreeMap<SkillId, Skill> {
        &self.skills
    }
}
