use super::*;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::ProjectId;

const C: CanvasId = CanvasId::from_bytes([1; 16]);

fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Import").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Session::new(project).unwrap()
}

fn ids(n: u8) -> Vec<NodeId> {
    (1..=n).map(|i| NodeId::from_bytes([i; 16])).collect()
}

fn png() -> Vec<u8> {
    nodecraft_media::encode_png(3, 2, &[200; 3 * 2 * 4], &Limits::default()).unwrap()
}

fn node(s: &Session, id: NodeId) -> &Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap()
}

const AT: Position = Position { x: 10.0, y: 20.0 };

#[test]
fn text_and_images_become_source_nodes_in_a_row_as_one_undo_step() {
    let mut s = session();
    let before = s.document().clone();
    let image = png();
    let files = [Incoming { name: "notes.txt", bytes: "A forest at dawn".as_bytes() }, Incoming { name: "photo.png", bytes: &image }];
    let imported = import_files(&mut s, C, AT, 300.0, &files, &ids(2)).unwrap();
    assert_eq!(imported, Imported { nodes: ids(2), skipped: vec![] });
    let text = node(&s, ids(2)[0]);
    assert_eq!((text.kind.as_str(), text.params.get("text")), ("source.text", Some(&Value::Text("A forest at dawn".into()))));
    assert_eq!((text.pos.x, text.pos.y), (10.0, 20.0));
    let picture = node(&s, ids(2)[1]);
    assert_eq!(picture.kind, "source.image");
    assert_eq!(picture.pos.x, 310.0);
    let Some(Value::Media(asset)) = picture.params.get("asset") else { panic!("an asset parameter") };
    assert_eq!((asset.mime.essence(), asset.meta.width, asset.meta.height), ("image/png", Some(3), Some(2)));
    assert!(s.assets().get(&asset.hash).is_some(), "the bytes are in the project's asset store");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before, "one undo removes them all");
}

#[test]
fn utf16_and_bom_text_is_decoded() {
    let mut s = session();
    let mut le = vec![0xFF, 0xFE];
    le.extend("Hé".encode_utf16().flat_map(u16::to_le_bytes));
    let mut be = vec![0xFE, 0xFF];
    be.extend("Ünï".encode_utf16().flat_map(u16::to_be_bytes));
    let bom = b"\xEF\xBB\xBFplain".to_vec();
    let files = [Incoming { name: "le.txt", bytes: &le }, Incoming { name: "be.txt", bytes: &be }, Incoming { name: "bom.txt", bytes: &bom }];
    import_files(&mut s, C, AT, 300.0, &files, &ids(3)).unwrap();
    let texts: Vec<_> = ids(3).into_iter().map(|id| node(&s, id).params.get("text").cloned()).collect();
    assert_eq!(texts, [Some(Value::Text("Hé".into())), Some(Value::Text("Ünï".into())), Some(Value::Text("plain".into()))]);
}

#[test]
fn files_it_cant_hold_are_skipped_with_a_reason_and_the_rest_still_arrive() {
    let mut s = session();
    let huge_text = "x".repeat(MAX_TEXT_BYTES + 1);
    let broken_png = b"\x89PNG\r\n\x1a\nnot really".to_vec();
    let pdf = b"%PDF-1.7\n".to_vec();
    let files = [
        Incoming { name: "empty", bytes: b"" },
        Incoming { name: "blob.bin", bytes: &[0, 1, 2, 3, 0xFF, 0] },
        Incoming { name: "huge.txt", bytes: huge_text.as_bytes() },
        Incoming { name: "broken.png", bytes: &broken_png },
        Incoming { name: "paper.pdf", bytes: &pdf },
        Incoming { name: "ok.txt", bytes: b"kept" },
    ];
    let imported = import_files(&mut s, C, AT, 300.0, &files, &ids(6)).unwrap();
    assert_eq!(imported.nodes, [ids(6)[5]]);
    assert_eq!(node(&s, ids(6)[5]).pos.x, 10.0, "kept files close up the row");
    let skipped: Vec<_> = imported.skipped.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(skipped, ["empty", "blob.bin", "huge.txt", "broken.png", "paper.pdf"]);
    assert_eq!(imported.notice().unwrap(), "empty wasn't added: the file is empty (and 4 more files weren't).");
    assert!(s.assets().iter().next().is_none(), "nothing half-imported for the skipped media");
}

#[test]
fn nothing_addable_changes_nothing_and_too_many_files_are_capped() {
    let mut s = session();
    let before = s.document().clone();
    let only_bad = import_files(&mut s, C, AT, 300.0, &[Incoming { name: "x", bytes: b"" }], &ids(1)).unwrap();
    assert!(only_bad.nodes.is_empty());
    assert_eq!(s.document(), &before);
    assert!(!s.can_undo(), "no empty undo step");
    let many: Vec<_> = (0..MAX_FILES + 3).map(|_| Incoming { name: "t", bytes: b"t" }).collect();
    let ids: Vec<NodeId> =
        (0..MAX_FILES as u32 + 3).map(|i| NodeId::from_bytes([i.to_le_bytes(), [7; 4], [7; 4], [7; 4]].concat().try_into().unwrap())).collect();
    let imported = import_files(&mut s, C, AT, 300.0, &many, &ids).unwrap();
    assert_eq!((imported.nodes.len(), imported.skipped.len()), (MAX_FILES, 3));
    assert!(import_files(&mut s, C, AT, 300.0, &many, &ids[..2]).is_err(), "an id per file");
}
