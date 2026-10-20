use super::*;
use crate::{Session, commands::Registry};
use nodecraft_graph::{Project, TechniquePort};
use nodecraft_types::{CanvasId, NodeId, PortType, ProjectId};

fn version(technique: u8, number: u32) -> Canvas {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let node = kinds.get("source.text").unwrap().instantiate_instance(NodeId::from_bytes([technique; 16]), Default::default(), &Default::default()).unwrap();
    let info = Technique {
        id: TechniqueId::from_bytes([technique; 16]),
        version: number,
        name: "Say".into(),
        description: String::new(),
        category: String::new(),
        tags: Vec::new(),
        inputs: Vec::new(),
        outputs: vec![TechniquePort {
            name: "Out".into(),
            description: String::new(),
            node: node.id,
            port: "out".into(),
            port_type: PortType { modality: nodecraft_types::Modality::Text, many: false },
        }],
        params: Vec::new(),
        source: None,
        retired: false,
    };
    Canvas::technique(CanvasId::from_bytes([technique; 16]), "Say v1", info, vec![node], Vec::new()).unwrap()
}

fn request(op: &str, canvas: &Canvas) -> Json {
    json!({"op": op, "canvas": serde_json::to_string(canvas).unwrap()})
}

#[test]
fn the_library_file_round_trips_and_hostile_files_are_refused() {
    let mut library = Library::default();
    assert!(library.insert(version(1, 1)));
    assert!(library.insert(version(1, 2)));
    assert!(!library.insert(version(1, 2)), "one version once");
    let bytes = library.to_bytes().unwrap();
    assert_eq!(Library::from_bytes(&bytes).unwrap(), library);
    assert!(Library::from_bytes(b"{").is_err());
    assert!(Library::from_bytes(br#"{"version":2,"techniques":[]}"#).is_err());
    assert!(Library::from_bytes(br#"{"version":1,"techniques":[],"extra":1}"#).is_err());
    assert!(Library::from_bytes(&vec![b' '; MAX_LIBRARY_BYTES + 1]).is_err());
    let twice = json!({"version": 1, "techniques": [version(1, 1), version(1, 1)]});
    assert!(Library::from_bytes(&serde_json::to_vec(&twice).unwrap()).is_err());
    let plain = json!({"version": 1, "techniques": [Canvas::new(CanvasId::from_bytes([3; 16]), "Plain")]});
    assert!(Library::from_bytes(&serde_json::to_vec(&plain).unwrap()).is_err());
    let retired = json!({"version": 1, "techniques": [version(1, 1).retired_technique().unwrap()]});
    assert!(Library::from_bytes(&serde_json::to_vec(&retired).unwrap()).is_err());
}

#[test]
fn the_service_reads_before_each_change_and_shelves_what_it_read() {
    let store = MemoryLibrary::default();
    let shelf = Shelf(Rc::new(RefCell::new(Ok(Library::default()))));
    assert_eq!(handle(&store, &shelf, &request("add", &version(1, 1))).unwrap()["added"], json!(true));
    // Another process adds a version: the next change keeps it.
    let mut elsewhere = store.saved().unwrap();
    elsewhere.insert(version(2, 1));
    store.save(&elsewhere.to_bytes().unwrap()).unwrap();
    handle(&store, &shelf, &request("add", &version(1, 2))).unwrap();
    let saved = store.saved().unwrap();
    assert_eq!(saved.versions().count(), 3);
    assert_eq!(shelf.with(|library| Ok(library.versions().count())).unwrap(), 3);
    // Listing reads it again too.
    store.save(&Library::default().to_bytes().unwrap()).unwrap();
    assert_eq!(handle(&store, &shelf, &json!({"op": "list"})).unwrap()["techniques"], json!([]));
    assert_eq!(shelf.with(|library| Ok(library.versions().count())).unwrap(), 0);
    // Malformed requests and versions are refused.
    for bad in [
        json!({"op": "drop"}),
        json!({"op": "add", "canvas": "{"}),
        json!({"op": "list", "extra": 1}),
        request("add", &Canvas::new(CanvasId::from_bytes([3; 16]), "Plain")),
    ] {
        assert!(handle(&store, &shelf, &bad).is_err(), "{bad}");
    }
}

#[test]
fn a_damaged_library_is_reported_and_left_as_it_is() {
    let store = MemoryLibrary::default();
    store.save(b"not json").unwrap();
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut s = Session::with_registries(Project::new(ProjectId::from_bytes([1; 16]), "P").unwrap(), Registry::builtins().unwrap(), kinds).unwrap();
    install(&mut s, Box::new(store.clone())).unwrap();
    let error = s.dispatch("library.list", &json!({})).unwrap_err();
    assert!(error.message.contains("damaged"), "{}", error.message);
    assert!(s.dispatch("library.remove", &json!({"technique": TechniqueId::from_bytes([1; 16]), "version": 1})).is_err());
    // Nothing overwrote it.
    assert_eq!(store.load().unwrap().as_deref(), Some(&b"not json"[..]));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_file_library_survives_and_one_change_at_a_time_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileLibrary::new(dir.path().join("library"));
    assert_eq!(store.load().unwrap(), None);
    let shelf = Shelf(Rc::new(RefCell::new(Ok(Library::default()))));
    handle(&store, &shelf, &request("add", &version(1, 1))).unwrap();
    let again = FileLibrary::new(dir.path().join("library"));
    assert_eq!(read(&again).unwrap().versions().count(), 1);
    let held = store.lock().unwrap();
    assert_eq!(handle(&again, &shelf, &request("add", &version(1, 2))).unwrap_err().kind, ErrorKind::Busy);
    drop(held);
    handle(&again, &shelf, &request("add", &version(1, 2))).unwrap();
    assert_eq!(read(&store).unwrap().versions().count(), 2);
}
