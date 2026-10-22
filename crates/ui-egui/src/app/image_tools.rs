//! The "Image tools" submenu of an image card (#103), drawn from `image_tool_cmds::menu`: every
//! tool, greyed with the reason when it can't be used there. A model tool adds a card wired to
//! the image (`image.tool`) and marks itself "AI": that card is priced when it runs. Inpaint and
//! expand open the mask editor, Make Element makes one from this image, and Split into grid
//! keeps its own size controls (drawn by the caller).
use super::{NodeCraftApp, mask_editor};
use crate::ids;
use nodecraft_engine::image_tool_cmds::{Entry, Tool};
use nodecraft_engine::mask_cmds;
use nodecraft_graph::{Endpoint, Node};
use nodecraft_types::{Modality, NodeId};
use serde_json::json;

/// What a click in the menu asks for, done once the frame no longer borrows the canvas.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Request {
    /// Open the mask editor on the card (inpaint, expand).
    Mask(NodeId),
    /// Make an Element from the card.
    Element(NodeId),
    /// Add the tool's node through `image.tool`.
    Tool { tool: Tool, from: Endpoint },
}

/// The image output the menu works on: the card's first image output.
pub(super) fn output(node: &Node) -> Option<Endpoint> {
    node.outputs.iter().find(|p| p.port_type.modality == Modality::Image).map(|p| Endpoint { node: node.id, port: p.name.clone() })
}

/// Whether `tool` adds a card that calls a model.
fn paid(tool: Tool) -> bool {
    matches!(tool, Tool::RemoveBackground | Tool::Upscale | Tool::SplitLayers | Tool::To3d)
}

/// Draws `entry` (anything but Split into grid); the request when it is clicked. `has_image`:
/// the card holds an image now, which the mask editor paints over.
pub(super) fn button(ui: &mut egui::Ui, entry: &Entry, node: &Node, from: &Endpoint, has_image: bool) -> Option<Request> {
    let tool = entry.tool;
    let painted = matches!(tool, Tool::Inpaint | Tool::Expand);
    let why = match &entry.reason {
        Some(reason) => Some(sentence(reason)),
        None if (painted && mask_editor::image_output(node).is_none()) || ((painted || tool == Tool::Element) && !has_image) => {
            Some("Run or load the image first".to_owned())
        }
        None => None,
    };
    let mut widget = egui::Button::new(tool.label());
    if paid(tool) {
        widget = widget.right_text("AI");
    }
    let r = ui.add_enabled(why.is_none(), widget);
    let r = match &why {
        Some(why) => r.on_disabled_hover_text(why.as_str()),
        None => r.on_hover_text(hover(entry)),
    };
    if !r.clicked() {
        return None;
    }
    ui.close();
    Some(match tool {
        Tool::Inpaint | Tool::Expand => Request::Mask(node.id),
        Tool::Element => Request::Element(node.id),
        _ => Request::Tool { tool, from: from.clone() },
    })
}

fn hover(entry: &Entry) -> String {
    match entry.tool {
        Tool::Inpaint => "Paint where the image should change".to_owned(),
        Tool::Expand => "Widen the image's canvas and fill the margins".to_owned(),
        Tool::Grade => "Adjust exposure, contrast and colour, on this computer".to_owned(),
        Tool::Element => "A reusable subject prompts can mention by #name".to_owned(),
        _ => match entry.models.first() {
            Some(model) => format!("Adds a {model} card fed by this image; it is priced when you run it"),
            None => String::new(),
        },
    }
}

/// `reason` as a sentence for a tooltip.
fn sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl NodeCraftApp {
    pub(super) fn image_tool(&mut self, request: Request) {
        match request {
            Request::Mask(node) => self.open_mask_editor_for(node),
            Request::Element(node) => {
                self.selection.only(node);
                self.make_element();
            }
            Request::Tool { tool, from } => self.add_image_tool(tool, from),
        }
    }

    /// Adds `tool`'s node beside the image, wired to it, as one undo step, and selects it.
    fn add_image_tool(&mut self, tool: Tool, from: Endpoint) {
        let document = self.session.document();
        let Some(source) = document.canvases().get(&self.canvas).and_then(|c| c.nodes().get(&from.node)) else { return };
        let revision = self.session.revision();
        let seed = ("image-tool", tool.id(), from.node, revision);
        let (Some(id), Some(first)) = (ids::node_id(document, (seed, 0)), ids::edge_id(document, (seed, 0))) else {
            self.notice = Some("Couldn't allocate node ids; try again.".into());
            return;
        };
        let mut params = json!({"canvas": self.canvas, "tool": tool.id(), "from": from, "id": id, "edges": [first], "pos": mask_cmds::beside(source.pos, 1.0)});
        if tool == Tool::SplitLayers {
            let (Some(editor), Some(second)) = (ids::node_id(document, (seed, 1)), ids::edge_id(document, (seed, 1))) else {
                self.notice = Some("Couldn't allocate node ids; try again.".into());
                return;
            };
            params["edges"] = json!([first, second]);
            params["editor"] = json!(editor);
        }
        match self.session.dispatch("image.tool", &params) {
            Ok(_) => {
                self.notice = None;
                self.selection.only(id);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}
