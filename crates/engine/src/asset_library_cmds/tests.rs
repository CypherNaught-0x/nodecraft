//! My library through the session's commands: saving from one project, placing in another.
use crate::Session;
use crate::asset_library::{MAX_ENTRIES, MemoryAssetLibrary, install};
use crate::library_browse_cmds::tests::{ALPHA, BETA, PHOTO, SOUND, png, session};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{Blake3, CanvasId, ErrorKind, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::rc::Rc;

const OTHER: CanvasId = CanvasId::from_bytes([7; 16]);

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([0xB0 + n; 16])
}

fn hash_of(s: &Session, canvas: CanvasId, node: NodeId) -> Blake3 {
    match s.document().canvases()[&canvas].nodes()[&node].params.get("asset") {
        Some(Value::Media(asset)) => asset.hash,
        other => panic!("no asset: {other:?}"),
    }
}

/// The browse fixture ("Browse": a photo, two painted images, a sound) with My library in `store`.
fn saving(store: &MemoryAssetLibrary) -> Session {
    let mut s = session();
    install(&mut s, Rc::new(store.clone()));
    s
}

/// An empty project, "Elsewhere", with one canvas and My library in `store`.
fn elsewhere(store: &MemoryAssetLibrary) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([5; 16]), "Elsewhere").unwrap();
    Change::AddCanvas { canvas: Canvas::new(OTHER, "Other") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    install(&mut s, Rc::new(store.clone()));
    s
}

fn painted() -> Blake3 {
    Blake3::from_bytes(*blake3::hash(&png(200)).as_bytes())
}

fn listed(s: &mut Session, params: Json) -> Json {
    let revision = s.revision();
    let listed = s.dispatch("library.saved", &params).unwrap();
    assert_eq!(s.revision(), revision, "listing never edits the project");
    listed
}

#[test]
fn saved_files_are_named_marked_with_their_project_and_placed_in_another_project_in_one_undo_step() {
    let store = MemoryAssetLibrary::default();
    let mut s = saving(&store);
    let (photo, sound) = (hash_of(&s, ALPHA, PHOTO), hash_of(&s, BETA, SOUND));
    let before = s.document().clone();
    let saved = s.dispatch("library.save_assets", &json!({"items": [{"hash": photo}, {"hash": painted()}, {"hash": sound}, {"hash": photo}]})).unwrap();
    assert_eq!(saved["saved"].as_array().unwrap().len(), 3, "a hash named twice is saved once");
    assert!(saved["saved"].as_array().unwrap().iter().all(|item| item["state"] == "saved"), "{saved:#}");
    assert_eq!(s.document(), &before, "saving never changes the project");
    assert_eq!(store.media_count(), 3);

    let again = s.dispatch("library.save_assets", &json!({"items": [{"hash": photo}]})).unwrap();
    assert_eq!(again["saved"][0]["state"], "already");

    let mut other = elsewhere(&store);
    let page = listed(&mut other, json!({}));
    assert_eq!(page["total"], 3);
    let names: Vec<&Json> = page["assets"].as_array().unwrap().iter().map(|a| &a["name"]).collect();
    assert_eq!(names, [&json!("Sound"), &json!("Painter · 1/2"), &json!("Holiday photo")], "newest first");
    assert!(page["assets"].as_array().unwrap().iter().all(|a| a["origin"] == "Browse"));
    let text = page.to_string();
    for private in ["Alpha", "Beta", "canvas", "node"] {
        assert!(!text.contains(private), "no canvas or card reaches My library: {private}");
    }
    assert_eq!(listed(&mut other, json!({"kind": "audio"}))["total"], 1);
    assert_eq!(listed(&mut other, json!({"query": "holiday"}))["total"], 1);
    assert_eq!(listed(&mut other, json!({"query": "browse"}))["total"], 3, "the origin is searched");

    let empty = other.document().clone();
    let placed = other
        .dispatch(
            "library.place_saved",
            &json!({"canvas": OTHER, "items": [{"hash": photo}, {"hash": sound}], "ids": [id(1), id(2)], "at": {"x": 10, "y": 20}, "step": 300}),
        )
        .unwrap();
    assert_eq!(placed["placed"][0]["origin"], "Browse");
    let canvas = &other.document().canvases()[&OTHER];
    assert_eq!(canvas.nodes()[&id(1)].kind, "source.image");
    assert_eq!(canvas.nodes()[&id(1)].title, "Holiday photo");
    assert_eq!((canvas.nodes()[&id(2)].pos.x, canvas.nodes()[&id(2)].pos.y), (310.0, 20.0));
    assert!(other.assets().contains(&photo) && other.assets().contains(&sound), "the bytes were copied into the project");
    other.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(other.document(), &empty, "one undo step");
}

#[test]
fn removing_entries_deletes_files_no_entry_holds_and_placed_cards_keep_theirs() {
    let store = MemoryAssetLibrary::default();
    let mut s = saving(&store);
    let photo = hash_of(&s, ALPHA, PHOTO);
    s.dispatch("library.save_assets", &json!({"items": [{"hash": photo}, {"hash": painted()}]})).unwrap();
    let mut other = elsewhere(&store);
    other.dispatch("library.place_saved", &json!({"canvas": OTHER, "items": [{"hash": photo}], "ids": [id(1)], "at": {"x": 0, "y": 0}})).unwrap();
    let removed = s.dispatch("library.unsave_assets", &json!({"hashes": [photo]})).unwrap();
    assert_eq!(removed["removed"], 1);
    assert_eq!(store.media_count(), 1);
    assert_eq!(listed(&mut other, json!({}))["total"], 1, "another window sees it once it lists");
    assert!(other.assets().contains(&photo), "the placed card keeps its copy");
    let refused = other.dispatch("library.place_saved", &json!({"canvas": OTHER, "items": [{"hash": photo}], "ids": [id(2)], "at": {"x": 0, "y": 0}}));
    assert_eq!(refused.unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn two_windows_saving_in_turn_keep_both_saves() {
    let store = MemoryAssetLibrary::default();
    let mut first = saving(&store);
    let mut second = saving(&store);
    let photo = hash_of(&first, ALPHA, PHOTO);
    first.dispatch("library.save_assets", &json!({"items": [{"hash": photo}]})).unwrap();
    // The second window never listed; its change reads the index again under the lock.
    second.dispatch("library.save_assets", &json!({"items": [{"hash": painted()}]})).unwrap();
    assert_eq!(listed(&mut first, json!({}))["total"], 2);
}

#[test]
fn a_full_library_refuses_new_files_and_evicts_nothing() {
    let store = MemoryAssetLibrary::default();
    // 64 entries of 16 MiB fill the 1 GiB.
    let big: Vec<Json> = (0..64u32)
        .map(|n| {
            let hash = Blake3::from_bytes(*blake3::hash(&n.to_le_bytes()).as_bytes());
            json!({"asset": {"hash": hash, "mime": "image/png"}, "name": format!("Big {n}"), "bytes": 16 * 1024 * 1024})
        })
        .collect();
    store.write_index(&serde_json::to_vec(&json!({"version": 1, "assets": big})).unwrap());
    let mut s = saving(&store);
    let photo = hash_of(&s, ALPHA, PHOTO);
    let saved = s.dispatch("library.save_assets", &json!({"items": [{"hash": photo}]})).unwrap();
    assert_eq!(saved["saved"][0]["state"], "failed");
    assert!(saved["saved"][0]["reason"].as_str().unwrap().contains("full (1 GiB)"));
    assert_eq!(listed(&mut s, json!({"limit": 1}))["entries"], 64, "nothing was evicted");

    let crowded: Vec<Json> = (0..MAX_ENTRIES as u32)
        .map(|n| {
            let hash = Blake3::from_bytes(*blake3::hash(&n.to_le_bytes()).as_bytes());
            json!({"asset": {"hash": hash, "mime": "image/png"}, "name": "Tiny", "bytes": 1})
        })
        .collect();
    store.write_index(&serde_json::to_vec(&json!({"version": 1, "assets": crowded})).unwrap());
    let saved = s.dispatch("library.save_assets", &json!({"items": [{"hash": photo}]})).unwrap();
    assert!(saved["saved"][0]["reason"].as_str().unwrap().contains("4096 entries"));
}

#[test]
fn an_entry_whose_file_went_missing_or_bad_changes_nothing() {
    let store = MemoryAssetLibrary::default();
    let mut s = saving(&store);
    let photo = hash_of(&s, ALPHA, PHOTO);
    s.dispatch("library.save_assets", &json!({"items": [{"hash": photo}]})).unwrap();
    use crate::asset_library::AssetLibraryStore;
    store.put_media(&photo, b"bit rot").unwrap();
    let mut other = elsewhere(&store);
    let before = other.document().clone();
    let error =
        other.dispatch("library.place_saved", &json!({"canvas": OTHER, "items": [{"hash": photo}], "ids": [id(1)], "at": {"x": 0, "y": 0}})).unwrap_err();
    assert!(error.message.contains("damaged"), "{}", error.message);
    assert_eq!(other.document(), &before);
    assert!(!other.assets().contains(&photo));
}

#[test]
fn requests_are_checked_before_anything_happens() {
    let store = MemoryAssetLibrary::default();
    let mut s = saving(&store);
    let photo = hash_of(&s, ALPHA, PHOTO);
    for (command, params) in [
        ("library.save_assets", json!({"items": []})),
        ("library.save_assets", json!({"items": vec![json!({"hash": photo}); 33]})),
        ("library.save_assets", json!({"items": [{"hash": "00".repeat(32)}]})),
        ("library.save_assets", json!({"items": [{"hash": photo, "path": "/home/me"}]})),
        ("library.unsave_assets", json!({"hashes": []})),
        ("library.saved", json!({"kind": "style"})),
        ("library.saved", json!({"limit": 0})),
        ("library.saved", json!({"query": "q".repeat(257)})),
        ("library.place_saved", json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [], "at": {"x": 0, "y": 0}})),
        ("library.place_saved", json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [PHOTO], "at": {"x": 0, "y": 0}})),
        ("library.place_saved", json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [id(1)], "at": {"x": 0, "y": 0}, "step": -1})),
        ("library.place_saved", json!({"canvas": OTHER, "items": [{"hash": photo}], "ids": [id(1)], "at": {"x": 0, "y": 0}})),
        ("library.place_saved", json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [id(1)], "at": {"x": 0, "y": 0}})),
    ] {
        assert!(s.dispatch(command, &params).is_err(), "{command} {params}");
    }
    assert_eq!(store.media_count(), 0);

    // A host without My library says so.
    let mut bare = session();
    assert_eq!(bare.dispatch("library.saved", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    assert!(bare.enabled("library.save_assets", &json!({"items": [{"hash": photo}]})).is_err());
}

#[test]
fn a_file_whose_entry_names_another_type_is_never_placed() {
    use crate::asset_library::AssetLibraryStore;
    let store = MemoryAssetLibrary::default();
    let image = png(77);
    let hash = Blake3::from_bytes(*blake3::hash(&image).as_bytes());
    store.put_media(&hash, &image).unwrap();
    let entry = json!({"asset": {"hash": hash, "mime": "application/pdf"}, "name": "Report", "bytes": image.len()});
    store.write_index(&serde_json::to_vec(&json!({"version": 1, "assets": [entry]})).unwrap());
    let mut other = elsewhere(&store);
    let before = other.document().clone();
    let error =
        other.dispatch("library.place_saved", &json!({"canvas": OTHER, "items": [{"hash": hash}], "ids": [id(1)], "at": {"x": 0, "y": 0}})).unwrap_err();
    assert!(error.message.contains("isn't the type"), "{}", error.message);
    assert_eq!(other.document(), &before);
    assert!(!other.assets().contains(&hash));
}

#[test]
fn a_damaged_index_is_listed_as_such_and_repaired_by_a_person() {
    let store = MemoryAssetLibrary::default();
    let mut s = saving(&store);
    let photo = hash_of(&s, ALPHA, PHOTO);
    s.dispatch("library.save_assets", &json!({"items": [{"hash": photo}]})).unwrap();
    store.write_index(b"not json");
    let page = listed(&mut s, json!({}));
    assert!(page.get("index_damaged").is_some());
    assert_eq!(page["total"], 0);
    assert!(s.dispatch("library.save_assets", &json!({"items": [{"hash": painted()}]})).is_err(), "changes wait for the repair");
    let repaired = s.dispatch("library.repair_saved", &json!({})).unwrap();
    assert_eq!((repaired["repaired"].clone(), repaired["entries"].clone()), (json!(true), json!(1)));
    assert_eq!(listed(&mut s, json!({}))["assets"][0]["name"], "Recovered image");
    assert_eq!(s.dispatch("library.repair_saved", &json!({})).unwrap()["repaired"], false);
}
