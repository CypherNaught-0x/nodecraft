//! Fonts belong to their Deck card, as on a Layer Editor card (#693).
use super::{card, object, revision_value, stored, writable};
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::layer_cmds::fonts as shared;
use nodecraft_actions::deck::{Item, Slide};
use nodecraft_actions::layers::{self, Content, Layer, same_name};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value as Json, json};

pub use shared::{held, listed};

/// Imports bytes through the ordinary asset store, then adds the font as one undo step. The
/// optional revision is checked before import and again by the command, never inferred later.
pub fn import(session: &mut crate::Session, canvas: CanvasId, id: NodeId, bytes: &[u8], expected_revision: Option<u64>) -> Result<String, Error> {
    if expected_revision.is_some_and(|expected| expected != session.revision()) {
        return Err(Error::new(ErrorKind::Conflict, "the document changed; list the deck again before changing it"));
    }
    let listing = session.dispatch("deck.list", &json!({"canvas": canvas, "id": id}))?;
    if listing.get("writable").and_then(Json::as_bool) != Some(true) {
        return Err(Error::new(ErrorKind::Unsupported, "this deck was made by a newer NodeCraft; it is kept as it is and can't be changed here"));
    }
    let revision = session.revision();
    shared::import_into(session, canvas, id, bytes, "deck.font_add", Some(revision))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParams {
    canvas: CanvasId,
    id: NodeId,
    font: AssetRef,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}

fn add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AddParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    writable(c, &stored(node)?, p.expected_revision)?;
    shared::add_to(c, p.canvas, node, &p.font, "a Deck card")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    canvas: CanvasId,
    id: NodeId,
    name: String,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RemoveParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    writable(c, &stored(node)?, p.expected_revision)?;
    if p.name.trim().is_empty() || p.name.len() > layers::MAX_NAME_BYTES {
        return Err(invalid("the font name is empty or too long"));
    }
    shared::remove_from(p.canvas, node, &p.name)
}

/// Resolve requested names to the card's imported references without opening font payloads on
/// every listing. A renderer additionally checks that the referenced bytes can be read/drawn.
pub(super) fn resolutions(slide: &Slide, fonts: &[(String, AssetRef)]) -> Json {
    Json::Array(
        slide
            .layers
            .iter()
            .filter_map(|item| match item {
                Item::Layer(Layer { id, content: Content::Text { font, .. }, .. }) => {
                    let matched = fonts.iter().find(|(name, _)| same_name(name, font));
                    let builtin = same_name(font, layers::BUILTIN_FONT);
                    Some(json!({
                        "layer": id,
                        "requested": font,
                        "resolved": matched.map_or(layers::BUILTIN_FONT, |(name, _)| name.as_str()),
                        "font": matched.map(|(_, asset)| asset),
                        "fallback": !builtin && matched.is_none(),
                    }))
                }
                _ => None,
            })
            .collect(),
    )
}

fn add_schema() -> Json {
    object(&["font"], json!({"font": shared::font_schema()}), true)
}

fn remove_schema() -> Json {
    object(&["name"], json!({"name": {"type": "string", "minLength": 1, "maxLength": layers::MAX_NAME_BYTES}}), true)
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "deck.font_add",
            label: "Add deck font",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: add_schema,
            params_doc: "{canvas, id: Deck NodeId, font: imported asset, expected_revision?} -> {changed, name}; adds a single TrueType/OpenType font to this Deck card as one undo step. Its declared family/style names it, ignoring case and surrounding space; a same-name font is replaced. At most 16 fonts, 16 MiB each. Lato is built in and cannot be replaced. The references save, copy and undo with the deck. Text naming an unavailable font draws with Lato and reports the fallback.",
            enabled: super::always,
            run: add,
        },
        CommandSpec {
            id: "deck.font_remove",
            label: "Remove deck font",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: remove_schema,
            params_doc: "{canvas, id: Deck NodeId, name, expected_revision?} -> {changed}; removes this deck's font of that name, case-insensitively, as one undo step. Text retains its requested name and draws with Lato until the font is added again. A revision conflict refuses before changing the deck.",
            enabled: super::always,
            run: remove,
        },
    ]
}

#[cfg(test)]
mod tests;
