//! `plugin.*` commands (#172): list, import, update, enable, disable, roll back, uninstall and
//! prune data packages in the local store. They change no document and add no undo step; the
//! host's `plugins` service ([`register_plugin_service`]) owns the store and the trust file.
//! Nothing in a package runs, and nothing here opens a dialog.
//!
//! Projects don't yet record which package digests they use (#172 part 3), so uninstall and
//! prune keep only what installed packages lock.
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

/// The host-service capability the `plugin.*` commands use.
pub const PLUGIN_SERVICE: &str = "plugins.store";
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
struct RollbackParams {
    package_id: String,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn always(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}

fn service(request: Value) -> Effect {
    Effect::Service { capability: PLUGIN_SERVICE, request }
}

fn list(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let _: Empty = params(p)?;
    Ok(service(json!({"op": "list"})))
}

fn prune(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let _: Empty = params(p)?;
    Ok(service(json!({"op": "prune"})))
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

fn uninstall(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    package_op("uninstall", p)
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
        enabled: always,
        run,
    };
    vec![
        spec(
            "plugin.list",
            "List plugin packages",
            none_schema,
            "{}; installed packages: {packages: [{package_id, version, digest, state, license, signed_by, kept: [digest]}]}.",
            list,
        ),
        spec(
            "plugin.import",
            "Import plugin package",
            file_schema,
            "{location: string, dry_run?: bool}; verifies a .ncraft-plugin data package and installs it disabled (dry_run: only reports). Returns {package_id, version, digest, provenance, signed_by, license, notices, registrations, dry_run}. Never opens a dialog.",
            import,
        ),
        spec(
            "plugin.update",
            "Update plugin package",
            file_schema,
            "{location: string, dry_run?: bool}; installs a new version of a disabled package, disabled, keeping the old one for rollback. Returns the import report with `replaced`.",
            update,
        ),
        spec(
            "plugin.enable",
            "Enable plugin package",
            package_only_schema,
            "{package_id}; checks its signature, exact dependency locks and IDs, then publishes all its registrations or none. Returns {registered: [id], unbound: [id]}.",
            enable,
        ),
        spec(
            "plugin.disable",
            "Disable plugin package",
            package_only_schema,
            "{package_id}; refused while an enabled package depends on it. Returns {disabled: true}.",
            disable,
        ),
        spec(
            "plugin.rollback",
            "Roll back plugin package",
            rollback_schema,
            "{package_id, digest}; makes a kept version of a disabled package active again. Returns {rolled_back: true}.",
            rollback,
        ),
        spec(
            "plugin.uninstall",
            "Uninstall plugin package",
            package_only_schema,
            "{package_id}; removes a disabled package, keeping versions installed packages lock. Returns {removed: [digest], kept: [digest], dependents: [package_id]}. Projects are never edited.",
            uninstall,
        ),
        spec("plugin.prune", "Prune old plugin versions", none_schema, "{}; drops kept versions nothing uses. Returns {removed: [digest]}.", prune),
    ]
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
    if !path.exists() {
        return Ok(TrustStore::default());
    }
    TrustStore::decode(&read_capped(path, MAX_TRUST_BYTES, "trust file")?)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the trust file is invalid ({e})")))
}

fn report_json(r: &ImportReport, dry_run: bool) -> Value {
    let provenance = match &r.provenance {
        Provenance::Unsigned => "unsigned",
        Provenance::Verified { .. } => "verified",
        Provenance::UnknownKey { .. } => "unknown_key",
    };
    json!({
        "package_id": r.entry.package_id.as_str(), "version": r.entry.version.to_string(), "digest": r.entry.digest.to_string(),
        "provenance": provenance, "signed_by": r.entry.signed_by, "license": r.entry.license, "notices": r.notices,
        "registrations": r.registrations.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
        "replaced": r.replaced.map(|d| d.to_string()), "dry_run": dry_run,
    })
}

fn package_id(request: &Value) -> Result<PackageId, Error> {
    let text = request.get("package_id").and_then(Value::as_str).unwrap_or_default();
    PackageId::parse(text).map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))
}

/// Runs one `plugins` request against `store`.
pub fn handle<B: StoreBackend>(store: &mut Store<B>, host: &PluginHost, request: &Value) -> Result<Value, Error> {
    let referenced = BTreeSet::new();
    match request.get("op").and_then(Value::as_str) {
        Some("list") => {
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
            let report = store.enable(&package_id(request)?, &trust(host)?, &host.taken, &host.bindings)?;
            Ok(
                json!({"registered": report.registered.iter().map(|i| i.as_str()).collect::<Vec<_>>(), "unbound": report.unbound.iter().map(|i| i.as_str()).collect::<Vec<_>>()}),
            )
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
