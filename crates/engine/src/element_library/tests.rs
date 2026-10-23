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
    assert!(Elements::from_bytes(br#"{"version": 1, "elements": 3}"#).is_err());
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

#[test]
fn entries_that_cant_be_used_are_set_aside_reported_and_kept() {
    let newer = json!({"name": "Villain", "category": "character", "guidelines": "", "images": [asset(3)], "shared": false, "voice": "low"});
    let file = json!({"version": 1, "elements": [saved("Hero"), saved("hero"), Saved { name: String::new(), ..saved("x") }, newer, 7]});
    let bytes = serde_json::to_vec(&file).unwrap();
    let read = Elements::from_bytes(&bytes).unwrap();
    assert_eq!(read.entries().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Hero"]);
    let damaged: Vec<_> = read.damaged().iter().map(|d| (d.index, d.name.clone())).collect();
    assert_eq!(damaged, [(1, Some("hero".into())), (2, Some(String::new())), (3, Some("Villain".into())), (4, None)]);
    assert!(read.damaged()[0].reason.contains("name"));

    // Changing the library keeps the entries it couldn't use, as they were, and their images.
    let store = MemoryElements::default();
    store.save(&bytes).unwrap();
    store.put_media(&asset(3).hash, &[3]).unwrap();
    let shelf = ElementShelf::new(Rc::new(store.clone()));
    let assets = nodecraft_format::AssetStore::default();
    let listed = handle(&shelf, &assets, Request::List).unwrap();
    assert_eq!(listed["damaged"].as_array().unwrap().len(), 4);
    assert_eq!(listed["damaged"][2]["name"], "Villain");
    handle(&shelf, &assets, Request::Share { name: "Hero".into(), shared: true }).unwrap();
    let written: Json = serde_json::from_slice(&store.load().unwrap().unwrap()).unwrap();
    let elements = written["elements"].as_array().unwrap();
    assert_eq!(elements.len(), 5);
    assert_eq!(elements[0]["shared"], true);
    assert_eq!(elements[3], file["elements"][3], "kept as it was");
    assert_eq!(store.media_kept().unwrap(), vec![asset(3).hash], "a damaged entry's images stay");
    assert_eq!(handle(&shelf, &assets, Request::Remove { name: "Villain".into() }).unwrap_err().kind, ErrorKind::NotFound);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_library_file_that_cant_be_read_names_itself() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileElements::new(dir.path().join("library"));
    store.save(b"not json").unwrap();
    let error = read(&store).unwrap_err();
    assert!(error.message.contains("elements.json"), "{}", error.message);
}

fn bundle_of(entry: &Saved, media: &[(Blake3, Vec<u8>)]) -> Vec<u8> {
    crate::element_bundle::encode(entry, |hash| {
        media.iter().find(|(h, _)| h == hash).map(|(_, b)| b.clone()).ok_or_else(|| Error::new(ErrorKind::NotFound, "no"))
    })
    .unwrap()
}

#[test]
fn a_bundle_round_trips_private_with_its_images_checked() {
    let one = (asset(1).hash, vec![1]);
    let two = (asset(2).hash, vec![2]);
    let entry = Saved { shared: true, images: vec![asset(1), asset(2)], sheet: Some(asset(2)), ..saved("Hero") };
    let bytes = bundle_of(&entry, &[one.clone(), two.clone()]);
    let (read, media) = crate::element_bundle::decode(&bytes).unwrap();
    assert_eq!(read, Saved { shared: false, ..entry.clone() }, "the receiver decides whether to share it");
    assert_eq!(media, BTreeMap::from([one.clone(), two.clone()]), "the sheet goes in once");

    // A bundle whose image doesn't match its hash, or misses one, is refused.
    let wrong = bundle_of(&entry, &[one.clone(), (two.0, vec![9])]);
    assert_eq!(crate::element_bundle::decode(&wrong).unwrap_err().kind, ErrorKind::InvalidInput);
    assert!(crate::element_bundle::decode(b"PK not a zip").is_err());
    assert!(crate::element_bundle::decode(&vec![0; crate::element_bundle::MAX_BUNDLE_BYTES + 1]).is_err());
}

#[test]
fn hostile_bundles_are_refused() {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let zip = |files: &[(&str, Vec<u8>)]| {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    };
    let entry = |format: &str, version: u32| {
        serde_json::to_vec(&json!({"format": format, "version": version, "element": Saved { sheet: None, ..saved("Hero") }})).unwrap()
    };
    let image = (format!("media/{}", asset(1).hash), vec![1]);
    let good = entry("nodecraft.element", 1);
    let cases: Vec<(Vec<(&str, Vec<u8>)>, ErrorKind)> = vec![
        (vec![("element.json", good.clone()), (image.0.as_str(), image.1.clone())], ErrorKind::InvalidInput),
        (vec![("element.json", entry("nodecraft.element", 2)), (image.0.as_str(), image.1.clone())], ErrorKind::Unsupported),
        (vec![("element.json", entry("other", 1)), (image.0.as_str(), image.1.clone())], ErrorKind::InvalidInput),
        (vec![("element.json", good.clone())], ErrorKind::InvalidInput),
        (vec![("element.json", good.clone()), (image.0.as_str(), image.1.clone()), ("../evil", vec![1])], ErrorKind::InvalidInput),
        (vec![(image.0.as_str(), image.1.clone())], ErrorKind::InvalidInput),
    ];
    // The first is the good one.
    for (index, (files, kind)) in cases.into_iter().enumerate() {
        let decoded = crate::element_bundle::decode(&zip(&files));
        if index == 0 {
            assert!(decoded.is_ok(), "{decoded:?}");
        } else {
            assert_eq!(decoded.unwrap_err().kind, kind, "case {index}");
        }
    }
    let crowded: Vec<(String, Vec<u8>)> = (0..=MAX_IMAGES + 2).map(|n| (format!("media/{n}"), vec![])).collect();
    let crowded: Vec<(&str, Vec<u8>)> = crowded.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    assert_eq!(crate::element_bundle::decode(&zip(&crowded)).unwrap_err().kind, ErrorKind::InvalidInput);
}
