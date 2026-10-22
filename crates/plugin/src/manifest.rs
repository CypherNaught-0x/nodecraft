//! The package manifest (`manifest.json`): what a package is, what it needs and what it
//! registers. Decoding is strict: unknown fields, duplicate keys, floats and anything outside
//! the limits below are errors, and the archive's manifest bytes must already be canonical.
use crate::digest::PackageDigest;
use crate::error::{Code, Result, WireError, err};
use crate::id::{HostId, LocalId, PackageId, RegisteredId, Token, located};
use crate::inventory::InventoryPath;
use crate::json::{self, Fields, Json, Limits};
use std::collections::BTreeSet;

/// Manifest format this host reads.
pub const MANIFEST_VERSION: u32 = 1;
/// Largest manifest.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Most registrations in one package.
pub const MAX_REGISTRATIONS: usize = 256;
/// Most direct dependencies.
pub const MAX_DEPENDENCIES: usize = 16;
/// Most capabilities or features in one list.
pub const MAX_NAMES: usize = 32;
/// Longest SPDX license expression.
pub const MAX_LICENSE: usize = 128;

/// The JSON profile manifests are read under.
pub const MANIFEST_LIMITS: Limits = Limits { bytes: MAX_MANIFEST_BYTES, depth: 32, string_bytes: 4096, entries: 1024, raw_numbers: false };

/// A package version: `MAJOR.MINOR.PATCH`, each a decimal without leading zeros. Pre-release
/// and build suffixes aren't part of protocol 1; the digest, not the version, identifies what
/// runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn parse(text: &str) -> Result<Self> {
        let part = |p: Option<&str>| -> Result<u32> {
            let p = p.unwrap_or("");
            let canonical = !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()) && (p == "0" || !p.starts_with('0'));
            if !canonical {
                return err(Code::BadVersion, "a version is MAJOR.MINOR.PATCH, decimals without leading zeros");
            }
            p.parse().map_err(|_| WireError::new(Code::BadVersion, "a version part is too large"))
        };
        let mut parts = text.split('.');
        let version = Version { major: part(parts.next())?, minor: part(parts.next())?, patch: part(parts.next())? };
        if parts.next().is_some() {
            return err(Code::BadVersion, "a version is MAJOR.MINOR.PATCH, decimals without leading zeros");
        }
        Ok(version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The protocol versions a package speaks: one major, a range of minors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolRange {
    pub major: u32,
    pub min_minor: u32,
    pub max_minor: u32,
}

/// The descriptor contract (#162) a package's descriptors were written against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractVersion {
    pub major: u32,
    pub minor: u32,
}

/// The text action definition schema a `text_actions` package's descriptors use.
pub const TEXT_ACTIONS_SCHEMA: u32 = 1;

/// What runs a package's registrations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runtime {
    /// Data only: descriptors, catalog data, Technique references. Nothing executes.
    Data,
    /// Text actions written as definitions (#173): pure expressions over text the host evaluates
    /// itself, so nothing foreign executes. Still runs only once the user grants it.
    TextActions { schema: u32 },
    /// A sandboxed module (a later phase; the contract fixes its shape now).
    RustWasm { module: InventoryPath },
}

/// A host capability a package may ask for. Installation never grants one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    AssetRead,
    AssetWrite,
    Network,
    Filesystem,
    Process,
    Gpu,
    Secret,
}

impl Capability {
    pub const ALL: [Capability; 7] =
        [Capability::AssetRead, Capability::AssetWrite, Capability::Network, Capability::Filesystem, Capability::Process, Capability::Gpu, Capability::Secret];

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::AssetRead => "asset.read",
            Capability::AssetWrite => "asset.write",
            Capability::Network => "network",
            Capability::Filesystem => "filesystem",
            Capability::Process => "process",
            Capability::Gpu => "gpu",
            Capability::Secret => "secret",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == text)
    }
}

/// A name in an optional list this host may not know: kept, so the manifest round-trips, but
/// never acted on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Optional<T> {
    Known(T),
    Unknown(String),
}

/// Required and optional capabilities. An unknown required capability is an error; an unknown
/// optional one is kept and ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub required: BTreeSet<Capability>,
    pub optional: BTreeSet<Optional<Capability>>,
}

/// Protocol features beyond the baseline, negotiated at `initialize`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub required: BTreeSet<String>,
    pub optional: BTreeSet<String>,
}

/// An exact lock on another (data) package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub package_id: PackageId,
    pub version: Version,
    pub digest: PackageDigest,
}

/// What an entry registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegistrationKind {
    Action,
    NodeKind,
    Provider,
    Catalog,
    Technique,
}

impl RegistrationKind {
    pub const ALL: [RegistrationKind; 5] =
        [RegistrationKind::Action, RegistrationKind::NodeKind, RegistrationKind::Provider, RegistrationKind::Catalog, RegistrationKind::Technique];

    pub fn as_str(self) -> &'static str {
        match self {
            RegistrationKind::Action => "action",
            RegistrationKind::NodeKind => "node_kind",
            RegistrationKind::Provider => "provider",
            RegistrationKind::Catalog => "catalog",
            RegistrationKind::Technique => "technique",
        }
    }
}

/// One entry the package registers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub id: LocalId,
    pub kind: RegistrationKind,
    /// Its descriptor file in the package (shaped by #162, not by this contract).
    pub descriptor: InventoryPath,
    /// For a data package: the host implementation this descriptor configures. Without one,
    /// an executable kind in a data package stays disabled.
    pub binding: Option<HostId>,
}

/// A decoded manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub package_id: PackageId,
    pub version: Version,
    pub host_protocol: ProtocolRange,
    pub descriptor_contract: ContractVersion,
    pub publisher_key_id: Option<Token>,
    pub license: String,
    pub runtime: Runtime,
    pub dependencies: Vec<Dependency>,
    pub capabilities: Capabilities,
    pub features: Features,
    pub registrations: Vec<Registration>,
}

impl Manifest {
    /// Decodes manifest bytes, which must be canonical JSON (as the inventory hashes them).
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = json::parse(bytes, MANIFEST_LIMITS)?;
        if json::canonical(&value)?.as_bytes() != bytes {
            return err(Code::NotCanonical, "manifest bytes must be canonical JSON");
        }
        Self::from_json(value)
    }

    /// Decodes an already parsed manifest (canonical form not checked).
    pub fn from_json(value: Json) -> Result<Self> {
        let mut f = Fields::new(value, "")?;
        if f.u32("manifest_version")? != MANIFEST_VERSION {
            return Err(WireError::new(Code::BadVersion, "this host reads manifest version 1").at("/manifest_version"));
        }
        let package_id = located(PackageId::parse(&f.string("package_id")?), "/package_id")?;
        let version = located(Version::parse(&f.string("version")?), "/version")?;
        let host_protocol = protocol_range(f.required("host_protocol")?)?;
        let descriptor_contract = contract(f.required("descriptor_contract")?)?;
        let publisher_key_id = match f.optional("publisher_key_id") {
            Some(v) => Some(located(Token::parse(&json::string(v, "/publisher_key_id")?), "/publisher_key_id")?),
            None => None,
        };
        let license = f.string("license")?;
        let license_ok =
            !license.trim().is_empty() && license.len() <= MAX_LICENSE && license.bytes().all(|b| b.is_ascii_alphanumeric() || b" .+-()".contains(&b));
        if !license_ok {
            return Err(WireError::new(Code::WrongType, "license is an SPDX expression of at most 128 ASCII bytes").at("/license"));
        }
        let runtime = runtime(f.required("runtime")?)?;
        let dependencies = f
            .array("dependencies", MAX_DEPENDENCIES)?
            .into_iter()
            .enumerate()
            .map(|(i, d)| dependency(d, &format!("/dependencies/{i}")))
            .collect::<Result<Vec<_>>>()?;
        let mut seen = BTreeSet::new();
        for (i, d) in dependencies.iter().enumerate() {
            if d.package_id == package_id || !seen.insert(&d.package_id) {
                return Err(WireError::new(Code::Collision, "a package depends on itself or locks one package twice").at(&format!("/dependencies/{i}")));
            }
        }
        let capabilities = capabilities(f.required("capabilities")?)?;
        let features = match f.optional("features") {
            Some(v) => features(v)?,
            None => Features::default(),
        };
        let registrations = f
            .array("registrations", MAX_REGISTRATIONS)?
            .into_iter()
            .enumerate()
            .map(|(i, r)| registration(r, &format!("/registrations/{i}")))
            .collect::<Result<Vec<_>>>()?;
        let mut ids = BTreeSet::new();
        for (i, r) in registrations.iter().enumerate() {
            if !ids.insert(&r.id) {
                return Err(WireError::new(Code::Collision, "two registrations share an ID").at(&format!("/registrations/{i}/id")));
            }
        }
        f.finish()?;
        if runtime != Runtime::Data
            && let Some(i) = registrations.iter().position(|r| r.binding.is_some())
        {
            return Err(
                WireError::new(Code::Incompatible, "only a data package binds descriptors to host implementations").at(&format!("/registrations/{i}/binding"))
            );
        }
        if runtime == Runtime::Data && !capabilities.required.is_empty() {
            return Err(WireError::new(Code::Incompatible, "a data package runs nothing, so it can't require capabilities").at("/capabilities/required"));
        }
        if let Runtime::TextActions { .. } = runtime {
            if !capabilities.required.is_empty() {
                return Err(
                    WireError::new(Code::Incompatible, "text actions only read their inputs, so they can't require capabilities").at("/capabilities/required")
                );
            }
            if let Some(i) = registrations.iter().position(|r| r.kind != RegistrationKind::Action) {
                return Err(WireError::new(Code::Incompatible, "a text_actions package registers only actions").at(&format!("/registrations/{i}/kind")));
            }
        }
        Ok(Self {
            package_id,
            version,
            host_protocol,
            descriptor_contract,
            publisher_key_id,
            license,
            runtime,
            dependencies,
            capabilities,
            features,
            registrations,
        })
    }

    /// The IDs this package registers, in manifest order.
    pub fn registered_ids(&self) -> impl Iterator<Item = RegisteredId> + '_ {
        self.registrations.iter().map(|r| RegisteredId::new(&self.package_id, &r.id))
    }
}

fn protocol_range(value: Json) -> Result<ProtocolRange> {
    let mut f = Fields::new(value, "/host_protocol")?;
    let range = ProtocolRange { major: f.u32("major")?, min_minor: f.u32("min_minor")?, max_minor: f.u32("max_minor")? };
    f.finish()?;
    if range.min_minor > range.max_minor {
        return Err(WireError::new(Code::BadVersion, "min_minor is above max_minor").at("/host_protocol"));
    }
    Ok(range)
}

fn contract(value: Json) -> Result<ContractVersion> {
    let mut f = Fields::new(value, "/descriptor_contract")?;
    let version = ContractVersion { major: f.u32("major")?, minor: f.u32("minor")? };
    f.finish()?;
    Ok(version)
}

fn runtime(value: Json) -> Result<Runtime> {
    let mut f = Fields::new(value, "/runtime")?;
    let runtime = match f.string("kind")?.as_str() {
        "data" => Runtime::Data,
        "text_actions" => match f.u32("schema")? {
            TEXT_ACTIONS_SCHEMA => Runtime::TextActions { schema: TEXT_ACTIONS_SCHEMA },
            _ => return Err(WireError::new(Code::UnsupportedFeature, "text_actions schema is 1").at("/runtime/schema")),
        },
        "rust_wasm" => Runtime::RustWasm { module: located(InventoryPath::parse(&f.string("module")?), "/runtime/module")? },
        _ => return Err(WireError::new(Code::UnsupportedFeature, "runtime kind is data, text_actions or rust_wasm").at("/runtime/kind")),
    };
    f.finish()?;
    Ok(runtime)
}

fn dependency(value: Json, at: &str) -> Result<Dependency> {
    let mut f = Fields::new(value, at)?;
    let package_id = located(PackageId::parse(&f.string("package_id")?), &f.path("package_id"))?;
    let version = located(Version::parse(&f.string("version")?), &f.path("version"))?;
    let digest = located(PackageDigest::parse(&f.string("digest")?), &f.path("digest"))?;
    f.finish()?;
    Ok(Dependency { package_id, version, digest })
}

/// A capability or feature name: dot-separated parts of `[a-z][a-z0-9_]*`, at most 64 bytes.
pub(crate) fn is_feature_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .split('.')
            .all(|p| p.bytes().next().is_some_and(|b| b.is_ascii_lowercase()) && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

fn names(value: Json, at: &str) -> Result<Vec<String>> {
    let items = json::array(value, MAX_NAMES, at)?;
    let mut names = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
    for (i, item) in items.into_iter().enumerate() {
        let at = format!("{at}/{i}");
        let name = json::string(item, &at)?;
        if !is_feature_name(&name) {
            return Err(WireError::new(Code::BadId, "a capability or feature name is dotted lowercase, at most 64 bytes").at(&at));
        }
        if !seen.insert(name.clone()) {
            return Err(WireError::new(Code::Collision, "a name is listed twice").at(&at));
        }
        names.push(name);
    }
    Ok(names)
}

fn capabilities(value: Json) -> Result<Capabilities> {
    let mut f = Fields::new(value, "/capabilities")?;
    let required = names(f.required("required")?, "/capabilities/required")?;
    let optional = names(f.required("optional")?, "/capabilities/optional")?;
    f.finish()?;
    let mut out = Capabilities::default();
    for (i, name) in required.iter().enumerate() {
        let capability = Capability::parse(name).ok_or_else(|| {
            WireError::new(Code::UnsupportedFeature, "a required capability this host doesn't know").at(&format!("/capabilities/required/{i}"))
        })?;
        out.required.insert(capability);
    }
    for (i, name) in optional.into_iter().enumerate() {
        let entry = Capability::parse(&name).map_or(Optional::Unknown(name), Optional::Known);
        if let Optional::Known(c) = &entry
            && out.required.contains(c)
        {
            return Err(WireError::new(Code::Collision, "a capability is both required and optional").at(&format!("/capabilities/optional/{i}")));
        }
        out.optional.insert(entry);
    }
    Ok(out)
}

fn features(value: Json) -> Result<Features> {
    let mut f = Fields::new(value, "/features")?;
    let required: BTreeSet<String> = names(f.required("required")?, "/features/required")?.into_iter().collect();
    let optional: BTreeSet<String> = names(f.required("optional")?, "/features/optional")?.into_iter().collect();
    f.finish()?;
    if required.intersection(&optional).next().is_some() {
        return Err(WireError::new(Code::Collision, "a feature is both required and optional").at("/features"));
    }
    Ok(Features { required, optional })
}

fn registration(value: Json, at: &str) -> Result<Registration> {
    let mut f = Fields::new(value, at)?;
    let id = located(LocalId::parse(&f.string("id")?), &f.path("id"))?;
    let kind_text = f.string("kind")?;
    let kind = RegistrationKind::ALL
        .into_iter()
        .find(|k| k.as_str() == kind_text)
        .ok_or_else(|| WireError::new(Code::UnsupportedFeature, "a registration kind this host doesn't know").at(&f.path("kind")))?;
    let descriptor = located(InventoryPath::parse(&f.string("descriptor")?), &f.path("descriptor"))?;
    let binding = match f.optional("binding") {
        Some(v) => Some(located(HostId::parse(&json::string(v, &f.path("binding"))?), &f.path("binding"))?),
        None => None,
    };
    f.finish()?;
    Ok(Registration { id, kind, descriptor, binding })
}
