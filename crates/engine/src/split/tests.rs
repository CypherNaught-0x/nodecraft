use super::*;
use crate::import::{Incoming, import_files};
use nodecraft_format::MAX_ASSET_BYTES;
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_media::{Limits, decode_image, encode_png};
use nodecraft_types::ProjectId;
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([1; 16]);

/// A session with one Image node holding a `width`×`height` picture whose pixel at (x, y) is
/// red = x, green = y, so each tile's origin can be read back.
fn session(width: u32, height: u32) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Split").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            rgba.extend([x as u8, y as u8, 7, 255]);
        }
    }
    let png = encode_png(width, height, &rgba, &Limits::default()).unwrap();
    import_files(&mut s, C, Position { x: 0.0, y: 0.0 }, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[SOURCE]).unwrap();
    s
}

fn ids(n: u8) -> Vec<NodeId> {
    (0..n).map(|i| NodeId::from_bytes([100 + i; 16])).collect()
}

fn split(s: &mut Session, rows: u32, columns: u32, ids: &[NodeId]) -> Result<Vec<NodeId>, Error> {
    split_grid(s, &Split { canvas: C, node: SOURCE, rows, columns, ids, step: (300.0, 200.0) })
}

/// A tile's size and its top-left pixel (which records where it came from).
fn tile(s: &Session, id: NodeId) -> (u32, u32, [u8; 2], Position) {
    let node = s.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap();
    let Some(Value::Media(asset)) = node.params.get("asset") else { panic!("an asset") };
    let (_, image) = decode_image(s.assets().get(&asset.hash).unwrap(), &Limits::default()).unwrap();
    let image = image.to_rgba8();
    let pixel = image.get_pixel(0, 0).0;
    assert_eq!((asset.meta.width, asset.meta.height), (Some(image.width()), Some(image.height())));
    (image.width(), image.height(), [pixel[0], pixel[1]], node.pos)
}

#[test]
fn tiles_are_row_major_cover_uneven_images_exactly_and_are_one_undo_step() {
    let mut s = session(10, 7);
    let before = s.document().clone();
    let made = split(&mut s, 2, 3, &ids(6)).unwrap();
    assert_eq!(made, ids(6));
    let tiles: Vec<_> = made.iter().map(|id| tile(&s, *id)).collect();
    // Columns at 0, 3, 6 (widths 3, 3, 4); rows at 0, 3 (heights 3, 4).
    let shapes: Vec<_> = tiles.iter().map(|(w, h, origin, _)| (*w, *h, *origin)).collect();
    assert_eq!(shapes, [(3, 3, [0, 0]), (3, 3, [3, 0]), (4, 3, [6, 0]), (3, 4, [0, 3]), (3, 4, [3, 3]), (4, 4, [6, 3])]);
    let total: u32 = shapes.iter().map(|(w, h, _)| w * h).sum();
    assert_eq!(total, 70, "nothing cropped away or padded");
    // Laid out as the grid, beside the source.
    let positions: Vec<_> = tiles.iter().map(|(.., p)| (p.x, p.y)).collect();
    assert_eq!(positions, [(300.0, 0.0), (600.0, 0.0), (900.0, 0.0), (300.0, 200.0), (600.0, 200.0), (900.0, 200.0)]);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before, "one undo removes every tile");
}

#[test]
fn bad_counts_tiny_images_and_nodes_without_images_are_refused_without_storing() {
    let mut s = session(4, 4);
    let stored = s.assets().len();
    for (rows, columns, n) in [(0, 2, 0), (17, 1, 17), (9, 8, 72), (2, 2, 3)] {
        let error = split(&mut s, rows, columns, &ids(n)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{rows}×{columns}: {}", error.message);
    }
    assert_eq!(split(&mut s, 1, 5, &ids(5)).unwrap_err().message, "the image is smaller than one pixel per tile");
    let text = NodeId::from_bytes([50; 16]);
    import_files(&mut s, C, Position { x: 0.0, y: 600.0 }, 300.0, &[Incoming { name: "t.txt", bytes: b"words" }], &[text]).unwrap();
    let error = split_grid(&mut s, &Split { canvas: C, node: text, rows: 2, columns: 2, ids: &ids(4), step: (300.0, 200.0) }).unwrap_err();
    assert_eq!(error.message, "this node has no image to split");
    assert_eq!(s.assets().len(), stored, "nothing stored for refused splits");
}

#[test]
fn a_one_by_one_split_copies_the_image_and_a_generated_result_can_be_split() {
    let mut s = session(5, 5);
    let made = split(&mut s, 1, 1, &ids(1)).unwrap();
    assert_eq!(tile(&s, made[0]).0, 5);
    // A generator's active result with an image, as run.record would leave it.
    let source = s.document().canvases().get(&C).unwrap().nodes().get(&SOURCE).unwrap().clone();
    let Some(Value::Media(asset)) = source.params.get("asset").cloned() else { panic!() };
    let mut generator = Node::new(NodeId::from_bytes([60; 16]), "test.gen");
    generator
        .history
        .push(nodecraft_graph::Generation::new(nodecraft_types::GenerationId::from_bytes([61; 16]), vec![Value::Text("x".into()), Value::Media(asset)]));
    generator.active = Some(nodecraft_types::GenerationId::from_bytes([61; 16]));
    assert!(image_of(&generator).is_some());
    generator.active = None;
    assert!(image_of(&generator).is_none(), "only the active result counts");
}

#[test]
fn the_largest_grids_split_and_far_out_or_overfull_splits_store_nothing() {
    for (rows, columns) in [(8, 8), (16, 4), (4, 16)] {
        let mut s = session(32, 32);
        assert_eq!(split(&mut s, rows, columns, &ids((rows * columns) as u8)).unwrap().len(), (rows * columns) as usize);
    }
    // A source near the canvas edge: the grid would leave it.
    let mut s = session(8, 8);
    let stored = s.assets().len();
    let canvas = C;
    s.dispatch("node.move", &json!({"canvas": canvas, "id": SOURCE, "pos": {"x": 9_999_800.0, "y": 0.0}})).unwrap();
    let error = split(&mut s, 2, 2, &ids(4)).unwrap_err();
    assert_eq!(error.message, "the tiles would land outside the canvas; move the node and try again");
    assert_eq!(s.assets().len(), stored);
    // A store with no room left for the tiles.
    let mut s = session(64, 64);
    let filler = |n: u8| {
        let mut bytes = encode_png(1, 1, &[n, 0, 0, 255], &Limits::default()).unwrap();
        bytes.resize(MAX_ASSET_BYTES - 1, n);
        bytes
    };
    for n in 0..2 {
        s.import_asset(&filler(n)).unwrap();
    }
    let mut almost = filler(9);
    almost.truncate(MAX_ASSET_BYTES - 3000);
    s.import_asset(&almost).unwrap();
    let stored = s.assets().len();
    let error = split(&mut s, 4, 4, &ids(16)).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Conflict, "the tiles don't fit in the project's media"));
    assert_eq!(s.assets().len(), stored, "nothing stored");
}

#[test]
fn only_decodable_image_types_can_be_split() {
    let s = session(4, 4);
    let mut node = s.document().canvases().get(&C).unwrap().nodes().get(&SOURCE).unwrap().clone();
    assert!(image_of(&node).is_some());
    let Some(Value::Media(asset)) = node.params.get_mut("asset") else { panic!() };
    asset.mime = "image/svg+xml".parse().unwrap();
    assert!(image_of(&node).is_none(), "SVG isn't offered");
}

#[test]
fn a_fanned_result_shows_a_row_of_images_per_item() {
    let s = session(5, 5);
    let source = s.document().canvases().get(&C).unwrap().nodes().get(&SOURCE).unwrap().clone();
    let Some(Value::Media(asset)) = source.params.get("asset").cloned() else { panic!() };
    let svg = nodecraft_types::AssetRef { mime: "image/svg+xml".parse().unwrap(), ..asset.clone() };
    let media = |n: usize| Value::List(vec![Value::Media(asset.clone()); n]);
    // `items` batch items fanned the result out (none for an unbatched run).
    let fanned = |outputs: Vec<Value>, items: u8| {
        let mut node = Node::new(NodeId::from_bytes([60; 16]), "test.gen");
        let mut result = nodecraft_graph::Generation::new(nodecraft_types::GenerationId::from_bytes([61; 16]), outputs);
        let key = |n: u8| nodecraft_graph::ItemKey { batch: NodeId::from_bytes([62; 16]), item: nodecraft_types::ItemId::from_bytes([n; 16]) };
        result.items = (0..items).map(|n| vec![key(n)]).collect();
        node.history.push(result);
        node.active = Some(nodecraft_types::GenerationId::from_bytes([61; 16]));
        let (rows, count) = image_rows(&node);
        let first = image_of(&node).map(|a| a.mime.essence().to_owned());
        assert_eq!(first, rows.first().and_then(|r| r.first()).map(|a| a.mime.essence().to_owned()), "image_of is the first shown");
        (rows.iter().map(Vec::len).collect::<Vec<_>>(), count)
    };
    let shown = |outputs: Vec<Value>| fanned(outputs, 0);
    assert_eq!(shown(vec![media(3)]), (vec![3], 3), "one run's images make one row");
    assert_eq!(fanned(vec![Value::List(vec![media(2), media(0), media(1)])], 3), (vec![2, 1], 3), "a row per item; an empty item shows none");
    assert_eq!(fanned(vec![Value::List(vec![media(9); 9])], 9), (vec![GALLERY_COLUMNS; GALLERY_ROWS], 81), "capped, but counted");
    assert_eq!(shown(vec![Value::List(vec![media(2), media(1)])]), (vec![], 0), "an unfanned list of lists isn't opened");
    assert_eq!(shown(vec![Value::Text("x".into()), Value::List(vec![Value::Media(svg), Value::Media(asset)])]), (vec![1], 1));
    assert_eq!(shown(vec![Value::Text("x".into())]), (vec![], 0));
}
