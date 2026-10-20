//! Building the showcase documents the way the app does it: one command at a time.
//!
//! Nothing here reaches into the document's internals. Every node, wire, tag and discussion in a
//! showcase arrives through the same command ids the UI, the CLI, the MCP server and the control
//! channel dispatch, so a rendered scene is evidence about real behaviour, not a drawing of it.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::error::Error;

use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_graph::Position;
use nodecraft_types::{CanvasId, CommentId, EdgeId, Modality, NodeId, TagId, Unsigned, Value};
use nodecraft_ui_egui::NodeCraftApp;
use serde_json::json;

pub type Fail = Box<dyn Error + Send + Sync>;

pub fn fail<T: std::fmt::Display>(message: T) -> Fail {
    Box::<dyn Error + Send + Sync>::from(message.to_string())
}

/// A node, edge, tag or comment key: one byte, expanded to a deterministic id.
pub type Key = u8;

pub fn node_id(key: Key) -> NodeId {
    NodeId::from_bytes([key; 16])
}

fn edge_id(key: Key) -> EdgeId {
    EdgeId::from_bytes([0x40 + key; 16])
}

/// The tag id a showcase tag key stands for, for the filter panel.
pub fn tag_id(key: Key) -> TagId {
    TagId::from_bytes([0x80 + key; 16])
}

/// The discussion id a showcase comment key stands for.
pub fn comment_id(key: Key) -> CommentId {
    CommentId::from_bytes([0x20 + key; 16])
}

/// The text of the note card, and the prompt the whole scene is built around.
pub const PROMPT: &str = "A quiet forest at dawn, soft light through the trees.";
pub const NOTE: &str = "Dawn pass: keep the warm horizon, lift the shadows once, then hold the \
                        tree line crisp. Grade on plate 01 only; the aurora plate is a look test.";

/// An app plus the canvas a showcase is built on.
pub struct Stage {
    pub app: NodeCraftApp,
    canvas: CanvasId,
}

impl Stage {
    /// A headless app with an empty canvas: the starter sample is removed through `node.remove`,
    /// so every card in a shot was placed by this file.
    ///
    /// # Errors
    /// Returns the engine's message if the registries or a command fail.
    pub fn new() -> Result<Self, Fail> {
        let mut app = NodeCraftApp::headless().map_err(|error| fail(error.message))?;
        let canvas = app.canvas;
        let starter: Vec<NodeId> = app.session.document().canvases().get(&canvas).map(|canvas| canvas.nodes().keys().copied().collect()).unwrap_or_default();
        for id in starter {
            app.session.dispatch("node.remove", &json!({"canvas": canvas, "id": id})).map_err(|error| fail(error.message))?;
        }
        Ok(Self { app, canvas })
    }

    fn dispatch(&mut self, command: &str, params: serde_json::Value) -> Result<(), Fail> {
        self.app.session.dispatch(command, &params).map(|_| ()).map_err(|error| fail(format!("{command}: {}", error.message)))
    }

    /// A card of `kind` at world `x`,`y`, with the kind's own ports and defaults (`node.create`).
    ///
    /// # Errors
    /// Returns the engine's message if the kind is unknown or the command is refused.
    pub fn node(&mut self, key: Key, kind: &str, x: f32, y: f32) -> Result<(), Fail> {
        self.dispatch("node.create", json!({"canvas": self.canvas, "id": node_id(key), "kind": kind, "pos": {"x": x, "y": y}}))
    }

    /// A card whose ports come from its instance config: a router carrying `modality`.
    ///
    /// # Errors
    /// Returns the engine's message if the kind is unknown or the command is refused.
    pub fn router(&mut self, key: Key, modality: Modality, x: f32, y: f32) -> Result<(), Fail> {
        let kinds = nodecraft_nodes::Registry::builtins().map_err(|error| fail(error.message))?;
        let kind = kinds.get("structure.router").map_err(|error| fail(error.message))?;
        let mut node =
            kind.instantiate_instance(node_id(key), nodecraft_nodes::router::config(modality), &BTreeMap::new()).map_err(|error| fail(error.message))?;
        node.pos = Position { x, y };
        self.dispatch("node.add", json!({"canvas": self.canvas, "node": node}))
    }

    /// `node.param_set`: one parameter, exactly as the inspector writes it.
    ///
    /// # Errors
    /// Returns the engine's message if the parameter is unknown or the value is refused.
    pub fn set(&mut self, key: Key, param: &str, value: Value) -> Result<(), Fail> {
        self.dispatch("node.param_set", json!({"canvas": self.canvas, "id": node_id(key), "name": param, "value": value}))
    }

    /// A number for a slider or a slider-like parameter.
    pub fn number(&mut self, key: Key, param: &str, value: f64) -> Result<(), Fail> {
        let parsed = nodecraft_types::Number::new(value).map_err(|error| fail(error.message))?;
        self.set(key, param, Value::Number(parsed))
    }

    /// A whole-number parameter (the `unsigned` control).
    pub fn count(&mut self, key: Key, param: &str, value: u64) -> Result<(), Fail> {
        self.set(key, param, Value::Unsigned(Unsigned(value)))
    }

    pub fn text(&mut self, key: Key, param: &str, value: &str) -> Result<(), Fail> {
        self.set(key, param, Value::Text(value.to_owned()))
    }

    pub fn toggle(&mut self, key: Key, param: &str, value: bool) -> Result<(), Fail> {
        self.set(key, param, Value::Bool(value))
    }

    /// `node.title`: the name painted above the card.
    pub fn title(&mut self, key: Key, title: &str) -> Result<(), Fail> {
        self.dispatch("node.title", json!({"canvas": self.canvas, "id": node_id(key), "title": title}))
    }

    /// `edge.connect`: one noodle between two named ports, at input `order`.
    pub fn wire(&mut self, key: Key, from: (Key, &str), to: (Key, &str), order: u32) -> Result<(), Fail> {
        self.dispatch(
            "edge.connect",
            json!({"canvas": self.canvas, "edge": {
                "id": edge_id(key),
                "from": {"node": node_id(from.0), "port": from.1},
                "to": {"node": node_id(to.0), "port": to.1},
                "order": order,
            }}),
        )
    }

    /// `tag.put` plus `node.tag_set`: a named colour tag on a set of cards.
    pub fn tag(&mut self, key: Key, title: &str, color: &str, nodes: &[Key]) -> Result<(), Fail> {
        let tag = tag_id(key);
        self.dispatch("tag.put", json!({"canvas": self.canvas, "tag": {"id": tag, "title": title, "color": color}}))?;
        let ids: Vec<NodeId> = nodes.iter().copied().map(node_id).collect();
        self.dispatch("node.tag_set", json!({"canvas": self.canvas, "nodes": ids, "tag": tag}))
    }

    /// `comment.create`: a discussion pinned at world `x`,`y`, or beside a card when anchored.
    pub fn comment(&mut self, key: Key, text: &str, anchor: Option<Key>, offset: (f32, f32)) -> Result<(), Fail> {
        self.dispatch(
            "comment.create",
            json!({"canvas": self.canvas, "id": comment_id(key), "text": text, "pos": {"x": offset.0, "y": offset.1},
                   "node": anchor.map(node_id)}),
        )
    }

    /// `comment.reply`: a second message in an existing discussion.
    pub fn reply(&mut self, key: Key, root: Key, text: &str) -> Result<(), Fail> {
        self.dispatch("comment.reply", json!({"canvas": self.canvas, "id": comment_id(root), "reply": comment_id(key), "text": text}))
    }

    /// `engine::import::import_files`: the same path an OS file drop takes. It puts the bytes in
    /// the content-addressed store and creates a `source.*` card that references them, so the
    /// card's preview is a real decode of a real asset.
    pub fn import_image(&mut self, key: Key, name: &str, bytes: &[u8], x: f32, y: f32) -> Result<(), Fail> {
        let incoming = [Incoming { name, bytes }];
        let ids = [node_id(key)];
        import_files(&mut self.app.session, self.canvas, Position { x, y }, 0.0, &incoming, &ids).map_err(|error| fail(error.message))?;
        Ok(())
    }
}

/// Whether the live pass has recorded a result on a card yet. Worker-thread work lands a frame
/// or two after the edit, so the renderer waits on this instead of guessing a delay.
pub fn has_result(app: &NodeCraftApp, key: Key) -> bool {
    app.session.document().canvases().get(&app.canvas).and_then(|canvas| canvas.nodes().get(&node_id(key))).is_some_and(|node| !node.history.is_empty())
}

/// The showcase document: a dawn-plate look-development pass with a text chain beside it, three
/// colour tags, a discussion, and the modality pairs that show off the wire palette.
///
/// Shared by every shot, so all the images in the README are the same workflow seen from
/// different distances — and each card is real, placed by a command.
///
/// # Errors
/// Returns the first command failure, with the command id in the message.
pub fn showcase(plates: &[(&str, Vec<u8>)]) -> Result<Stage, Fail> {
    let mut stage = Stage::new()?;

    // Top row: the words. One prompt feeds a splitter and a rewrite; both join into one list
    // input, which is what makes the numbered order badges appear on the noodles.
    stage.node(1, "source.text", -1_020.0, -560.0)?;
    stage.text(1, "text", PROMPT)?;
    stage.title(1, "Idea")?;
    stage.node(2, "text.split", -460.0, -620.0)?;
    stage.title(2, "Beat sheet")?;
    stage.text(2, "mode", "separator")?;
    stage.text(2, "separator", " | ")?;
    stage.count(2, "max_parts", 8)?;
    stage.node(3, "text.replace", 100.0, -600.0)?;
    stage.title(3, "Rewrite")?;
    stage.text(3, "find", "quiet")?;
    stage.text(3, "replacement", "still")?;
    stage.node(4, "text.concat", 660.0, -560.0)?;
    stage.title(4, "Final copy")?;
    stage.text(4, "separator", "\n")?;
    stage.toggle(4, "trim", true)?;

    // Bottom row: the picture. The plate is imported, so its card shows a real decoded preview,
    // and the three action cards recompute for real (the app's live pass, #88).
    if let Some((name, bytes)) = plates.first() {
        stage.import_image(5, name, bytes, -1_020.0, 120.0)?;
        stage.title(5, "Plate 01")?;
    }
    stage.node(6, "image.grade", -460.0, 100.0)?;
    stage.title(6, "Colour grade")?;
    stage.number(6, "brightness", 0.12)?;
    stage.number(6, "contrast", 1.28)?;
    stage.number(6, "saturation", 1.16)?;
    stage.number(6, "warmth", 0.34)?;
    stage.node(7, "image.blur", 100.0, 140.0)?;
    stage.title(7, "Soften")?;
    stage.number(7, "sigma", 6.0)?;
    stage.node(8, "image.aspect", 660.0, 80.0)?;
    stage.title(8, "Poster 16:9")?;
    stage.text(8, "ratio", "landscape16_9")?;
    stage.text(8, "mode", "crop")?;

    // Annotations and the modality palette: a note card, and routers that carry the two
    // modalities no builtin action produces.
    stage.node(9, "structure.note", -1_020.0, -1_020.0)?;
    stage.title(9, "Notes")?;
    stage.text(9, "text", NOTE)?;
    stage.router(10, Modality::Mesh, 3_600.0, 140.0)?;
    stage.title(10, "3D pass")?;
    stage.router(11, Modality::Document, 3_600.0, -260.0)?;
    stage.title(11, "Brief")?;
    stage.node(12, "source.video", 2_200.0, -260.0)?;
    stage.title(12, "Clip 01")?;
    stage.node(13, "source.audio", 2_200.0, 140.0)?;
    stage.title(13, "Bed 01")?;
    stage.router(14, Modality::Video, 2_760.0, -260.0)?;
    stage.title(14, "Edit conform")?;
    stage.router(15, Modality::Audio, 2_760.0, 140.0)?;
    stage.title(15, "Bed mix")?;

    stage.wire(1, (1, "out"), (2, "text"), 0)?;
    stage.wire(2, (1, "out"), (3, "text"), 0)?;
    stage.wire(3, (2, "out"), (4, "texts"), 0)?;
    stage.wire(4, (3, "out"), (4, "texts"), 1)?;
    stage.wire(5, (5, "out"), (6, "image"), 0)?;
    stage.wire(6, (6, "out"), (7, "image"), 0)?;
    stage.wire(7, (7, "out"), (8, "image"), 0)?;
    stage.wire(8, (12, "out"), (14, "in"), 0)?;
    stage.wire(9, (13, "out"), (15, "in"), 0)?;

    stage.tag(1, "Look", "green", &[5, 6, 7, 8])?;
    stage.tag(2, "Copy", "blue", &[2, 3, 4])?;
    stage.tag(3, "Draft", "pink", &[1])?;

    stage.comment(1, "Warmth is doing all the work here - can we keep the tree line crisp?", Some(6), (0.0, 0.0))?;
    stage.reply(2, 1, "Yes: soften is at 6, dropping it to 3 keeps the edges.")?;
    stage.comment(3, "Poster crop must keep the sun out of the corner.", Some(8), (0.0, 0.0))?;
    Ok(stage)
}
