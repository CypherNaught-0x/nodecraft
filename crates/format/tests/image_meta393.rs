//! Image header facts on references (#393): a project written before them opens and re-saves
//! unchanged, one carrying them round-trips, and a claim its bytes contradict is refused on save
//! and on open.
use nodecraft_format::{AssetStore, Snapshot, decode, encode, image_header_meta};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, CanvasId, ColorSpace, MediaMeta, NodeId, ProjectId, Value};

fn snapshot(meta: impl FnOnce(&[u8]) -> MediaMeta) -> Snapshot {
    let png = nodecraft_media::encode_png(3, 2, &[7; 3 * 2 * 4], &nodecraft_media::Limits::default()).unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(&png).unwrap();
    let canvas = CanvasId::from_bytes([2; 16]);
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Image facts").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut node = Node::new(NodeId::from_bytes([3; 16]), "source.image");
    node.config.insert("asset".into(), Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: meta(&png) }));
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    Snapshot { project, assets, epoch: 1 }
}

fn reference(snapshot: &Snapshot) -> AssetRef {
    let node = snapshot.project.canvases().values().flat_map(|canvas| canvas.nodes().values()).next().unwrap();
    match node.config.get("asset") {
        Some(Value::Media(asset)) => asset.clone(),
        other => panic!("{other:?}"),
    }
}

fn manifest(archive: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).unwrap();
    std::io::read_to_string(zip.by_name("project.json").unwrap()).unwrap()
}

/// The manifest as written before #393, by hand rather than by today's encoder, so a change to
/// the encoder can't make this test agree with itself.
const OLD_MANIFEST: &str = r#"{"version":3,"epoch":1,"project":{"id":"01010101-0101-0101-0101-010101010101","title":"Image facts","canvases":{"02020202-0202-0202-0202-020202020202":{"id":"02020202-0202-0202-0202-020202020202","title":"Canvas","nodes":{"03030303-0303-0303-0303-030303030303":{"id":"03030303-0303-0303-0303-030303030303","kind":"source.image","title":"","pos":{"x":0.0,"y":0.0},"params":{},"config":{"asset":{"type":"media","value":{"hash":"e02160bd72d87e140e8e7fabdcd0b86d34d27329979ed3048af6e1679aaa7124","mime":"image/png","meta":{"width":3,"height":2}}}},"inputs":[],"outputs":[],"history":[],"active":null,"tag":null,"field":null}},"edges":{},"fields":{},"comments":{},"tags":{}}}}}"#;

#[test]
fn a_reference_written_before_the_new_facts_opens_and_saves_unchanged() {
    let old = snapshot(|_| MediaMeta { width: Some(3), height: Some(2), ..Default::default() });
    let bytes = rewrite_manifest(&encode(&old).unwrap(), OLD_MANIFEST);
    let opened = decode(&bytes).unwrap();
    let meta = reference(&opened).meta;
    assert_eq!((meta.width, meta.height, meta.has_alpha, meta.bits_per_channel, meta.color_space), (Some(3), Some(2), None, None, None));
    assert_eq!(manifest(&encode(&opened).unwrap()), OLD_MANIFEST);
}

#[test]
fn the_facts_round_trip_and_a_contradicted_claim_is_refused_on_save_and_open() {
    let full = snapshot(|png| image_header_meta(&nodecraft_media::probe_image(png, &nodecraft_media::Limits::default()).unwrap()));
    let meta = reference(&full).meta;
    assert_eq!((meta.has_alpha, meta.bits_per_channel, meta.color_space), (Some(true), Some(8), Some(ColorSpace::Untagged)));
    let bytes = encode(&full).unwrap();
    assert_eq!(reference(&decode(&bytes).unwrap()), reference(&full));

    let wrong = snapshot(|_| MediaMeta { bits_per_channel: Some(16), ..Default::default() });
    assert!(encode(&wrong).unwrap_err().message.contains("image format does not match"));
    // A file edited outside NodeCraft to claim a 16-bit image is refused when it opens.
    let tampered = manifest(&bytes).replace("\"bits_per_channel\":8", "\"bits_per_channel\":16");
    assert!(tampered.contains("\"bits_per_channel\":16"));
    let archive = rewrite_manifest(&bytes, &tampered);
    assert!(decode(&archive).unwrap_err().message.contains("image format does not match"));
}

/// The archive with its manifest replaced and every other entry copied as is.
fn rewrite_manifest(archive: &[u8], manifest: &str) -> Vec<u8> {
    use std::io::Write;
    let mut source = zip::ZipArchive::new(std::io::Cursor::new(archive)).unwrap();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for index in 0..source.len() {
        let mut entry = source.by_index(index).unwrap();
        let name = entry.name().unwrap().into_owned();
        writer.start_file(name.as_str(), stored).unwrap();
        if name == "project.json" {
            writer.write_all(manifest.as_bytes()).unwrap();
        } else {
            std::io::copy(&mut entry, &mut writer).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}
