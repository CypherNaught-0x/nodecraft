//! Local export (#98): writing a *batch* of results to a folder, with a naming pattern.
//!
//! This module is the platform-neutral half: it decides *what* an export means (a name, a
//! collision policy, an output-format operation, a stable source mapping) and never touches a
//! filesystem, so the browser build compiles and can say what it cannot do. The bytes are written
//! by `engine`'s `local_export`, which reuses the same verifying, stage-and-link single-file
//! writer `history.export` (#557) uses — one file per planned entry, one shared publication path.
//!
//! Two properties are the contract, and everything below exists to keep them true:
//!
//! * **A published file is a complete file.** A cancel, a full disk or a failing read never leaves
//!   a half-written file at the final name, and never leaves a stray temporary behind.
//! * **Nothing is invented.** What is written is the stored result, byte for byte, and the export
//!   reports that it was a pass-through. Conversion, vectorisation, cloud destinations and
//!   timeline interchange (#166) are refused with an actionable error rather than approximated.
//!
//! # What #557 already owns
//!
//! `history.export` writes one result to one path a person chose, and it owns: the absolute-path
//! check, the "folder must exist" rule, refusing to replace without consent, following a link at
//! the target, hash-verifying the asset and streaming it through the store's verifying copy, and
//! refusing a text result. This module does not reimplement any of that. What is additive here is
//! only the things a *batch* needs: a naming pattern, a collision policy, folder authority,
//! cancellation between files, and one outcome per request.

use nodecraft_types::{Blake3, CanvasId, Error, ErrorKind, GenerationId, NodeId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Most results one export writes. A request for more is refused before anything is written.
pub const MAX_ENTRIES: usize = 256;
/// A filename template, in UTF-8 bytes.
pub const MAX_TEMPLATE_BYTES: usize = 128;
/// A rendered file name without its extension. Filesystems allow 255 bytes, so this leaves room
/// for the extension, a numbered suffix and a temporary name built around it.
pub const MAX_NAME_BYTES: usize = 120;
/// A derived extension, without the dot.
pub const MAX_EXTENSION_BYTES: usize = 8;
/// An export destination, in UTF-8 bytes.
pub const MAX_DESTINATION_BYTES: usize = 4096;
/// A requested output MIME type.
pub const MAX_MIME_BYTES: usize = 64;
/// The largest accepted list-item index. Larger is a parameter error, never a truncation.
pub const MAX_ITEM_INDEX: u32 = 4095;
/// How far the `numbered` collision policy searches for a free name.
pub const MAX_COLLISION_SUFFIX: u32 = 999;
/// The tokens a filename template may use, as shown in the input schema and the parameter error.
pub const TEMPLATE_TOKENS: &str = "index, node, generation, kind, title, output, item, hash";

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

pub(crate) fn unsupported(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

/// The accurate output-format story for this build (#98). Pass-through is the only operation that
/// exists, so codecs, quality, animated frames and SVG stay exactly as the result already was. The
/// other modes exist *so a form can say what is not available* rather than offering a format and
/// then writing the wrong bytes.
pub const FORMAT_CAPABILITY: &str = "original (pass-through) is the only supported output operation: \
    no conversion, re-encode, quality, size or vectorisation is performed. Write the stored PNG, JPEG, \
    WebP, GIF, BMP, TIFF, SVG, video or audio bytes exactly as the project holds them.";

/// Reasons an entry can fail or be left out. Fixed strings, so a report never carries a
/// caller-supplied path, MIME type or stored name.
pub mod reasons {
    /// The node is not on the requested canvas.
    pub const UNKNOWN_NODE: &str = "no node with this id is on this canvas";
    /// The node has no active generation.
    pub const NO_RESULT: &str = "this node has no active result to export; run it first";
    /// The port's value in the active generation is not a value an export can write. Text is
    /// refused for the same reason `history.export` refuses it (#557): copying is the way out.
    pub const UNSUPPORTED_VALUE: &str = "only an image, video, audio, 3D or document result exports to a file; copy text instead";
    /// A list output has no item at that index.
    pub const ITEM_OUT_OF_RANGE: &str = "this output has no item with that index";
    /// The name the template asked for could not be formed for this result.
    pub const NAME_FAILED: &str =
        "no safe file name could be formed; remove path separators and '..' from the node title or output, or choose a simpler template";
    /// The stored asset is not in the project's asset store.
    pub const READING_ASSET: &str = "the project no longer holds this result's bytes";
    /// The `numbered` policy ran out of suffixes.
    pub const NO_FREE_NAME: &str = "too many files already use this name for the numbered policy to find a free one";
    /// The `skip` policy left an existing file alone.
    pub const SKIPPED: &str = "a file with this name already exists and on_collision is skip";
    /// The export stopped before this entry was attempted.
    pub const NOT_STARTED: &str = "the export stopped before this file was started";
    /// The destination is not inside a folder the app authorized.
    pub const UNSUPPORTED_TARGET: &str = "the destination folder is not one the app has authorized for export";
    /// The destination does not exist. Phase 1 never creates folders.
    pub const DESTINATION_MISSING: &str = "the destination folder does not exist; create it before exporting";
    /// No host injected an export authority into this session.
    pub const NO_HOST: &str = "export needs an app that has authorized a destination folder; this host has not authorized one";
    /// The browser build has no folder to write into.
    pub const WASM_HOST: &str = "the browser build cannot write files into a folder; local export runs in the desktop app";
}

/// What to do when the destination name already holds a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collision {
    /// Leave the existing file alone and say so. The default: an export never replaces a file the
    /// person asking did not choose to replace.
    #[default]
    Skip,
    /// Replace the existing file, atomically.
    Overwrite,
    /// Try `name (2)`, `name (3)` … up to [`MAX_COLLISION_SUFFIX`].
    Numbered,
}

impl Collision {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Overwrite => "overwrite",
            Self::Numbered => "numbered",
        }
    }
}

/// The requested output-format operation. See [`FORMAT_CAPABILITY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatMode {
    /// Write the stored bytes unchanged.
    Original,
    /// Re-encode into `mime`: not implemented in this build.
    Convert,
    /// Turn a raster result into vectors: a separate quoted operation, not a format choice.
    Vectorise,
}

impl FormatMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Convert => "convert",
            Self::Vectorise => "vectorise",
        }
    }
}

/// The `format` parameter: an operation, plus the MIME type the unimplemented ones name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatRequest {
    pub mode: FormatMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

impl Default for FormatRequest {
    fn default() -> Self {
        Self { mode: FormatMode::Original, mime: None }
    }
}

/// An output operation a build actually performed on a file. Only one exists today; the type is
/// here so a later conversion can say which one ran instead of leaving a reader to guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatAction {
    /// The written bytes are the stored bytes.
    PassThrough,
}

impl FormatAction {
    pub const fn name(self) -> &'static str {
        match self {
            Self::PassThrough => "passthrough",
        }
    }
}

/// A MIME type as it may be echoed back in a message: lowercase `type/subtype`, no parameters.
/// Anything else is a parameter error, which is what keeps caller-controlled text out of errors.
pub fn validate_mime(mime: &str) -> Result<(), Error> {
    let valid = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-'));
    let (primary, subtype) = match mime.split_once('/') {
        Some(parts) if mime.len() <= MAX_MIME_BYTES => parts,
        _ => return Err(invalid("format.mime must be a lowercase type/subtype media type of at most 64 bytes")),
    };
    if valid(primary) && valid(subtype) { Ok(()) } else { Err(invalid("format.mime must be a lowercase type/subtype media type of at most 64 bytes")) }
}

impl FormatRequest {
    /// Validates the parameter pair, kept apart from [`Self::action`] so a plan reports a bad
    /// parameter rather than an unsupported operation.
    pub fn validate(&self) -> Result<(), Error> {
        match (self.mode, self.mime.as_deref()) {
            (FormatMode::Original, None) => Ok(()),
            (FormatMode::Original, Some(_)) => Err(invalid("format.mime is only used when format.mode is convert or vectorise")),
            (_, None) => Err(invalid("format.mime is required when format.mode is convert or vectorise")),
            (_, Some(mime)) => validate_mime(mime),
        }
    }

    /// The operation this build can perform, or why it cannot. Never a silent fallback: an
    /// unimplemented conversion must not write the stored bytes and claim otherwise.
    pub fn action(&self) -> Result<FormatAction, Error> {
        match self.mode {
            FormatMode::Original => Ok(FormatAction::PassThrough),
            FormatMode::Convert => {
                let mime = self.mime.as_deref().unwrap_or("the requested type");
                Err(unsupported(format!("converting a result to {mime} is not implemented in this build; {FORMAT_CAPABILITY}")))
            }
            FormatMode::Vectorise => {
                let mime = self.mime.as_deref().unwrap_or("the requested type");
                Err(unsupported(format!(
                    "turning a result into {mime} vectors is a separate quoted operation, not a format: choosing \
                     SVG does not convert a raster result. Write the stored bytes with format.mode \"original\" \
                     (an SVG source's own bytes pass through unchanged), or use a vectorisation model when one is available."
                )))
            }
        }
    }
}

/// One result an export names: which node, which generation, which port and which item of it.
/// This *is* the stable source mapping — the same value identifies the same file in a later run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportSource {
    pub canvas: CanvasId,
    pub node: NodeId,
    pub port: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationId>,
}

impl ExportSource {
    /// A deterministic key for this source, for reports and for callers that track files.
    pub fn stable_id(&self) -> String {
        let generation = self.generation.map(|id| id.to_string()).unwrap_or_else(|| "none".to_owned());
        match self.item {
            Some(item) => format!("{}/{}/{}/#{item}", self.node, generation, self.port),
            None => format!("{}/{}/{}", self.node, generation, self.port),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Index,
    Node,
    Generation,
    Kind,
    Title,
    Output,
    Item,
    Hash,
}

impl Token {
    const ALL: &'static [(&'static str, Self)] = &[
        ("index", Self::Index),
        ("node", Self::Node),
        ("generation", Self::Generation),
        ("kind", Self::Kind),
        ("title", Self::Title),
        ("output", Self::Output),
        ("item", Self::Item),
        ("hash", Self::Hash),
    ];

    /// The tokens a template may use, as shown in the parameter error and the input schema.
    pub const NAMES: &'static str = TEMPLATE_TOKENS;

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(token, _)| *token == name).map(|(_, token)| *token)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Literal(char),
    Token(Token),
}

/// A parsed filename template. Parsing is where a hostile template is refused: a literal reaches a
/// path only after [`sanitize`], and an unknown token is an error rather than a guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template(Vec<Part>);

impl Template {
    pub const DEFAULT: &'static str = "{title}-{output}-{index}";

    /// The default template, used when the parameter is absent.
    pub fn default_template() -> Self {
        Self::parse(Self::DEFAULT).unwrap_or_else(|_| Self(Vec::new()))
    }

    pub fn parse(value: &str) -> Result<Self, Error> {
        if value.is_empty() {
            return Err(invalid("the filename template is empty"));
        }
        if value.len() > MAX_TEMPLATE_BYTES {
            return Err(invalid("the filename template is longer than 128 bytes"));
        }
        if value.contains(['/', '\\']) || value.contains("..") {
            return Err(invalid("the filename template cannot contain path separators or '..'; use one filename component"));
        }
        // A control character is never what the person meant, and a name that silently absorbs one
        // is harder to spot in a folder than a parameter error that says what to change.
        if value.chars().any(|c| c.is_control()) {
            return Err(invalid("the filename template contains a control character; use the tokens and ordinary characters"));
        }
        let chars: Vec<char> = value.chars().collect();
        let mut parts = Vec::new();
        let mut index = 0_usize;
        while let Some(c) = chars.get(index).copied() {
            match c {
                '{' => {
                    let name_start = index.saturating_add(1);
                    let mut end = None;
                    let mut scan = name_start;
                    while let Some(next) = chars.get(scan).copied() {
                        if next == '}' {
                            end = Some(scan);
                            break;
                        }
                        if next == '{' {
                            return Err(invalid("the filename template has a nested opening brace"));
                        }
                        scan = scan.saturating_add(1);
                    }
                    let end = end.ok_or_else(|| invalid("the filename template has an unterminated token"))?;
                    let name: String = chars.get(name_start..end).unwrap_or_default().iter().collect();
                    parts.push(Part::Token(
                        Token::parse(&name)
                            .ok_or_else(|| invalid(format!("the filename template token \"{name}\" is unknown; use one of {}", Token::NAMES)))?,
                    ));
                    index = end.saturating_add(1);
                }
                '}' => return Err(invalid("the filename template has an unmatched closing brace")),
                other => {
                    parts.push(Part::Literal(other));
                    index = index.saturating_add(1);
                }
            }
        }
        Ok(Self(parts))
    }
}

impl Default for Template {
    fn default() -> Self {
        Self::default_template()
    }
}

/// The values a template can name for one entry.
#[derive(Clone, Debug)]
pub struct NameFields<'a> {
    pub index: u32,
    pub node: NodeId,
    pub generation: Option<GenerationId>,
    pub kind: &'a str,
    pub title: &'a str,
    pub output: &'a str,
    pub item: Option<u32>,
    pub hash: Option<Blake3>,
}

/// A name to write, without its directory: a stem and the extension derived from the MIME type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportName {
    pub stem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

impl ExportName {
    /// The name as it appears in the folder.
    pub fn file_name(&self) -> String {
        match &self.extension {
            Some(extension) => format!("{}.{extension}", self.stem),
            None => self.stem.clone(),
        }
    }

    /// The same name with a `numbered` collision suffix.
    pub fn with_suffix(&self, suffix: u32) -> String {
        let stem = sanitize(&self.stem, MAX_NAME_BYTES.saturating_sub(8));
        match &self.extension {
            Some(extension) => format!("{stem} ({suffix}).{extension}"),
            None => format!("{stem} ({suffix})"),
        }
    }
}

impl Template {
    /// Renders and sanitises one entry's name. The result is never empty, never holds a path
    /// separator, and is never longer than [`MAX_NAME_BYTES`] plus a bounded extension.
    pub fn render(&self, fields: &NameFields<'_>, mime: &str) -> Result<ExportName, Error> {
        // Names derived from a document are untrusted too. Refuse path syntax before any
        // sanitization, rather than silently turning a traversal-like name into a file.
        if [fields.kind, fields.title, fields.output].iter().any(|value| value.contains(['/', '\\']) || value.contains("..")) {
            return Err(invalid("a result name contains a path separator or '..'; rename the node or output before exporting"));
        }
        let mut out = String::new();
        for part in &self.0 {
            if out.len() >= MAX_NAME_BYTES {
                break;
            }
            let value = match part {
                Part::Literal(c) => c.to_string(),
                Part::Token(token) => match token {
                    Token::Index => fields.index.to_string(),
                    Token::Node => fields.node.to_string(),
                    Token::Generation => fields.generation.map(|id| id.to_string()).unwrap_or_else(|| "no-generation".to_owned()),
                    Token::Kind => fields.kind.chars().take(MAX_NAME_BYTES).collect::<String>().replace('.', "_"),
                    Token::Title => fields.title.chars().take(MAX_NAME_BYTES).collect(),
                    Token::Output => fields.output.chars().take(MAX_NAME_BYTES).collect(),
                    Token::Item => fields.item.map(|item| item.to_string()).unwrap_or_default(),
                    Token::Hash => fields.hash.map(|hash| hash.to_string().chars().take(8).collect()).unwrap_or_default(),
                },
            };
            for c in value.chars() {
                if out.len().saturating_add(c.len_utf8()) > MAX_NAME_BYTES {
                    break;
                }
                push_sanitized(&mut out, c);
            }
        }
        Ok(ExportName { stem: sanitize(&out, MAX_NAME_BYTES), extension: extension_for(mime) })
    }
}

/// Whether a file name may hold this character as itself. Everything else becomes `_`, which is
/// what makes traversal impossible: a separator can never be produced, only replaced.
fn keep(c: char) -> bool {
    !matches!(c, '/' | '\\') && !std::path::is_separator(c) && !c.is_control() && (c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '))
}

fn push_sanitized(out: &mut String, c: char) {
    // Overflowing the bound here is harmless: `sanitize` truncates on a character boundary.
    out.push(if keep(c) { c } else { '_' });
}

/// Trims the filesystem-hostile edges, replaces an empty result with a usable name, and truncates
/// on a character boundary so a multi-byte title can never be cut mid-character.
pub fn sanitize(value: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if out.len().saturating_add(c.len_utf8()) > max_bytes {
            break;
        }
        out.push(if keep(c) { c } else { '_' });
    }
    let trimmed = out.trim_matches(|c: char| c == '.' || c == ' ').to_owned();
    let named = match trimmed.as_str() {
        "" => "export".to_owned(),
        other => other.to_owned(),
    };
    if is_reserved_windows_name(&named) { format!("_{named}") } else { named }
}

/// Names Windows refuses to create at all. Without this, a template that happens to render one
/// would fail every export on that platform.
fn is_reserved_windows_name(name: &str) -> bool {
    let upper: String = name.chars().flat_map(char::to_uppercase).take(12).collect();
    let stem = upper.split('.').next().unwrap_or_default();
    if matches!(stem, "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let tail = stem.get(3..).unwrap_or_default();
    (stem.starts_with("COM") || stem.starts_with("LPT")) && !tail.is_empty() && tail.len() <= 3 && tail.bytes().all(|b| b.is_ascii_digit())
}

/// The extension a stored MIME type is written with. Known types have a fixed spelling; anything
/// else uses its own lowercase subtype, and nothing is invented for a type without a usable one.
pub fn extension_for(mime: &str) -> Option<String> {
    let essence = mime.split(';').next().unwrap_or_default().trim();
    let known = match essence {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/tiff" => "tif",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/mp4" => "m4a",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "application/pdf" => "pdf",
        "application/json" => "json",
        "model/gltf-binary" => "glb",
        _ => {
            let subtype = essence.rsplit('/').next().unwrap_or_default().split('+').next().unwrap_or_default();
            let usable = !subtype.is_empty() && subtype.len() <= MAX_EXTENSION_BYTES && subtype.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
            return usable.then(|| subtype.to_owned());
        }
    };
    Some(known.to_owned())
}

/// What one requested source resolved to, before any byte is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlannedEntry {
    /// A file to write, with its name, its stored MIME type and the operation it will get. `asset`
    /// is a reference into the project's asset store: `history.export`'s writer streams it out
    /// verified, so a planned entry is never a second copy of the result's bytes.
    Ready { source: ExportSource, name: ExportName, mime: String, bytes: u64, action: FormatAction, asset: nodecraft_types::AssetRef },
    /// A source that cannot become a file. It is reported, never dropped, so the count of results
    /// always matches the count of requests. `reason` is one of [`reasons`] or the message the
    /// shared document reader gave — the same words `history.export` (#557) would have said.
    Failed { source: ExportSource, reason: String },
}

impl PlannedEntry {
    pub fn source(&self) -> &ExportSource {
        match self {
            Self::Ready { source, .. } | Self::Failed { source, .. } => source,
        }
    }
}

/// A validated export: where it goes, what it does on a collision, and what each entry is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportPlan {
    pub destination: PathBuf,
    pub collision: Collision,
    pub format: FormatRequest,
    pub entries: Vec<PlannedEntry>,
}

/// Where an exported file got to. Every entry reaches exactly one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    /// Published under its final name.
    Written,
    /// Attempted, and it failed; `reason` says why, in `history.export`'s own words (#557).
    Failed,
    /// Not written because the policy said to leave the existing file alone.
    Skipped,
    /// Never started, because the export was cancelled or stopped first.
    NotStarted,
}

impl EntryState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::NotStarted => "not_started",
        }
    }
}

/// One entry's outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EntryOutcome {
    pub source: ExportSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub state: EntryState,
    /// Why this entry is not a file. Either one of [`reasons`] or the message the shared writer
    /// returned; both are fixed text that never repeats a path, a MIME type or a stored name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub bytes: u64,
    /// `passthrough`, or absent when nothing was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<FormatAction>,
}

/// The whole export's outcome: one entry per request, in the requested order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportOutcome {
    pub entries: Vec<EntryOutcome>,
    pub cancelled: bool,
}

impl ExportOutcome {
    pub fn count(&self, state: EntryState) -> usize {
        self.entries.iter().filter(|entry| entry.state == state).count()
    }

    /// Whether every requested entry became a published file.
    pub fn complete(&self) -> bool {
        !self.entries.is_empty() && self.count(EntryState::Written) == self.entries.len()
    }
}

/// What one document value can become in an export: the stored asset to write, the MIME type to
/// name it with, how long it is, and the content hash a `{hash}` token can use. `Err` carries a
/// reason naming why this value cannot be exported, which fails that entry only.
///
/// A media value is a *reference*: the export writes the stored asset, never a re-encoding of it.
/// A text value is refused here, the same way `history.export` refuses it (#557) — copying is the
/// way out for text, and a batch that wrote `.txt` files would contradict that decision.
pub fn media_for(store: &crate::AssetStore, value: &nodecraft_types::Value) -> Result<(nodecraft_types::AssetRef, String, u64, Option<Blake3>), &'static str> {
    match value {
        nodecraft_types::Value::Media(asset) => match store.descriptor(&asset.hash) {
            Some(descriptor) => Ok((asset.clone(), asset.mime.as_str().to_owned(), descriptor.bytes, Some(asset.hash))),
            None => Err(reasons::READING_ASSET),
        },
        _ => Err(reasons::UNSUPPORTED_VALUE),
    }
}

#[cfg(test)]
mod tests;
