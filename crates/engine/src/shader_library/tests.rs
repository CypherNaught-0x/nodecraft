use super::*;
use crate::library::MemoryLibrary;
use nodecraft_nodes::shader::StoredParam;

fn shader(source: &str) -> StoredShader {
    StoredShader { source: source.into(), params: vec![StoredParam { name: "amount".into(), label: "Amount".into(), min: 0.0, max: 1.0, default: 0.5 }] }
}

fn ask(store: &dyn LibraryStore, shelf: &ShaderShelf, request: Json) -> Result<Json, Error> {
    handle(store, shelf, &request)
}

fn shelf() -> ShaderShelf {
    ShaderShelf(Rc::new(RefCell::new(Ok(ShaderLibrary::default()))))
}

#[test]
fn saving_numbers_versions_and_skips_an_unchanged_shader() {
    let (store, shelf) = (MemoryLibrary::default(), shelf());
    let save = |source: &str, name: &str| ask(&store, &shelf, json!({"op": "save", "name": name, "shader": shader(source)})).unwrap();
    assert_eq!(save("a", "Glow"), json!({"name": "Glow", "version": 1, "saved": true}));
    assert_eq!(save("a", "glow"), json!({"name": "Glow", "version": 1, "saved": false}), "same name ignoring case, same shader");
    assert_eq!(save("b", "GLOW"), json!({"name": "Glow", "version": 2, "saved": true}));
    // Removing the latest never frees its number.
    ask(&store, &shelf, json!({"op": "remove", "name": "Glow", "version": 2})).unwrap();
    assert_eq!(save("c", "Glow"), json!({"name": "Glow", "version": 3, "saved": true}));
    let listed = ask(&store, &shelf, json!({"op": "list"})).unwrap();
    assert_eq!(listed, json!({"shaders": [{"name": "Glow", "versions": [1, 3], "latest": 3}]}));
    shelf
        .with(|library| {
            assert_eq!(library.get("glow").and_then(|e| e.get(3)).map(|v| v.shader.source.clone()), Some("c".into()));
            Ok(())
        })
        .unwrap();
    // Removing the last version, or the name, removes the shader.
    ask(&store, &shelf, json!({"op": "remove", "name": "glow"})).unwrap();
    assert_eq!(ask(&store, &shelf, json!({"op": "list"})).unwrap(), json!({"shaders": []}));
    assert_eq!(ask(&store, &shelf, json!({"op": "remove", "name": "glow"})).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn names_and_sizes_are_bounded() {
    let (store, shelf) = (MemoryLibrary::default(), shelf());
    for name in ["", " Glow", "Glow\n", "a\u{7}b", &"x".repeat(41)] {
        let error = ask(&store, &shelf, json!({"op": "save", "name": name, "shader": shader("a")})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{name:?}");
    }
    let huge = shader(&"x".repeat(nodecraft_gpu::shader::MAX_SOURCE + 1));
    assert_eq!(ask(&store, &shelf, json!({"op": "save", "name": "Big", "shader": huge})).unwrap_err().kind, ErrorKind::InvalidInput);
    let mut library = ShaderLibrary::default();
    for n in 0..MAX_VERSIONS {
        library.save("Many", shader(&n.to_string())).unwrap();
    }
    assert_eq!(library.save("Many", shader("one more")).unwrap_err().kind, ErrorKind::InvalidInput);
    assert!(ask(&store, &shelf, json!({"op": "nope"})).is_err());
}

#[test]
fn a_hostile_file_is_refused_not_trusted() {
    let entry = |name: &str, next: u32, versions: Json| json!({"version": 1, "shaders": [{"name": name, "next": next, "versions": versions}]});
    let one = |version: u32| json!({"version": version, "shader": shader("a")});
    for file in [
        json!("not a library"),
        json!({"version": 2, "shaders": []}),
        entry("", 2, json!([one(1)])),
        entry("Glow", 2, json!([])),
        entry("Glow", 1, json!([one(1)])),
        entry("Glow", 9, json!([one(3), one(2)])),
        json!({"version": 1, "shaders": [{"name": "Glow", "next": 2, "versions": [one(1)]}, {"name": "GLOW", "next": 2, "versions": [one(1)]}]}),
        entry("Glow", 9, json!([{"version": 1, "shader": shader(&"x".repeat(nodecraft_gpu::shader::MAX_SOURCE + 1))}])),
    ] {
        let bytes = serde_json::to_vec(&file).unwrap();
        assert!(ShaderLibrary::from_bytes(&bytes).is_err(), "{file}");
        let store = MemoryLibrary::default();
        store.save(&bytes).unwrap();
        let shelf = shelf();
        // Every request says why, and nothing overwrites the file.
        assert!(ask(&store, &shelf, json!({"op": "save", "name": "New", "shader": shader("a")})).is_err());
        assert_eq!(store.load().unwrap(), Some(bytes));
        assert!(shelf.with(|_| Ok(())).is_err());
    }
    let good = ShaderLibrary::from_bytes(&serde_json::to_vec(&entry("Glow", 9, json!([one(2), one(5)]))).unwrap()).unwrap();
    assert_eq!(ShaderLibrary::from_bytes(&good.to_bytes().unwrap()).unwrap(), good);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_file_store_keeps_shaders_json_in_its_own_folder() {
    let dir = std::env::temp_dir().join(format!("nodecraft-shaders-{}", std::process::id()));
    let store = file_store(dir.clone());
    let shelf = shelf();
    ask(&store, &shelf, json!({"op": "save", "name": "Glow", "shader": shader("a")})).unwrap();
    let read = ShaderLibrary::from_bytes(&std::fs::read(dir.join("shaders.json")).unwrap()).unwrap();
    assert_eq!(read.get("Glow").map(|e| e.versions.len()), Some(1));
    std::fs::remove_dir_all(dir).unwrap();
}
