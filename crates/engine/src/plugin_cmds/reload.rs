//! `plugins.reload` (#866): installs the package file a package was last imported or updated
//! from again, as an update, for an author trying a change. Nothing watches a folder: each reload
//! is one explicit request, and it opens no dialog.
//!
//! The file must still hold the same package, and something in it must have changed (its digest).
//! The new version is verified in full before anything changes. An enabled package is disabled
//! for the update and enabled again only when it needs no grant (a data package) and is signed
//! by the same key as before (or unsigned both times): grants cover one digest and an update
//! never carries one over, so a package that runs anything stays off until the person grants
//! the new version, and a new signer is something the person sees before it goes live.
use super::{PackageParams, ReadPackage, compiles, import_bytes, package_id, params, service, trust};
use crate::commands::{Context, Effect};
use nodecraft_plugin::id::PackageId;
use nodecraft_plugin::manifest::Runtime;
use nodecraft_plugin_store::{State, Store, StoreBackend};
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

pub(super) const PARAMS_DOC: &str = "{package_id}; installs the package file the package was last imported or updated from (by absolute path) again, as an update. Refused when no file is remembered, when the file now holds another package, or when it is unchanged (same digest). An enabled package is disabled for the update and enabled again only if it needs no grant and its signer (signature key, or unsigned) is unchanged; a package that runs anything stays disabled until plugins.grant, and one with a new signer until plugins.enable. If the update fails after the package was disabled, the error says when it couldn't be enabled again. Direct result: the plugins.update report plus {reloaded_from, enabled: bool (enabled again), needs_grant: bool (the new version runs something: plugins.grant it, then plugins.enable), signer_changed: bool, signed_by_before: string|null (the old signature's key ID), enable_error?: string, location_not_saved?: string}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget. Not in the browser. No dialog, no watched folder.";

pub(super) fn run(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let p: PackageParams = params(p)?;
    PackageId::parse(&p.package_id).map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))?;
    Ok(service(json!({"op": "reload", "package_id": p.package_id})))
}

/// Enables `id` again as it was enabled before: compiled, checked and under its live grant.
fn enable_again<B: StoreBackend>(store: &mut Store<B>, host: &super::PluginHost, id: &PackageId) -> Result<(), Error> {
    compiles(store, id)?;
    store.enable(id, &trust(host)?, &host.taken, &host.bindings).map(|_| ())
}

/// Who signed a package: the signature's key ID, or none for an unsigned package.
fn signer(package: &nodecraft_plugin_store::Package) -> Option<String> {
    package.signature.as_ref().map(|s| s.key_id.to_string())
}

/// Handles a `reload` request.
pub(super) fn handle<B: StoreBackend>(store: &mut Store<B>, host: &super::PluginHost, request: &Value, read: &mut ReadPackage) -> Result<Value, Error> {
    let id = package_id(request)?;
    let Some((active, state)) = store.index().active.get(&id).copied() else {
        return Err(Error::new(ErrorKind::NotFound, "that package isn't installed"));
    };
    let location = store.source(&id)?.ok_or_else(|| {
        Error::new(ErrorKind::NotFound, "NodeCraft doesn't know which file this package came from; update it from its file once, then it can be reloaded")
    })?;
    let bytes = read(&location)?;
    let incoming = store.inspect(&bytes, &trust(host)?)?;
    if incoming.manifest.package_id != id {
        return Err(Error::new(
            ErrorKind::Conflict,
            format!("the file this package came from now holds {}; import that package instead", incoming.manifest.package_id),
        ));
    }
    if incoming.digest == active {
        return Err(Error::new(ErrorKind::Conflict, "the package file is unchanged since it was installed; pack it again first"));
    }
    let signer_before = signer(&store.load(&id)?);
    let signer_after = signer(&incoming);
    let digest = incoming.digest.to_string();
    // Everything an update checks, before the package is disabled: a file that won't install
    // changes nothing.
    import_bytes(store, host, &json!({"op": "update", "dry_run": true, "digest": digest}), "update", &bytes)?;
    let was_enabled = state == State::Enabled;
    if was_enabled {
        // Refused while enabled packages depend on it; nothing has changed yet.
        store.disable(&id)?;
    }
    let mut out = match import_bytes(store, host, &json!({"op": "update", "digest": digest}), "update", &bytes) {
        Ok(out) => out,
        Err(error) if was_enabled => {
            // The old version is still active, but the update may already have revoked its
            // grant: put it back as it was if it still may be, and say so if it can't.
            return Err(match enable_again(store, host, &id) {
                Ok(()) => error,
                Err(again) => Error::new(
                    error.kind,
                    format!(
                        "{}; {id} was left disabled and couldn't be enabled again ({}): grant it again and enable it, or import its file again",
                        error.message, again.message
                    ),
                ),
            });
        }
        Err(error) => return Err(error),
    };
    let location_error = store.remember_source(&id, Some(&location)).err();
    let runs = !matches!(incoming.manifest.runtime, Runtime::Data);
    let signer_changed = signer_before != signer_after;
    let mut enable_error = None;
    // Only a data package signed as before goes live again unseen: a new signer is shown first.
    let enabled = was_enabled
        && !runs
        && !signer_changed
        && match enable_again(store, host, &id) {
            Ok(()) => true,
            Err(error) => {
                enable_error = Some(error.message);
                false
            }
        };
    if let Some(o) = out.as_object_mut() {
        o.insert("reloaded_from".into(), json!(location));
        o.insert("enabled".into(), json!(enabled));
        o.insert("needs_grant".into(), json!(runs));
        o.insert("signer_changed".into(), json!(signer_changed));
        o.insert("signed_by_before".into(), json!(signer_before));
        if let Some(message) = enable_error {
            o.insert("enable_error".into(), json!(message));
        }
        if let Some(error) = location_error {
            o.insert("location_not_saved".into(), json!(error.message));
        }
    }
    Ok(out)
}
