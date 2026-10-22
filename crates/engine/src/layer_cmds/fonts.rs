//! The fonts a Layer Editor node holds (#548). A font is an imported TrueType or OpenType file,
//! stored in the node's config under the name it declares; a text layer naming it draws with it,
//! and one naming a font the node doesn't hold draws with the bundled Lato. Adding a font of a
//! name the node already holds replaces it. Each change is one undo step.
use super::{always, editor, object, props};
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_actions::layers::{self, MAX_FONT_BYTES, MAX_FONTS, font_info, same_name};
use nodecraft_graph::{Change, Changeset, Node};
use nodecraft_nodes::layers::FONTS_CONFIG;
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};

/// The node's fonts, by name, in the order they were added.
pub fn held(node: &Node) -> Result<Vec<(String, AssetRef)>, Error> {
    Ok(nodecraft_nodes::layers::fonts(node)?.into_iter().map(|(name, asset)| (name.to_owned(), asset.clone())).collect())
}

/// The fonts as `layers.list` shows them.
pub fn listed(node: &Node) -> Result<Json, Error> {
    Ok(Json::Array(held(node)?.into_iter().map(|(name, font)| json!({"name": name, "font": font})).collect()))
}

/// The bytes of a font asset in this project, at most [`MAX_FONT_BYTES`].
fn bytes(c: &Context<'_>, font: &AssetRef) -> Result<Vec<u8>, Error> {
    if let Some(resident) = c.assets.get(&font.hash) {
        return Ok(resident.to_vec());
    }
    let missing = || Error::new(ErrorKind::NotFound, "the font isn't in this project; import it first");
    let read = c.assets.preview_read(font).map_err(|error| match error {
        nodecraft_format::PreviewReadError::TooLarge => invalid("the font is larger than 16 MiB"),
        nodecraft_format::PreviewReadError::Missing => missing(),
        _ => invalid("the font can't be read"),
    })?;
    let bytes = read.read_verified(&Default::default(), &Default::default()).map_err(|_| invalid("the font can't be read"))?;
    if bytes.len() > MAX_FONT_BYTES {
        return Err(invalid("the font is larger than 16 MiB"));
    }
    Ok(bytes)
}

/// Imports a font file and adds it to the Layer Editor node `id` (`layers.font_add`), returning
/// the name layers use for it. A file that isn't a font is refused before anything is stored.
pub fn import(session: &mut crate::Session, canvas: CanvasId, id: NodeId, bytes: &[u8]) -> Result<String, Error> {
    let info = font_info(bytes).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("that isn't a font NodeCraft can draw with: {e}")))?;
    let hash = session.import_asset(bytes)?;
    let added = session.dispatch("layers.font_add", &json!({"canvas": canvas, "id": id, "font": {"hash": hash, "mime": info.mime}}))?;
    Ok(added.get("name").and_then(Json::as_str).unwrap_or(&info.name).to_owned())
}

fn store(canvas: CanvasId, node: &Node, fonts: Vec<(String, AssetRef)>, result: Json) -> Effect {
    let mut changed = node.clone();
    if fonts.is_empty() {
        changed.config.remove(FONTS_CONFIG);
    } else {
        let pairs = fonts.into_iter().map(|(name, font)| Value::List(vec![Value::Text(name), Value::Media(font)])).collect();
        changed.config.insert(FONTS_CONFIG.into(), Value::List(pairs));
    }
    if changed == *node {
        let mut result = result;
        if let Some(map) = result.as_object_mut() {
            map.insert("changed".into(), json!(false));
        }
        return Effect::None { result };
    }
    Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas, node: changed }]), result, gesture: None }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParams {
    canvas: CanvasId,
    id: NodeId,
    font: AssetRef,
}

fn add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AddParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let info = font_info(&bytes(c, &p.font)?).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("that isn't a font NodeCraft can draw with: {e}")))?;
    if same_name(&info.name, layers::BUILTIN_FONT) {
        return Err(invalid("Lato is built in; a font of that name can't be added"));
    }
    let mime = info.mime.parse().map_err(|_| invalid("the font's type can't be recorded"))?;
    let font = AssetRef { hash: p.font.hash, mime, meta: Default::default() };
    let mut fonts = held(node)?;
    fonts.retain(|(name, _)| !same_name(name, &info.name));
    if fonts.len() >= MAX_FONTS {
        return Err(invalid("a Layer Editor holds at most 16 fonts; remove one first"));
    }
    fonts.push((info.name.clone(), font));
    Ok(store(p.canvas, node, fonts, json!({"changed": true, "name": info.name})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    canvas: CanvasId,
    id: NodeId,
    name: String,
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RemoveParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut fonts = held(node)?;
    let before = fonts.len();
    fonts.retain(|(name, _)| !same_name(name, &p.name));
    if fonts.len() == before {
        return Err(Error::new(ErrorKind::NotFound, "the node holds no font of that name"));
    }
    Ok(store(p.canvas, node, fonts, json!({"changed": true})))
}

fn add_schema() -> Json {
    let font = json!({"type": "object", "required": ["hash", "mime"], "additionalProperties": false, "properties": {
        "hash": {"type": "string", "pattern": "^[0-9a-f]{64}$"}, "mime": {"type": "string"}, "meta": {"type": "object"}}});
    object(&["font"], props(json!({"font": font})))
}

fn remove_schema() -> Json {
    object(&["name"], props(json!({"name": {"type": "string", "minLength": 1, "maxLength": 128}})))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "layers.font_add",
            label: "Add font",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: add_schema,
            params_doc: "{canvas, id: Layer Editor NodeId, font: imported asset} -> {changed, name}; adds a TrueType (.ttf) or OpenType (.otf) font to a Layer Editor node, as one undo step. Its name is the family it declares (with its style when that isn't Regular), which a text layer's font field names; a font of the same name is replaced. At most 16 fonts, 16 MiB each; collections (.ttc) and bitmap-only fonts are refused. A text layer naming a font the node doesn't hold draws with the bundled Lato.",
            enabled: always,
            run: add,
        },
        CommandSpec {
            id: "layers.font_remove",
            label: "Remove font",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: remove_schema,
            params_doc: "{canvas, id, name} -> {changed}; removes the font of that name from a Layer Editor node, as one undo step. Text layers naming it draw with Lato until a font of that name is added again.",
            enabled: always,
            run: remove,
        },
    ]
}
