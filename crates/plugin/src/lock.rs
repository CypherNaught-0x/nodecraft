//! Dependency locks: exact version and digest, data packages only, no cycles, at most 8 deep,
//! one version of a package at a time. No resolver and no fetching: every lock is satisfied by
//! a package already installed, or the dependent stays disabled.
use crate::digest::PackageDigest;
use crate::error::{Code, Result, err};
use crate::id::PackageId;
use crate::manifest::{Manifest, Runtime};
use std::collections::{BTreeMap, BTreeSet};

/// Deepest dependency chain below a package.
pub const MAX_DEPTH: usize = 8;
/// Most packages in one closure.
pub const MAX_CLOSURE: usize = 256;

/// An installed package: its manifest and digest.
pub struct Installed<'a> {
    pub manifest: &'a Manifest,
    pub digest: PackageDigest,
}

/// The packages `root` needs, dependencies before dependents (root excluded), or why it can't
/// be enabled.
pub fn resolve(root: &Manifest, installed: &BTreeMap<PackageId, Installed<'_>>) -> Result<Vec<PackageId>> {
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    let mut path = vec![root.package_id.clone()];
    visit(root, installed, &mut path, &mut done, &mut order)?;
    Ok(order)
}

fn visit(
    manifest: &Manifest,
    installed: &BTreeMap<PackageId, Installed<'_>>,
    path: &mut Vec<PackageId>,
    done: &mut BTreeSet<PackageId>,
    order: &mut Vec<PackageId>,
) -> Result<()> {
    for dep in &manifest.dependencies {
        if path.contains(&dep.package_id) {
            return err(Code::DependencyCycle, "dependencies form a cycle");
        }
        let Some(found) = installed.get(&dep.package_id) else {
            return err(Code::DependencyMissing, "a locked dependency isn't installed");
        };
        // Every lock is checked, also on a package already resolved through another path: two
        // locks naming different versions of one package can't both be met, whatever the order.
        if found.manifest.version != dep.version || found.digest != dep.digest {
            return err(Code::DependencyMissing, "the installed dependency isn't the locked version and digest");
        }
        if done.contains(&dep.package_id) {
            continue;
        }
        if found.manifest.runtime != Runtime::Data {
            return err(Code::DependencyRejected, "dependencies are data packages only");
        }
        if path.len() > MAX_DEPTH {
            return err(Code::DependencyRejected, "dependencies nest deeper than 8");
        }
        if done.len() >= MAX_CLOSURE {
            return err(Code::TooLarge, "too many packages in one dependency closure");
        }
        path.push(dep.package_id.clone());
        visit(found.manifest, installed, path, done, order)?;
        path.pop();
        done.insert(dep.package_id.clone());
        order.push(dep.package_id.clone());
    }
    Ok(())
}
