//! `sheet.place`, the row history a sheet reports, and exporting rows' results (#163).
use super::{COLUMNS, PLACE_SPACING, RESULT_CONFIG};
use crate::{Session, commands::Registry, generation_catalog::GenerationCatalog};
use nodecraft_graph::{Canvas, Change, Generation, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, ErrorKind, GenerationId, Modality, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};

const SHEET: CanvasId = CanvasId::from_bytes([1; 16]);
const PLAIN: CanvasId = CanvasId::from_bytes([2; 16]);

fn seed(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn g(n: u8) -> GenerationId {
    GenerationId::from_bytes([n; 16])
}

fn bytes(n: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG result ".to_vec();
    bytes.push(n);
    bytes
}

fn asset(n: u8) -> Value {
    let hash = Blake3::from_bytes(*blake3::hash(&bytes(n)).as_bytes());
    Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() })
}

fn registries() -> (Registry, nodecraft_nodes::Registry) {
    (Registry::builtins().unwrap(), GenerationCatalog::builtin().unwrap().registry().unwrap())
}

/// A sheet with `prompts` rows and a plain canvas, and the rows' ids.
fn sheet(prompts: &[&str]) -> (Session, Vec<NodeId>) {
    let (commands, kinds) = registries();
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Sheets").unwrap();
    Change::AddCanvas { canvas: Canvas::new(PLAIN, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::with_registries(project, commands, kinds).unwrap();
    s.dispatch("sheet.create", &json!({"canvas": SHEET, "title": "Mugs"})).unwrap();
    let made = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(5), "prompts": prompts})).unwrap();
    let rows = serde_json::from_value(made["rows"].clone()).unwrap();
    (s, rows)
}

/// A result for `row` whose image output holds `value`: every other output holds an empty list.
fn result(row: &Node, id: GenerationId, value: &Value) -> Generation {
    let outputs = row.outputs.iter().map(|port| if port.port_type.modality == Modality::Image { value.clone() } else { Value::List(Vec::new()) }).collect();
    Generation::new(id, outputs)
}

/// `s` again with `results` recorded on rows as a run would record them (oldest first), the
/// result assets imported.
fn with_results(s: &Session, results: &[(NodeId, GenerationId, Value)], imported: &[u8]) -> Session {
    let mut project = s.document().clone();
    for (row, id, value) in results {
        let node = project.canvases().get(&SHEET).unwrap().nodes().get(row).unwrap().clone();
        for change in nodecraft_exec::run::record_changes(SHEET, &node, result(&node, *id, value)).unwrap() {
            change.apply(&mut project).unwrap();
        }
    }
    let (commands, kinds) = registries();
    let mut s = Session::with_registries(project, commands, kinds).unwrap();
    for n in imported {
        s.import_asset(&bytes(*n)).unwrap();
    }
    s
}

fn place(s: &mut Session, extra: Json) -> Result<Json, nodecraft_types::Error> {
    let mut params = json!({"canvas": SHEET, "seed": seed(20), "target": PLAIN});
    for (key, value) in extra.as_object().unwrap() {
        params[key] = value.clone();
    }
    s.dispatch("sheet.place", &params)
}

fn plain(s: &Session) -> &Canvas {
    s.document().canvases().get(&PLAIN).unwrap()
}

#[test]
fn selected_results_go_on_the_canvas_as_source_cards_in_one_undo_step() {
    let (s, rows) = sheet(&["red", "green", "blue", "grey", "white", "black"]);
    let results: Vec<(NodeId, GenerationId, Value)> = [0u8, 1, 2, 4, 5].iter().map(|n| (rows[usize::from(*n)], g(n + 1), asset(n + 1))).collect();
    let mut s = with_results(&s, &results, &[]);
    // Listed out of order and with a row that has no result: placed in sheet order, that row skipped.
    let all = [rows[5], rows[3], rows[0], rows[1], rows[2], rows[4]];
    let placed = place(&mut s, json!({"rows": all, "pos": {"x": 100, "y": -50}})).unwrap();
    let cards: Vec<NodeId> = placed["placed"].as_array().unwrap().iter().map(|card| serde_json::from_value(card["id"].clone()).unwrap()).collect();
    assert_eq!(cards.len(), 5);
    assert_eq!(placed["skipped"], json!([{"row": rows[3], "reason": "it has no result yet"}]));
    let from: Vec<NodeId> = placed["placed"].as_array().unwrap().iter().map(|card| serde_json::from_value(card["row"].clone()).unwrap()).collect();
    assert_eq!(from, [rows[0], rows[1], rows[2], rows[4], rows[5]], "in the sheet's order");
    for (n, id) in cards.iter().enumerate() {
        let card = plain(&s).nodes().get(id).unwrap();
        assert_eq!(card.kind, "source.image");
        let column = (n % COLUMNS) as f32;
        let line = (n / COLUMNS) as f32;
        assert_eq!((card.pos.x, card.pos.y), (100.0 + column * PLACE_SPACING.x, -50.0 + line * PLACE_SPACING.y), "a grid four across");
        assert!(card.history.is_empty(), "a source holds its value as a parameter");
    }
    // The fifth card (row 6's result) records where it came from.
    let card = plain(&s).nodes().get(&cards[4]).unwrap();
    assert_eq!(card.params.get("asset"), Some(&asset(6)));
    let Some(Value::Text(recorded)) = card.config.get(RESULT_CONFIG) else { panic!("no provenance") };
    let recorded: Json = serde_json::from_str(recorded).unwrap();
    let Value::Media(media) = asset(6) else { unreachable!() };
    assert_eq!(
        recorded,
        json!({"sheet": SHEET, "row": rows[5], "generation": g(6), "port": card_port(&s, rows[5]), "item": null, "asset": media.hash.to_string()})
    );
    // The same seed makes the same ids.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(plain(&s).nodes().is_empty(), "one undo removes every card");
    let again = place(&mut s, json!({"rows": all, "pos": {"x": 100, "y": -50}})).unwrap();
    assert_eq!(again["placed"], placed["placed"]);
}

fn card_port(s: &Session, row: NodeId) -> String {
    let node = s.document().canvases().get(&SHEET).unwrap().nodes().get(&row).unwrap();
    node.outputs.iter().find(|port| port.port_type.modality == Modality::Image).unwrap().name.clone()
}

#[test]
fn without_a_place_cards_go_right_of_what_the_canvas_holds() {
    let (s, rows) = sheet(&["red"]);
    let mut s = with_results(&s, &[(rows[0], g(1), asset(1))], &[]);
    let first = place(&mut s, json!({"rows": [rows[0]]})).unwrap();
    let id: NodeId = serde_json::from_value(first["placed"][0]["id"].clone()).unwrap();
    assert_eq!(plain(&s).nodes().get(&id).unwrap().pos, nodecraft_graph::Position { x: 0.0, y: 0.0 }, "an empty canvas starts at the origin");
    let second = place(&mut s, json!({"rows": [rows[0]], "seed": seed(21)})).unwrap();
    let id: NodeId = serde_json::from_value(second["placed"][0]["id"].clone()).unwrap();
    assert_eq!(plain(&s).nodes().get(&id).unwrap().pos, nodecraft_graph::Position { x: 480.0, y: 0.0 });
}

#[test]
fn every_item_of_a_list_result_is_its_own_card() {
    let (s, rows) = sheet(&["red"]);
    let mut s = with_results(&s, &[(rows[0], g(1), Value::List(vec![asset(1), asset(2), asset(3)]))], &[]);
    let placed = place(&mut s, json!({"rows": [rows[0]]})).unwrap();
    let items: Vec<Json> = placed["placed"].as_array().unwrap().iter().map(|card| card["item"].clone()).collect();
    assert_eq!(items, [json!(0), json!(1), json!(2)]);
    assert_eq!(plain(&s).nodes().len(), 3);
}

#[test]
fn placing_is_refused_without_a_result_a_plain_target_or_an_editable_document() {
    let (s, rows) = sheet(&["red", "green"]);
    let mut s = with_results(&s, &[(rows[0], g(1), Value::List(vec![asset(1), asset(2)]))], &[]);
    let before = s.document().clone();
    let none = place(&mut s, json!({"rows": [rows[1]]})).unwrap_err();
    assert_eq!(none.kind, ErrorKind::Unsupported);
    assert_eq!(none.message, "nothing to place: it has no result yet");
    assert_eq!(place(&mut s, json!({"rows": [rows[0]], "target": SHEET})).unwrap_err().kind, ErrorKind::InvalidInput, "not onto a sheet");
    assert_eq!(place(&mut s, json!({"rows": [rows[0]], "target": CanvasId::from_bytes([7; 16])})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(place(&mut s, json!({"rows": [seed(99)]})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(place(&mut s, json!({"rows": [rows[0], rows[0]]})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(place(&mut s, json!({"rows": []})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(place(&mut s, json!({"rows": [rows[0]], "pos": {"x": 2.0e7, "y": 0}})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(
        place(&mut s, json!({"rows": [rows[0]], "pos": {"x": 9_999_900.0, "y": 0}})).unwrap_err().kind,
        ErrorKind::InvalidInput,
        "the grid stays inside"
    );
    assert_eq!(s.document(), &before);
    s.set_access(crate::access::Access::new(nodecraft_sync::Role::View, None).unwrap());
    assert!(s.enabled("sheet.place", &json!({"canvas": SHEET, "seed": seed(20), "target": PLAIN, "rows": [rows[0]]})).is_err());
    assert!(place(&mut s, json!({"rows": [rows[0]]})).is_err(), "a document this session can only view takes no cards");
    assert_eq!(s.document(), &before);
}

/// A re-run adds a result and makes it active; the older one stays in the row's history, and
/// what is placed (and exported) is whichever one is active.
#[test]
fn a_rerun_keeps_the_older_result_in_the_rows_history() {
    let (s, rows) = sheet(&["red"]);
    let mut s = with_results(&s, &[(rows[0], g(1), asset(1)), (rows[0], g(2), asset(2))], &[]);
    let listed = s.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap();
    let row = &listed["rows"][0];
    assert_eq!((row["results"].clone(), row["position"].clone(), row["generation"].clone()), (json!(2), json!(2), json!(g(2))), "the newest is active");
    assert_eq!(row["output"], json!(card_port(&s, rows[0])));
    s.dispatch("history.previous", &json!({"canvas": SHEET, "id": rows[0]})).unwrap();
    let listed = s.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap();
    assert_eq!((listed["rows"][0]["results"].clone(), listed["rows"][0]["position"].clone()), (json!(2), json!(1)));
    let placed = place(&mut s, json!({"rows": [rows[0]]})).unwrap();
    assert_eq!(placed["placed"][0]["generation"], json!(g(1)), "the active result is placed");
    // Without a result a row reports none.
    let (mut empty, _) = sheet(&["blue"]);
    let listed = empty.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap();
    assert_eq!((listed["rows"][0]["results"].clone(), listed["rows"][0]["position"].clone()), (json!(0), Json::Null));
}

/// The rows' active results export through #98's folder export, by the output `sheet.rows` names.
#[cfg(unix)]
#[test]
fn rows_active_results_export_through_the_local_folder_export() {
    let (s, rows) = sheet(&["red", "green", "blue"]);
    let mut s = with_results(&s, &[(rows[0], g(1), asset(1)), (rows[2], g(2), asset(2))], &[1, 2]);
    let listed = s.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap();
    let sources: Vec<Json> = listed["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["result"] == "complete")
        .map(|row| json!({"node": row["id"], "port": row["output"]}))
        .collect();
    assert_eq!(sources.len(), 2);
    let dir = tempfile::tempdir().unwrap();
    let host = crate::local_export::ExportHost::authorize(dir.path()).unwrap();
    let params = json!({"canvas": SHEET, "sources": sources, "destination": dir.path().display().to_string()});
    let run = crate::local_export::dispatch_with_host(&mut s, &host, "export.local_run", &params).unwrap();
    assert_eq!(run["counts"]["written"], 2, "{run}");
    let mut written: Vec<Vec<u8>> = std::fs::read_dir(dir.path()).unwrap().map(|entry| std::fs::read(entry.unwrap().path()).unwrap()).collect();
    written.sort();
    let mut expected = vec![bytes(1), bytes(2)];
    expected.sort();
    assert_eq!(written, expected);
    // Without a folder the app authorized, nothing is written anywhere.
    assert_eq!(s.dispatch("export.local_run", &params).unwrap_err().kind, ErrorKind::Unsupported);
}
