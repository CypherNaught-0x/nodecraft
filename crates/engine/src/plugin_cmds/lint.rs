//! Checking a package the way installing, granting and enabling it would, for its author (#175).
//! The package goes into a store in memory that is dropped afterwards; nothing is kept, granted
//! or run, and no build or install step of the author's is ever executed.
use super::*;
use nodecraft_plugin::manifest::Runtime;
use nodecraft_plugin_store::MemoryBackend;

/// IDs NodeCraft itself registers (commands and generation node kinds): no package may take one.
pub fn host_ids() -> Result<BTreeSet<String>, Error> {
    let mut taken: BTreeSet<String> = crate::commands::Registry::builtins()?.specs().map(|s| s.id.to_owned()).collect();
    taken.extend(crate::generation_catalog::default_registry()?.0.descriptors().map(|d| d.id.clone()));
    Ok(taken)
}

/// Checks `bytes` as `host` would install them, then what granting and enabling would refuse: an
/// ID NodeCraft already registers, a provider template that breaks a rule, text action
/// definitions that don't compile. The result is the import report plus `runtime`, `rights`
/// and `warnings` (what installs but won't do anything yet); an error is what install, grant or
/// enable would refuse.
pub fn lint(bytes: &[u8], host: &PluginHost) -> Result<Value, Error> {
    let trust = trust(host)?;
    let mut store = Store::open(MemoryBackend::default())?;
    let report = store.import(bytes, &trust, true)?;
    let package = store.inspect(bytes, &trust)?;
    nodecraft_plugin::register::stage(&package.manifest, &host.taken)
        .map_err(|e| Error::new(ErrorKind::Conflict, format!("one of its IDs is already NodeCraft's ({e})")))?;
    crate::plugin_rights::asks(&package)?;
    let runtime = match &package.manifest.runtime {
        Runtime::Data => "data",
        Runtime::TextActions { .. } => {
            crate::plugin_text::compile(&package)?;
            "text_actions"
        }
        Runtime::ProviderOperations { .. } => "provider_operations",
        Runtime::RustWasm { .. } => "rust_wasm",
    };
    let mut warnings = Vec::new();
    if matches!(report.provenance, Provenance::Unsigned) {
        warnings.push("it isn't signed: it installs as a local package, and can't use a reserved namespace".to_owned());
    } else if matches!(report.provenance, Provenance::UnknownKey { .. }) {
        warnings.push("its signing key isn't in the trust file: it installs as signed by an unknown key".to_owned());
    }
    if matches!(package.manifest.runtime, Runtime::Data) {
        for r in &package.manifest.registrations {
            match &r.binding {
                Some(binding) if host.bindings.contains(binding) => {}
                Some(binding) => warnings.push(format!("{} binds to {binding}, which this NodeCraft doesn't have: it stays unbound", r.id)),
                None => warnings.push(format!("{} binds to nothing: it stays unbound", r.id)),
            }
        }
    }
    for dep in &package.manifest.dependencies {
        warnings.push(format!("it needs {} {} ({}) installed and enabled first", dep.package_id, dep.version, dep.digest));
    }
    let mut out = report_json(&report, true);
    if let Some(o) = out.as_object_mut() {
        o.remove("replaced");
        o.remove("dry_run");
        o.insert("runtime".into(), json!(runtime));
        o.insert("rights".into(), crate::plugin_rights::asks_json(&package));
        o.insert("warnings".into(), json!(warnings));
    }
    Ok(out)
}
