//! Fingerprint selected Cargo configuration inputs, not an evaluation of arbitrary build scripts.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const FILE_BYTES: usize = 64 * 1024;
const TOTAL_BYTES: usize = 256 * 1024;
fn error() -> String {
    "unsupported or excessive Cargo configuration; see docs/performance.md".into()
}
fn flag_key(key: &str) -> bool {
    matches!(key, "rustflags" | "rustdocflags") || key.contains("RUSTFLAGS") || key.contains("RUSTDOCFLAGS")
}

fn response_file(config: &toml::Table) -> bool {
    let mut pending: Vec<_> = config.iter().map(|(k, v)| (v, flag_key(k))).collect();
    while let Some((value, flags)) = pending.pop() {
        match value {
            toml::Value::String(s) if flags && s.contains('@') => return true,
            toml::Value::Array(a) => pending.extend(a.iter().map(|v| (v, flags))),
            toml::Value::Table(t) => pending.extend(t.iter().map(|(k, v)| (v, flags || flag_key(k)))),
            _ => {}
        }
    }
    false
}

fn directories(root: &Path, home: &Path) -> Result<Vec<PathBuf>, String> {
    let mut result = vec![];
    for parent in root.ancestors() {
        if result.len() == 32 {
            return Err(error());
        }
        result.push(parent.join(".cargo"));
    }
    result.push(home.to_path_buf());
    Ok(result)
}

fn selected(dir: &Path, total: &mut usize) -> Result<Value, String> {
    for name in ["config", "config.toml"] {
        let path = dir.join(name);
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(error()),
        };
        if !metadata.is_file() || metadata.len() > FILE_BYTES as u64 {
            return Err(error());
        }
        let mut bytes = vec![];
        fs::File::open(&path).map_err(|_| error())?.take((FILE_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|_| error())?;
        *total = total.checked_add(bytes.len()).ok_or_else(error)?;
        if bytes.len() > FILE_BYTES || *total > TOTAL_BYTES {
            return Err(error());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| error())?;
        // The pinned TOML parser retains its default recursion guard. No unbounded feature.
        let config: toml::Table = toml::from_str(text).map_err(|_| error())?;
        // Includes and dependency redirects introduce unmeasured transitive file inputs.
        let supported = [
            "alias",
            "build",
            "cache",
            "cargo-new",
            "credential-alias",
            "doc",
            "env",
            "future-incompat-report",
            "http",
            "install",
            "net",
            "profile",
            "registries",
            "registry",
            "resolver",
            "target",
            "term",
        ];
        let compiler = config
            .get("build")
            .and_then(toml::Value::as_table)
            .is_some_and(|b| ["rustc", "rustc-wrapper", "rustc-workspace-wrapper"].iter().any(|k| b.contains_key(*k)));
        if compiler {
            return Err("unsupported Cargo build compiler/wrapper override; see docs/performance.md".into());
        }
        if config.keys().any(|k| !supported.contains(&k.as_str()))
            || response_file(&config)
            || config.get("resolver").and_then(toml::Value::as_table).is_some_and(|r| r.contains_key("lockfile-path"))
        {
            return Err(error());
        }
        return Ok(json!({"name":name,"resolved":path.canonicalize().map_err(|_| error())?.to_str().ok_or_else(error)?,"contents":text}));
    }
    Ok(Value::Null)
}

fn relevant(key: &str) -> bool {
    // Include new Cargo options conservatively; exclude credentials and ephemeral jobserver fds.
    (key.starts_with("CARGO_") && key != "CARGO_MAKEFLAGS" && !key.ends_with("_TOKEN") && !key.contains("_PASSWORD") && !key.contains("_SECRET"))
        || key.contains("RUSTFLAGS")
        || key.contains("RUSTDOCFLAGS")
        || key.starts_with("RUSTC")
        || ["CARGO", "RUSTDOC", "RUSTUP_TOOLCHAIN", "RAYON_NUM_THREADS"].contains(&key)
}

pub(super) fn inputs(root: &Path, home: &Path, vars: &BTreeMap<String, String>) -> Result<Value, String> {
    let mut total = 0;
    let mut files = vec![];
    for dir in directories(root, home)? {
        if dir.as_os_str().len() > 4096 {
            return Err(error());
        }
        files.push(json!({"location":dir.to_str().ok_or_else(error)?,"selected":selected(&dir,&mut total)?}));
    }
    let mut env = BTreeMap::new();
    let mut size = 0usize;
    for (k, v) in vars.iter().filter(|(k, _)| relevant(k)) {
        size = size.checked_add(k.len()).and_then(|n| n.checked_add(v.len())).ok_or_else(error)?;
        if k.len() > 256
            || v.len() > 4096
            || env.len() >= 128
            || size > 65536
            || k.starts_with("CARGO_UNSTABLE_")
            || k == "RUSTC_BOOTSTRAP"
            || (flag_key(k) && v.contains('@'))
        {
            return Err(error());
        }
        env.insert(k, v);
    }
    Ok(json!({"provenance":"cargo-selected-inputs-v1","files":files,"environment":env}))
}

pub(super) fn snapshot(root: &Path) -> Result<Value, String> {
    let root = root.canonicalize().map_err(|_| error())?;
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|p| PathBuf::from(p).join(".cargo")))
        .ok_or_else(error)?;
    let home = if home.is_absolute() { home } else { root.join(home) };
    let mut vars = BTreeMap::new();
    let mut size = 0usize;
    for (k, v) in std::env::vars_os() {
        let name = if cfg!(windows) { k.to_string_lossy().to_ascii_uppercase() } else { k.to_string_lossy().into_owned() };
        if relevant(&name) {
            size = size.checked_add(k.len()).and_then(|n| n.checked_add(v.len())).ok_or_else(error)?;
            if k.len() > 256 || v.len() > 4096 || vars.len() >= 128 || size > 65536 {
                return Err(error());
            }
            let name = if cfg!(windows) { k.into_string().map_err(|_| error())?.to_ascii_uppercase() } else { k.into_string().map_err(|_| error())? };
            if vars.insert(name, v.into_string().map_err(|_| error())?).is_some() {
                return Err(error());
            }
        }
    }
    inputs(&root, &home, &vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ancestor/project");
        let home = dir.path().join("cargo-home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        (dir, root, home)
    }
    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn fingerprint(root: &Path, home: &Path, vars: &BTreeMap<String, String>) -> String {
        super::super::collect::build_fingerprint(root, &inputs(root, home, vars).unwrap()).unwrap()
    }
    #[test]
    fn repository_ancestor_and_cargo_home_config_changes_are_incomparable() {
        let (_dir, root, home) = fixture();
        for path in [root.join(".cargo/config.toml"), root.parent().unwrap().join(".cargo/config.toml"), home.join("config.toml")] {
            write(&path, "[build]\nrustflags = []\n");
            let before = fingerprint(&root, &home, &BTreeMap::new());
            write(&path, "[build]\nrustflags = [\"-C\", \"opt-level=0\"]\n");
            let after = fingerprint(&root, &home, &BTreeMap::new());
            assert_ne!(before, after);
            let mut a = super::super::tests::fixture();
            a["environment"]["flags_fingerprint"] = json!(before);
            let mut b = a.clone();
            b["environment"]["flags_fingerprint"] = json!(after);
            assert!(super::super::report::compare(&a, &b).is_err());
            b = a.clone();
            b["source"]["revision"] = json!("e".repeat(40));
            assert!(!super::super::report::compare(&a, &b).unwrap().0);
            fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn legacy_precedence_and_environment_inputs_are_explicit() {
        let (_dir, root, home) = fixture();
        let legacy = root.join(".cargo/config");
        let modern = root.join(".cargo/config.toml");
        write(&legacy, "[build]\nrustflags = []\n");
        write(&modern, "include = [\"not-read.toml\"]");
        let first = fingerprint(&root, &home, &BTreeMap::new());
        write(&modern, "not valid TOML");
        assert_eq!(first, fingerprint(&root, &home, &BTreeMap::new()));
        fs::remove_file(legacy).unwrap();
        assert!(inputs(&root, &home, &BTreeMap::new()).is_err());
        write(&modern, "[build]\nrustflags = []\n");
        assert_ne!(first, fingerprint(&root, &home, &BTreeMap::new()));
        let mut vars = BTreeMap::new();
        vars.insert("CARGO_INCREMENTAL".into(), "1".into());
        assert_ne!(fingerprint(&root, &home, &BTreeMap::new()), fingerprint(&root, &home, &vars));
        vars.clear();
        vars.insert("CARGO_NEW_OPTION".into(), "changed".into());
        assert_ne!(fingerprint(&root, &home, &BTreeMap::new()), fingerprint(&root, &home, &vars));
        vars.clear();
        vars.insert("CARGO_REGISTRY_TOKEN".into(), "not-a-real-credential".into());
        assert_eq!(fingerprint(&root, &home, &BTreeMap::new()), fingerprint(&root, &home, &vars));
    }
    #[test]
    fn transitive_unknown_or_excessive_configuration_is_refused() {
        let (_dir, root, home) = fixture();
        let config = root.join(".cargo/config.toml");
        for text in [
            r#""in\u0063lude" = ["other.toml"]"#,
            r#"paths = ["other"]"#,
            r#"[source.other]
directory="other""#,
            r#"[build]
rustc="other""#,
            r#"[resolver]
lockfile-path="other""#,
            r#"[build]
rustflags=["@other"]"#,
            r#"[future_unknown]
value=1"#,
        ] {
            write(&config, text);
            assert!(inputs(&root, &home, &BTreeMap::new()).is_err(), "{text}");
        }
        write(&config, &format!("[build]\nx={}0{}", "[".repeat(256), "]".repeat(256)));
        assert!(inputs(&root, &home, &BTreeMap::new()).is_err());
        write(&config, &format!("#{}", "x".repeat(FILE_BYTES)));
        assert!(inputs(&root, &home, &BTreeMap::new()).is_err());
        let deep = root.join((0..40).map(|_| "x").collect::<Vec<_>>().join("/"));
        assert!(directories(&deep, &home).is_err());
        assert!(inputs(&root, &PathBuf::from("x".repeat(4097)), &BTreeMap::new()).is_err());
        let deep = root.join("a/b/c/d/e");
        fs::create_dir_all(&deep).unwrap();
        fs::remove_file(config).unwrap();
        for dir in deep.ancestors().take(5) {
            write(&dir.join(".cargo/config.toml"), &format!("#{}", "x".repeat(60000)));
        }
        assert!(inputs(&deep, &home, &BTreeMap::new()).is_err());
    }
    #[test]
    fn compiler_override_diagnostic_is_specific_without_private_values() {
        let (_dir, root, home) = fixture();
        for key in ["rustc", "rustc-wrapper", "rustc-workspace-wrapper"] {
            write(&root.join(".cargo/config.toml"), &format!("[build]\n{key}=\"/private/compiler\"\n"));
            assert_eq!(inputs(&root, &home, &BTreeMap::new()).unwrap_err(), "unsupported Cargo build compiler/wrapper override; see docs/performance.md");
        }
    }
    #[test]
    fn environment_bounds_and_unsupported_options_refuse_before_hashing() {
        let (_dir, root, home) = fixture();
        for vars in [
            BTreeMap::from([("RUSTFLAGS".into(), "@flags".into())]),
            BTreeMap::from([("RUSTDOCFLAGS".into(), "@flags".into())]),
            BTreeMap::from([("CARGO_UNSTABLE_CONFIG_INCLUDE".into(), "1".into())]),
            BTreeMap::from([("RUSTC_BOOTSTRAP".into(), "1".into())]),
            BTreeMap::from([("CARGO_NEW".into(), "x".repeat(4097))]),
            BTreeMap::from([(format!("CARGO_{}", "x".repeat(256)), "1".into())]),
            (0..129).map(|n| (format!("CARGO_NEW_{n}"), "1".into())).collect(),
            (0..17).map(|n| (format!("CARGO_NEW_{n}"), "x".repeat(4096))).collect(),
        ] {
            assert!(inputs(&root, &home, &vars).is_err());
        }
        fs::create_dir_all(root.join(".cargo/config")).unwrap();
        assert!(inputs(&root, &home, &BTreeMap::new()).is_err());
    }
}
