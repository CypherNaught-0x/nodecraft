//! Execution grants (#173): the user's explicit yes to running one exact package digest. Kept in
//! their own file beside the index, so a build that predates grants still reads the index.
//!
//! - **One digest.** A grant names a package and the digest the user looked at; an update or a
//!   rollback revokes it, so a changed package never runs on an old yes.
//! - **Epochs only go up.** Every grant takes the next epoch; a host running a package's actions
//!   holds the epoch it started under and drops results once that grant is gone. A failed write
//!   changes nothing, so an epoch never comes back.
//! - **Bounded.** Revoked grants stay as tombstones, oldest dropped first past [`MAX_GRANTS`].
//! - **Scoped (#174).** A grant also names the [`Rights`] the user gave: capabilities, and for
//!   network the exact origins they saw. A grant of other rights for the same digest is a new
//!   grant; the old one is revoked, so a host holding it stops.
use crate::archive;
use nodecraft_plugin::PackageDigest;
use nodecraft_plugin::id::PackageId;
use nodecraft_plugin::json::{self, Json, Limits};
use nodecraft_plugin::manifest::Capability;
use nodecraft_types::{Error, ErrorKind};
use std::collections::{BTreeMap, BTreeSet};

/// Grants file format this build writes when a grant holds rights. A file whose grants hold none
/// is still written as version 1, so a build that predates rights keeps reading it.
pub const GRANTS_VERSION: i64 = 2;
const PLAIN_VERSION: i64 = 1;
/// Most grants kept, live and revoked.
pub const MAX_GRANTS: usize = 1024;
/// Most origins one grant names.
pub const MAX_ORIGINS: usize = 8;

/// What a grant lets a package reach beyond running its own code: empty for text actions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rights {
    pub capabilities: BTreeSet<Capability>,
    /// For network: the origins (`https://host`) the user saw, and nothing else.
    pub origins: BTreeSet<String>,
}

impl Rights {
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty() && self.origins.is_empty()
    }

    /// Whether these rights are well formed: origins exactly when network is granted, each a
    /// canonical `https://` origin, at most [`MAX_ORIGINS`].
    pub fn well_formed(&self) -> bool {
        let network = self.capabilities.contains(&Capability::Network);
        network == !self.origins.is_empty() && self.origins.len() <= MAX_ORIGINS && self.origins.iter().all(|o| origin(o))
    }
}

/// `https://` and a lowercase DNS name: the form the broker's templates name. The broker checks
/// a template's origins in full; this keeps a damaged file from naming anything else.
pub fn origin(text: &str) -> bool {
    let Some(host) = text.strip_prefix("https://") else { return false };
    let labels_ok = host.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    host.len() <= 253 && host.contains('.') && labels_ok
}

/// One execution grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub grant_id: String,
    pub package_id: PackageId,
    pub digest: PackageDigest,
    pub epoch: u64,
    pub revoked: bool,
    pub rights: Rights,
}

/// Every grant, and the epoch the next one takes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    pub next_epoch: u64,
    pub grants: Vec<Grant>,
}

impl Grants {
    /// The live grant for exactly this digest of `package`.
    pub fn live(&self, package: &PackageId, digest: &PackageDigest) -> Option<&Grant> {
        self.grants.iter().find(|g| !g.revoked && g.package_id == *package && g.digest == *digest)
    }

    pub fn get(&self, grant_id: &str) -> Option<&Grant> {
        self.grants.iter().find(|g| g.grant_id == grant_id)
    }

    /// Whether the grant a host started under still stands: the live grant of this digest is
    /// the one with `epoch`. Checked again just before anything paid or outward is committed.
    pub fn holds(&self, package: &PackageId, digest: &PackageDigest, epoch: u64) -> bool {
        self.live(package, digest).is_some_and(|g| g.epoch == epoch)
    }

    /// Grants `digest` of `package` exactly `rights`, or returns the live grant it already has
    /// with those rights. A live grant of other rights is revoked: the new one replaces it.
    pub(crate) fn grant(&mut self, package: &PackageId, digest: &PackageDigest, rights: &Rights) -> Result<Grant, Error> {
        if let Some(live) = self.live(package, digest)
            && live.rights == *rights
        {
            return Ok(live.clone());
        }
        let epoch = self.next_epoch.max(1);
        let next_epoch = epoch.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "the plugin store has run out of grant numbers"))?;
        while self.grants.len() >= MAX_GRANTS {
            let Some(oldest) = self.grants.iter().position(|g| g.revoked) else {
                return Err(Error::new(ErrorKind::Budget, "the plugin store holds as many grants as it can; revoke some first"));
            };
            self.grants.remove(oldest);
        }
        for g in self.grants.iter_mut().filter(|g| !g.revoked && g.package_id == *package && g.digest == *digest) {
            g.revoked = true;
        }
        let grant = Grant { grant_id: format!("g{epoch}"), package_id: package.clone(), digest: *digest, epoch, revoked: false, rights: rights.clone() };
        self.grants.push(grant.clone());
        self.next_epoch = next_epoch;
        Ok(grant)
    }

    /// Revokes every live grant of `package`; whether any was.
    pub(crate) fn revoke_package(&mut self, package: &PackageId) -> bool {
        let mut changed = false;
        for g in self.grants.iter_mut().filter(|g| !g.revoked && g.package_id == *package) {
            g.revoked = true;
            changed = true;
        }
        changed
    }
}

const GRANTS_LIMITS: Limits = Limits { bytes: 1024 * 1024, depth: 4, string_bytes: 256, entries: 2 * MAX_GRANTS, raw_numbers: false };

pub(crate) fn encode(grants: &Grants) -> Result<String, Error> {
    let s = |t: String| Json::Str(t);
    let plain = grants.grants.iter().all(|g| g.rights.is_empty());
    let list = grants
        .grants
        .iter()
        .map(|g| {
            let capabilities = g.rights.capabilities.iter().map(|c| s(c.as_str().to_owned())).collect();
            let mut m = BTreeMap::from([
                ("capabilities".to_owned(), Json::Array(capabilities)),
                ("digest".to_owned(), s(g.digest.to_string())),
                ("epoch".to_owned(), s(g.epoch.to_string())),
                ("grant_id".to_owned(), s(g.grant_id.clone())),
                ("package_id".to_owned(), s(g.package_id.to_string())),
                ("revoked".to_owned(), Json::Bool(g.revoked)),
                ("scope".to_owned(), s("local".to_owned())),
            ]);
            if !plain {
                m.insert("origins".to_owned(), Json::Array(g.rights.origins.iter().cloned().map(s).collect()));
            }
            Json::Object(m)
        })
        .collect();
    let root = Json::Object(BTreeMap::from([
        ("grants".to_owned(), Json::Array(list)),
        ("grants_version".to_owned(), Json::Int(if plain { PLAIN_VERSION } else { GRANTS_VERSION })),
        ("next_epoch".to_owned(), s(grants.next_epoch.to_string())),
    ]));
    json::canonical(&root).map_err(archive::wire)
}

fn damaged() -> Error {
    Error::new(ErrorKind::InvalidInput, "the plugin store's grants are damaged; nothing runs until a package is granted again")
}

fn text(m: &mut BTreeMap<String, Json>, key: &str) -> Result<String, Error> {
    match m.remove(key) {
        Some(Json::Str(s)) => Ok(s),
        _ => Err(damaged()),
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Grants, Error> {
    let Json::Object(mut root) = archive::parse(bytes, GRANTS_LIMITS)? else { return Err(damaged()) };
    let version = match root.remove("grants_version") {
        Some(Json::Int(v @ (PLAIN_VERSION | GRANTS_VERSION))) => v,
        Some(Json::Int(v)) if v > GRANTS_VERSION => {
            return Err(Error::new(ErrorKind::Unsupported, "the plugin store's grants were written by a newer NodeCraft"));
        }
        _ => return Err(damaged()),
    };
    let next_epoch = json::decimal(&text(&mut root, "next_epoch")?).map_err(|_| damaged())?;
    let Some(Json::Array(items)) = root.remove("grants") else { return Err(damaged()) };
    if !root.is_empty() || items.len() > MAX_GRANTS {
        return Err(damaged());
    }
    let mut grants = Vec::with_capacity(items.len());
    for item in items {
        let Json::Object(mut m) = item else { return Err(damaged()) };
        let epoch = json::decimal(&text(&mut m, "epoch")?).map_err(|_| damaged())?;
        let grant = Grant {
            grant_id: text(&mut m, "grant_id")?,
            package_id: PackageId::parse(&text(&mut m, "package_id")?).map_err(|_| damaged())?,
            digest: PackageDigest::parse(&text(&mut m, "digest")?).map_err(|_| damaged())?,
            epoch,
            revoked: match m.remove("revoked") {
                Some(Json::Bool(b)) => b,
                _ => return Err(damaged()),
            },
            rights: rights(&mut m, version)?,
        };
        // Local grants are all this build makes; anything else isn't its to run.
        if text(&mut m, "scope")? != "local"
            || !m.is_empty()
            || grant.grant_id != format!("g{epoch}")
            || epoch == 0
            || epoch >= next_epoch
            || grants.iter().any(|g: &Grant| g.epoch == epoch)
        {
            return Err(damaged());
        }
        grants.push(grant);
    }
    Ok(Grants { next_epoch, grants })
}

/// A grant's rights: none in a version 1 file; in version 2, known capabilities and origins,
/// each listed once, and well formed together.
fn rights(m: &mut BTreeMap<String, Json>, version: i64) -> Result<Rights, Error> {
    let Some(Json::Array(capabilities)) = m.remove("capabilities") else { return Err(damaged()) };
    if version == PLAIN_VERSION {
        return if capabilities.is_empty() { Ok(Rights::default()) } else { Err(damaged()) };
    }
    let Some(Json::Array(origins)) = m.remove("origins") else { return Err(damaged()) };
    let mut rights = Rights::default();
    for c in capabilities {
        let Json::Str(c) = c else { return Err(damaged()) };
        if !rights.capabilities.insert(Capability::parse(&c).ok_or_else(damaged)?) {
            return Err(damaged());
        }
    }
    for o in origins {
        let Json::Str(o) = o else { return Err(damaged()) };
        if !rights.origins.insert(o) {
            return Err(damaged());
        }
    }
    if rights.well_formed() { Ok(rights) } else { Err(damaged()) }
}
