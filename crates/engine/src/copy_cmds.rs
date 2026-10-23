//! `history.copy` (#831): puts one history result on the OS clipboard, text as text and an image
//! as pixels. The engine reads and decodes the result; the host only owns the clipboard, so a
//! host without one (the CLI, a headless session) refuses the command instead of pretending.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_format::AssetStore;
use nodecraft_format::asset_source::{StreamMetrics, TransferCancel};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, GenerationId, Modality, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};

/// Largest encoded image copied, in bytes.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// Widest or tallest image copied, in pixels: 8192² RGBA is 256 MiB on the clipboard.
pub const MAX_SIDE: u32 = 8192;
/// Most memory decoding may take: a 16-bit image, or an 8-bit one and its rotated copy, at
/// [`MAX_SIDE`]. Kept well under what a browser tab can allocate.
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

/// What `history.copy` puts on the clipboard, once read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clip {
    Text(String),
    /// Straight RGBA, row by row, `width * height * 4` bytes.
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

/// What the command chose to copy, before anything is read.
#[derive(Clone, Debug, PartialEq)]
pub enum Copied {
    Text(String),
    Image(AssetRef),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyParams {
    canvas: CanvasId,
    id: NodeId,
    generation: GenerationId,
    /// The output port to copy; the node's first output when omitted.
    #[serde(default)]
    port: Option<String>,
    /// Which item of a list output to copy.
    #[serde(default)]
    item: Option<usize>,
}

fn copy(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CopyParams = params(value)?;
    let (value, modality) = crate::history_cmds::result_value(c, p.canvas, p.id, p.generation, p.port.as_deref(), p.item)?;
    let what = match (value, modality) {
        (Value::Text(text), _) => Copied::Text(text.clone()),
        (Value::Media(asset), Modality::Image) => {
            c.validate_asset(asset)?;
            Copied::Image(asset.clone())
        }
        (Value::Media(_), _) => {
            return Err(Error::new(ErrorKind::Unsupported, "only text and image results copy to the clipboard; export this one to a file instead"));
        }
        _ => return Err(Error::new(ErrorKind::Unsupported, "this result has nothing to copy")),
    };
    if !c.services.contains_clipboard() {
        return Err(Error::new(ErrorKind::Unsupported, "there is no clipboard here; export the result to a file instead"));
    }
    Ok(Effect::Copy { what })
}

fn enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    copy(c, value).map(|_| ())
}

/// Reads and decodes what `history.copy` chose, within the caps.
pub(crate) fn read(assets: &AssetStore, what: Copied) -> Result<Clip, Error> {
    let asset = match what {
        Copied::Text(text) => return Ok(Clip::Text(text)),
        Copied::Image(asset) => asset,
    };
    // Checked before opening: opening hashes every byte.
    if assets.descriptor(&asset.hash).is_some_and(|descriptor| descriptor.bytes > MAX_IMAGE_BYTES as u64) {
        return Err(too_big());
    }
    let metrics = StreamMetrics::default();
    let verified = assets.open_verified(&asset, &TransferCancel::default(), &metrics)?;
    let length = verified.byte_len();
    if length > MAX_IMAGE_BYTES as u64 {
        return Err(too_big());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(MAX_IMAGE_BYTES));
    std::io::Read::read_to_end(&mut std::io::Read::take(verified.reader(metrics), length), &mut bytes)
        .map_err(|_| Error::new(ErrorKind::Internal, "the image couldn't be read from the project"))?;
    let limits = nodecraft_media::Limits { max_input_bytes: MAX_IMAGE_BYTES as u64, max_width: MAX_SIDE, max_height: MAX_SIDE, max_alloc: MAX_DECODE_BYTES };
    let (_, image) = nodecraft_media::decode_image(&bytes, &limits).map_err(|error| match error {
        nodecraft_media::MediaError::TooLarge { .. } | nodecraft_media::MediaError::LimitExceeded(_) => too_big(),
        error => Error::new(ErrorKind::InvalidInput, format!("the image can't be read: {error}")),
    })?;
    let image = image.into_rgba8();
    Ok(Clip::Image { width: image.width(), height: image.height(), rgba: image.into_raw() })
}

fn too_big() -> Error {
    Error::new(
        ErrorKind::Unsupported,
        format!(
            "images over {MAX_SIDE}×{MAX_SIDE} pixels, {} MiB, or {} MiB once decoded don't copy; export the result instead",
            MAX_IMAGE_BYTES / (1024 * 1024),
            MAX_DECODE_BYTES / (1024 * 1024)
        ),
    )
}

/// What a copy reports: the kind, and the size of what went on the clipboard.
pub(crate) fn summary(clip: &Clip) -> Json {
    match clip {
        Clip::Text(text) => json!({"copied": "text", "chars": text.chars().count()}),
        Clip::Image { width, height, .. } => json!({"copied": "image", "width": width, "height": height}),
    }
}

fn schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "generation"],
        "properties": {"canvas": uuid, "id": uuid, "generation": uuid, "port": {"type": "string", "maxLength": 256},
            "item": {"type": "integer", "minimum": 0}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "history.copy",
        label: "Copy result",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, id: NodeId, generation, port?: output name (default the first), item?: list index (as history.extract)} -> {copied: \"text\", chars} | {copied: \"image\", width, height}; puts a text or image result on the OS clipboard. Images are copied as pixels, up to 8192×8192 and 64 MiB; other media are refused (export them instead). Needs a host with a clipboard: the app, not the CLI.",
        enabled,
        run: copy,
    }]
}

#[cfg(test)]
mod tests;
