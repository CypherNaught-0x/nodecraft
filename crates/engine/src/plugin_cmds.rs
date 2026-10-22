//! `plugins.*` commands (#172): list, import, update, enable, disable, roll back, uninstall and
//! prune packages in the local store, and grant or revoke a text action package's right to run
//! (#173; people only, never agents). They change no document and add no undo step; the
//! host's `plugins` service ([`register_plugin_service`]) owns the store and the trust file.
//! Native desktop admission returns `{pending: string}` immediately. Poll `plugins.result`, then
//! explicitly acknowledge terminal results with `plugins.forget`. Synchronous hosts retain the
//! direct result contract. Nothing here runs a package or opens a dialog.
//!
//! Uninstall and prune keep every digest a card of the open project was made with
//! ([`crate::plugin_runtime::PluginLock`]), as well as what installed packages lock.
use crate::commands::{CommandSpec, Context, Effect, Services, params};
use nodecraft_plugin::PackageDigest;
use nodecraft_plugin::id::{HostId, PackageId};
use nodecraft_plugin::signature::{Provenance, TrustStore};
use nodecraft_plugin_store::{ImportReport, State, Store, StoreBackend};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// The host-service capability the `plugins.*` commands use.
pub const PLUGIN_SERVICE: &str = "plugins.store";
/// Retained asynchronous results; separate from package operations.
pub const RESULT_SERVICE: &str = "plugins.results";
/// A worker nonce plus a checked decimal sequence.
pub const MAX_PENDING_ID: usize = 53;
#[cfg(not(target_arch = "wasm32"))]
pub mod worker;
#[cfg(not(target_arch = "wasm32"))]
pub use worker::{PluginWorker, register_plugin_worker};
/// Largest trust file read.
const MAX_TRUST_BYTES: u64 = 256 * 1024;

fn object(required: &[&str], properties: Value) -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

fn package_schema() -> Value {
    json!({"type": "string", "minLength": 5, "maxLength": 80, "pattern": "^[a-z][a-z0-9_]*(\\\\.[a-z][a-z0-9_]*){2,5}$", "description": "Package ID, e.g. io.example.palette"})
}

fn none_schema() -> Value {
    object(&[], json!({}))
}

fn pending_schema() -> Value {
    object(
        &["pending"],
        json!({"pending":{"type":"string","minLength":1,"maxLength":MAX_PENDING_ID,"description":"Opaque ID returned by this host's plugin worker."}}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingParams {
    pending: String,
}

fn results_enabled(c: &Context<'_>, _: &Value) -> Result<(), Error> {
    if c.services.contains(RESULT_SERVICE) { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "this host answers plugin operations synchronously")) }
}

fn pending_op(op: &'static str, p: &Value) -> Result<Effect, Error> {
    // Inspect before DeserializeOwned can copy a caller's oversized string.
    if p.get("pending").and_then(Value::as_str).is_none_or(|id| id.is_empty() || id.len() > MAX_PENDING_ID) {
        return Err(Error::new(ErrorKind::InvalidInput, "invalid plugin pending ID"));
    }
    let p: PendingParams = params(p)?;
    Ok(Effect::Service { capability: RESULT_SERVICE, request: json!({"op":op,"pending":p.pending}) })
}

fn result(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    pending_op("result", p)
}

fn forget(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    pending_op("forget", p)
}

fn file_schema() -> Value {
    object(
        &["location"],
        json!({"location": {"type": "string", "minLength": 1, "maxLength": 4096, "description": "Path of the .ncraft-plugin file (UTF-8)."},
               "dry_run": {"type": "boolean", "description": "Only verify and report; install nothing."}}),
    )
}

fn package_only_schema() -> Value {
    object(&["package_id"], json!({"package_id": package_schema()}))
}

fn grant_id_schema() -> Value {
    object(
        &["grant_id"],
        json!({"grant_id": {"type": "string", "pattern": "^g[1-9][0-9]{0,19}$", "description": "Grant ID from plugins.grant or plugins.list"}}),
    )
}

fn rollback_schema() -> Value {
    object(&["package_id", "digest"], json!({"package_id": package_schema(), "digest": {"type": "string", "pattern": "^blake3:[0-9a-f]{64}$"}}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileParams {
    location: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageParams {
    package_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantParams {
    grant_id: String,
}

/// `g` and an epoch: the shape the store gives grant IDs.
fn valid_grant_id(id: &str) -> bool {
    id.len() <= 21 && id.strip_prefix('g').is_some_and(|n| !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RollbackParams {
    package_id: String,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

/// Available only where the host keeps a package store.
fn enabled(c: &Context<'_>, _: &Value) -> Result<(), Error> {
    if c.services.contains(PLUGIN_SERVICE) { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "plugin packages aren't available here")) }
}

fn service(request: Value) -> Effect {
    Effect::Service { capability: PLUGIN_SERVICE, request }
}

fn list(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let _: Empty = params(p)?;
    Ok(service(json!({"op": "list"})))
}

fn prune(c: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let _: Empty = params(p)?;
    Ok(service(json!({"op": "prune", "referenced": referenced_json(c)})))
}

/// The digests the open project's plugin cards were made with.
fn referenced_json(c: &Context<'_>) -> Vec<String> {
    crate::plugin_runtime::referenced(c.document).iter().map(|d| d.to_string()).collect()
}

/// Most referenced digests a request may name.
pub(crate) fn referenced_of(request: &Value) -> Result<BTreeSet<PackageDigest>, Error> {
    let bad = || Error::new(ErrorKind::InvalidInput, "the package digests the project uses are invalid or too many");
    let Some(list) = request.get("referenced") else { return Ok(BTreeSet::new()) };
    let list = list.as_array().filter(|l| l.len() <= crate::plugin_runtime::MAX_REFERENCED).ok_or_else(bad)?;
    list.iter().map(|d| d.as_str().and_then(|d| PackageDigest::parse(d).ok()).ok_or_else(bad)).collect()
}

fn file_op(op: &'static str) -> impl Fn(&Context<'_>, &Value) -> Result<Effect, Error> {
    move |_, p| {
        let p: FileParams = params(p)?;
        Ok(service(json!({"op": op, "location": p.location, "dry_run": p.dry_run})))
    }
}

fn import(c: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    file_op("import")(c, p)
}

fn update(c: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    file_op("update")(c, p)
}

fn package_op(op: &'static str, p: &Value) -> Result<Effect, Error> {
    let p: PackageParams = params(p)?;
    PackageId::parse(&p.package_id).map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))?;
    Ok(service(json!({"op": op, "package_id": p.package_id})))
}

fn enable(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    package_op("enable", p)
}

fn disable(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    package_op("disable", p)
}

fn uninstall(c: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let mut effect = package_op("uninstall", p)?;
    if let Effect::Service { request: Value::Object(request), .. } = &mut effect {
        request.insert("referenced".into(), json!(referenced_json(c)));
    }
    Ok(effect)
}

fn grant(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    package_op("grant", p)
}

fn revoke(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    if p.get("grant_id").and_then(Value::as_str).is_none_or(|id| !valid_grant_id(id)) {
        return Err(Error::new(ErrorKind::InvalidInput, "invalid grant ID"));
    }
    let p: GrantParams = params(p)?;
    Ok(service(json!({"op": "revoke", "grant_id": p.grant_id})))
}

fn rollback(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let p: RollbackParams = params(p)?;
    PackageId::parse(&p.package_id).map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))?;
    PackageDigest::parse(&p.digest).map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))?;
    Ok(service(json!({"op": "rollback", "package_id": p.package_id, "digest": p.digest})))
}

pub fn specs() -> Vec<CommandSpec> {
    let spec = |id, label, input_schema: fn() -> Value, params_doc, run| CommandSpec {
        id,
        label,
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema,
        params_doc,
        enabled,
        run,
    };
    let mut specs = vec![
        spec(
            "plugins.list",
            "List plugin packages",
            none_schema,
            "{}; installed packages: {packages: [{package_id, version, digest, state, license, signed_by, kept: [digest], grant: {grant_id, epoch}|null}]}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget.",
            list,
        ),
        spec(
            "plugins.import",
            "Import plugin package",
            file_schema,
            "{location: string, dry_run?: bool}; verifies a .ncraft-plugin and installs it disabled (dry_run: reports only). Direct result: {package_id, version, digest, provenance, key_id, signed_by, license, notices, registrations, replaced, dry_run}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget. No dialog.",
            import,
        ),
        spec(
            "plugins.update",
            "Update plugin package",
            file_schema,
            "{location: string, dry_run?: bool}; installs a disabled package version, keeping the previous digest for rollback. Direct result: import report with replaced. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget.",
            update,
        ),
        spec(
            "plugins.enable",
            "Enable plugin package",
            package_only_schema,
            "{package_id}; checks signature, locks and IDs before publishing registrations; a text action package also needs a live grant (plugins.grant) and definitions that compile. Direct result: {registered: [id], unbound: [id], grant: {grant_id, epoch}|null}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget.",
            enable,
        ),
        spec(
            "plugins.disable",
            "Disable plugin package",
            package_only_schema,
            "{package_id}; refuses enabled dependents. Direct result: {disabled: true}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget.",
            disable,
        ),
        spec(
            "plugins.rollback",
            "Roll back plugin package",
            rollback_schema,
            "{package_id, digest}; activates a kept disabled version. Direct result: {rolled_back: true}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget.",
            rollback,
        ),
        spec(
            "plugins.uninstall",
            "Uninstall plugin package",
            package_only_schema,
            "{package_id}; removes a disabled package, retaining locked digests. Direct result: {removed: [digest], kept: [digest], dependents: [package_id]}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget. No project edit.",
            uninstall,
        ),
        spec(
            "plugins.grant",
            "Allow plugin package to run",
            package_only_schema,
            "{package_id}; grants the active version of a text action package the right to run, after its definitions compile. People only: agents can't call it. Direct result: {grant_id, epoch, package_id, digest}; the same live grant if there is one. An update, rollback or uninstall revokes it. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget.",
            grant,
        ),
        spec(
            "plugins.revoke",
            "Revoke plugin package grant",
            grant_id_schema,
            "{grant_id}; revokes a grant and disables its package if enabled under it; running actions stop and their results are dropped. People only. Direct result: {grant_id, revoked: true, disabled: bool}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget.",
            revoke,
        ),
        spec(
            "plugins.prune",
            "Prune old plugin versions",
            none_schema,
            "{}; removes unused kept versions. Direct result: {removed: [digest]}. Native worker hosts return {pending: string}; poll plugins.result, then plugins.forget.",
            prune,
        ),
    ];
    specs.push(CommandSpec {
        id: "plugins.result",
        label: "Read plugin operation result",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: pending_schema,
        params_doc: "{pending: string}; reads a retained queued/running/completed/failed envelope without consuming it. Terminal results/errors stay until plugins.forget or oldest-terminal eviction at host capacity. Evicted IDs return NotFound. Pending IDs belong to this host.",
        enabled: results_enabled,
        run: result,
    });
    specs.push(CommandSpec {
        id: "plugins.forget",
        label: "Acknowledge plugin operation result",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: pending_schema,
        params_doc: "{pending: string}; releases only a completed/failed host result, returning {forgotten: true}. Does not cancel work, change packages or edit a document.",
        enabled: results_enabled,
        run: forget,
    });
    specs
}

/// What the host's `plugins` service needs besides the store.
pub struct PluginHost {
    /// The trust file (`trust.json`); absent means no key is trusted.
    pub trust: Option<PathBuf>,
    /// IDs the host already registers (commands, node kinds): no package may take one.
    pub taken: BTreeSet<String>,
    /// Host implementations data packages may bind descriptors to.
    pub bindings: BTreeSet<HostId>,
}

fn read_capped(path: &std::path::Path, cap: u64, what: &str) -> Result<Vec<u8>, Error> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::Read;
        if !path.is_absolute() {
            return Err(Error::new(ErrorKind::InvalidInput, format!("the {what} location must be an absolute path")));
        }
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(Error::new(ErrorKind::InvalidInput, format!("the {what} location isn't a file"))),
            Err(_) => return Err(Error::new(ErrorKind::NotFound, format!("the {what} can't be opened"))),
        }
        let file = std::fs::File::open(path).map_err(|_| Error::new(ErrorKind::NotFound, format!("the {what} can't be opened")))?;
        let mut bytes = Vec::new();
        file.take(cap + 1).read_to_end(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, format!("the {what} can't be read")))?;
        if bytes.len() as u64 > cap {
            return Err(Error::new(ErrorKind::InvalidInput, format!("the {what} is too large")));
        }
        Ok(bytes)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (path, cap, what);
        Err(Error::new(ErrorKind::Unsupported, "reading files isn't available in the browser"))
    }
}

fn trust(host: &PluginHost) -> Result<TrustStore, Error> {
    let Some(path) = &host.trust else { return Ok(TrustStore::default()) };
    // Only a missing file means no trusted keys; any other failure (permissions, a broken link)
    // is reported, so a revoked key can never pass as merely unknown.
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(TrustStore::default()),
        Err(_) => return Err(Error::new(ErrorKind::Internal, "the trust file can't be read")),
        Ok(_) => {}
    }
    TrustStore::decode(&read_capped(path, MAX_TRUST_BYTES, "trust file")?)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the trust file is invalid ({e})")))
}

fn report_json(r: &ImportReport, dry_run: bool) -> Value {
    let (provenance, key_id) = match &r.provenance {
        Provenance::Unsigned => ("unsigned", None),
        Provenance::Verified { key_id } => ("verified", Some(key_id.to_string())),
        Provenance::UnknownKey { key_id } => ("unknown_key", Some(key_id.to_string())),
    };
    json!({
        "package_id": r.entry.package_id.as_str(), "version": r.entry.version.to_string(), "digest": r.entry.digest.to_string(),
        "provenance": provenance, "key_id": key_id, "signed_by": r.entry.signed_by, "license": r.entry.license, "notices": r.notices,
        "registrations": r.registrations.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
        "replaced": r.replaced.map(|d| d.to_string()), "dry_run": dry_run,
    })
}

fn grant_json(grant: &nodecraft_plugin_store::Grant) -> Value {
    json!({"grant_id": grant.grant_id, "epoch": grant.epoch})
}

/// A text action package's definitions must compile before it is granted or enabled; other
/// packages have nothing to compile.
fn compiles<B: StoreBackend>(store: &Store<B>, id: &PackageId) -> Result<(), Error> {
    let package = store.load(id)?;
    if matches!(package.manifest.runtime, nodecraft_plugin::manifest::Runtime::TextActions { .. }) {
        crate::plugin_text::compile(&package)?;
    }
    Ok(())
}

fn package_id(request: &Value) -> Result<PackageId, Error> {
    let text = request.get("package_id").and_then(Value::as_str).unwrap_or_default();
    PackageId::parse(text).map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))
}

/// Runs one `plugins` request against `store`.
pub fn handle<B: StoreBackend>(store: &mut Store<B>, host: &PluginHost, request: &Value) -> Result<Value, Error> {
    // Another window or the CLI may have changed the store since the last request.
    store.refresh()?;
    let referenced = referenced_of(request)?;
    match request.get("op").and_then(Value::as_str) {
        Some("list") => {
            // Grants that can't be read grant nothing; the list still shows the packages.
            let grants = store.grants().unwrap_or_default();
            let index = store.index();
            let packages: Vec<Value> = index
                .active
                .iter()
                .filter_map(|(id, (digest, state))| {
                    let entry = index.objects.get(digest)?;
                    let kept: Vec<String> =
                        index.objects.values().filter(|e| e.package_id == *id && e.digest != *digest).map(|e| e.digest.to_string()).collect();
                    Some(json!({
                        "package_id": id.as_str(), "version": entry.version.to_string(), "digest": digest.to_string(),
                        "state": if *state == State::Enabled { "enabled" } else { "disabled" },
                        "license": entry.license, "signed_by": entry.signed_by, "kept": kept,
                        "grant": grants.live(id, digest).map(grant_json),
                    }))
                })
                .collect();
            Ok(json!({"packages": packages}))
        }
        Some(op @ ("import" | "update")) => {
            let location = request.get("location").and_then(Value::as_str).unwrap_or_default();
            let dry_run = request.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
            let limit = u64::try_from(nodecraft_plugin_store::ArchiveLimits::DEFAULT.archive_bytes).unwrap_or(u64::MAX);
            let bytes = read_capped(std::path::Path::new(location), limit, "package file")?;
            let trust = trust(host)?;
            let report = if op == "import" { store.import(&bytes, &trust, dry_run)? } else { store.update(&bytes, &trust, dry_run)? };
            Ok(report_json(&report, dry_run))
        }
        Some("enable") => {
            let id = package_id(request)?;
            compiles(store, &id)?;
            let report = store.enable(&id, &trust(host)?, &host.taken, &host.bindings)?;
            Ok(json!({
                "registered": report.registered.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
                "unbound": report.unbound.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
                "grant": report.grant.as_ref().map(grant_json),
            }))
        }
        Some("grant") => {
            let id = package_id(request)?;
            compiles(store, &id)?;
            let grant = store.grant(&id, &trust(host)?)?;
            let mut out = grant_json(&grant);
            if let Some(o) = out.as_object_mut() {
                o.insert("package_id".into(), json!(grant.package_id.as_str()));
                o.insert("digest".into(), json!(grant.digest.to_string()));
            }
            Ok(out)
        }
        Some("revoke") => {
            let grant_id = request
                .get("grant_id")
                .and_then(Value::as_str)
                .filter(|id| valid_grant_id(id))
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "invalid grant ID"))?;
            let report = store.revoke(grant_id)?;
            let dependents: Vec<&str> = report.dependents.iter().map(PackageId::as_str).collect();
            Ok(json!({"grant_id": report.grant.grant_id, "revoked": true, "disabled": report.disabled, "dependents": dependents}))
        }
        Some("disable") => store.disable(&package_id(request)?).map(|()| json!({"disabled": true})),
        Some("rollback") => {
            let digest = PackageDigest::parse(request.get("digest").and_then(Value::as_str).unwrap_or_default())
                .map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))?;
            store.rollback(&package_id(request)?, &digest).map(|()| json!({"rolled_back": true}))
        }
        Some("uninstall") => {
            let report = store.uninstall(&package_id(request)?, &referenced)?;
            let digests = |d: &[PackageDigest]| d.iter().map(|d| d.to_string()).collect::<Vec<_>>();
            Ok(
                json!({"removed": digests(&report.removed), "kept": digests(&report.kept), "dependents": report.dependents.iter().map(|p| p.as_str()).collect::<Vec<_>>()}),
            )
        }
        Some("prune") => Ok(json!({"removed": store.prune(&referenced)?.iter().map(|d| d.to_string()).collect::<Vec<_>>()})),
        _ => Err(Error::new(ErrorKind::InvalidInput, "unknown plugins request")),
    }
}

/// Registers the `plugins` service over `store`.
pub fn register_plugin_service<B: StoreBackend + 'static>(services: &mut Services, mut store: Store<B>, host: PluginHost) -> Result<(), Error> {
    services.register(PLUGIN_SERVICE, move |request: &Value| handle(&mut store, &host, request))
}

#[cfg(test)]
mod tests;
