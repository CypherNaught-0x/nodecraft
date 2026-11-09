//! Where the relay keeps its rooms across a restart (#1068), when it is given a folder to keep
//! them in. Each room is a folder named by a hash of its id (the id itself, the key to the room,
//! is never written): `links.json` holds its share links with their terms and when it was last
//! used, `room` its document as one update, and `assets/` the media it held, one file per content
//! hash. Every write goes to a new file that replaces the old one whole, so a crash leaves the
//! last complete copy.
//!
//! The store is bounded: a room nobody used for `keep` is forgotten, and when what a save would
//! add goes past the most rooms or bytes, the rooms used longest ago go first. A room someone is
//! in is never forgotten. What is read back is hostile: sizes are capped before reading, and the
//! room checks what it restores ([`Room::restore`]).
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use nodecraft_sync::{Grant, Room, RoomId};
use serde_json::{Value, json};

/// A room's name in the store and in the relay's memory: a hash of its id.
pub type RoomKey = [u8; 32];

/// The most rooms the store keeps.
pub const MAX_STORED_ROOMS: usize = 256;
/// The most bytes one room's document may take on disk, as a room holds at most.
const MAX_DOCUMENT_BYTES: u64 = 512 << 20;
/// The most bytes one room's assets may take on disk, as a room keeps at most.
const MAX_ASSET_BYTES: u64 = 512 << 20;
/// The most bytes a links file may take: a room's 256 links and then some.
const MAX_LINKS_BYTES: u64 = 1 << 20;
/// What a room's document file starts with.
const MAGIC: &[u8; 8] = b"NCRROOM1";

/// The key of the room `id`.
pub fn key(id: &RoomId) -> RoomKey {
    let mut hasher = blake3::Hasher::new_derive_key("nodecraft relay room key 2026-11-06");
    hasher.update(id.as_str().as_bytes());
    *hasher.finalize().as_bytes()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        let at = index.checked_mul(2)?;
        *byte = u8::from_str_radix(text.get(at..at.checked_add(2)?)?, 16).ok()?;
    }
    Some(out)
}

/// What the store knows of one room without reading its document.
#[derive(Clone, Debug)]
struct Kept {
    links: Vec<([u8; 32], Grant)>,
    /// When it was last used, in seconds since 1970.
    used: u64,
    /// Bytes on disk: its document and assets.
    bytes: u64,
    /// The assets already on disk.
    assets: BTreeSet<[u8; 32]>,
}

/// A room as the relay hands it over to be kept.
pub struct Snapshot {
    pub document: Vec<u8>,
    pub links: Vec<([u8; 32], Grant)>,
    /// Assets not yet on disk.
    pub assets: Vec<([u8; 32], Vec<u8>)>,
    pub used: u64,
}

/// Why a save didn't keep a room.
#[derive(Debug, PartialEq, Eq)]
pub enum NotKept {
    /// The store is full of rooms in use, or the room alone is past what it holds.
    Full,
    /// Writing failed: the disk is full, or the folder can't be written.
    Io,
}

pub struct Store {
    dir: PathBuf,
    rooms: BTreeMap<RoomKey, Kept>,
    /// How long a room nobody uses is kept, in seconds.
    keep: u64,
    /// The most bytes every room together may take.
    max_bytes: u64,
}

impl Store {
    /// Opens the store in `dir`, making it if it isn't there, and forgets the rooms nobody used
    /// within `keep` seconds of `now`. Folders that aren't rooms are left alone; a room that
    /// can't be read is forgotten.
    pub fn open(dir: &Path, keep: u64, max_bytes: u64, now: u64) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|_| "the relay can't make its store folder".to_owned())?;
        let entries = fs::read_dir(dir).map_err(|_| "the relay can't read its store folder".to_owned())?;
        let mut store = Self { dir: dir.to_owned(), rooms: BTreeMap::new(), keep, max_bytes };
        for entry in entries.flatten() {
            let Some(key) = entry.file_name().to_str().and_then(unhex) else { continue };
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            match store.read_kept(&key) {
                Some(kept) if store.rooms.len() < MAX_STORED_ROOMS => {
                    store.rooms.insert(key, kept);
                }
                _ => store.remove(&key),
            }
        }
        store.expire(now, &BTreeSet::new());
        Ok(store)
    }

    fn room_dir(&self, key: &RoomKey) -> PathBuf {
        self.dir.join(hex(key))
    }

    fn read_kept(&self, key: &RoomKey) -> Option<Kept> {
        let dir = self.room_dir(key);
        let links = read_capped(&dir.join("links.json"), MAX_LINKS_BYTES)?;
        let value: Value = serde_json::from_slice(&links).ok()?;
        let used = value.get("used")?.as_u64()?;
        let mut links = Vec::new();
        for link in value.get("links")?.as_array()?.iter().take(nodecraft_sync::MAX_LINKS) {
            let hash = unhex(link.get("hash")?.as_str()?)?;
            let grant: Grant = serde_json::from_value(link.get("grant")?.clone()).ok()?;
            links.push((hash, grant));
        }
        let document = fs::metadata(dir.join("room")).map(|meta| meta.len()).unwrap_or(0);
        let mut assets = BTreeSet::new();
        let mut bytes = document;
        for entry in fs::read_dir(dir.join("assets")).into_iter().flatten().flatten() {
            let Some(hash) = entry.file_name().to_str().and_then(unhex) else { continue };
            bytes = bytes.saturating_add(entry.metadata().map(|meta| meta.len()).unwrap_or(0));
            assets.insert(hash);
        }
        Some(Kept { links, used, bytes, assets })
    }

    /// Whether the store keeps the room `key`.
    pub fn has(&self, key: &RoomKey) -> bool {
        self.rooms.contains_key(key)
    }

    /// The kept room holding the link with this hash, if its terms are still good at `now`.
    pub fn holding(&self, link: &[u8; 32], now: u64) -> Option<RoomKey> {
        self.rooms.iter().find(|(_, kept)| kept.links.iter().any(|(hash, grant)| hash == link && grant.live(now))).map(|(key, _)| *key)
    }

    /// Reads the room `key` back. A room that can't be read is forgotten.
    pub fn load(&mut self, key: &RoomKey) -> Option<Room> {
        let kept = self.rooms.get(key)?.clone();
        let dir = self.room_dir(key);
        let room = (|| {
            let document = match read_capped(&dir.join("room"), MAX_DOCUMENT_BYTES.saturating_add(8)) {
                Some(bytes) => bytes.strip_prefix(MAGIC.as_slice())?.to_vec(),
                None if !dir.join("room").exists() => Vec::new(),
                None => return None,
            };
            let mut assets = Vec::new();
            let mut total = 0u64;
            for hash in &kept.assets {
                let Some(bytes) = read_capped(&dir.join("assets").join(hex(hash)), MAX_ASSET_BYTES.saturating_sub(total)) else { continue };
                total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                assets.push((*hash, bytes));
            }
            Room::restore(&document, kept.links.clone(), assets).ok()
        })();
        if room.is_none() {
            self.remove(key);
        }
        room
    }

    /// Keeps `snapshot` as the room `key`, making space by forgetting rooms nobody uses, the
    /// longest unused first, but never one in `busy`.
    pub fn save(&mut self, key: RoomKey, snapshot: Snapshot, busy: &BTreeSet<RoomKey>) -> Result<(), NotKept> {
        let dir = self.room_dir(&key);
        let (old_bytes, mut assets) = self.rooms.get(&key).map_or((0, BTreeSet::new()), |kept| (kept.bytes, kept.assets.clone()));
        let new_assets: Vec<&([u8; 32], Vec<u8>)> = snapshot.assets.iter().filter(|(hash, _)| !assets.contains(hash)).collect();
        let document_len = u64::try_from(snapshot.document.len()).unwrap_or(u64::MAX).saturating_add(8);
        let old_document = fs::metadata(dir.join("room")).map(|meta| meta.len()).unwrap_or(0);
        let added: u64 = new_assets.iter().map(|(_, bytes)| u64::try_from(bytes.len()).unwrap_or(u64::MAX)).fold(0, u64::saturating_add);
        let bytes = old_bytes.saturating_sub(old_document).saturating_add(document_len).saturating_add(added);
        if document_len > MAX_DOCUMENT_BYTES.saturating_add(8) || bytes > self.max_bytes {
            return Err(NotKept::Full);
        }
        // Make space: the rooms used longest ago go first, never this one or one in use. A room
        // the store doesn't keep yet takes only free space, so new rooms (anyone can open one)
        // never push out kept ones.
        let new_room = !self.rooms.contains_key(&key);
        loop {
            let others: u64 = self.rooms.iter().filter(|(k, _)| **k != key).map(|(_, kept)| kept.bytes).fold(0, u64::saturating_add);
            let count = self.rooms.len().saturating_add(usize::from(!self.rooms.contains_key(&key)));
            if others.saturating_add(bytes) <= self.max_bytes && count <= MAX_STORED_ROOMS {
                break;
            }
            if new_room {
                return Err(NotKept::Full);
            }
            let oldest = self.rooms.iter().filter(|(k, _)| **k != key && !busy.contains(*k)).min_by_key(|(_, kept)| kept.used).map(|(k, _)| *k);
            match oldest {
                Some(oldest) => self.remove(&oldest),
                None => return Err(NotKept::Full),
            }
        }
        fs::create_dir_all(dir.join("assets")).map_err(|_| NotKept::Io)?;
        for (hash, bytes) in new_assets {
            replace(&dir.join("assets"), &hex(hash), &[bytes.as_slice()])?;
            assets.insert(*hash);
        }
        replace(&dir, "room", &[MAGIC.as_slice(), &snapshot.document])?;
        let links: Vec<Value> = snapshot.links.iter().map(|(hash, grant)| json!({"hash": hex(hash), "grant": grant})).collect();
        let file = serde_json::to_vec(&json!({"version": 1, "used": snapshot.used, "links": links})).map_err(|_| NotKept::Io)?;
        // The links go last: they are what lets anyone in, and what a revocation takes away.
        replace(&dir, "links.json", &[&file])?;
        self.rooms.insert(key, Kept { links: snapshot.links, used: snapshot.used, bytes, assets });
        Ok(())
    }

    /// Forgets the rooms nobody used within the store's keep of `now`, except those in `busy`.
    pub fn expire(&mut self, now: u64, busy: &BTreeSet<RoomKey>) {
        let gone: Vec<RoomKey> =
            self.rooms.iter().filter(|(key, kept)| !busy.contains(*key) && now.saturating_sub(kept.used) > self.keep).map(|(key, _)| *key).collect();
        for key in gone {
            self.remove(&key);
        }
    }

    /// The assets the room `key` has on disk.
    pub fn assets_of(&self, key: &RoomKey) -> BTreeSet<[u8; 32]> {
        self.rooms.get(key).map(|kept| kept.assets.clone()).unwrap_or_default()
    }

    /// How many rooms, and how many bytes, the store keeps.
    #[cfg(test)]
    pub fn held(&self) -> (usize, u64) {
        (self.rooms.len(), self.rooms.values().map(|kept| kept.bytes).fold(0, u64::saturating_add))
    }

    /// Forgets the room `key` and its files: what a failed save leaves behind is never read back.
    pub fn discard(&mut self, key: &RoomKey) {
        self.remove(key);
    }

    fn remove(&mut self, key: &RoomKey) {
        self.rooms.remove(key);
        let _ = fs::remove_dir_all(self.room_dir(key));
    }
}

/// Reads `path` whole if it holds at most `cap` bytes.
fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > cap {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes).ok()?;
    (u64::try_from(bytes.len()).ok()? <= cap).then_some(bytes)
}

/// Writes `parts` as the file `name` in `dir`, replacing what was there whole.
fn replace(dir: &Path, name: &str, parts: &[&[u8]]) -> Result<(), NotKept> {
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|_| NotKept::Io)?;
    for part in parts {
        file.write_all(part).map_err(|_| NotKept::Io)?;
    }
    file.as_file().sync_all().map_err(|_| NotKept::Io)?;
    file.persist(dir.join(name)).map_err(|_| NotKept::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests;
