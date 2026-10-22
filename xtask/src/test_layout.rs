//! Crates that set `autotests = false` build their integration tests as one binary, `tests/it.rs`,
//! which pulls each file in with `#[path]`, plus a `[[test]]` for each test that must run alone.
//! Cargo would silently skip a new `tests/*.rs` file that neither lists, so this checks none is.
//! A `#[path]` line counts as built even inside a `#[cfg]`-gated block of `it.rs`.

use std::path::Path;

/// Fail when any workspace package has a test file Cargo would not build.
pub fn check(root: &Path) -> Result<(), String> {
    let mut packages = vec![root.join("xtask")];
    for group in ["crates", "apps"] {
        for entry in std::fs::read_dir(root.join(group)).map_err(|e| format!("{group}: {e}"))? {
            let path = entry.map_err(|e| format!("{group}: {e}"))?.path();
            if path.join("Cargo.toml").is_file() {
                packages.push(path);
            }
        }
    }
    let mut missing = Vec::new();
    for package in &packages {
        missing.extend(unbuilt(package)?);
    }
    if missing.is_empty() {
        println!("OK: every integration test file is built.");
        Ok(())
    } else {
        Err(format!("Cargo never builds {}: add each to its crate's tests/it.rs, or as a [[test]] in Cargo.toml", missing.join(", ")))
    }
}

/// Every `tests/*.rs` and `tests/*/main.rs` of an `autotests = false` package that no `[[test]]`
/// target and no `#[path]` line of `tests/it.rs` builds, as `package/tests/file` paths.
pub fn unbuilt(package: &Path) -> Result<Vec<String>, String> {
    let manifest = std::fs::read_to_string(package.join("Cargo.toml")).map_err(|e| format!("{}: {e}", package.display()))?;
    let manifest: toml::Table = toml::from_str(&manifest).map_err(|e| format!("{}/Cargo.toml: {e}", package.display()))?;
    let autotests = manifest.get("package").and_then(|p| p.get("autotests")).and_then(toml::Value::as_bool).unwrap_or(true);
    let tests = package.join("tests");
    if autotests || !tests.is_dir() {
        return Ok(Vec::new());
    }
    let targets: Vec<String> = manifest
        .get("test")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|t| match (t.get("path").and_then(toml::Value::as_str), t.get("name").and_then(toml::Value::as_str)) {
            (Some(path), _) => vec![path.replace('\\', "/").trim_start_matches("./").to_owned()],
            // Without a path, Cargo infers one from the name.
            (None, Some(name)) => vec![format!("tests/{name}.rs"), format!("tests/{name}/main.rs")],
            (None, None) => Vec::new(),
        })
        .collect();
    let it = std::fs::read_to_string(tests.join("it.rs")).unwrap_or_default();
    let included: Vec<&str> = it.lines().filter_map(|l| l.trim().strip_prefix("#[path = \"")?.strip_suffix("\"]")).collect();
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(&tests).map_err(|e| format!("{}: {e}", tests.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", tests.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let file = if entry.path().is_dir() {
            if !entry.path().join("main.rs").is_file() {
                continue;
            }
            format!("{name}/main.rs")
        } else if name.ends_with(".rs") {
            name
        } else {
            continue;
        };
        if !targets.contains(&format!("tests/{file}")) && !included.contains(&file.as_str()) {
            missing.push(format!("{}/tests/{file}", package.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
        }
    }
    missing.sort();
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_integration_test_file_is_built() {
        check(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()).unwrap();
    }

    #[test]
    fn an_unlisted_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("demo");
        std::fs::create_dir_all(pkg.join("tests/solo")).unwrap();
        std::fs::write(pkg.join("Cargo.toml"), "[package]\nname = \"demo\"\nautotests = false\n\n[[test]]\nname = \"it\"\npath = \"tests/it.rs\"\n\n[[test]]\nname = \"alone\"\npath = \"tests/alone.rs\"\n\n[[test]]\nname = \"bare\"\n\n[[test]]\nname = \"dotted\"\npath = \"./tests/dotted.rs\"\n").unwrap();
        std::fs::write(pkg.join("tests/it.rs"), "#[path = \"a.rs\"]\nmod a;\n").unwrap();
        for f in ["a.rs", "alone.rs", "bare.rs", "dotted.rs", "new.rs", "solo/main.rs", "notes.txt"] {
            std::fs::write(pkg.join("tests").join(f), "").unwrap();
        }
        assert_eq!(unbuilt(&pkg).unwrap(), ["demo/tests/new.rs", "demo/tests/solo/main.rs"]);
        std::fs::write(pkg.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        assert!(unbuilt(&pkg).unwrap().is_empty(), "autotests builds every file itself");
    }
}
