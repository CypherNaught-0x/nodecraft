//! Portable backup files (#182): a versioned container holding named entries (projects, media,
//! library items, settings) with a manifest, per-entry integrity hashes and optional, recommended
//! passphrase encryption. See `docs/backup-format.md` for the byte layout.
//!
//! The container is pure: it reads and writes `std::io` streams and takes its random salt and
//! nonce prefix from the caller (the host's platform randomness), so it builds for wasm and never
//! touches the filesystem itself. Choosing what to back up, staging a restore and publishing it
//! atomically belong to the host.
//!
//! - **Integrity:** the manifest lists every entry's size and BLAKE3 hash. Writing checks each
//!   entry's bytes against it as they stream; reading checks them again before reporting an entry
//!   complete. An unencrypted file ends with a BLAKE3 hash of everything before it, so truncation
//!   and accidental corruption are detected. Those hashes carry no key, so only encryption also
//!   detects deliberate edits.
//! - **Encryption:** XChaCha20-Poly1305 over 64 KiB chunks in the STREAM construction (a 19-byte
//!   random nonce prefix, a 32-bit chunk counter and a last-chunk flag), with the header as
//!   associated data. The key comes from the passphrase through Argon2id with the header's
//!   parameters, which are bounded on read so a hostile file can't demand unbounded memory or
//!   time. The manifest is inside the encrypted payload. A lost passphrase can't be recovered.
//! - **Secrets:** passphrases and keys are never stored or put in errors; the derived key and the
//!   cipher's key state are zeroed when dropped. (Argon2's large working memory and decrypted
//!   buffers are freed but not wiped.) Key derivation takes
//!   seconds by design, so hosts run it off the UI thread. What goes into a backup is the
//!   caller's choice; credentials must not be offered as entries.
use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{Read, Write};

/// The first eight bytes of every backup.
pub const BACKUP_MAGIC: &[u8; 8] = b"NCBACKUP";
/// The container version this build writes and reads.
pub const BACKUP_VERSION: u16 = 1;
/// Plaintext bytes per encrypted chunk.
pub const BACKUP_CHUNK_BYTES: usize = 64 * 1024;
/// Most entries one backup may list.
pub const MAX_BACKUP_ENTRIES: usize = 100_000;
/// Longest entry path, in bytes.
pub const MAX_BACKUP_PATH_BYTES: usize = 255;
/// Deepest entry path, in components.
pub const MAX_BACKUP_PATH_DEPTH: usize = 16;
/// Largest encoded manifest.
pub const MAX_BACKUP_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
/// Largest single entry.
pub const MAX_BACKUP_ENTRY_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// Largest total of all entries.
pub const MAX_BACKUP_TOTAL_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

const TAG_BYTES: usize = 16;
const PREFIX_BYTES: usize = 19;
const SALT_BYTES: usize = 16;
const KDF_ARGON2ID: u8 = 1;
const FLAG_ENCRYPTED: u8 = 1;

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn io(error: &std::io::Error) -> Error {
    match error.kind() {
        std::io::ErrorKind::UnexpectedEof => invalid("the backup is truncated"),
        std::io::ErrorKind::StorageFull => Error::new(ErrorKind::Busy, "not enough disk space for the backup"),
        _ => Error::new(ErrorKind::Internal, "the backup could not be read or written"),
    }
}
fn damaged() -> Error {
    Error::new(ErrorKind::Authentication, "wrong passphrase, or the backup is damaged or was changed")
}

/// Argon2id cost parameters. [`KdfParams::RECOMMENDED`] is what new backups use; reading accepts
/// only parameters within [`KdfParams::check`]'s bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory, in KiB.
    pub memory_kib: u32,
    pub iterations: u32,
    pub lanes: u32,
}

impl KdfParams {
    /// 64 MiB, 3 passes, 1 lane.
    pub const RECOMMENDED: Self = Self { memory_kib: 64 * 1024, iterations: 3, lanes: 1 };

    /// From 19 MiB to 256 MiB, 1 to 6 passes, 1 to 8 lanes: a hostile file can't demand more.
    pub fn check(self) -> Result<Self, Error> {
        if !(19 * 1024..=256 * 1024).contains(&self.memory_kib) || !(1..=6).contains(&self.iterations) || !(1..=8).contains(&self.lanes) {
            return Err(invalid("the backup's key-derivation parameters are outside the supported range"));
        }
        Ok(self)
    }

    fn key(self, passphrase: &str, salt: &[u8; SALT_BYTES]) -> Result<XChaCha20Poly1305, Error> {
        if passphrase.is_empty() {
            return Err(invalid("an encrypted backup needs a passphrase"));
        }
        let params = argon2::Params::new(self.memory_kib, self.iterations, self.lanes, Some(32)).map_err(|_| invalid("invalid key-derivation parameters"))?;
        let kdf = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        let mut key = zeroize::Zeroizing::new([0u8; 32]);
        kdf.hash_password_into(passphrase.as_bytes(), salt, key.as_mut_slice())
            .map_err(|_| Error::new(ErrorKind::Busy, "the backup key could not be derived"))?;
        XChaCha20Poly1305::new_from_slice(key.as_slice()).map_err(|_| Error::new(ErrorKind::Internal, "the backup key has the wrong size"))
    }
}

/// How a new backup is protected.
///
/// `salt` and `nonce_prefix` must never be reused together with the same passphrase: that would
/// reuse the keystream. Take both from the platform's random source for every backup.
pub enum Protection<'a> {
    /// No encryption. Integrity is still checked; anyone with the file can read it.
    Unencrypted,
    /// Encrypted with a key derived from `passphrase`. `salt` and `nonce_prefix` must be fresh
    /// random bytes from the platform for every backup.
    Passphrase { passphrase: &'a str, kdf: KdfParams, salt: [u8; SALT_BYTES], nonce_prefix: [u8; PREFIX_BYTES] },
}

/// The plaintext header: readable without a passphrase, so a backup can say whether it is
/// encrypted before asking for one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupHeader {
    pub version: u16,
    /// `None` for an unencrypted backup.
    pub encryption: Option<Encryption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encryption {
    pub kdf: KdfParams,
    salt: [u8; SALT_BYTES],
    nonce_prefix: [u8; PREFIX_BYTES],
}

impl BackupHeader {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(BACKUP_MAGIC);
        out.extend_from_slice(&self.version.to_le_bytes());
        match &self.encryption {
            None => out.push(0),
            Some(e) => {
                out.push(FLAG_ENCRYPTED);
                out.push(KDF_ARGON2ID);
                for n in [e.kdf.memory_kib, e.kdf.iterations, e.kdf.lanes] {
                    out.extend_from_slice(&n.to_le_bytes());
                }
                out.extend_from_slice(&e.salt);
                out.extend_from_slice(&e.nonce_prefix);
            }
        }
        out
    }

    /// Reads and checks the header from the start of a backup, returning it and its bytes.
    pub fn read(input: &mut impl Read) -> Result<(Self, Vec<u8>), Error> {
        let mut fixed = [0u8; 11];
        if fill(input, &mut fixed)? < fixed.len() {
            return Err(invalid("this is not a NodeCraft backup"));
        }
        if fixed.get(..8) != Some(BACKUP_MAGIC.as_slice()) {
            return Err(invalid("this is not a NodeCraft backup"));
        }
        let version = u16::from_le_bytes([fixed[8], fixed[9]]);
        if version != BACKUP_VERSION {
            return Err(Error::new(ErrorKind::Unsupported, "this backup was made by a newer NodeCraft; update to restore it"));
        }
        let mut bytes = fixed.to_vec();
        let encryption = match fixed[10] {
            0 => None,
            FLAG_ENCRYPTED => {
                let mut rest = [0u8; 1 + 12 + SALT_BYTES + PREFIX_BYTES];
                if fill(input, &mut rest)? < rest.len() {
                    return Err(invalid("the backup is truncated"));
                }
                bytes.extend_from_slice(&rest);
                if rest[0] != KDF_ARGON2ID {
                    return Err(Error::new(ErrorKind::Unsupported, "this backup uses an unknown key derivation; update NodeCraft"));
                }
                let word = |at: usize| rest.get(at..at + 4).and_then(|b| b.try_into().ok()).map_or(0, u32::from_le_bytes);
                let kdf = KdfParams { memory_kib: word(1), iterations: word(5), lanes: word(9) }.check()?;
                let mut salt = [0u8; SALT_BYTES];
                salt.copy_from_slice(rest.get(13..13 + SALT_BYTES).unwrap_or(&[0; SALT_BYTES]));
                let mut nonce_prefix = [0u8; PREFIX_BYTES];
                nonce_prefix.copy_from_slice(rest.get(13 + SALT_BYTES..).unwrap_or(&[0; PREFIX_BYTES]));
                Some(Encryption { kdf, salt, nonce_prefix })
            }
            _ => return Err(Error::new(ErrorKind::Unsupported, "this backup uses an unknown option; update NodeCraft")),
        };
        Ok((Self { version, encryption }, bytes))
    }
}

/// What a backup holds, stored at its start (inside the encryption when encrypted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    /// A short description chosen by the person.
    pub label: String,
    /// Milliseconds since the Unix epoch, from the host.
    pub created_ms: u64,
    /// In the order their bytes follow.
    pub entries: Vec<BackupEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupEntry {
    /// A relative path such as `projects/brief.ncraft`: lowercase or uppercase ASCII letters,
    /// digits, `.`, `-`, `_` and spaces per component, `/` between components.
    pub path: String,
    pub size: u64,
    /// Lowercase hex BLAKE3 of the entry's bytes.
    pub blake3: String,
}

/// Names Windows reserves for devices, with or without an extension.
fn reserved_on_windows(part: &str) -> bool {
    // Windows ignores spaces before the extension, so `nul .txt` is a device too.
    let stem = part.split('.').next().unwrap_or(part).trim_end_matches(' ').to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
}

fn valid_path(path: &str) -> bool {
    path.len() <= MAX_BACKUP_PATH_BYTES
        && path.split('/').count() <= MAX_BACKUP_PATH_DEPTH
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.starts_with(' ')
                && !part.ends_with(' ')
                && !part.ends_with('.')
                && !reserved_on_windows(part)
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b' '))
        })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl BackupManifest {
    /// Checks paths (relative, no `..`, no duplicates, ignoring case so restores work on
    /// case-insensitive disks), sizes, hashes and counts.
    pub fn check(&self) -> Result<(), Error> {
        if self.label.len() > 512 || self.label.chars().any(char::is_control) {
            return Err(invalid("the backup label is too long or has control characters"));
        }
        if self.entries.len() > MAX_BACKUP_ENTRIES {
            return Err(invalid("the backup lists too many entries"));
        }
        let mut seen = BTreeSet::new();
        let mut total = 0u64;
        for e in &self.entries {
            if !valid_path(&e.path) {
                return Err(invalid("a backup entry has an unsafe or invalid path"));
            }
            if !seen.insert(e.path.to_ascii_lowercase()) {
                return Err(invalid("a backup entry path appears twice"));
            }
            if e.size > MAX_BACKUP_ENTRY_BYTES {
                return Err(invalid("a backup entry is too large"));
            }
            total = total.checked_add(e.size).filter(|t| *t <= MAX_BACKUP_TOTAL_BYTES).ok_or_else(|| invalid("the backup is too large"))?;
            if e.blake3.len() != 64 || !e.blake3.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return Err(invalid("a backup entry hash is malformed"));
            }
        }
        // A path can't also be a folder of another entry.
        for e in &self.entries {
            let lower = e.path.to_ascii_lowercase();
            let mut prefix = String::new();
            for part in lower.split('/') {
                if !prefix.is_empty() && seen.contains(&prefix) {
                    return Err(invalid("a backup entry path is also a folder of another entry"));
                }
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
            }
        }
        Ok(())
    }

    /// Bytes of all entries together.
    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().fold(0u64, |n, e| n.saturating_add(e.size))
    }
}

/// The encrypted or plain payload writer.
enum Sink<W: Write> {
    Plain { out: W, hasher: Box<blake3::Hasher> },
    Sealed { out: W, cipher: Box<XChaCha20Poly1305>, prefix: [u8; PREFIX_BYTES], aad: Vec<u8>, counter: u32, chunk: Vec<u8> },
}

fn nonce(prefix: &[u8; PREFIX_BYTES], counter: u32, last: bool) -> XNonce {
    let mut n = [0u8; 24];
    n[..PREFIX_BYTES].copy_from_slice(prefix);
    n[PREFIX_BYTES..PREFIX_BYTES + 4].copy_from_slice(&counter.to_be_bytes());
    n[23] = u8::from(last);
    XNonce::from(n)
}

impl<W: Write> Sink<W> {
    fn write(&mut self, mut bytes: &[u8]) -> Result<(), Error> {
        match self {
            Sink::Plain { out, hasher } => {
                hasher.update(bytes);
                out.write_all(bytes).map_err(|e| io(&e))
            }
            Sink::Sealed { .. } => {
                while !bytes.is_empty() {
                    // Seal a full chunk only once more bytes arrive, so the last one is flagged.
                    if matches!(self, Sink::Sealed { chunk, .. } if chunk.len() == BACKUP_CHUNK_BYTES) {
                        self.seal(false)?;
                    }
                    let Sink::Sealed { chunk, .. } = self else { return Ok(()) };
                    let take = (BACKUP_CHUNK_BYTES - chunk.len()).min(bytes.len());
                    let (head, rest) = bytes.split_at(take);
                    chunk.extend_from_slice(head);
                    bytes = rest;
                }
                Ok(())
            }
        }
    }
    fn seal(&mut self, last: bool) -> Result<(), Error> {
        let Sink::Sealed { out, cipher, prefix, aad, counter, chunk } = self else { return Ok(()) };
        let tag = cipher
            .encrypt_inout_detached(&nonce(prefix, *counter, last), aad, chunk.as_mut_slice().into())
            .map_err(|_| Error::new(ErrorKind::Internal, "a backup chunk could not be encrypted"))?;
        out.write_all(chunk).map_err(|e| io(&e))?;
        out.write_all(&tag).map_err(|e| io(&e))?;
        chunk.clear();
        *counter = counter.checked_add(1).ok_or_else(|| invalid("the backup is too large"))?;
        Ok(())
    }
    fn finish(mut self) -> Result<W, Error> {
        match self {
            Sink::Plain { mut out, hasher } => {
                out.write_all(hasher.finalize().as_bytes()).map_err(|e| io(&e))?;
                out.flush().map_err(|e| io(&e))?;
                Ok(out)
            }
            Sink::Sealed { .. } => {
                self.seal(true)?;
                let Sink::Sealed { mut out, .. } = self else { return Err(Error::new(ErrorKind::Internal, "backup writer state changed")) };
                out.flush().map_err(|e| io(&e))?;
                Ok(out)
            }
        }
    }
}

/// Writes a backup: the header and manifest first, then each entry's bytes in manifest order.
pub struct BackupWriter<W: Write> {
    sink: Sink<W>,
    entries: std::vec::IntoIter<BackupEntry>,
    /// Set by any failed write: the output is incomplete and can't be finished.
    failed: bool,
}

impl<W: Write> BackupWriter<W> {
    /// Starts a backup of the entries `manifest` lists.
    pub fn create(mut out: W, protection: Protection<'_>, manifest: &BackupManifest) -> Result<Self, Error> {
        manifest.check()?;
        let encoded = serde_json::to_vec(manifest).map_err(|_| Error::new(ErrorKind::Internal, "the backup manifest could not be encoded"))?;
        if encoded.len() > MAX_BACKUP_MANIFEST_BYTES {
            return Err(invalid("the backup manifest is too large"));
        }
        let (header, cipher) = match protection {
            Protection::Unencrypted => (BackupHeader { version: BACKUP_VERSION, encryption: None }, None),
            Protection::Passphrase { passphrase, kdf, salt, nonce_prefix } => {
                let kdf = kdf.check()?;
                (
                    BackupHeader { version: BACKUP_VERSION, encryption: Some(Encryption { kdf, salt, nonce_prefix }) },
                    Some((kdf.key(passphrase, &salt)?, nonce_prefix)),
                )
            }
        };
        let aad = header.encode();
        out.write_all(&aad).map_err(|e| io(&e))?;
        let mut sink = match cipher {
            None => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(&aad);
                Sink::Plain { out, hasher: Box::new(hasher) }
            }
            Some((cipher, prefix)) => Sink::Sealed { out, cipher: Box::new(cipher), prefix, aad, counter: 0, chunk: Vec::with_capacity(BACKUP_CHUNK_BYTES) },
        };
        let len = u32::try_from(encoded.len()).map_err(|_| invalid("the backup manifest is too large"))?;
        sink.write(&len.to_le_bytes())?;
        sink.write(&encoded)?;
        Ok(Self { sink, entries: manifest.entries.clone().into_iter(), failed: false })
    }

    /// Copies the next entry's bytes from `source`, which must yield exactly the manifest's size
    /// and hash. A source that changed since the manifest was made is an error.
    pub fn write_entry(&mut self, source: &mut impl Read) -> Result<(), Error> {
        if self.failed {
            return Err(invalid("an earlier entry failed; this backup can't be completed"));
        }
        let result = self.copy_entry(source);
        self.failed = result.is_err();
        result
    }

    fn copy_entry(&mut self, source: &mut impl Read) -> Result<(), Error> {
        let entry = self.entries.next().ok_or_else(|| invalid("more entries were written than the manifest lists"))?;
        let mut hasher = blake3::Hasher::new();
        let mut left = entry.size;
        let mut buffer = vec![0u8; BACKUP_CHUNK_BYTES];
        while left > 0 {
            let want = usize::try_from(left.min(BACKUP_CHUNK_BYTES as u64)).unwrap_or(BACKUP_CHUNK_BYTES);
            let slot = buffer.get_mut(..want).unwrap_or_default();
            // A misbehaving reader can't claim more than it was given room for.
            let n = read_some(source, slot)?.min(want);
            if n == 0 {
                return Err(Error::new(ErrorKind::Conflict, "a backup entry is shorter than when it was listed; it may have changed"));
            }
            let part = slot.get(..n).unwrap_or_default();
            hasher.update(part);
            self.sink.write(part)?;
            left = left.saturating_sub(n as u64);
        }
        let mut probe = [0u8; 1];
        if read_some(source, &mut probe)? != 0 {
            return Err(Error::new(ErrorKind::Conflict, "a backup entry is longer than when it was listed; it may have changed"));
        }
        if hex(hasher.finalize().as_bytes()) != entry.blake3 {
            return Err(Error::new(ErrorKind::Conflict, "a backup entry changed since it was listed"));
        }
        Ok(())
    }

    /// Completes the backup once every entry is written, returning the output.
    pub fn finish(self) -> Result<W, Error> {
        if self.failed {
            return Err(invalid("an earlier entry failed; this backup can't be completed"));
        }
        if self.entries.len() > 0 {
            return Err(invalid("the backup is missing entries the manifest lists"));
        }
        self.sink.finish()
    }
}

/// The plain or decrypting payload reader.
enum Source<R: Read> {
    Plain {
        input: R,
        hasher: Box<blake3::Hasher>,
    },
    Sealed {
        input: R,
        cipher: Box<XChaCha20Poly1305>,
        prefix: [u8; PREFIX_BYTES],
        aad: Vec<u8>,
        counter: u32,
        plain: Vec<u8>,
        pos: usize,
        last: bool,
        peeked: Option<u8>,
    },
}

/// One read, retried when interrupted.
fn read_some(input: &mut impl Read, buf: &mut [u8]) -> Result<usize, Error> {
    loop {
        match input.read(buf) {
            Ok(n) => return Ok(n.min(buf.len())),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io(&e)),
        }
    }
}

/// Reads until `buf` is full or the input ends; returns the bytes read.
fn fill(input: &mut impl Read, buf: &mut [u8]) -> Result<usize, Error> {
    let mut n = 0;
    while n < buf.len() {
        let slot = buf.get_mut(n..).unwrap_or_default();
        match input.read(slot) {
            Ok(0) => break,
            Ok(k) => n += k.min(slot.len()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io(&e)),
        }
    }
    Ok(n)
}

impl<R: Read> Source<R> {
    fn next_chunk(&mut self) -> Result<(), Error> {
        let Source::Sealed { input, cipher, prefix, aad, counter, plain, pos, last, peeked } = self else { return Ok(()) };
        if *last {
            return Err(invalid("the backup has less data than its manifest lists"));
        }
        let mut buf = vec![0u8; BACKUP_CHUNK_BYTES + TAG_BYTES];
        let mut n = 0;
        if let (Some(b), Some(first)) = (peeked.take(), buf.first_mut()) {
            *first = b;
            n = 1;
        }
        n += fill(input, buf.get_mut(n..).unwrap_or_default())?;
        // A full chunk is the last only when nothing follows it.
        let mut probe = [0u8; 1];
        let is_last = n < buf.len() || fill(input, &mut probe)? == 0;
        if !is_last {
            *peeked = Some(probe[0]);
        }
        if n < TAG_BYTES {
            return Err(damaged());
        }
        buf.truncate(n);
        let tag_bytes = buf.split_off(n - TAG_BYTES);
        let tag = chacha20poly1305::Tag::try_from(tag_bytes.as_slice()).map_err(|_| damaged())?;
        cipher.decrypt_inout_detached(&nonce(prefix, *counter, is_last), aad, buf.as_mut_slice().into(), &tag).map_err(|_| damaged())?;
        *counter = counter.checked_add(1).ok_or_else(damaged)?;
        *plain = buf;
        *pos = 0;
        *last = is_last;
        Ok(())
    }

    fn read_exact(&mut self, mut out: &mut [u8]) -> Result<(), Error> {
        match self {
            Source::Plain { input, hasher } => {
                if fill(input, out)? < out.len() {
                    return Err(invalid("the backup is truncated"));
                }
                hasher.update(out);
                Ok(())
            }
            Source::Sealed { .. } => {
                while !out.is_empty() {
                    let Source::Sealed { plain, pos, .. } = self else { return Ok(()) };
                    if *pos >= plain.len() {
                        self.next_chunk()?;
                        continue;
                    }
                    let available = plain.get(*pos..).unwrap_or_default();
                    let take = available.len().min(out.len());
                    let (head, rest) = std::mem::take(&mut out).split_at_mut(take);
                    head.copy_from_slice(available.get(..take).unwrap_or_default());
                    *pos += take;
                    out = rest;
                }
                Ok(())
            }
        }
    }

    /// Checks the end: every chunk read and nothing after it, or the plain trailer hash.
    fn finish(mut self) -> Result<(), Error> {
        match self {
            Source::Plain { mut input, hasher } => {
                let mut trailer = [0u8; 32];
                if fill(&mut input, &mut trailer)? < trailer.len() {
                    return Err(invalid("the backup is truncated"));
                }
                if &trailer != hasher.finalize().as_bytes() {
                    return Err(Error::new(ErrorKind::Authentication, "the backup is damaged or was changed"));
                }
                let mut probe = [0u8; 1];
                if fill(&mut input, &mut probe)? != 0 {
                    return Err(invalid("the backup has unexpected data at its end"));
                }
                Ok(())
            }
            Source::Sealed { .. } => {
                // The manifest's sizes say where the payload ends; the last chunk must end there.
                let Source::Sealed { plain, pos, last, .. } = &self else { return Ok(()) };
                if *pos < plain.len() {
                    return Err(invalid("the backup has more data than its manifest lists"));
                }
                if !*last {
                    self.next_chunk()?;
                    let Source::Sealed { plain, last, .. } = &self else { return Ok(()) };
                    if !plain.is_empty() || !*last {
                        return Err(invalid("the backup has more data than its manifest lists"));
                    }
                }
                Ok(())
            }
        }
    }
}

/// Reads a backup: the header, then (after the passphrase, when encrypted) the manifest, then
/// each entry. An entry's bytes are reported complete only after its hash matched, and the
/// whole backup only after [`BackupReader::finish`]: a host stages entries and publishes them
/// once `finish` succeeds.
pub struct BackupReader<R: Read> {
    source: Source<R>,
    manifest: BackupManifest,
    next: usize,
    failure: Option<Error>,
}

impl<R: Read> BackupReader<R> {
    /// Opens a backup whose header was already read with [`BackupHeader::read`]. `passphrase`
    /// is required for encrypted backups and refused for unencrypted ones (read the header first
    /// to know which).
    pub fn open(input: R, header: &BackupHeader, header_bytes: Vec<u8>, passphrase: Option<&str>) -> Result<Self, Error> {
        let mut source = match &header.encryption {
            None if passphrase.is_some() => {
                // An encrypted backup may have been swapped for a crafted plain one: say so.
                return Err(Error::new(
                    ErrorKind::Authentication,
                    "this backup isn't encrypted, although a passphrase was given; it may not be the backup you expect",
                ));
            }
            None => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(&header_bytes);
                Source::Plain { input, hasher: Box::new(hasher) }
            }
            Some(e) => {
                let passphrase = passphrase.ok_or_else(|| invalid("this backup is encrypted; enter its passphrase"))?;
                let cipher = e.kdf.key(passphrase, &e.salt)?;
                Source::Sealed {
                    input,
                    cipher: Box::new(cipher),
                    prefix: e.nonce_prefix,
                    aad: header_bytes,
                    counter: 0,
                    plain: Vec::new(),
                    pos: 0,
                    last: false,
                    peeked: None,
                }
            }
        };
        let mut len = [0u8; 4];
        source.read_exact(&mut len)?;
        let len = usize::try_from(u32::from_le_bytes(len)).unwrap_or(usize::MAX);
        if len > MAX_BACKUP_MANIFEST_BYTES {
            return Err(invalid("the backup manifest is too large"));
        }
        let mut encoded = vec![0u8; len];
        source.read_exact(&mut encoded)?;
        let manifest: BackupManifest = serde_json::from_slice(&encoded).map_err(|_| invalid("the backup manifest is malformed"))?;
        manifest.check()?;
        Ok(Self { source, manifest, next: 0, failure: None })
    }

    pub fn manifest(&self) -> &BackupManifest {
        &self.manifest
    }

    /// Copies the next entry into `sink` and checks its hash. Returns `None` after the last
    /// entry. On an error, whatever reached `sink` must be discarded. The reader remains failed:
    /// later entry reads and `finish` return the first error without further source or sink work.
    pub fn next_entry(&mut self, sink: &mut impl Write) -> Result<Option<&BackupEntry>, Error> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let index = self.next;
        if self.manifest.entries.get(index).is_none() {
            return Ok(None);
        }
        if let Err(error) = self.copy_entry(sink) {
            // A failed operation may have consumed input or written a partial destination. Even
            // a later recoverable I/O success cannot restore this stream's integrity authority.
            self.failure = Some(error.clone());
            return Err(error);
        }
        self.next = self.next.saturating_add(1);
        Ok(self.manifest.entries.get(index))
    }

    fn copy_entry(&mut self, sink: &mut impl Write) -> Result<(), Error> {
        let entry = self.manifest.entries.get(self.next).ok_or_else(|| invalid("the backup reader has no pending entry"))?;
        let mut hasher = blake3::Hasher::new();
        let mut left = entry.size;
        let mut buffer = vec![0u8; BACKUP_CHUNK_BYTES];
        while left > 0 {
            let want = usize::try_from(left.min(BACKUP_CHUNK_BYTES as u64)).unwrap_or(BACKUP_CHUNK_BYTES);
            let slot = buffer.get_mut(..want).unwrap_or_default();
            self.source.read_exact(slot)?;
            hasher.update(slot);
            sink.write_all(slot).map_err(|e| io(&e))?;
            left -= want as u64;
        }
        if hex(hasher.finalize().as_bytes()) != entry.blake3 {
            return Err(Error::new(ErrorKind::Authentication, "a backup entry is damaged or was changed"));
        }
        Ok(())
    }

    /// Confirms every entry was read and the backup ends exactly where it should. A reader that
    /// failed an entry returns that original error without further input reads.
    pub fn finish(self) -> Result<(), Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.next < self.manifest.entries.len() {
            return Err(invalid("not every backup entry was read"));
        }
        self.source.finish()
    }
}

#[cfg(test)]
mod tests;
