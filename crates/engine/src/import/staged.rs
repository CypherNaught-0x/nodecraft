//! One bounded admitted batch; never a series of published imports plus broad pruning.
use super::*;
use crate::PreparedImports;
use nodecraft_format::asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel};
use nodecraft_types::{AssetRef, Blake3, ErrorKind, ParamValues, Value};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
    sync::Arc,
};

struct Unique<'a> {
    hash: Blake3,
    bytes: &'a [u8],
}
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn allocation() -> Error {
    invalid("cannot reserve import bookkeeping")
}

fn prepare(session: &Session, unique: &[Unique<'_>]) -> Result<PreparedImports, Error> {
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let prepared = if session.assets().context().supports_large_assets() {
        // Borrow caller bytes; do not add a resident copy of a native 32 × 16 MiB input batch.
        let mut cursors = Vec::new();
        cursors.try_reserve_exact(unique.len()).map_err(|_| allocation())?;
        cursors.extend(unique.iter().map(|file| Cursor::new(file.bytes)));
        let mut readers = Vec::new();
        readers.try_reserve_exact(cursors.len()).map_err(|_| allocation())?;
        readers.extend(cursors.iter_mut().map(|cursor| cursor as &mut dyn Read));
        session.prepare_reader_imports(&mut readers, &cancel, &metrics)?
    } else {
        unique.iter().try_fold(0u64, |total, file| {
            total
                .checked_add(file.bytes.len() as u64)
                .filter(|n| *n <= MAX_TOTAL_ASSET_BYTES as u64)
                .ok_or_else(|| invalid("resident import sources exceed 48 MiB"))
        })?;
        let mut ranges = Vec::new();
        ranges.try_reserve_exact(unique.len()).map_err(|_| allocation())?;
        for file in unique {
            let source = Arc::new(MemorySource::new(Arc::from(file.bytes)));
            ranges.push(AssetRange::new(source, 0, file.bytes.len() as u64)?);
        }
        session.prepare_imports(&ranges, &cancel, &metrics)?
    };
    if prepared.hashes().len() != unique.len() || prepared.hashes().iter().zip(unique).any(|(actual, file)| *actual != file.hash) {
        return Err(invalid("prepared import identity disagrees with its immutable input"));
    }
    Ok(prepared)
}

pub(super) fn run(session: &mut Session, canvas: CanvasId, at: Position, step: f32, files: &[Incoming<'_>], ids: &[NodeId]) -> Result<Imported, Error> {
    if ids.len() < files.len().min(MAX_FILES) {
        return Err(invalid("an id is needed for every file"));
    }
    let mut imported = Imported::default();
    // Public callers can supply >32 files; preserve one bounded-name/reason result per skip.
    imported.skipped.try_reserve_exact(files.len()).map_err(|_| allocation())?;
    imported.nodes.try_reserve_exact(files.len().min(MAX_FILES)).map_err(|_| allocation())?;
    let mut plans = Vec::new();
    plans.try_reserve_exact(files.len().min(MAX_FILES)).map_err(|_| allocation())?;
    let mut unique: Vec<Unique<'_>> = Vec::new();
    unique.try_reserve_exact(files.len().min(MAX_FILES)).map_err(|_| allocation())?;
    let mut text_left = MAX_DROP_TEXT_BYTES;
    let native = session.assets().context().supports_large_assets();
    let cap = if native { nodecraft_format::asset_source::MAX_LARGE_TOTAL_ASSET_BYTES } else { MAX_TOTAL_ASSET_BYTES as u64 };
    let (mut retained_count, mut retained_bytes) = session.assets().context().retained_usage()?;
    for (index, file) in files.iter().enumerate() {
        let name = file.name.chars().take(80).collect::<String>();
        let (Some(id), true) = (ids.get(index), index < MAX_FILES) else {
            imported.skipped.push((name, format!("at most {MAX_FILES} files are added at once")));
            continue;
        };
        let plan = match plan(file, text_left) {
            Ok(plan) => plan,
            Err(why) => {
                imported.skipped.push((name, why));
                continue;
            }
        };
        let hash = match &plan {
            Plan::Text(text) => {
                text_left = text_left.checked_sub(text.len()).ok_or_else(|| invalid("import text budget exceeded"))?;
                None
            }
            Plan::Media { bytes, .. } => {
                let length = u64::try_from(bytes.len()).map_err(|_| invalid("import length cannot be represented"))?;
                let hash = Blake3::from_bytes(*blake3::hash(bytes).as_bytes());
                if let Some(existing) = unique.iter().find(|file| file.hash == hash) {
                    if existing.bytes.len() != bytes.len() {
                        return Err(invalid("duplicate import has inconsistent content length"));
                    }
                } else {
                    if let Some(existing) = session.assets().descriptor(&hash) {
                        if existing.bytes != length {
                            return Err(invalid("existing import has inconsistent content length"));
                        }
                    } else {
                        let count = retained_count.checked_add(1).ok_or_else(|| invalid("import count overflow"))?;
                        let total = retained_bytes.checked_add(length).ok_or_else(|| invalid("import size overflow"))?;
                        if count > MAX_ASSETS || total > cap {
                            let why = if native {
                                "the project's retained media is full (64 GiB or 1024 assets); release unused imports/readers".to_owned()
                            } else {
                                format!("the project's media is full ({} MiB)", MAX_TOTAL_ASSET_BYTES / (1024 * 1024))
                            };
                            imported.skipped.push((name, why));
                            continue;
                        }
                        retained_count = count;
                        retained_bytes = total;
                    }
                    unique.push(Unique { hash, bytes });
                }
                Some(hash)
            }
        };
        plans.push((*id, plan, hash));
    }
    if plans.is_empty() {
        return Ok(imported);
    }
    let prepared = if unique.is_empty() { None } else { Some(prepare(session, &unique)?) };
    let mut nodes = Vec::new();
    nodes.try_reserve_exact(plans.len()).map_err(|_| allocation())?;
    for (id, plan, hash) in plans {
        let (kind, parameter, value) = match plan {
            Plan::Text(text) => ("source.text", "text", Value::Text(text)),
            Plan::Media { kind, mime, meta, .. } => {
                let hash = hash.ok_or_else(|| invalid("media import has no planned identity"))?;
                (kind, "asset", Value::Media(AssetRef { hash, mime, meta }))
            }
        };
        let supplied: ParamValues = [(parameter.to_owned(), value)].into_iter().collect();
        let mut node = session.kinds().get(kind)?.instantiate_instance(id, BTreeMap::new(), &supplied)?;
        node.pos = Position { x: at.x + step * nodes.len() as f32, y: at.y };
        imported.nodes.push(id);
        nodes.push(node);
    }
    let params = json!({"canvas":canvas,"nodes":nodes,"edges":[]});
    match prepared {
        Some(batch) => {
            session.dispatch_with_imports("canvas.paste", &params, batch)?;
        }
        None => {
            session.dispatch("canvas.paste", &params)?;
        }
    }
    // All result ownership predates the command's durable publication.
    Ok(imported)
}
