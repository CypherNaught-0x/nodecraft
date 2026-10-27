//! Original SVG starter author and real, offline vector-to-raster contract proof.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{action_exec::LocalActions, generation_catalog::GenerationCatalog, templates};
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::{AssetStore, Snapshot};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Position, Project};
use nodecraft_types::{AssetRef, CanvasId, EdgeId, MediaMeta, NodeId, ParamValues, ProjectId, Unsigned, Value};

const ID: &str = "action-image-rasterize";
const TITLE: &str = "Rasterize an SVG";
const DESCRIPTION: &str = "An original SVG feeds Rasterize SVG. Run all to create a 512-pixel PNG for raster-only models or tools; change Longest edge to choose its size. The vector source stays unchanged. No provider, key or native GPU is needed.";
const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#663399"/><circle cx="130" cy="45" r="25" fill="#44ddaa"/></svg>"##;
fn ident(role: &str) -> [u8; 16] {
    blake3::hash(format!("nodecraft-template:{ID}:v1:{role}").as_bytes()).as_bytes()[..16].try_into().unwrap()
}
fn example() -> (templates::Entry, Vec<u8>) {
    let kinds = GenerationCatalog::builtin().unwrap().registry().unwrap();
    let canvas = CanvasId::from_bytes(ident("canvas"));
    let mut project = Project::new(ProjectId::from_bytes(ident("project")), TITLE).unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "SVG to PNG") }.apply(&mut project).unwrap();
    let info = nodecraft_media::svg::probe(SVG, &Default::default()).unwrap();
    assert_eq!((info.width, info.height, info.preview_unavailable), (200, 100, None));
    let mut assets = AssetStore::default();
    let source = Value::Media(AssetRef {
        hash: assets.insert(SVG).unwrap(),
        mime: "image/svg+xml".parse().unwrap(),
        meta: MediaMeta { width: Some(info.width), height: Some(info.height), ..Default::default() },
    });
    let source_id = NodeId::from_bytes(ident("source"));
    let action_id = NodeId::from_bytes(ident("rasterize"));
    for (kind, id, pos, params) in [
        ("source.image", source_id, Position { x: 0.0, y: 0.0 }, [("asset".into(), source)].into()),
        ("image.rasterize", action_id, Position { x: 420.0, y: 0.0 }, [("edge".into(), Value::Unsigned(Unsigned(512)))].into()),
        (
            "structure.note",
            NodeId::from_bytes(ident("note")),
            Position { x: 0.0, y: -260.0 },
            [("text".into(), Value::Text(format!("{TITLE}\n\n{DESCRIPTION}\n\nAfter running, preview or save the PNG from Rasterize SVG. Keep the original SVG for vector editing. Replace its source with an imported or generated SVG when ready.")))].into(),
        ),
    ] {
        let mut node = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &params).unwrap();
        node.pos = pos;
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    }
    Change::Connect {
        canvas,
        edge: Edge {
            id: EdgeId::from_bytes(ident("edge")),
            from: Endpoint { node: source_id, port: "out".into() },
            to: Endpoint { node: action_id, port: "image".into() },
            order: 0,
        },
    }
    .apply(&mut project)
    .unwrap();
    project.validate().unwrap();
    assets.validate_project(&project).unwrap();
    let snapshot = Snapshot { project, assets, epoch: 0 };
    templates::validate_nodes(&snapshot, &kinds).unwrap();
    let bytes = nodecraft_format::encode(&snapshot).unwrap();
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let entry = templates::Entry {
        id: ID.into(),
        title: TITLE.into(),
        description: DESCRIPTION.into(),
        version: 1,
        file: format!("templates/{ID}/1/{digest}.ncraft"),
        blake3: digest,
        features: vec!["image.rasterize".into(), "svg".into(), "vector-to-raster".into()],
        canvas,
    };
    templates::parse_index(&serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":[entry]})).unwrap()).unwrap();
    templates::validate_archive(&entry, &bytes).unwrap();
    (entry, bytes)
}
fn prove(entry: &templates::Entry, bytes: &[u8]) {
    let snapshot = templates::validate_archive(entry, bytes).unwrap();
    let canvas = snapshot.project.canvases().get(&entry.canvas).unwrap();
    assert_eq!((canvas.nodes().len(), canvas.edges().len()), (3, 1));
    let source = canvas.nodes().get(&NodeId::from_bytes(ident("source"))).unwrap();
    let action = canvas.nodes().get(&NodeId::from_bytes(ident("rasterize"))).unwrap();
    assert_eq!(action.kind, "image.rasterize");
    assert!(canvas.nodes().values().all(|node| node.history.is_empty()));
    let original = source.params.get("asset").unwrap();
    let Value::Media(svg) = original else { panic!("SVG media") };
    assert_eq!(snapshot.assets.get(&svg.hash).unwrap(), SVG);
    assert_eq!(svg.mime.essence(), "image/svg+xml");
    assert_eq!(svg.meta, MediaMeta { width: Some(200), height: Some(100), ..Default::default() });
    let actions = LocalActions::new(snapshot.assets.clone());
    let converted = actions.execute(&action.kind, &action.params, std::slice::from_ref(original)).unwrap();
    let Value::Media(png) = converted else { panic!("PNG media") };
    assert_eq!(png.mime.essence(), "image/png");
    assert_eq!((png.meta.width, png.meta.height), (Some(512), Some(256)));
    assert_ne!(png.hash, svg.hash);
    let staged = actions.staged();
    let png_bytes = staged.get(&png.hash).unwrap();
    let (_, image) = nodecraft_media::decode_image(png_bytes, &Default::default()).unwrap();
    let rgba = image.to_rgba8();
    assert_eq!(rgba.get_pixel(10, 10).0, [0x66, 0x33, 0x99, 255]);
    assert_eq!(rgba.get_pixel(333, 115).0, [0x44, 0xdd, 0xaa, 255]);
    assert_eq!(snapshot.assets.get(&svg.hash).unwrap(), SVG, "conversion never replaces the source");
    let ordinary: ParamValues = [("width".into(), Value::Unsigned(Unsigned(512)))].into();
    assert!(actions.execute("image.resize", &ordinary, std::slice::from_ref(original)).is_err(), "conversion remains explicit");
}
#[test]
fn recraft35_svg_starter_roundtrips_and_runs_real_png_conversion() {
    let (entry, bytes) = example();
    prove(&entry, &bytes);
    let (again, rebuilt) = example();
    assert_eq!(entry, again);
    assert_eq!(bytes, rebuilt, "immutable authoring is deterministic");
}
#[test]
#[ignore = "author tool: NODECRAFT_SVG35_TEMPLATE_DIR selects a scratch output directory"]
fn recraft35_author_svg_starter() {
    let (entry, bytes) = example();
    prove(&entry, &bytes);
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_SVG35_TEMPLATE_DIR").unwrap());
    let path = root.join(&entry.file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    if path.exists() {
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "never overwrite immutable published bytes");
    } else {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).unwrap();
        file.write_all(&bytes).unwrap();
    }
    std::fs::write(root.join("svg-template-entry.json"), serde_json::to_vec_pretty(&entry).unwrap()).unwrap();
    std::fs::write(root.join("geometric-vector.svg"), SVG).unwrap();
}
