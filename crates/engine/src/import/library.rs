//! Files imported into the project's library (#96): the Assets browser's Import. Each file is
//! read by the host (never from a path a command names), recognised from its bytes as a dropped
//! file is, and becomes a source card on a canvas through the drop's own bounded, one-undo-step
//! import. What differs is what the library needs:
//!
//! * **One outcome per file**, in input order, so a caller can retry exactly the ones that failed.
//! * **Deduplication by content.** A file whose bytes another file of the batch already has, or
//!   that the project already holds, adds no card and no copy: it is reported as a duplicate, or
//!   as already in the library. The asset store is content-addressed, so bytes are never kept twice.
//! * **Media only.** Text and ComfyUI workflows still go on the canvas by dropping them; the
//!   library lists images, videos, sounds, 3D models and PDFs.
//! * **Only the file's name is kept**, as the card's title: never the folder it came from.
use super::*;
use nodecraft_types::{Blake3, ErrorKind};
use std::collections::HashMap;

/// The most characters of a file's name kept as its card's title.
pub const MAX_NAME_CHARS: usize = 80;

/// One file to import: its name (a bare file name, or a path whose folders are dropped) and bytes.
pub type LibraryFile<'a> = Incoming<'a>;

/// What became of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileState {
    /// A new source card holds it.
    Added { node: NodeId, hash: Blake3 },
    /// An earlier file of the same import has the same bytes (its index); no card was added.
    Duplicate { of: usize, hash: Blake3 },
    /// The project already holds these bytes; no card was added.
    InLibrary { hash: Blake3 },
    /// It wasn't imported, and why. Importing it again may succeed.
    Failed { reason: String },
}

/// One outcome per file, in the order they were given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryImported {
    pub files: Vec<(String, FileState)>,
}

impl LibraryImported {
    /// The cards added, in input order.
    pub fn nodes(&self) -> Vec<NodeId> {
        self.files.iter().filter_map(|(_, state)| if let FileState::Added { node, .. } = state { Some(*node) } else { None }).collect()
    }
    /// The input indices of the files that failed.
    pub fn failed(&self) -> Vec<usize> {
        self.files.iter().enumerate().filter(|(_, (_, state))| matches!(state, FileState::Failed { .. })).map(|(index, _)| index).collect()
    }
    /// "2 added · 1 already in the library · 1 failed".
    pub fn summary(&self) -> String {
        let count = |f: fn(&FileState) -> bool| self.files.iter().filter(|(_, state)| f(state)).count();
        let parts = [
            (count(|s| matches!(s, FileState::Added { .. })), "added"),
            (count(|s| matches!(s, FileState::Duplicate { .. })), "duplicate"),
            (count(|s| matches!(s, FileState::InLibrary { .. })), "already in the library"),
            (count(|s| matches!(s, FileState::Failed { .. })), "failed"),
        ];
        let said: Vec<String> = parts.iter().filter(|(n, _)| *n > 0).map(|(n, what)| format!("{n} {what}")).collect();
        if said.is_empty() { "Nothing to import.".into() } else { said.join(" · ") }
    }
}

/// The part of `name` that is safe to keep in the project: the last path component, without
/// control characters, at most [`MAX_NAME_CHARS`] characters. A private folder never reaches
/// the document, even when a host passes a full path by mistake.
pub fn public_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let kept: String = last.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
    let kept = kept.trim();
    if kept.is_empty() { "Imported file".into() } else { kept.to_owned() }
}

/// Imports `files` into the project's library as source cards on `canvas`, one per new file,
/// in one undo step; `ids[i]` is the card id for `files[i]` (every file needs one). Placement
/// follows [`import_files_positioned`]. Per-file problems are outcomes, never an error: an
/// error means the request itself is malformed.
#[allow(clippy::too_many_arguments)]
pub fn import_library(
    session: &mut Session,
    canvas: CanvasId,
    at: Position,
    step: f32,
    files: &[LibraryFile<'_>],
    ids: &[NodeId],
    movies: &[ProbedMovie],
    position: &dyn Fn(&nodecraft_graph::Node, &nodecraft_nodes::Registry) -> Position,
) -> Result<LibraryImported, Error> {
    if ids.len() < files.len().min(MAX_FILES) {
        return Err(Error::new(ErrorKind::InvalidInput, "an id is needed for every file"));
    }
    if !session.document().canvases().contains_key(&canvas) {
        return Err(Error::new(ErrorKind::NotFound, "canvas does not exist"));
    }
    let held = crate::library_browse_cmds::listed_hashes(session.document());
    let mut outcome = LibraryImported::default();
    outcome.files.try_reserve_exact(files.len()).map_err(|_| Error::new(ErrorKind::InvalidInput, "cannot reserve import bookkeeping"))?;
    let mut seen: HashMap<Blake3, usize> = HashMap::new();
    // The files handed to the drop's import: their input index and hash, card id and title.
    let mut pending: Vec<(usize, Blake3)> = Vec::new();
    let mut fresh = Vec::new();
    let mut titles = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let name = public_name(file.name);
        let failed = |reason: &str| FileState::Failed { reason: reason.to_owned() };
        let state = match (ids.get(index), index < MAX_FILES, sniff(file.bytes)) {
            (Some(_), true, _) if file.bytes.len() > MAX_ASSET_BYTES => {
                FileState::Failed { reason: format!("files over {} MiB can't be added to a project yet", MAX_ASSET_BYTES / (1024 * 1024)) }
            }
            (Some(id), true, Some(sniffed)) => match sniffed.kind {
                MediaKind::Text => failed("text isn't kept in the library; drop it on the canvas to make a text card"),
                MediaKind::Style => failed("a style stays on the card that trained it; wire that card instead"),
                _ => {
                    let hash = Blake3::from_bytes(*blake3::hash(file.bytes).as_bytes());
                    if let Some(of) = seen.get(&hash) {
                        FileState::Duplicate { of: *of, hash }
                    } else if held.contains(&hash) && session.assets().contains(&hash) {
                        seen.insert(hash, index);
                        FileState::InLibrary { hash }
                    } else {
                        seen.insert(hash, index);
                        pending.push((index, hash));
                        fresh.push(*id);
                        titles.push(name.clone());
                        // Settled below, once the batch is imported.
                        FileState::Failed { reason: String::new() }
                    }
                }
            },
            (Some(_), true, None) if file.bytes.is_empty() => failed("the file is empty"),
            (Some(_), true, None) => failed("NodeCraft doesn't recognise this kind of file"),
            _ => FileState::Failed { reason: format!("at most {MAX_FILES} files are imported at once; import the rest next") },
        };
        outcome.files.push((name, state));
    }
    if pending.is_empty() {
        return Ok(outcome);
    }
    // Only the public names travel on, so no message the import makes can quote a folder.
    let mut incoming = Vec::new();
    incoming.try_reserve_exact(pending.len()).map_err(|_| Error::new(ErrorKind::InvalidInput, "cannot reserve import bookkeeping"))?;
    for ((index, _), title) in pending.iter().zip(&titles) {
        let bytes = files.get(*index).map(|file| file.bytes).unwrap_or_default();
        incoming.push(Incoming { name: title, bytes });
    }
    let hashes: Vec<Blake3> = pending.iter().map(|(_, hash)| *hash).collect();
    match import_titled(session, canvas, at, step, &incoming, &fresh, movies, position, &titles, &hashes) {
        Ok(imported) => {
            // Each pending file either became the card with its id or was skipped, in order.
            let mut skipped = imported.skipped.into_iter();
            for ((index, hash), id) in pending.into_iter().zip(fresh) {
                let state = if imported.nodes.contains(&id) {
                    FileState::Added { node: id, hash }
                } else {
                    FileState::Failed { reason: skipped.next().map_or_else(|| "it wasn't imported".to_owned(), |(_, why)| why) }
                };
                if let Some(slot) = outcome.files.get_mut(index) {
                    slot.1 = state;
                }
            }
        }
        Err(error) => {
            for (index, _) in pending {
                if let Some(slot) = outcome.files.get_mut(index) {
                    slot.1 = FileState::Failed { reason: error.message.clone() };
                }
            }
        }
    }
    // A copy of a file that failed failed with it, so a retry includes it.
    for index in 0..outcome.files.len() {
        let of = match outcome.files.get(index) {
            Some((_, FileState::Duplicate { of, .. })) => *of,
            _ => continue,
        };
        if let Some((_, FileState::Failed { reason })) = outcome.files.get(of) {
            let reason = reason.clone();
            if let Some(slot) = outcome.files.get_mut(index) {
                slot.1 = FileState::Failed { reason };
            }
        }
    }
    Ok(outcome)
}
