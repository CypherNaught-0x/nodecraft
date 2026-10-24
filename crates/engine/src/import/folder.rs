//! A folder from the operating system becomes one Folder card ([`nodecraft_nodes::folder`]):
//! dropped on the canvas, picked with Import folder, or imported again into a card that has one.
//!
//! Each file is recognised from its bytes, as a dropped file is. The folder holds one kind of
//! file: the kind most of its files are, unless the card it fills is wired, which keeps its own.
//! Files of other kinds, and files that can't be held, are reported with a reason. Media go into
//! the project's asset store, as for a drop, and the card is added or filled in one undo step.
use super::staged::{Unique, prepare};
use super::*;
use nodecraft_nodes::folder;
use nodecraft_types::{AssetRef, Blake3, ErrorKind, Modality, Value};
use serde_json::json;
use std::collections::BTreeSet;

/// Where an imported folder goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FolderTarget {
    /// A new Folder card with id `id`, its top-left corner at `at`.
    New { canvas: CanvasId, at: Position, id: NodeId },
    /// The Folder card `id`, whose files it replaces.
    Existing { canvas: CanvasId, id: NodeId },
}

/// What a folder import did: the card it added or filled, the kind and number of files it holds,
/// and why each other file was left out.
#[derive(Debug, PartialEq, Eq)]
pub struct FolderImported {
    pub node: NodeId,
    pub modality: Modality,
    pub files: usize,
    pub skipped: Vec<(String, String)>,
}

impl FolderImported {
    /// A one-line notice for anything left out, if anything was.
    pub fn notice(&self) -> Option<String> {
        Imported { nodes: Vec::new(), skipped: self.skipped.clone() }.notice()
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn modality_of(kind: MediaKind) -> Option<Modality> {
    match kind {
        MediaKind::Text => Some(Modality::Text),
        MediaKind::Image => Some(Modality::Image),
        MediaKind::Video => Some(Modality::Video),
        MediaKind::Audio => Some(Modality::Audio),
        MediaKind::Model3d => Some(Modality::Mesh),
        MediaKind::Document => Some(Modality::Document),
        MediaKind::Style => None,
    }
}

/// How the folder's files are named to people: "images", "text files".
fn plural(modality: Modality) -> &'static str {
    match modality {
        Modality::Text => "text files",
        Modality::Image => "images",
        Modality::Video => "videos",
        Modality::Audio => "sounds",
        Modality::Mesh => "3D models",
        Modality::Document => "PDFs",
        Modality::Style => "styles",
    }
}

/// The kind of file most of `kinds` are; ties go to the earlier of [`folder::MODALITIES`].
fn majority(kinds: &[Option<Modality>]) -> Option<Modality> {
    let count = |modality: Modality| kinds.iter().filter(|kind| **kind == Some(modality)).count();
    folder::MODALITIES.into_iter().filter(|m| count(*m) > 0).fold(None, |best: Option<Modality>, m| match best {
        Some(best) if count(best) >= count(m) => Some(best),
        _ => Some(m),
    })
}

/// Imports `files` (named by their paths inside the folder, `/`-separated) as the folder named
/// `name`. Fails, adding nothing, when no file can be held; the error names why.
pub fn import_folder(session: &mut Session, target: FolderTarget, name: &str, files: &[Incoming<'_>]) -> Result<FolderImported, Error> {
    let result = crate::session::guard(|| run(session, target, name, files));
    if result.is_err() && crate::session::guard(|| session.assets().collect_garbage()).is_err() {
        // As for a drop: nothing is published, and cleanup debt stays charged until retried.
        return result.map_err(|mut error| {
            error.message.push_str(" Asset maintenance also failed; retry maintenance before importing again.");
            error
        });
    }
    result
}

fn run(session: &mut Session, target: FolderTarget, name: &str, files: &[Incoming<'_>]) -> Result<FolderImported, Error> {
    session.settle_for_edit()?;
    let (canvas, id) = match target {
        FolderTarget::New { canvas, id, .. } | FolderTarget::Existing { canvas, id } => (canvas, id),
    };
    // A wired card keeps the kind of file it holds; anything else holds what most files are.
    let fixed = match target {
        FolderTarget::New { .. } => None,
        FolderTarget::Existing { .. } => {
            let document = session.document().canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "the canvas does not exist"))?;
            let node = document.nodes().get(&id).filter(|node| node.kind == folder::ID).ok_or_else(|| invalid("only a Folder card holds a folder's files"))?;
            let wired = document.edges().values().any(|e| e.from.node == id || e.to.node == id);
            if wired { Some(folder::modality(node)?) } else { None }
        }
    };
    let mut skipped = Vec::new();
    skipped.try_reserve_exact(files.len()).map_err(|_| invalid("cannot reserve import bookkeeping"))?;
    let shown = |file: &Incoming<'_>| file.name.chars().take(80).collect::<String>();
    let considered = files.get(..files.len().min(folder::MAX_FILES)).unwrap_or(files);
    for file in files.iter().skip(folder::MAX_FILES) {
        skipped.push((shown(file), format!("a folder holds at most {} files", folder::MAX_FILES)));
    }
    let kinds: Vec<Option<Modality>> = considered.iter().map(|file| sniff(file.bytes).and_then(|sniffed| modality_of(sniffed.kind))).collect();
    let modality = match fixed.or_else(|| majority(&kinds)) {
        Some(modality) => modality,
        None => return Err(invalid(format!("nothing in {} can be added: NodeCraft doesn't recognise its files", name.chars().take(80).collect::<String>()))),
    };
    let native = session.assets().context().supports_large_assets();
    let cap = if native { nodecraft_format::asset_source::MAX_LARGE_TOTAL_ASSET_BYTES } else { MAX_TOTAL_ASSET_BYTES as u64 };
    let (mut retained_count, mut retained_bytes) = session.assets().context().retained_usage()?;
    let mut text_left = MAX_DROP_TEXT_BYTES;
    let mut names = BTreeSet::new();
    let mut entries: Vec<(String, Value)> = Vec::new();
    let mut unique: Vec<Unique<'_>> = Vec::new();
    for (file, kind) in considered.iter().zip(&kinds) {
        let shown = shown(file);
        match kind {
            None => {
                let why = if file.bytes.is_empty() { "the file is empty" } else { "NodeCraft doesn't recognise this kind of file" };
                skipped.push((shown, why.into()));
                continue;
            }
            Some(kind) if *kind != modality => {
                skipped.push((shown, format!("the folder holds {}", plural(modality))));
                continue;
            }
            Some(_) => {}
        }
        if file.name.is_empty() || file.name.len() > folder::MAX_NAME_BYTES || file.name.chars().any(char::is_control) || !names.insert(file.name) {
            skipped.push((shown, "its name is empty, too long or repeated".into()));
            continue;
        }
        let value = match plan(file, text_left) {
            Err(why) => {
                skipped.push((shown, why));
                continue;
            }
            Ok(Plan::Card(_)) => {
                skipped.push((shown, "a folder can't hold this file".into()));
                continue;
            }
            Ok(Plan::Text(text)) => {
                text_left = text_left.checked_sub(text.len()).ok_or_else(|| invalid("import text budget exceeded"))?;
                Value::Text(text)
            }
            Ok(Plan::Media { mime, meta, bytes, .. }) => {
                let length = u64::try_from(bytes.len()).map_err(|_| invalid("import length cannot be represented"))?;
                let hash = Blake3::from_bytes(*blake3::hash(bytes).as_bytes());
                let known = unique.iter().any(|file| file.hash == hash) || session.assets().descriptor(&hash).is_some_and(|existing| existing.bytes == length);
                if !known {
                    if session.assets().descriptor(&hash).is_some() {
                        return Err(invalid("existing import has inconsistent content length"));
                    }
                    let count = retained_count.checked_add(1).ok_or_else(|| invalid("import count overflow"))?;
                    let total = retained_bytes.checked_add(length).ok_or_else(|| invalid("import size overflow"))?;
                    if count > MAX_ASSETS || total > cap {
                        let why = if native {
                            "the project's retained media is full (64 GiB or 1024 assets)".to_owned()
                        } else {
                            format!("the project's media is full ({} MiB)", MAX_TOTAL_ASSET_BYTES / (1024 * 1024))
                        };
                        skipped.push((shown, why));
                        continue;
                    }
                    (retained_count, retained_bytes) = (count, total);
                    unique.push(Unique { hash, bytes });
                }
                Value::Media(AssetRef { hash, mime, meta })
            }
        };
        entries.push((file.name.to_owned(), value));
    }
    if entries.is_empty() {
        let why = skipped.first().map_or_else(|| "it has no files".to_owned(), |(file, why)| format!("{file}: {why}"));
        return Err(invalid(format!("nothing in {} can be added ({why})", name.chars().take(80).collect::<String>())));
    }
    let count = entries.len();
    let prepared = if unique.is_empty() { None } else { Some(prepare(session, &unique)?) };
    let (command, params) = match target {
        FolderTarget::New { at, .. } => {
            let config = folder::config(modality, name, entries)?;
            let mut node = session.kinds().get(folder::ID)?.instantiate_instance(id, config, &Default::default())?;
            node.pos = at;
            ("canvas.paste", json!({"canvas": canvas, "nodes": [node], "edges": []}))
        }
        FolderTarget::Existing { .. } => {
            let files: Vec<_> = entries.into_iter().map(|(name, value)| json!({"name": name, "value": value})).collect();
            ("folder.fill", json!({"canvas": canvas, "id": id, "folder": name.chars().take(256).collect::<String>(), "modality": modality, "files": files}))
        }
    };
    match prepared {
        Some(batch) => session.dispatch_with_imports(command, &params, batch)?,
        None => session.dispatch(command, &params)?,
    };
    Ok(FolderImported { node: id, modality, files: count, skipped })
}

#[cfg(test)]
mod tests;
