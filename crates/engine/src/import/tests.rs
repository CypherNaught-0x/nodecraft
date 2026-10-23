use super::*;
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{ProjectId, Value};
use serde_json::json;

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

fn video_bytes(duration: u32) -> Vec<u8> {
    let boxed = |kind: &[u8; 4], body: &[u8]| [u32::try_from(body.len() + 8).unwrap().to_be_bytes().as_slice(), kind, body].concat();
    let mut header = vec![0; 100];
    header[12..16].copy_from_slice(&1000u32.to_be_bytes());
    header[16..20].copy_from_slice(&duration.to_be_bytes());
    [boxed(b"ftyp", b"isom\0\0\x02\0isom"), boxed(b"moov", &boxed(b"mvhd", &header))].concat()
}

#[test]
fn probed_movie_metadata_is_bound_to_the_input_and_only_fills_unknown_fields() {
    let bytes = video_bytes(0);
    let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(&bytes).as_bytes());
    let proof = ProbedMovie { hash, meta: MediaMeta { duration_ms: Some(500), width: Some(64), height: Some(48), ..Default::default() } };
    let mut s = session();
    let files = [Incoming { name: "Fragmented", bytes: &bytes }, Incoming { name: "Notes", bytes: b"Public notes" }];
    let before = s.document().clone();
    import_files_with_movies(&mut s, C, AT, 300.0, &files, &ids(2), std::slice::from_ref(&proof)).unwrap();
    let Some(Value::Media(asset)) = node(&s, ids(1)[0]).params.get("asset") else { panic!("video") };
    assert_eq!((asset.meta.duration_ms, asset.meta.width, asset.meta.height), (Some(500), Some(64), Some(48)));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before, "probe enrichment stays in the same atomic import undo step");

    let known = video_bytes(1200);
    let known_hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(&known).as_bytes());
    let another = ProbedMovie { hash: known_hash, ..proof.clone() };
    let imported = import_media_with_movie(&mut s, &Incoming { name: "Known", bytes: &known }, Some(&another)).unwrap();
    assert_eq!(imported.meta.duration_ms, Some(1200), "a native probe cannot overwrite known header facts");
    let unknown = import_media_with_movie(&mut s, &Incoming { name: "Fragmented", bytes: &bytes }, Some(&another)).unwrap();
    assert_eq!(unknown.meta.duration_ms, None, "another video's metadata cannot be borrowed");
    let image = png();
    let image_proof = ProbedMovie { hash: nodecraft_types::Blake3::from_bytes(*blake3::hash(&image).as_bytes()), ..proof };
    let imported = import_media_with_movie(&mut s, &Incoming { name: "Image", bytes: &image }, Some(&image_proof)).unwrap();
    assert_eq!(imported.meta.duration_ms, None, "only video fields are enriched");
}

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
    // What the header proves travels with the asset (#393).
    let facts = (asset.meta.has_alpha, asset.meta.bits_per_channel, asset.meta.color_space);
    assert_eq!(facts, (Some(true), Some(8), Some(nodecraft_types::ColorSpace::Untagged)));
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
    let bom = b"\xEF\xBB\xBFplain\x0Cpage two".to_vec();
    let files = [Incoming { name: "le.txt", bytes: &le }, Incoming { name: "be.txt", bytes: &be }, Incoming { name: "bom.txt", bytes: &bom }];
    import_files(&mut s, C, AT, 300.0, &files, &ids(3)).unwrap();
    let texts: Vec<_> = ids(3).into_iter().map(|id| node(&s, id).params.get("text").cloned()).collect();
    assert_eq!(texts, [Some(Value::Text("Hé".into())), Some(Value::Text("Ünï".into())), Some(Value::Text("plain\x0Cpage two".into()))]);
}

#[test]
fn files_it_cant_hold_are_skipped_with_a_reason_and_the_rest_still_arrive() {
    let mut s = session();
    let huge_text = "x".repeat(MAX_DROP_TEXT_BYTES + 1);
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

#[test]
fn a_drops_text_shares_one_budget_and_media_is_only_stored_for_added_nodes() {
    let mut s = session();
    let big = "a".repeat(600 * 1024);
    let image = png();
    let files =
        [Incoming { name: "one.txt", bytes: big.as_bytes() }, Incoming { name: "pic.png", bytes: &image }, Incoming { name: "two.txt", bytes: big.as_bytes() }];
    let imported = import_files(&mut s, C, AT, 300.0, &files, &ids(3)).unwrap();
    assert_eq!(imported.nodes, [ids(3)[0], ids(3)[1]], "the paste fits: one big text and the picture");
    assert_eq!(imported.skipped, [("two.txt".to_owned(), "a drop adds at most 960 KiB of text".to_owned())]);
    assert_eq!(s.assets().len(), 1);
    // Undo, then redo: the picture's bytes are still there for it.
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("edit.redo", &json!({})).unwrap();
    let Some(Value::Media(asset)) = node(&s, ids(3)[1]).params.get("asset").cloned() else { panic!("an asset") };
    assert!(s.assets().get(&asset.hash).is_some());
}

#[test]
fn camera_formats_are_kept_without_a_size_and_bad_text_is_refused() {
    let mut s = session();
    // An ISO-BMFF header with the HEIC brand: storable, not decodable here yet.
    let mut heic = vec![0, 0, 0, 24];
    heic.extend(b"ftypheic\0\0\0\0mif1heic");
    heic.extend([0u8; 64]);
    let odd_utf16 = [0xFF, 0xFE, b'h', 0, b'i'];
    let mut late_binary = "ok ".repeat(600).into_bytes();
    late_binary.push(0);
    let files =
        [Incoming { name: "photo.heic", bytes: &heic }, Incoming { name: "odd.txt", bytes: &odd_utf16 }, Incoming { name: "late.txt", bytes: &late_binary }];
    let imported = import_files(&mut s, C, AT, 300.0, &files, &ids(3)).unwrap();
    let skipped: Vec<_> = imported.skipped.iter().map(|(name, why)| (name.as_str(), why.as_str())).collect();
    assert_eq!(skipped, [("odd.txt", "the UTF-16 text ends in half a character"), ("late.txt", "the file has binary data in it, so it isn't text")]);
    let photo = node(&s, ids(3)[0]);
    assert_eq!(photo.kind, "source.image");
    let Some(Value::Media(asset)) = photo.params.get("asset") else { panic!("an asset") };
    assert_eq!((asset.mime.essence(), asset.meta.width), ("image/heic", None));
    assert_eq!(asset.meta, MediaMeta::default(), "nothing is claimed about an image this build can't read");
}

#[test]
fn media_that_doesnt_fit_the_store_is_skipped_before_anything_is_stored() {
    let mut s = session();
    let chunk = |n: u8| {
        let mut bytes = png();
        bytes.extend(vec![n; MAX_MEDIA_BYTES - bytes.len() - 1]);
        bytes
    };
    let (a, b, c, d) = (chunk(1), chunk(2), chunk(3), chunk(4));
    let files = [
        Incoming { name: "a.png", bytes: &a },
        Incoming { name: "b.png", bytes: &b },
        Incoming { name: "c.png", bytes: &c },
        Incoming { name: "d.png", bytes: &d },
    ];
    let imported = import_files(&mut s, C, AT, 300.0, &files, &ids(4)).unwrap();
    assert_eq!(imported.nodes.len(), 3, "48 MiB holds three 16 MiB files");
    assert_eq!(imported.skipped, [("d.png".to_owned(), "the project's media is full (48 MiB)".to_owned())]);
    assert_eq!(s.assets().len(), 3);
}

#[test]
fn text_right_at_the_budget_still_fits_one_paste() {
    let mut s = session();
    let text = "b".repeat(MAX_DROP_TEXT_BYTES);
    let imported = import_files(&mut s, C, AT, 300.0, &[Incoming { name: "full.txt", bytes: text.as_bytes() }], &ids(1)).unwrap();
    assert_eq!(imported.nodes, ids(1), "{:?}", imported.skipped);
}

#[test]
fn hdr394_import_skips_hdr_without_assets_edits_or_text_fallback() {
    for bytes in [b"\x76\x2f\x31\x01".as_slice(), b"#?RGBE\n", b"#?RADIANCE\n"] {
        let mut s = session();
        let before = s.document().clone();
        let revision = s.revision();
        let got = import_files(&mut s, C, AT, 300.0, &[Incoming { name: "sample.dat", bytes }], &ids(1)).unwrap();
        assert!(got.nodes.is_empty());
        assert_eq!(got.notice().unwrap(), "sample.dat wasn't added: HDR images can't be added yet; convert to PNG or JPEG first.");
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
        assert!(s.assets().is_empty());
        assert!(!s.can_undo());
    }
    let mut s = session();
    let before = s.document().clone();
    let got = import_files(
        &mut s,
        C,
        AT,
        300.0,
        &[Incoming { name: "empty", bytes: b"" }, Incoming { name: "sample.hdr", bytes: b"#?RGBE\n" }, Incoming { name: "ok.txt", bytes: b"kept" }],
        &ids(3),
    )
    .unwrap();
    assert_eq!(got.nodes, [ids(3)[2]]);
    assert_eq!(got.skipped[0].1, "the file is empty");
    assert!(got.skipped[1].1.contains("HDR"));
    assert!(s.assets().is_empty());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
}

#[test]
fn import_media_stores_one_file_for_a_later_edit_and_refuses_text() {
    let mut s = session();
    let image = png();
    let asset = import_media(&mut s, &Incoming { name: "photo.png", bytes: &image }).unwrap();
    assert_eq!((asset.mime.essence(), asset.meta.width, asset.meta.height), ("image/png", Some(3), Some(2)));
    s.validate_asset(&asset).unwrap();
    assert!(s.document().canvases().get(&C).unwrap().nodes().is_empty(), "it adds no node");
    let text = import_media(&mut s, &Incoming { name: "notes.txt", bytes: b"A forest at dawn" }).unwrap_err();
    assert!(text.message.contains("notes.txt") && text.message.contains("text"), "{}", text.message);
    assert_eq!(import_media(&mut s, &Incoming { name: "empty", bytes: b"" }).unwrap_err().kind, nodecraft_types::ErrorKind::InvalidInput);
}

#[test]
fn models_become_3d_nodes_and_unreadable_ones_are_refused_with_a_reason() {
    let mut s = session();
    let obj = b"v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nf 1 2 4 3\n".to_vec();
    let mesh = nodecraft_media::decode_mesh(&obj, &MeshLimits::default()).unwrap();
    let glb = nodecraft_media::encode_glb(&mesh).unwrap();
    let external = br#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":36,"uri":"model.bin"}],"bufferViews":[{"buffer":0,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}],"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],"nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}]}"#;
    let files = [Incoming { name: "quad.obj", bytes: &obj }, Incoming { name: "quad.glb", bytes: &glb }, Incoming { name: "split.gltf", bytes: external }];
    let imported = import_files(&mut s, C, AT, 300.0, &files, &ids(3)).unwrap();
    assert_eq!(imported.nodes, ids(2));
    for (id, mime) in ids(2).into_iter().zip(["model/obj", "model/gltf-binary"]) {
        let model = node(&s, id);
        assert_eq!(model.kind, "source.mesh");
        let Some(Value::Media(asset)) = model.params.get("asset") else { panic!("an asset parameter") };
        assert_eq!((asset.mime.essence(), asset.meta.vertices, asset.meta.triangles), (mime, Some(4), Some(2)));
    }
    let [(name, why)] = imported.skipped.as_slice() else { panic!("one skipped: {:?}", imported.skipped) };
    assert_eq!(name, "split.gltf");
    assert!(why.contains("3D model can't be read") && why.contains(".glb"), "{why}");
}

#[test]
fn pdfs_become_document_nodes_with_their_page_count_and_damaged_ones_are_refused() {
    let mut s = session();
    let pdf = nodecraft_media::pdf_fixture::pdf(&["", "", ""]);
    let damaged = b"%PDF-1.7\nnot really".to_vec();
    let empty = nodecraft_media::pdf_fixture::pdf(&[]);
    let files = [Incoming { name: "paper.pdf", bytes: &pdf }, Incoming { name: "damaged.pdf", bytes: &damaged }, Incoming { name: "empty.pdf", bytes: &empty }];
    let imported = import_files(&mut s, C, AT, 300.0, &files, &ids(3)).unwrap();
    assert_eq!(imported.nodes, ids(1));
    let document = node(&s, ids(1)[0]);
    assert_eq!(document.kind, nodecraft_nodes::document::SOURCE);
    let Some(Value::Media(asset)) = document.params.get("asset") else { panic!("an asset parameter") };
    assert_eq!((asset.mime.essence(), asset.meta.page_count), ("application/pdf", Some(3)));
    let [(name, why), (empty_name, empty_why)] = imported.skipped.as_slice() else { panic!("two skipped: {:?}", imported.skipped) };
    assert_eq!(name, "damaged.pdf");
    assert!(why.contains("PDF can't be read"), "{why}");
    assert_eq!((empty_name.as_str(), empty_why.as_str()), ("empty.pdf", "the PDF has no pages"));
}
