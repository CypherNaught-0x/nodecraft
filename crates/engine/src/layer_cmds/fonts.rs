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
pub fn held(node: &Node) -> Vec<(String, AssetRef)> {
    let Some(Value::List(pairs)) = node.config.get(FONTS_CONFIG) else { return Vec::new() };
    pairs
        .iter()
        .filter_map(|pair| match pair {
            Value::List(pair) => match pair.as_slice() {
                [Value::Text(name), Value::Media(asset)]
                    if !name.trim().is_empty()
                        && name.len() <= layers::MAX_NAME_BYTES
                        && !name.chars().any(char::is_control)
                        && !same_name(name, layers::BUILTIN_FONT) =>
                {
                    Some((name.clone(), asset.clone()))
                }
                _ => None,
            },
            _ => None,
        })
        .take(MAX_FONTS)
        .collect()
}

/// The fonts as `layers.list` shows them.
pub fn listed(node: &Node) -> Json {
    Json::Array(held(node).into_iter().map(|(name, font)| json!({"name": name, "font": font})).collect())
}

/// The bytes of a font asset in this project, at most [`MAX_FONT_BYTES`]; a larger asset is
/// refused before any of it is read.
pub(crate) fn bytes(assets: &nodecraft_format::AssetStore, font: &AssetRef) -> Result<Vec<u8>, Error> {
    let too_large = || invalid("the font is larger than 16 MiB");
    if let Some(resident) = assets.get(&font.hash) {
        return if resident.len() > MAX_FONT_BYTES { Err(too_large()) } else { Ok(resident.to_vec()) };
    }
    let missing = || Error::new(ErrorKind::NotFound, "the font isn't in this project; import it first");
    let read = assets.preview_read(font).map_err(|error| match error {
        nodecraft_format::PreviewReadError::TooLarge => too_large(),
        nodecraft_format::PreviewReadError::Missing => missing(),
        _ => invalid("the font can't be read"),
    })?;
    if read.byte_len() > u64::try_from(MAX_FONT_BYTES).unwrap_or(u64::MAX) {
        return Err(too_large());
    }
    let bytes = read.read_verified(&Default::default(), &Default::default()).map_err(|_| invalid("the font can't be read"))?;
    if bytes.len() > MAX_FONT_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Imports a font file and adds it to the Layer Editor node `id` (`layers.font_add`), returning
/// the name layers use for it. A file that isn't a font, or is named Lato, is refused before
/// anything is stored.
pub fn import(session: &mut crate::Session, canvas: CanvasId, id: NodeId, bytes: &[u8]) -> Result<String, Error> {
    import_into(session, canvas, id, bytes, "layers.font_add", None)
}

/// Shared byte validation and import; each caller owns its target and revision checks.
pub(crate) fn import_into(
    session: &mut crate::Session,
    canvas: CanvasId,
    id: NodeId,
    bytes: &[u8],
    command: &str,
    expected: Option<u64>,
) -> Result<String, Error> {
    let info = font_info(bytes).map_err(not_a_font)?;
    refuse_builtin(&info.name)?;
    let hash = session.import_asset(bytes)?;
    let mut input = json!({"canvas": canvas, "id": id, "font": {"hash": hash, "mime": info.mime}});
    if let Some(expected) = expected {
        input.as_object_mut().ok_or_else(|| invalid("the font request can't be formed"))?.insert("expected_revision".into(), json!(expected));
    }
    let added = session.dispatch(command, &input)?;
    Ok(added.get("name").and_then(Json::as_str).unwrap_or(&info.name).to_owned())
}

fn not_a_font(error: nodecraft_actions::image::ImageError) -> Error {
    Error::new(ErrorKind::InvalidInput, format!("that isn't a font NodeCraft can draw with: {error}"))
}

fn refuse_builtin(name: &str) -> Result<(), Error> {
    if same_name(name, layers::BUILTIN_FONT) {
        return Err(invalid("Lato is built in; a font of that name can't be added"));
    }
    Ok(())
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
    add_to(c, p.canvas, node, &p.font, "a Layer Editor")
}

/// Stores the same bounded font collection on either card kind, after its own edit guard.
pub(crate) fn add_to(c: &Context<'_>, canvas: CanvasId, node: &Node, asset: &AssetRef, label: &str) -> Result<Effect, Error> {
    let info = font_info(&bytes(c.assets, asset)?).map_err(not_a_font)?;
    refuse_builtin(&info.name)?;
    let mime = info.mime.parse().map_err(|_| invalid("the font's type can't be recorded"))?;
    let font = AssetRef { hash: asset.hash, mime, meta: Default::default() };
    let mut fonts = held(node);
    fonts.retain(|(name, _)| !same_name(name, &info.name));
    if fonts.len() >= MAX_FONTS {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{label} holds at most {MAX_FONTS} fonts; remove one first")));
    }
    fonts.push((info.name.clone(), font));
    Ok(store(canvas, node, fonts, json!({"changed": true, "name": info.name})))
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
    remove_from(p.canvas, node, &p.name)
}

pub(crate) fn remove_from(canvas: CanvasId, node: &Node, name: &str) -> Result<Effect, Error> {
    let mut fonts = held(node);
    let before = fonts.len();
    fonts.retain(|(held, _)| !same_name(held, name));
    if fonts.len() == before {
        return Err(Error::new(ErrorKind::NotFound, "the node holds no font of that name"));
    }
    Ok(store(canvas, node, fonts, json!({"changed": true})))
}

pub(crate) fn font_schema() -> Json {
    json!({"type": "object", "required": ["hash", "mime"], "additionalProperties": false, "properties": {
        "hash": {"type": "string", "pattern": "^[0-9a-f]{64}$"}, "mime": {"type": "string"}, "meta": {"type": "object"}}})
}

fn add_schema() -> Json {
    object(&["font"], props(json!({"font": font_schema()})))
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
