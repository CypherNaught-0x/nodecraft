//! Where held results (#369) are kept across a restart (#614).
//!
//! Each held result is one record, written once when it is held and removed when it is placed,
//! recorded or discarded, so large media are never rewritten (the run journal, by contrast, is
//! rewritten on every usage change). A record is a magic line, the length of a JSON header, the
//! header (where the result goes, what recording it needs, its outputs with the length of each
//! one's bytes) and then those bytes, back to back.
//!
//! All the records together are bounded by [`MAX_HELD_STORE_BYTES`]: a result that would go over
//! is still held, in memory only, and the queue panel says it won't survive a restart. A record
//! that can't be read is set aside, never overwritten.
use nodecraft_exec::run::ResumeRecipe;
use nodecraft_nodes::GenerateOutput;
use nodecraft_providers::{Completed, Output, Usage};
use nodecraft_types::{BillableUnits, CanvasId, Error, ErrorKind, Money, NodeId, ProjectId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The record format's version.
pub const HELD_RECORD_VERSION: u32 = 1;
/// Most bytes all the saved records may take together; a single record is refused past it too.
pub const MAX_HELD_STORE_BYTES: u64 = 1024 * 1024 * 1024;
/// Most outputs a record lists.
const MAX_OUTPUTS: usize = 4096;
/// Most record files a store lists; a folder with more is read only this far.
pub const MAX_LISTED: usize = 1024;
/// What every record starts with.
const MAGIC: &[u8; 8] = b"NCHELD1\n";

/// A held result as saved: where it goes, what recording it needs, and what the provider returned.
#[derive(Clone, Debug, PartialEq)]
pub struct HeldRecord {
    pub project: ProjectId,
    pub canvas: CanvasId,
    pub node: NodeId,
    pub provider: String,
    pub model: String,
    pub reason: String,
    pub completed: Completed,
    pub routes: Vec<GenerateOutput>,
    pub recipe: Option<ResumeRecipe>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u32,
    project: ProjectId,
    canvas: CanvasId,
    node: NodeId,
    provider: String,
    model: String,
    reason: String,
    routes: Vec<GenerateOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipe: Option<ResumeRecipe>,
    #[serde(default)]
    usage: StoredUsage,
    outputs: Vec<StoredOutput>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost: Option<Money>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    billable_units: Option<BillableUnits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_ms: Option<u64>,
}

/// An output; inline bytes are listed by length and follow the header.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum StoredOutput {
    Text { text: String },
    Bytes { mime: String, len: u64 },
    Url { url: String, mime: Option<String> },
}

fn damaged(why: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, format!("the held result is damaged: {why}"))
}

impl HeldRecord {
    /// The record as saved, refusing one bigger than [`MAX_HELD_STORE_BYTES`].
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let too_big = || Error::new(ErrorKind::Busy, "the held result is too large to save");
        let mut payload: u64 = 0;
        let outputs = self
            .completed
            .outputs
            .iter()
            .map(|output| match output {
                Output::Text(text) => StoredOutput::Text { text: text.clone() },
                Output::Bytes { mime, bytes } => {
                    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                    payload = payload.saturating_add(len);
                    StoredOutput::Bytes { mime: mime.clone(), len }
                }
                Output::Url { url, mime } => StoredOutput::Url { url: url.clone(), mime: mime.clone() },
            })
            .collect();
        if payload > MAX_HELD_STORE_BYTES || self.completed.outputs.len() > MAX_OUTPUTS {
            return Err(too_big());
        }
        let usage = &self.completed.usage;
        let header = Header {
            version: HELD_RECORD_VERSION,
            project: self.project,
            canvas: self.canvas,
            node: self.node,
            provider: self.provider.clone(),
            model: self.model.clone(),
            reason: self.reason.clone(),
            routes: self.routes.clone(),
            recipe: self.recipe.clone(),
            usage: StoredUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cost: usage.cost,
                billable_units: usage.billable_units,
                execution_ms: usage.execution_ms,
            },
            outputs,
        };
        let header = serde_json::to_vec(&header).map_err(|e| Error::new(ErrorKind::Internal, format!("the held result can't be written: {e}")))?;
        let header_len = u32::try_from(header.len()).map_err(|_| too_big())?;
        let total = u64::try_from(MAGIC.len() + 4 + header.len()).unwrap_or(u64::MAX).saturating_add(payload);
        if total > MAX_HELD_STORE_BYTES {
            return Err(too_big());
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(usize::try_from(total).map_err(|_| too_big())?).map_err(|_| too_big())?;
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&header_len.to_le_bytes());
        bytes.extend_from_slice(&header);
        for output in &self.completed.outputs {
            if let Output::Bytes { bytes: data, .. } = output {
                bytes.extend_from_slice(data);
            }
        }
        Ok(bytes)
    }

    /// Reads a saved record, refusing one that is too big, of another version, or malformed.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_HELD_STORE_BYTES {
            return Err(damaged("it is too large to read"));
        }
        let rest = bytes.strip_prefix(MAGIC.as_slice()).ok_or_else(|| damaged("it isn't a held result"))?;
        let (len, rest) = rest.split_first_chunk::<4>().ok_or_else(|| damaged("it is cut short"))?;
        let len = usize::try_from(u32::from_le_bytes(*len)).map_err(|_| damaged("its header is too large"))?;
        let header = rest.get(..len).ok_or_else(|| damaged("it is cut short"))?;
        let mut payload = rest.get(len..).ok_or_else(|| damaged("it is cut short"))?;
        let header: Header = serde_json::from_slice(header).map_err(|e| damaged(&e.to_string()))?;
        if header.version != HELD_RECORD_VERSION {
            return Err(Error::new(ErrorKind::Unsupported, format!("the held result has version {}, which this NodeCraft can't read", header.version)));
        }
        if header.outputs.len() > MAX_OUTPUTS {
            return Err(damaged("it lists too many outputs"));
        }
        let mut outputs = Vec::new();
        outputs.try_reserve_exact(header.outputs.len()).map_err(|_| damaged("it lists too many outputs"))?;
        for output in header.outputs {
            outputs.push(match output {
                StoredOutput::Text { text } => Output::Text(text),
                StoredOutput::Url { url, mime } => Output::Url { url, mime },
                StoredOutput::Bytes { mime, len } => {
                    let len = usize::try_from(len).map_err(|_| damaged("an output is too large"))?;
                    let (data, rest) = payload.split_at_checked(len).ok_or_else(|| damaged("an output is cut short"))?;
                    payload = rest;
                    Output::Bytes { mime, bytes: Arc::from(data) }
                }
            });
        }
        if !payload.is_empty() {
            return Err(damaged("it has bytes no output lists"));
        }
        let mut usage = Usage::default();
        usage.input_tokens = header.usage.input_tokens;
        usage.output_tokens = header.usage.output_tokens;
        usage.cost = header.usage.cost;
        usage.billable_units = header.usage.billable_units;
        usage.execution_ms = header.usage.execution_ms;
        Ok(Self {
            project: header.project,
            canvas: header.canvas,
            node: header.node,
            provider: header.provider,
            model: header.model,
            reason: header.reason,
            completed: Completed { outputs, usage },
            routes: header.routes,
            recipe: header.recipe,
        })
    }
}

/// What a store holds: its records' numbers and sizes, lowest first (at most [`MAX_LISTED`]),
/// the bytes of all of them, listed or not, and the highest number any record has had, set
/// aside ones included, so a new record never takes an old one's number.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub records: Vec<(u64, u64)>,
    pub bytes: u64,
    pub highest: u64,
}

/// Where a host keeps its held results, one record per result, named by a number the host picks.
/// The host writes records outside its own lock, so a store is shared between threads.
pub trait HeldStore: Send + Sync {
    fn list(&self) -> Result<Listing, Error>;
    /// Record `id`'s bytes, refusing more than [`MAX_HELD_STORE_BYTES`].
    fn read(&self, id: u64) -> Result<Vec<u8>, Error>;
    /// Saves record `id`, whole or not at all.
    fn write(&self, id: u64, bytes: &[u8]) -> Result<(), Error>;
    /// Removes record `id`; one that isn't there is already removed.
    fn remove(&self, id: u64) -> Result<(), Error>;
    /// Moves record `id`, which couldn't be read, out of the way, so it is neither listed again
    /// nor replaced.
    fn set_aside(&self, id: u64) -> Result<(), Error>;
}

/// A record in memory, and whether it was set aside.
type MemoryRecord = (Vec<u8>, bool);

/// Records in memory: for tests, and hosts with nowhere to save (they don't survive the process).
#[derive(Clone, Default)]
pub struct MemoryHeld(Arc<std::sync::Mutex<std::collections::BTreeMap<u64, MemoryRecord>>>);

impl MemoryHeld {
    fn records(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<u64, MemoryRecord>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Replaces record `id` with `bytes` as they are, as a damaged file would be.
    pub fn put_raw(&self, id: u64, bytes: Vec<u8>) {
        self.records().insert(id, (bytes, false));
    }
    /// The numbers of the records set aside.
    pub fn set_aside_records(&self) -> Vec<u64> {
        self.records().iter().filter(|(_, (_, aside))| *aside).map(|(id, _)| *id).collect()
    }
}

fn size(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

impl HeldStore for MemoryHeld {
    fn list(&self) -> Result<Listing, Error> {
        let records = self.records();
        let live = records.iter().filter(|(_, (_, aside))| !aside);
        Ok(Listing {
            records: live.clone().take(MAX_LISTED).map(|(id, (bytes, _))| (*id, size(bytes))).collect(),
            bytes: live.map(|(_, (bytes, _))| size(bytes)).fold(0, u64::saturating_add),
            highest: records.keys().next_back().copied().unwrap_or(0),
        })
    }
    fn read(&self, id: u64) -> Result<Vec<u8>, Error> {
        self.records()
            .get(&id)
            .filter(|(_, aside)| !aside)
            .map(|(bytes, _)| bytes.clone())
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no held result {id} is saved")))
    }
    fn write(&self, id: u64, bytes: &[u8]) -> Result<(), Error> {
        self.records().insert(id, (bytes.to_vec(), false));
        Ok(())
    }
    fn remove(&self, id: u64) -> Result<(), Error> {
        self.records().remove(&id);
        Ok(())
    }
    fn set_aside(&self, id: u64) -> Result<(), Error> {
        if let Some((_, aside)) = self.records().get_mut(&id) {
            *aside = true;
        }
        Ok(())
    }
}

/// Records as files `<id>.held` in a folder, each written atomically. One process at a time may
/// use a folder: the desktop keeps it beside its run journal, whose lock it holds.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileHeld {
    dir: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileHeld {
    pub fn at(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }
    fn path(&self, id: u64) -> std::path::PathBuf {
        self.dir.join(format!("{id}.held"))
    }
}

/// The number in a record's file name (`<n>.held`), and whether it was set aside
/// (`<n>.held.damaged`, or `<n>.held.damaged-<k>`). Only the canonical form of `n` counts, so
/// every number listed names a file [`FileHeld`] can open.
#[cfg(not(target_arch = "wasm32"))]
fn record_name(name: &str) -> Option<(u64, bool)> {
    let (stem, aside) = match name.split_once(".held") {
        Some((stem, "")) => (stem, false),
        Some((stem, rest)) if rest.starts_with(".damaged") => (stem, true),
        _ => return None,
    };
    let id = stem.parse::<u64>().ok()?;
    (id.to_string() == stem).then_some((id, aside))
}

#[cfg(not(target_arch = "wasm32"))]
fn io_error(what: &str, e: &std::io::Error) -> Error {
    Error::new(ErrorKind::Internal, format!("{what}: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
impl HeldStore for FileHeld {
    fn list(&self) -> Result<Listing, Error> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Listing::default()),
            Err(e) => return Err(io_error("the held results' folder can't be read", &e)),
        };
        let mut listing = Listing::default();
        for entry in entries {
            let entry = entry.map_err(|e| io_error("the held results' folder can't be read", &e))?;
            let Some((id, aside)) = entry.file_name().to_str().and_then(record_name) else { continue };
            listing.highest = listing.highest.max(id);
            if aside {
                continue;
            }
            let len = entry.metadata().map_err(|e| io_error("a held result can't be read", &e))?.len();
            listing.bytes = listing.bytes.saturating_add(len);
            // Keep the lowest numbers only, so a hostile folder can't grow the list.
            listing.records.push((id, len));
            if listing.records.len() > MAX_LISTED.saturating_mul(2) {
                listing.records.sort_unstable();
                listing.records.truncate(MAX_LISTED);
            }
        }
        listing.records.sort_unstable();
        listing.records.truncate(MAX_LISTED);
        Ok(listing)
    }
    fn read(&self, id: u64) -> Result<Vec<u8>, Error> {
        use std::io::Read;
        let file = std::fs::File::open(self.path(id)).map_err(|e| io_error("a held result can't be read", &e))?;
        let mut bytes = Vec::new();
        file.take(MAX_HELD_STORE_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| io_error("a held result can't be read", &e))?;
        Ok(bytes)
    }
    fn write(&self, id: u64, bytes: &[u8]) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir).map_err(|e| io_error("the held results' folder can't be made", &e))?;
        craft_atomic::atomic_write(&self.path(id), bytes).map_err(|e| io_error("a held result can't be saved", &e))
    }
    fn remove(&self, id: u64) -> Result<(), Error> {
        match std::fs::remove_file(self.path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error("a held result can't be removed", &e)),
        }
    }
    fn set_aside(&self, id: u64) -> Result<(), Error> {
        let path = self.path(id);
        // Never over an earlier damaged record of the same number.
        let aside = (0..64u32)
            .map(|k| if k == 0 { self.dir.join(format!("{id}.held.damaged")) } else { self.dir.join(format!("{id}.held.damaged-{k}")) })
            .find(|aside| !aside.exists())
            .ok_or_else(|| Error::new(ErrorKind::Internal, "a damaged held result can't be moved aside: too many damaged copies"))?;
        match std::fs::rename(&path, aside) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error("a damaged held result can't be moved aside", &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> HeldRecord {
        let mut usage = Usage::default();
        usage.input_tokens = Some(7);
        usage.execution_ms = Some(1234);
        usage.billable_units = BillableUnits::from_millionths(1_500_000);
        HeldRecord {
            project: ProjectId::from_bytes([1; 16]),
            canvas: CanvasId::from_bytes([2; 16]),
            node: NodeId::from_bytes([3; 16]),
            provider: "mock".into(),
            model: "painter".into(),
            reason: "its card is gone".into(),
            completed: Completed {
                outputs: vec![
                    Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&[1u8, 2, 3][..]) },
                    Output::Text("a caption".into()),
                    Output::Url { url: "https://example.invalid/a.mp4".into(), mime: None },
                    Output::Bytes { mime: "audio/wav".into(), bytes: Arc::from(&[][..]) },
                    Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&[9u8; 5][..]) },
                ],
                usage,
            },
            routes: Vec::new(),
            recipe: None,
        }
    }

    #[test]
    fn a_record_round_trips() {
        let record = record();
        assert_eq!(HeldRecord::from_bytes(&record.to_bytes().unwrap()).unwrap(), record);
    }

    #[test]
    fn hostile_records_are_refused() {
        let bytes = record().to_bytes().unwrap();
        // Every truncation, and every extra byte.
        for cut in 0..bytes.len() {
            assert!(HeldRecord::from_bytes(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(HeldRecord::from_bytes(&longer).is_err());
        // A header claiming more than there is, or a huge output.
        let mut lying = bytes.clone();
        lying[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(HeldRecord::from_bytes(&lying).is_err());
        let header = br#"{"version":1,"project":"01010101-0101-0101-0101-010101010101","canvas":"02020202-0202-0202-0202-020202020202","node":"03030303-0303-0303-0303-030303030303","provider":"p","model":"m","reason":"r","routes":[],"outputs":[{"bytes":{"mime":"x","len":18446744073709551615}}]}"#;
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&u32::try_from(header.len()).unwrap().to_le_bytes());
        huge.extend_from_slice(header);
        assert!(HeldRecord::from_bytes(&huge).is_err());
        let newer = String::from_utf8(header.to_vec()).unwrap().replace(r#""version":1"#, r#""version":2"#).replace("18446744073709551615", "0");
        let mut newer_bytes = MAGIC.to_vec();
        newer_bytes.extend_from_slice(&u32::try_from(newer.len()).unwrap().to_le_bytes());
        newer_bytes.extend_from_slice(newer.as_bytes());
        assert_eq!(HeldRecord::from_bytes(&newer_bytes).unwrap_err().kind, ErrorKind::Unsupported);
        assert!(HeldRecord::from_bytes(b"not a record at all").is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_file_store_lists_reads_removes_and_sets_aside() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileHeld::at(dir.path().join("held"));
        assert_eq!(store.list().unwrap(), Listing::default(), "no folder yet");
        store.write(12, b"twelve").unwrap();
        store.write(3, b"three").unwrap();
        for ignored in ["notes.txt", "007.held", "+7.held", "7.helder"] {
            std::fs::write(dir.path().join("held").join(ignored), b"ignored").unwrap();
        }
        assert_eq!(store.list().unwrap(), Listing { records: vec![(3, 5), (12, 6)], bytes: 11, highest: 12 });
        assert_eq!(store.read(12).unwrap(), b"twelve");
        store.set_aside(12).unwrap();
        assert_eq!(store.list().unwrap(), Listing { records: vec![(3, 5)], bytes: 5, highest: 12 }, "a damaged record isn't listed, but keeps its number");
        assert!(dir.path().join("held/12.held.damaged").exists(), "and isn't deleted");
        store.write(12, b"again").unwrap();
        store.set_aside(12).unwrap();
        assert!(dir.path().join("held/12.held.damaged").exists() && dir.path().join("held/12.held.damaged-1").exists(), "nor replaced");
        store.remove(3).unwrap();
        store.remove(3).unwrap();
        assert_eq!(store.list().unwrap(), Listing { records: Vec::new(), bytes: 0, highest: 12 });
    }
}
