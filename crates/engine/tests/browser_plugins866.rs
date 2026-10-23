//! The browser's plugin store with the example packages (#866). The browser build registers a
//! session-only store over `MemoryBackend` (`ui-egui`'s `App::register_session_plugins`, wasm only);
//! this builds the same service the same way, natively, so it runs in CI. The repository has no
//! wasm-bindgen-test runner, so the wasm binary itself isn't exercised here; `cargo xtask wasm`
//! only checks that it builds.
//!
//! The examples are packed from their bytes as compiled into the test, as the browser has no
//! folder to read; natively the test also checks it packs every file the folders hold.
use nodecraft_engine::Session;
use nodecraft_engine::commands::Services;
use nodecraft_engine::plugin_cmds::{PickedPackage, PluginHost, host_ids, register_session_plugins};
use nodecraft_plugin_store::{MemoryBackend, Store, author};
use nodecraft_types::ErrorKind;
use serde_json::json;

const PALETTE: &[(&str, &[u8])] = &[
    ("LICENSE", include_bytes!("../../../examples/plugins/palette/LICENSE")),
    ("data/warm.json", include_bytes!("../../../examples/plugins/palette/data/warm.json")),
    ("manifest.json", include_bytes!("../../../examples/plugins/palette/manifest.json")),
];

const SHOUT: &[(&str, &[u8])] = &[
    ("LICENSE", include_bytes!("../../../examples/plugins/shout/LICENSE")),
    ("actions/shout.json", include_bytes!("../../../examples/plugins/shout/actions/shout.json")),
    ("manifest.json", include_bytes!("../../../examples/plugins/shout/manifest.json")),
];

fn packed(files: &[(&str, &[u8])]) -> Vec<u8> {
    author::pack(files.iter().map(|(p, b)| ((*p).to_owned(), b.to_vec())).collect(), None).unwrap().bytes
}

/// The browser's service, as the wasm app registers it: memory, no trust file, NodeCraft's IDs taken.
fn browser() -> (Session, PickedPackage) {
    let mut services = Services::default();
    let store = Store::open(MemoryBackend::default()).unwrap();
    let picked = register_session_plugins(&mut services, store, PluginHost { trust: None, taken: host_ids().unwrap(), bindings: Default::default() }).unwrap();
    let session =
        Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([8; 16]), "Browser").unwrap()).unwrap().with_services(services);
    (session, picked)
}

#[test]
fn the_compiled_in_examples_are_every_file_in_their_folders() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins");
    for (name, files) in [("palette", PALETTE), ("shout", SHOUT)] {
        let mut on_disk = Vec::new();
        let mut folders = vec![root.join(name)];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    let relative = path.strip_prefix(root.join(name)).unwrap().to_str().unwrap().replace('\\', "/");
                    on_disk.push((relative, std::fs::read(&path).unwrap()));
                }
            }
        }
        on_disk.sort();
        let compiled: Vec<(String, Vec<u8>)> = files.iter().map(|(p, b)| ((*p).to_owned(), b.to_vec())).collect();
        assert_eq!(on_disk, compiled, "{name}: add new example files to this test");
    }
}

#[test]
fn the_examples_import_into_the_browser_store_and_only_data_is_enabled() {
    let (mut s, picked) = browser();
    for (name, files) in [("palette.ncraft-plugin", PALETTE), ("shout.ncraft-plugin", SHOUT)] {
        let location = picked.put(name, packed(files)).unwrap();
        let dry = s.dispatch("plugins.import", &json!({"location": location, "dry_run": true})).unwrap();
        assert_eq!((dry["provenance"].clone(), dry["license"].clone()), (json!("unsigned"), json!("CC0-1.0")));
        let report = s.dispatch("plugins.import", &json!({"location": location, "digest": dry["digest"]})).unwrap();
        assert_eq!((report["digest"].clone(), report["session_only"].clone()), (dry["digest"].clone(), json!(true)));
    }
    let list = s.dispatch("plugins.list", &json!({})).unwrap();
    assert_eq!(list["session_only"], true);
    let packages = list["packages"].as_array().unwrap();
    let ids: Vec<&str> = packages.iter().filter_map(|p| p["package_id"].as_str()).collect();
    assert_eq!(ids, ["io.example.palette", "io.example.shout"]);
    assert!(
        packages.iter().all(|p| p["reloadable"] == false && p["reload_file"].is_null() && p["state"] == "disabled"),
        "a pick has no file to reload: {packages:?}"
    );

    // The data package is enabled; its palette binding isn't one the browser offers, so it stays unbound.
    let enabled = s.dispatch("plugins.enable", &json!({"package_id": "io.example.palette"})).unwrap();
    assert_eq!(enabled["unbound"], json!(["plugin.io.example.palette.warm"]));
    // The text action package installs but never runs here: no grant, no enable.
    let refused = s.dispatch("plugins.enable", &json!({"package_id": "io.example.shout"})).unwrap_err();
    assert!(refused.kind == ErrorKind::Unsupported && refused.message.contains("can't run in the browser"), "{refused:?}");
    assert_eq!(s.dispatch("plugins.grant", &json!({"package_id": "io.example.shout"})).unwrap_err().kind, ErrorKind::Unsupported);
    // Reload needs a file to read again; the browser let the pick go once it was installed.
    let reload = s.dispatch("plugins.reload", &json!({"package_id": "io.example.palette"})).unwrap_err();
    assert!(reload.kind == ErrorKind::Unsupported && reload.message.contains("pick it"), "{reload:?}");
    let states: Vec<String> =
        s.dispatch("plugins.list", &json!({})).unwrap()["packages"].as_array().unwrap().iter().map(|p| p["state"].as_str().unwrap().to_owned()).collect();
    assert_eq!(states, ["enabled", "disabled"]);
}
