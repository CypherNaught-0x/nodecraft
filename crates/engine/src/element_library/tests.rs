use super::*;

fn asset(n: u8) -> AssetRef {
    AssetRef { hash: Blake3::from_bytes(*blake3::hash(&[n]).as_bytes()), mime: "image/png".parse().unwrap(), meta: Default::default() }
}

fn saved(name: &str) -> Saved {
    Saved { name: name.into(), category: "character".into(), guidelines: "red scarf".into(), images: vec![asset(1)], sheet: Some(asset(2)), shared: false }
}

#[test]
fn entries_are_checked() {
    assert!(saved("Hero").checked().is_ok());
    for bad in [
        Saved { name: "no spaces".into(), ..saved("x") },
        Saved { category: "nope".into(), ..saved("Hero") },
        Saved { guidelines: "x".repeat(MAX_GUIDELINES + 1), ..saved("Hero") },
        Saved { images: (0..9).map(asset).collect(), ..saved("Hero") },
        Saved { images: Vec::new(), sheet: None, ..saved("Hero") },
        Saved { images: vec![AssetRef { mime: "text/plain".parse().unwrap(), ..asset(1) }], ..saved("Hero") },
    ] {
        assert_eq!(bad.checked().unwrap_err().kind, ErrorKind::InvalidInput);
    }
}

#[test]
fn the_library_file_round_trips_and_hostile_files_are_refused() {
    let mut elements = Elements::default();
    elements.entries.insert(key("Hero"), saved("Hero"));
    let bytes = elements.to_bytes().unwrap();
    let read = Elements::from_bytes(&bytes).unwrap();
    assert_eq!(read, elements);
    assert_eq!(read.get("hERO"), Some(&saved("Hero")));

    assert!(Elements::from_bytes(b"{").is_err());
    assert!(Elements::from_bytes(br#"{"version": 1, "elements": [], "keys": "x"}"#).is_err(), "unknown fields are refused");
    assert_eq!(Elements::from_bytes(br#"{"version": 2}"#).unwrap_err().kind, ErrorKind::Unsupported);
    let twice = serde_json::to_vec(&json!({"version": 1, "elements": [saved("Hero"), saved("hero")]})).unwrap();
    assert!(Elements::from_bytes(&twice).is_err(), "one entry per name");
    let bad = serde_json::to_vec(&json!({"version": 1, "elements": [Saved { name: String::new(), ..saved("x") }]})).unwrap();
    assert!(Elements::from_bytes(&bad).is_err());
    assert!(Elements::from_bytes(&vec![b' '; MAX_ELEMENTS_BYTES + 1]).is_err());
}

#[test]
fn library_media_is_checked_against_its_hash() {
    let store = MemoryElements::default();
    let shelf = ElementShelf::new(Rc::new(store.clone()));
    let hash = asset(1).hash;
    assert_eq!(shelf.media(&hash).unwrap_err().kind, ErrorKind::NotFound);
    store.put_media(&hash, &[9]).unwrap();
    assert_eq!(shelf.media(&hash).unwrap_err().kind, ErrorKind::InvalidInput, "damaged media is refused");
    store.put_media(&hash, &[1]).unwrap();
    assert_eq!(shelf.media(&hash).unwrap(), vec![1]);
}

#[test]
fn a_damaged_library_is_reported_not_replaced() {
    let store = MemoryElements::default();
    store.save(b"not json").unwrap();
    let shelf = ElementShelf::new(Rc::new(store.clone()));
    assert!(shelf.with(|_| Ok(())).is_err());
    let assets = nodecraft_format::AssetStore::default();
    assert!(handle(&shelf, &assets, Request::Remove { name: "Hero".into() }).is_err());
    assert_eq!(store.load().unwrap().unwrap(), b"not json", "the damaged file is kept for the person to recover");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_file_store_keeps_entries_and_media_and_locks() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileElements::new(dir.path().join("library"));
    assert_eq!(store.load().unwrap(), None);
    assert!(store.media_kept().unwrap().is_empty());
    store.save(b"{}").unwrap();
    assert_eq!(store.load().unwrap().unwrap(), b"{}");
    let hash = asset(1).hash;
    store.put_media(&hash, &[1]).unwrap();
    std::fs::write(dir.path().join("library/element-media/notes.txt"), b"mine").unwrap();
    assert_eq!(store.media_kept().unwrap(), vec![hash], "files not named by a hash are left alone");
    assert_eq!(store.media(&hash).unwrap().unwrap(), vec![1]);
    store.drop_media(&hash).unwrap();
    store.drop_media(&hash).unwrap();
    assert_eq!(store.media(&hash).unwrap(), None);

    let held = store.lock().unwrap();
    assert_eq!(FileElements::new(dir.path().join("library")).lock().err().map(|e| e.kind), Some(ErrorKind::Busy));
    drop(held);
    assert!(store.lock().is_ok());
}
