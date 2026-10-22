//! Execution grants (#173): the user's explicit yes to running one exact package digest. Kept in
//! their own file beside the index, so a build that predates grants still reads the index.
//!
//! - **One digest.** A grant names a package and the digest the user looked at; an update or a
//!   rollback revokes it, so a changed package never runs on an old yes.
//! - **Epochs only go up.** Every grant takes the next epoch; a host running a package's actions
//!   holds the epoch it started under and drops results once that grant is gone. A failed write
//!   changes nothing, so an epoch never comes back.
//! - **Bounded.** Revoked grants stay as tombstones, oldest dropped first past [`MAX_GRANTS`].
use crate::archive;
use nodecraft_plugin::PackageDigest;
use nodecraft_plugin::id::PackageId;
use nodecraft_plugin::json::{self, Json, Limits};
use nodecraft_types::{Error, ErrorKind};
use std::collections::BTreeMap;

/// Grants file format this build reads.
pub const GRANTS_VERSION: i64 = 1;
/// Most grants kept, live and revoked.
pub const MAX_GRANTS: usize = 1024;

/// One execution grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub grant_id: String,
    pub package_id: PackageId,
    pub digest: PackageDigest,
    pub epoch: u64,
    pub revoked: bool,
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

    /// Grants `digest` of `package`, or returns the live grant it already has.
    pub(crate) fn grant(&mut self, package: &PackageId, digest: &PackageDigest) -> Result<Grant, Error> {
        if let Some(live) = self.live(package, digest) {
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
        let grant = Grant { grant_id: format!("g{epoch}"), package_id: package.clone(), digest: *digest, epoch, revoked: false };
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
    let list = grants
        .grants
        .iter()
        .map(|g| {
            Json::Object(BTreeMap::from([
                ("capabilities".to_owned(), Json::Array(Vec::new())),
                ("digest".to_owned(), s(g.digest.to_string())),
                ("epoch".to_owned(), s(g.epoch.to_string())),
                ("grant_id".to_owned(), s(g.grant_id.clone())),
                ("package_id".to_owned(), s(g.package_id.to_string())),
                ("revoked".to_owned(), Json::Bool(g.revoked)),
                ("scope".to_owned(), s("local".to_owned())),
            ]))
        })
        .collect();
    let root = Json::Object(BTreeMap::from([
        ("grants".to_owned(), Json::Array(list)),
        ("grants_version".to_owned(), Json::Int(GRANTS_VERSION)),
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
    match root.remove("grants_version") {
        Some(Json::Int(GRANTS_VERSION)) => {}
        Some(Json::Int(v)) if v > GRANTS_VERSION => {
            return Err(Error::new(ErrorKind::Unsupported, "the plugin store's grants were written by a newer NodeCraft"));
        }
        _ => return Err(damaged()),
    }
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
        };
        // Local grants of no capability are all this build makes; anything else isn't its to run.
        let plain = text(&mut m, "scope")? == "local" && matches!(m.remove("capabilities"), Some(Json::Array(c)) if c.is_empty());
        if !plain
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
