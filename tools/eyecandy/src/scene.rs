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
use nodecraft_types::{CanvasId, CommentId, EdgeId, FieldId, Modality, NodeId, TagId, Unsigned, Value};
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
pub const NOTE: &str = "Three original plates, one local look lab. Branch dawn into soft and noir looks; \
                        tint the aurora, mirror the bloom, then recombine. No model calls.";

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

    /// A persistent coloured zone and its membership, through `field.create`.
    pub fn field(&mut self, key: Key, title: &str, color: &str, rect: (f32, f32, f32, f32), nodes: &[Key]) -> Result<(), Fail> {
        let ids: Vec<NodeId> = nodes.iter().copied().map(node_id).collect();
        self.dispatch(
            "field.create",
            json!({"canvas": self.canvas, "field": {
            "id": FieldId::from_bytes([key; 16]), "title": title, "color": color,
            "rect": {"pos": {"x": rect.0, "y": rect.1}, "width": rect.2, "height": rect.3}
        }, "nodes": ids}),
        )
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
    app.session
        .document()
        .canvases()
        .get(&app.canvas)
        .and_then(|canvas| canvas.nodes().get(&node_id(key)))
        .is_some_and(|node| nodecraft_engine::split::image_of(node).is_some())
}

/// The showcase document: three original plates, branching deterministic looks and two-input
/// composites, organised in coloured fields; text, notes and typed routing remain nearby.
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
    stage.node(1, "source.text", -1_020.0, -1_660.0)?;
    stage.text(1, "text", PROMPT)?;
    stage.title(1, "Idea")?;
    stage.node(2, "text.split", -460.0, -1_720.0)?;
    stage.title(2, "Beat sheet")?;
    stage.text(2, "mode", "separator")?;
    stage.text(2, "separator", " | ")?;
    stage.count(2, "max_parts", 8)?;
    stage.node(3, "text.replace", 100.0, -1_700.0)?;
    stage.title(3, "Rewrite")?;
    stage.text(3, "find", "quiet")?;
    stage.text(3, "replacement", "still")?;
    stage.node(4, "text.concat", 660.0, -1_660.0)?;
    stage.title(4, "Final copy")?;
    stage.text(4, "separator", "\n")?;
    stage.toggle(4, "trim", true)?;

    // Bottom row: the picture. The plate is imported, so its card shows a real decoded preview,
    // and the three action cards recompute for real (the app's live pass, #88).
    if plates.len() != 3 {
        return Err(fail("the showcase needs exactly three original plates: dawn, aurora, bloom"));
    }
    for ((key, title, y), (name, bytes)) in [(5, "01 / Dawn", 120.0), (17, "02 / Aurora", 1_040.0), (19, "03 / Bloom", 1_960.0)].into_iter().zip(plates) {
        stage.import_image(key, name, bytes, -1_020.0, y)?;
        stage.title(key, title)?;
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

    // Parallel local treatments, then two real two-input recombinations.
    stage.node(16, "image.filter", 100.0, 1_040.0)?;
    stage.title(16, "Noir / alternate")?;
    stage.text(16, "preset", "noir")?;
    stage.number(16, "intensity", 0.92)?;
    stage.node(18, "image.tint", -460.0, 1_040.0)?;
    stage.title(18, "Aurora / violet screen")?;
    stage.text(18, "color", "#9966ff")?;
    stage.text(18, "blend", "screen")?;
    stage.number(18, "intensity", 0.38)?;
    stage.node(20, "image.flip", -460.0, 1_960.0)?;
    stage.title(20, "Bloom / mirror")?;
    stage.text(20, "axis", "horizontal")?;
    stage.node(21, "image.overlay", 660.0, 1_040.0)?;
    stage.title(21, "Noir + aurora / inset")?;
    stage.text(21, "anchor", "top_right")?;
    stage.number(21, "size", 0.48)?;
    stage.number(21, "opacity", 0.88)?;
    stage.node(22, "image.overlay", 100.0, 1_960.0)?;
    stage.title(22, "Look board / bloom inset")?;
    stage.text(22, "anchor", "bottom_left")?;
    stage.number(22, "size", 0.42)?;
    stage.number(22, "opacity", 0.94)?;
    stage.node(23, "image.resize", 660.0, 1_960.0)?;
    stage.title(23, "Review / half resolution")?;
    stage.number(23, "factor", 0.5)?;
    stage.text(23, "resampling", "area")?;

    // A separate typed-routing explainer uses real text and a real imported image, never empty
    // video/audio cards that could be mistaken for working media or provider output.
    stage.node(9, "structure.note", -1_020.0, -2_120.0)?;
    stage.title(9, "Notes")?;
    stage.text(9, "text", NOTE)?;
    stage.router(10, Modality::Text, 3_320.0, -260.0)?;
    stage.title(10, "Copy / routed")?;
    stage.router(11, Modality::Image, 3_320.0, 140.0)?;
    stage.title(11, "Plate / routed")?;
    stage.node(12, "source.text", 2_200.0, -260.0)?;
    stage.title(12, "Copy / source")?;
    stage.text(12, "text", "Original plates. Local actions. Repeatable looks.")?;
    if let Some((name, bytes)) = plates.first() {
        stage.import_image(13, name, bytes, 2_200.0, 140.0)?;
    }
    stage.title(13, "Dawn / source")?;
    stage.router(14, Modality::Text, 2_760.0, -260.0)?;
    stage.title(14, "Text / cable tie")?;
    stage.router(15, Modality::Image, 2_760.0, 140.0)?;
    stage.title(15, "Image / cable tie")?;

    stage.wire(1, (1, "out"), (2, "text"), 0)?;
    stage.wire(2, (1, "out"), (3, "text"), 0)?;
    stage.wire(3, (2, "out"), (4, "texts"), 0)?;
    stage.wire(4, (3, "out"), (4, "texts"), 1)?;
    stage.wire(5, (5, "out"), (6, "image"), 0)?;
    stage.wire(6, (6, "out"), (7, "image"), 0)?;
    stage.wire(7, (7, "out"), (8, "image"), 0)?;
    stage.wire(8, (12, "out"), (14, "in"), 0)?;
    stage.wire(9, (13, "out"), (15, "in"), 0)?;
    stage.wire(10, (14, "out"), (10, "in"), 0)?;
    stage.wire(11, (15, "out"), (11, "in"), 0)?;
    stage.wire(12, (6, "out"), (16, "image"), 0)?;
    stage.wire(13, (17, "out"), (18, "image"), 0)?;
    stage.wire(14, (19, "out"), (20, "image"), 0)?;
    stage.wire(15, (16, "out"), (21, "image"), 0)?;
    stage.wire(16, (18, "out"), (21, "overlay"), 0)?;
    stage.wire(17, (21, "out"), (22, "image"), 0)?;
    stage.wire(18, (20, "out"), (22, "overlay"), 0)?;
    stage.wire(19, (22, "out"), (23, "image"), 0)?;

    stage.field(1, "01 / DAWN — branch the grade", "orange", (-1_110.0, -10.0, 2_180.0, 890.0), &[5, 6, 7, 8])?;
    stage.field(2, "02 / AURORA — tint + recombine", "blue", (-1_110.0, 910.0, 2_180.0, 890.0), &[17, 18, 16, 21])?;
    stage.field(3, "03 / BLOOM — mirror + deliver", "pink", (-1_110.0, 1_830.0, 2_180.0, 890.0), &[19, 20, 22, 23])?;

    stage.tag(1, "Look", "green", &[5, 6, 7, 8, 16, 17, 18, 19, 20, 21, 22, 23])?;
    stage.tag(2, "Copy", "blue", &[2, 3, 4])?;
    stage.tag(3, "Draft", "pink", &[1])?;

    stage.comment(1, "Warmth is doing all the work here - can we keep the tree line crisp?", Some(6), (0.0, 0.0))?;
    stage.reply(2, 1, "Yes: soften is at 6, dropping it to 3 keeps the edges.")?;
    stage.comment(3, "Poster crop must keep the sun out of the corner.", Some(8), (0.0, 0.0))?;
    Ok(stage)
}
