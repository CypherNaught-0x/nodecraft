//! `cargo xtask catalog-check`: every file in `assets/catalog` parses, validates, merges without
//! duplicate ids and is embedded in the build (`BUILTIN`), and nothing embedded is missing.
use nodecraft_catalog::{BUILTIN, Catalog};
use std::path::Path;

#[test]
fn catalog_files_are_valid_and_embedded() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/catalog");
    let mut files: Vec<_> =
        std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.extension().is_some_and(|ext| ext == "toml")).collect();
    files.sort();
    let mut all = Catalog::default();
    for path in &files {
        let name = path.file_name().unwrap().to_str().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        let catalog = Catalog::from_toml(&text).unwrap_or_else(|error| panic!("{name}: {}", error.message));
        println!("  {name}: {} models", catalog.len());
        all.merge(catalog).unwrap_or_else(|error| panic!("{name}: {}", error.message));
        let embedded = BUILTIN.iter().find(|(built, _)| *built == name);
        assert!(embedded.is_some(), "{name} is not embedded; add it to `BUILTIN` in crates/catalog/src/lib.rs");
        assert_eq!(embedded.map(|(_, built)| *built), Some(text.as_str()), "{name}");
    }
    for (built, _) in BUILTIN {
        assert!(files.iter().any(|path| path.file_name().and_then(|n| n.to_str()) == Some(built)), "`BUILTIN` lists {built}, which is not in assets/catalog");
    }
    assert_eq!(Catalog::builtin().unwrap().len(), all.len());
    println!("catalog-check: ok ({} models in {} files)", all.len(), files.len());
}
