use super::*;

fn png(shade: u8) -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[shade; 16], &Default::default()).unwrap()
}

fn hash(bytes: &[u8]) -> Blake3 {
    Blake3::from_bytes(*blake3::hash(bytes).as_bytes())
}

fn entry(n: u8, bytes: u64) -> Entry {
    Entry {
        asset: AssetRef { hash: hash(&[n]), mime: "image/png".parse().unwrap(), meta: Default::default() },
        name: format!("Photo {n}"),
        origin: Some("Holiday".into()),
        bytes,
    }
}

#[test]
fn entries_are_checked() {
    assert!(entry(1, 10).checked().is_ok());
    for bad in [
        Entry { name: " ".into(), ..entry(1, 10) },
        Entry { name: "x".repeat(81), ..entry(1, 10) },
        Entry { name: "line\nbreak".into(), ..entry(1, 10) },
        Entry { origin: Some("o".repeat(81)), ..entry(1, 10) },
        Entry { bytes: 0, ..entry(1, 10) },
        Entry { bytes: MAX_ASSET_BYTES as u64 + 1, ..entry(1, 10) },
        Entry { asset: AssetRef { mime: "text/plain".parse().unwrap(), ..entry(1, 10).asset }, ..entry(1, 10) },
        Entry { asset: AssetRef { mime: nodecraft_media::style::MIME.parse().unwrap(), ..entry(1, 10).asset }, ..entry(1, 10) },
    ] {
        assert_eq!(bad.checked().unwrap_err().kind, ErrorKind::InvalidInput);
    }
}

#[test]
fn the_index_round_trips_and_keeps_entries_it_cant_use() {
    let index = Index { entries: vec![entry(1, 10), entry(2, 20)], damaged: Vec::new() };
    let read = Index::from_bytes(&index.to_bytes().unwrap()).unwrap();
    assert_eq!(read, index);
    assert_eq!(read.used_bytes(), 30);

    let newer = json!({"asset": entry(3, 1).asset, "name": "From a newer build", "bytes": 1, "rating": 5});
    let twice = serde_json::to_value(entry(1, 10)).unwrap();
    let file = json!({"version": 1, "assets": [serde_json::to_value(entry(1, 10)).unwrap(), newer, twice, 7]});
    let read = Index::from_bytes(&serde_json::to_vec(&file).unwrap()).unwrap();
    assert_eq!(read.entries(), [entry(1, 10)]);
    assert_eq!(read.damaged.len(), 3);
    assert!(read.media().contains(&entry(3, 1).asset.hash), "a damaged entry's file is kept");
    let again = Index::from_bytes(&read.to_bytes().unwrap()).unwrap();
    assert_eq!(again.damaged.len(), 3, "nothing is lost by writing it back");

    assert!(Index::from_bytes(b"{").is_err());
    assert!(Index::from_bytes(br#"{"version": 1, "assets": [], "extra": 1}"#).is_err());
    assert!(Index::from_bytes(br#"{"version": 0}"#).is_err());
    assert_eq!(Index::from_bytes(br#"{"version": 2}"#).unwrap_err().kind, ErrorKind::Unsupported);
    assert!(Index::from_bytes(&vec![b' '; MAX_INDEX_BYTES + 1]).is_err());
    let crowded = json!({"version": 1, "assets": vec![0; MAX_ENTRIES + 1]});
    assert!(Index::from_bytes(&serde_json::to_vec(&crowded).unwrap()).is_err());
}

#[test]
fn files_are_checked_against_their_hash() {
    let store = MemoryAssetLibrary::default();
    let shelf = AssetShelf::new(Rc::new(store.clone()));
    let bytes = png(1);
    let id = hash(&bytes);
    assert_eq!(shelf.media(&id).unwrap_err().kind, ErrorKind::NotFound);
    store.put_media(&id, b"tampered").unwrap();
    assert_eq!(shelf.media(&id).unwrap_err().kind, ErrorKind::InvalidInput);
    store.put_media(&id, &bytes).unwrap();
    assert_eq!(shelf.media(&id).unwrap(), bytes);
}

fn list(shelf: &AssetShelf) -> Json {
    handle(shelf, &Default::default(), Request::List { offset: 0, limit: 50, kind: None, query: None }).unwrap()
}

const DAMAGED: &[u8] = b"{\"version\": 1, \"assets\": [tru";

#[test]
fn a_damaged_index_is_only_reported_until_a_person_repairs_it() {
    let store = MemoryAssetLibrary::default();
    store.write_index(DAMAGED);
    let shelf = AssetShelf::new(Rc::new(store.clone()));
    assert!(shelf.with(|_| Ok(())).unwrap_err().message.contains("Repair"));
    let listed = list(&shelf);
    assert!(listed["index_damaged"].as_str().unwrap().contains("damaged"));
    assert_eq!((listed["total"].clone(), listed["file"].clone()), (json!(0), json!("library.json")), "only the file's name, never its folder");
    assert!(store.aside().is_empty(), "listing never writes");
    assert_eq!(store.index().as_deref(), Some(DAMAGED));
    let refused = handle(&shelf, &Default::default(), Request::Remove { hashes: vec![hash(b"x")] }).unwrap_err();
    assert!(refused.message.contains("Repair"), "{}", refused.message);
    assert_eq!(store.index().as_deref(), Some(DAMAGED), "a change waits for the repair");
}

#[test]
fn a_repair_rebuilds_from_matching_files_and_keeps_every_file_it_cant_use() {
    let store = MemoryAssetLibrary::default();
    let good = png(1);
    store.put_media(&hash(&good), &good).unwrap();
    store.put_media(&hash(&png(2)), b"bit rot").unwrap();
    // Matches its hash, but isn't media this build can use: kept, not lost.
    store.put_media(&hash(b"notes"), b"notes").unwrap();
    store.write_index(DAMAGED);
    let shelf = AssetShelf::new(Rc::new(store.clone()));

    let repaired = handle(&shelf, &Default::default(), Request::Repair).unwrap();
    assert_eq!(repaired["repaired"], true);
    assert_eq!((repaired["entries"].clone(), repaired["kept"].clone()), (json!(1), json!(1)));
    assert_eq!(repaired["kept_as"], "library.damaged.json");
    assert_eq!(store.aside(), [DAMAGED.to_vec()], "the damaged index is kept for the person");
    let listed = list(&shelf);
    assert_eq!(listed["assets"][0]["name"], "Recovered image");
    assert_eq!(listed["assets"][0]["hash"], json!(hash(&good)));
    assert_eq!(listed["damaged"].as_array().unwrap().len(), 1);

    // A later change tidies only what no entry, usable or not, holds.
    handle(&shelf, &Default::default(), Request::Remove { hashes: vec![hash(b"absent")] }).unwrap();
    assert!(store.media(&hash(b"notes")).unwrap().is_some(), "the unusable file survives tidying");
    assert!(store.media(&hash(&good)).unwrap().is_some());
    assert!(store.media(&hash(&png(2))).unwrap().is_none(), "a file that no longer matches its hash goes");

    assert_eq!(handle(&shelf, &Default::default(), Request::Repair).unwrap()["repaired"], false, "a good index needs nothing");
    store.write_index(b"garbage");
    assert_eq!(handle(&shelf, &Default::default(), Request::Repair).unwrap()["kept_as"], "library.damaged-2.json", "never over an earlier copy");
    assert_eq!(store.aside().len(), 2);
}

#[test]
fn placing_checks_the_bytes_are_the_type_the_entry_says() {
    let store = MemoryAssetLibrary::default();
    let image = png(4);
    store.put_media(&hash(&image), &image).unwrap();
    let as_pdf = json!({"asset": {"hash": hash(&image), "mime": "application/pdf"}, "name": "Not a PDF", "bytes": image.len()});
    store.write_index(&serde_json::to_vec(&json!({"version": 1, "assets": [as_pdf]})).unwrap());
    let shelf = AssetShelf::new(Rc::new(store.clone()));
    assert!(shelf.media(&hash(&image)).is_ok(), "the bytes match their hash");
    assert!(shelf.media_as_listed(&hash(&image)).unwrap_err().message.contains("isn't the type"));
}

#[test]
fn a_newer_index_is_reported_and_left_alone() {
    let store = MemoryAssetLibrary::default();
    store.write_index(br#"{"version": 9, "assets": []}"#);
    let shelf = AssetShelf::new(Rc::new(store.clone()));
    let error = handle(&shelf, &Default::default(), Request::List { offset: 0, limit: 50, kind: None, query: None }).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let error = handle(&shelf, &Default::default(), Request::Remove { hashes: vec![hash(b"x")] }).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(handle(&shelf, &Default::default(), Request::Repair).unwrap_err().kind, ErrorKind::Unsupported, "never repaired");
    assert_eq!(store.index().as_deref(), Some(&br#"{"version": 9, "assets": []}"#[..]));
    assert!(store.aside().is_empty());
    assert!(!error.message.contains('/'), "no folder in the message");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_file_store_round_trips_locks_and_leaves_strangers_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileAssetLibrary::new(dir.path().join("assets"));
    assert_eq!(store.load().unwrap(), None);
    assert!(store.media_kept().unwrap().is_empty());
    let bytes = png(3);
    store.put_media(&hash(&bytes), &bytes).unwrap();
    store.save(b"{}").unwrap();
    std::fs::write(dir.path().join("assets/media/readme.txt"), b"not ours").unwrap();
    assert_eq!(store.load().unwrap().as_deref(), Some(&b"{}"[..]));
    assert_eq!(store.media_kept().unwrap(), [hash(&bytes)]);
    assert_eq!(store.media(&hash(&bytes)).unwrap(), Some(bytes.clone()));
    assert_eq!(store.set_aside(b"bad").unwrap(), "library.damaged.json");
    assert_eq!(store.set_aside(b"worse").unwrap(), "library.damaged-2.json");
    assert_eq!(std::fs::read(dir.path().join("assets/library.damaged.json")).unwrap(), b"bad", "the first copy is kept");

    let held = store.lock().unwrap();
    let other = FileAssetLibrary::new(dir.path().join("assets"));
    assert_eq!(other.lock().unwrap_err().kind, ErrorKind::Busy, "a second window waits its turn");
    let shelf = AssetShelf::new(Rc::new(FileAssetLibrary::new(dir.path().join("assets"))));
    let busy = handle(&shelf, &Default::default(), Request::Remove { hashes: vec![hash(b"x")] }).unwrap_err();
    assert_eq!(busy.kind, ErrorKind::Busy, "a change from another handle waits too");
    drop(held);
    assert!(other.lock().is_ok());

    store.drop_media(&hash(&bytes)).unwrap();
    store.drop_media(&hash(&bytes)).unwrap();
    assert!(dir.path().join("assets/media/readme.txt").exists());
}

#[cfg(unix)]
#[test]
fn the_file_store_never_reads_through_a_link_or_a_special_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("assets")).unwrap();
    std::fs::write(dir.path().join("secret"), b"{\"version\":1}").unwrap();
    std::os::unix::fs::symlink(dir.path().join("secret"), dir.path().join("assets/library.json")).unwrap();
    let store = FileAssetLibrary::new(dir.path().join("assets"));
    assert!(store.load().is_err());
}
