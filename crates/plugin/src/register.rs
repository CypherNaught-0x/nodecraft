//! Staged registration: a package's entries are checked against everything already registered
//! (built-ins and other packages) and published all together or not at all. Any collision
//! rejects the whole batch, so install order can never decide which entry wins.
use crate::error::{Code, Result, WireError};
use crate::id::{PLUGIN_PREFIX, RegisteredId};
use crate::manifest::Manifest;
use std::collections::BTreeSet;

/// The IDs `manifest` would register, checked against `taken`; nothing is published on error.
pub fn stage(manifest: &Manifest, taken: &BTreeSet<String>) -> Result<Vec<RegisteredId>> {
    let mut staged = Vec::with_capacity(manifest.registrations.len());
    let mut seen = BTreeSet::new();
    for (i, id) in manifest.registered_ids().enumerate() {
        if taken.contains(id.as_str()) || !seen.insert(id.clone()) {
            return Err(
                WireError::new(Code::Collision, "an ID is already registered; nothing from this package was registered").at(&format!("/registrations/{i}/id"))
            );
        }
        staged.push(id);
    }
    Ok(staged)
}

/// Whether a host-owned ID is allowed: never under the `plugin.` prefix packages own.
pub fn host_id_allowed(id: &str) -> bool {
    !id.starts_with(PLUGIN_PREFIX)
}
