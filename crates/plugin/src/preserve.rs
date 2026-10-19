//! What a project keeps about plugin nodes, so a project opened without the plugin loses
//! nothing and re-saving it changes nothing.
//!
//! - A [`Requirement`] names the exact package (ID, version, digest), the protocol and descriptor
//!   contract versions, and the registered IDs the project uses. It holds no grants, paths or
//!   credentials.
//! - A [`DescriptorSnapshot`] keeps a descriptor's canonical bytes, identified by their BLAKE3,
//!   so the placeholder card can show its ports; nodes share snapshots by digest.
//! - An [`OpaqueConfig`] keeps a node's plugin data **byte for byte**. In a project envelope it
//!   is stored as a JSON string holding the original text (`{"version":1,"payload":"…"}`), so
//!   however the envelope is re-serialised (#54 re-emits it with serde), the payload's bytes,
//!   key order, whitespace and number spellings survive. The payload is still checked to be
//!   bounded JSON with unique keys (floats allowed) before it is accepted.
use crate::digest::PackageDigest;
use crate::error::{Code, Result, WireError, err};
use crate::id::{PackageId, RegisteredId, located};
use crate::json::{self, Fields, Json, Limits};
use crate::manifest::{ContractVersion, Manifest, Version};
use std::collections::{BTreeMap, BTreeSet};

/// Largest opaque config per node, descriptor snapshot, and all opaque data in one project.
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
pub const MAX_PROJECT_PLUGIN_BYTES: usize = 8 * 1024 * 1024;

const OPAQUE: Limits = Limits { bytes: MAX_CONFIG_BYTES, depth: 32, string_bytes: MAX_CONFIG_BYTES, entries: 4096, raw_numbers: true };

/// A project's lock on one package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requirement {
    pub package_id: PackageId,
    pub version: Version,
    pub digest: PackageDigest,
    pub protocol_major: u32,
    pub descriptor: ContractVersion,
    pub used: BTreeSet<RegisteredId>,
}

impl Requirement {
    pub fn for_package(manifest: &Manifest, digest: PackageDigest, used: BTreeSet<RegisteredId>) -> Result<Self> {
        if used.iter().any(|id| !id.is_in(&manifest.package_id)) {
            return Err(WireError::new(Code::BadId, "a used ID isn't in this package").at("/used"));
        }
        Ok(Self {
            package_id: manifest.package_id.clone(),
            version: manifest.version,
            digest,
            protocol_major: manifest.host_protocol.major,
            descriptor: manifest.descriptor_contract,
            used,
        })
    }

    pub fn to_json(&self) -> Json {
        let n = |v: u32| Json::Int(i64::from(v));
        Json::Object(BTreeMap::from([
            (
                "descriptor_contract".to_owned(),
                Json::Object(BTreeMap::from([("major".to_owned(), n(self.descriptor.major)), ("minor".to_owned(), n(self.descriptor.minor))])),
            ),
            ("digest".to_owned(), Json::Str(self.digest.to_string())),
            ("package_id".to_owned(), Json::Str(self.package_id.to_string())),
            ("protocol_major".to_owned(), n(self.protocol_major)),
            ("used".to_owned(), Json::Array(self.used.iter().map(|id| Json::Str(id.to_string())).collect())),
            ("version".to_owned(), Json::Str(self.version.to_string())),
        ]))
    }

    pub fn from_json(value: Json) -> Result<Self> {
        let mut f = Fields::new(value, "")?;
        let package_id = located(PackageId::parse(&f.string("package_id")?), "/package_id")?;
        let version = located(Version::parse(&f.string("version")?), "/version")?;
        let digest = located(PackageDigest::parse(&f.string("digest")?), "/digest")?;
        let protocol_major = f.u32("protocol_major")?;
        let mut d = Fields::new(f.required("descriptor_contract")?, "/descriptor_contract")?;
        let descriptor = ContractVersion { major: d.u32("major")?, minor: d.u32("minor")? };
        d.finish()?;
        let mut used = BTreeSet::new();
        for (i, item) in f.array("used", crate::manifest::MAX_REGISTRATIONS)?.into_iter().enumerate() {
            let at = format!("/used/{i}");
            let id = located(RegisteredId::parse(&json::string(item, &at)?), &at)?;
            if !id.is_in(&package_id) {
                return Err(WireError::new(Code::BadId, "a used ID isn't in this package").at(&at));
            }
            if !used.insert(id) {
                return Err(WireError::new(Code::Collision, "a used ID is listed twice").at(&at));
            }
        }
        f.finish()?;
        Ok(Self { package_id, version, digest, protocol_major, descriptor, used })
    }
}

/// A descriptor's canonical bytes and their BLAKE3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptorSnapshot {
    pub blake3: [u8; 32],
    pub canonical: String,
}

impl DescriptorSnapshot {
    /// Snapshots descriptor bytes (any JSON within the limits; stored canonical).
    pub fn of(bytes: &[u8]) -> Result<Self> {
        let limits = Limits { bytes: MAX_SNAPSHOT_BYTES, depth: 32, string_bytes: 4096, entries: 1024, raw_numbers: false };
        let canonical = json::canonical(&json::parse(bytes, limits)?)?;
        if canonical.len() > MAX_SNAPSHOT_BYTES {
            return err(Code::TooLarge, "a descriptor snapshot is larger than 64 KiB");
        }
        Ok(Self { blake3: *blake3::hash(canonical.as_bytes()).as_bytes(), canonical })
    }
}

/// A node's plugin data, kept exactly as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpaqueConfig {
    pub version: u32,
    payload: String,
}

impl OpaqueConfig {
    /// Accepts `payload` if it is bounded JSON with unique keys; keeps its bytes as they are.
    pub fn new(version: u32, payload: &[u8]) -> Result<Self> {
        json::parse(payload, OPAQUE)?;
        let text = std::str::from_utf8(payload).map_err(|_| WireError::new(Code::Syntax, "input isn't UTF-8"))?;
        Ok(Self { version, payload: text.to_owned() })
    }

    pub fn payload(&self) -> &[u8] {
        self.payload.as_bytes()
    }

    /// How it sits in a project envelope: the payload as a string, so no re-serialisation of the
    /// envelope can touch its bytes.
    pub fn to_envelope(&self) -> Json {
        Json::Object(BTreeMap::from([("payload".to_owned(), Json::Str(self.payload.clone())), ("version".to_owned(), Json::Int(i64::from(self.version)))]))
    }

    pub fn from_envelope(value: Json) -> Result<Self> {
        let mut f = Fields::new(value, "")?;
        let version = f.u32("version")?;
        let payload = f.string("payload")?;
        f.finish()?;
        located(Self::new(version, payload.as_bytes()), "/payload")
    }
}

/// Counts the plugin data one project keeps against [`MAX_PROJECT_PLUGIN_BYTES`]; snapshots
/// count once per digest, however many nodes share them.
#[derive(Debug, Default)]
pub struct Budget {
    used: usize,
    snapshots: BTreeSet<[u8; 32]>,
}

impl Budget {
    pub fn config(&mut self, config: &OpaqueConfig) -> Result<()> {
        self.add(config.payload.len())
    }

    pub fn snapshot(&mut self, snapshot: &DescriptorSnapshot) -> Result<()> {
        if self.snapshots.insert(snapshot.blake3) { self.add(snapshot.canonical.len()) } else { Ok(()) }
    }

    fn add(&mut self, bytes: usize) -> Result<()> {
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|u| *u <= MAX_PROJECT_PLUGIN_BYTES)
            .ok_or_else(|| WireError::new(Code::TooLarge, "a project keeps more than 8 MiB of plugin data"))?;
        Ok(())
    }
}
