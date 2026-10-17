//! Runtime workspace discovery. Cargo owns workspace traversal, including nested members.
use std::path::{Path, PathBuf};

pub fn resolve(cwd: &Path) -> Result<PathBuf, String> {
    // CARGO_MANIFEST_DIR describes compilation/launch metadata, not invocation intent.
    // locate-project reads manifests without building or executing workspace code.
    let out = crate::cargo(cwd)
        .args(["locate-project", "--workspace", "--message-format", "json"])
        .output()
        .map_err(|e| format!("locate invoking Cargo workspace: {e}"))?;
    if !out.status.success() {
        return Err(format!("cannot locate invoking Cargo workspace: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let location: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo locate-project: bad JSON: {e}"))?;
    let manifest = location.get("root").and_then(serde_json::Value::as_str).ok_or("cargo locate-project omitted manifest path")?;
    let manifest = Path::new(manifest);
    if manifest.file_name().is_none_or(|name| name != "Cargo.toml") {
        return Err("invoking workspace manifest must be Cargo.toml".into());
    }
    let root = manifest.parent().ok_or("workspace manifest has no parent")?.canonicalize().map_err(|e| format!("resolve workspace directory: {e}"))?;
    // Read-only preflight: never refresh a lockfile merely to identify the workspace.
    let out = crate::cargo(&root)
        .args(["metadata", "--format-version", "1", "--no-deps", "--offline", "--locked"])
        .output()
        .map_err(|e| format!("validate invoking workspace: {e}"))?;
    if !out.status.success() {
        return Err(format!("invoking workspace validation failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: bad JSON: {e}"))?;
    let reported = metadata.get("workspace_root").and_then(serde_json::Value::as_str).ok_or("cargo metadata omitted workspace root")?;
    let reported = Path::new(reported).canonicalize().map_err(|e| format!("resolve reported workspace root: {e}"))?;
    if root != reported {
        return Err("located manifest and Cargo workspace root disagree".into());
    }
    let expected = root.join("xtask/Cargo.toml").canonicalize().map_err(|e| format!("invoking workspace needs xtask/Cargo.toml: {e}"))?;
    let members = metadata.get("workspace_members").and_then(serde_json::Value::as_array).ok_or("cargo metadata omitted workspace members")?;
    let packages = metadata.get("packages").and_then(serde_json::Value::as_array).ok_or("cargo metadata omitted packages")?;
    for package in packages {
        if package.get("name").and_then(serde_json::Value::as_str) != Some("xtask") {
            continue;
        }
        let id = package.get("id").ok_or("xtask metadata omitted package id")?;
        let path = package.get("manifest_path").and_then(serde_json::Value::as_str).ok_or("xtask metadata omitted manifest path")?;
        let path = Path::new(path).canonicalize().map_err(|e| format!("resolve xtask member manifest: {e}"))?;
        if path == expected && members.contains(id) {
            return Ok(root);
        }
    }
    Err("invoking workspace must contain the xtask member at xtask/Cargo.toml".into())
}
