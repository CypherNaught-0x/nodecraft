use super::{MAX_ROWS, ROW_SPACING};
use crate::{Session, commands::Registry, generation_catalog::GenerationCatalog};
use nodecraft_graph::{Canvas, CanvasKind, Change, Project};
use nodecraft_types::{CanvasId, ErrorKind, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

const SHEET: CanvasId = CanvasId::from_bytes([1; 16]);
const PLAIN: CanvasId = CanvasId::from_bytes([2; 16]);
const NOTE: NodeId = NodeId::from_bytes([4; 16]);

fn session() -> Session {
    let kinds = GenerationCatalog::builtin().unwrap().registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Sheets").unwrap();
    Change::AddCanvas { canvas: Canvas::new(PLAIN, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    s.dispatch("sheet.create", &json!({"canvas": SHEET, "title": "Product shots"})).unwrap();
    s
}

fn seed(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn add(s: &mut Session, n: u8, prompts: &[&str]) -> Vec<NodeId> {
    let made = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(n), "prompts": prompts})).unwrap();
    serde_json::from_value(made["rows"].clone()).unwrap()
}

fn rows(s: &mut Session) -> Json {
    s.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap()
}

fn ids(rows: &Json) -> Vec<NodeId> {
    rows["rows"].as_array().unwrap().iter().map(|row| serde_json::from_value(row["id"].clone()).unwrap()).collect()
}

fn prompts(rows: &Json) -> Vec<String> {
    rows["rows"].as_array().unwrap().iter().map(|row| row["prompt"].as_str().unwrap_or_default().to_owned()).collect()
}

/// A text source on the sheet, to use as a reference.
fn note(s: &mut Session) {
    let mut node = s.kinds().get("source.text").unwrap().descriptor().instantiate(NOTE, &BTreeMap::new()).unwrap();
    node.params.insert("text".into(), Value::Text("brand style".into()));
    s.dispatch("node.add", &json!({"canvas": SHEET, "node": node})).unwrap();
}

#[test]
fn a_sheet_is_a_canvas_of_kind_sheet_listed_with_its_rows() {
    let mut s = session();
    assert_eq!(s.document().canvases().get(&SHEET).unwrap().kind(), CanvasKind::Sheet);
    add(&mut s, 10, &["a red mug", "a blue mug"]);
    let listed = s.dispatch("sheet.list", &json!({})).unwrap();
    assert_eq!(listed["sheets"], json!([{"canvas": SHEET, "title": "Product shots", "rows": 2}]), "plain canvases aren't sheets");
    let again = s.dispatch("sheet.create", &json!({"canvas": SHEET, "title": "Again"}));
    assert_eq!(again.unwrap_err().kind, ErrorKind::Conflict);
    let plain = s.dispatch("sheet.rows", &json!({"canvas": PLAIN}));
    assert_eq!(plain.unwrap_err().kind, ErrorKind::InvalidInput, "row commands only work on sheets");
}

#[test]
fn rows_are_added_in_order_as_one_undo_step_with_stable_ids() {
    let mut s = session();
    let made = add(&mut s, 10, &["one", "two", "three"]);
    let listed = rows(&mut s);
    assert_eq!(ids(&listed), made);
    assert_eq!(prompts(&listed), ["one", "two", "three"]);
    assert!(listed["rows"].as_array().unwrap().iter().all(|row| row["result"] == "none" && row["kind"] == "generation.image"));
    let canvas = s.document().canvases().get(&SHEET).unwrap();
    let ys: Vec<f32> = made.iter().map(|id| canvas.nodes().get(id).unwrap().pos.y).collect();
    assert_eq!(ys, [0.0, ROW_SPACING, 2.0 * ROW_SPACING]);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(ids(&rows(&mut s)).is_empty(), "one undo step");
    // The same seed makes the same ids: replay is deterministic.
    assert_eq!(add(&mut s, 10, &["one", "two", "three"]), made);
}

#[test]
fn a_row_added_after_another_copies_its_settings_and_shifts_the_rest_down() {
    let mut s = session();
    let first = add(&mut s, 10, &["one", "three"]);
    let row = first[0];
    let model = rows(&mut s)["rows"][0]["model"].clone();
    let made = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(11), "after": row, "prompts": ["two"]})).unwrap();
    let listed = rows(&mut s);
    assert_eq!(prompts(&listed), ["one", "two", "three"]);
    assert_eq!(listed["rows"][1]["id"], made["rows"][0]);
    assert_eq!(listed["rows"][1]["model"], model);
    let canvas = s.document().canvases().get(&SHEET).unwrap();
    assert_eq!(canvas.nodes().get(&first[1]).unwrap().pos.y, 2.0 * ROW_SPACING, "the row below moved down");
}

#[test]
fn a_shared_reference_reaches_every_row_and_new_rows_get_it_too() {
    let mut s = session();
    let made = add(&mut s, 10, &["one", "two"]);
    note(&mut s);
    let wired = s.dispatch("sheet.reference_wire", &json!({"canvas": SHEET, "seed": seed(20), "reference": NOTE})).unwrap();
    assert_eq!(wired["rows"], json!(made));
    let listed = rows(&mut s);
    assert_eq!(listed["shared"], json!([NOTE]));
    assert_eq!(listed["rows"][0]["references"][0]["shared"], true);
    let new = add(&mut s, 12, &["three"]);
    let listed = rows(&mut s);
    assert_eq!(listed["shared"], json!([NOTE]), "the new row is wired like the row it follows");
    assert_eq!(listed["rows"][2]["references"][0]["node"], json!(NOTE));
    // Wiring again changes nothing and says so.
    assert!(s.dispatch("sheet.reference_wire", &json!({"canvas": SHEET, "seed": seed(21), "reference": NOTE})).is_err());
    // Unwired from one row, it belongs to the others only.
    s.dispatch("sheet.reference_unwire", &json!({"canvas": SHEET, "reference": NOTE, "rows": new})).unwrap();
    let listed = rows(&mut s);
    assert_eq!(listed["shared"], json!([]));
    assert_eq!(listed["rows"][0]["references"][0]["shared"], false);
    assert!(listed["rows"][2]["references"].as_array().unwrap().is_empty());
    let note_pos = s.document().canvases().get(&SHEET).unwrap().nodes().get(&NOTE).unwrap().pos;
    assert!(note_pos.x < 0.0, "references sit left of the rows");
}

#[test]
fn a_row_cant_be_another_rows_reference() {
    let mut s = session();
    let made = add(&mut s, 10, &["one", "two"]);
    let refused = s.dispatch("sheet.reference_wire", &json!({"canvas": SHEET, "seed": seed(20), "reference": made[0], "rows": [made[1]]}));
    assert_eq!(refused.unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn duplicates_follow_their_rows_with_prompts_and_references_but_no_results() {
    let mut s = session();
    let made = add(&mut s, 10, &["one", "two"]);
    note(&mut s);
    s.dispatch("sheet.reference_wire", &json!({"canvas": SHEET, "seed": seed(20), "reference": NOTE, "rows": [made[0]]})).unwrap();
    let copies = s.dispatch("sheet.rows_duplicate", &json!({"canvas": SHEET, "seed": seed(30), "rows": [made[0]]})).unwrap();
    let listed = rows(&mut s);
    assert_eq!(prompts(&listed), ["one", "one", "two"]);
    assert_eq!(listed["rows"][1]["id"], copies["rows"][0]);
    assert_eq!(listed["rows"][1]["references"][0]["node"], json!(NOTE));
    assert_eq!(listed["rows"][1]["result"], "none");
}

#[test]
fn removing_and_moving_rows_keeps_them_in_a_tidy_column() {
    let mut s = session();
    let made = add(&mut s, 10, &["one", "two", "three", "four"]);
    s.dispatch("sheet.rows_remove", &json!({"canvas": SHEET, "rows": [made[1]]})).unwrap();
    assert_eq!(prompts(&rows(&mut s)), ["one", "three", "four"]);
    s.dispatch("sheet.row_move", &json!({"canvas": SHEET, "row": made[3], "index": 0})).unwrap();
    let listed = rows(&mut s);
    assert_eq!(prompts(&listed), ["four", "one", "three"]);
    let canvas = s.document().canvases().get(&SHEET).unwrap();
    let ys: Vec<f32> = ids(&listed).iter().map(|id| canvas.nodes().get(id).unwrap().pos.y).collect();
    assert_eq!(ys, [0.0, ROW_SPACING, 2.0 * ROW_SPACING]);
    assert!(s.dispatch("sheet.row_move", &json!({"canvas": SHEET, "row": made[3], "index": 0})).is_err(), "already there");
    assert!(s.dispatch("sheet.row_move", &json!({"canvas": SHEET, "row": made[3], "index": 3})).is_err(), "past the end");
}

#[test]
fn bad_requests_change_nothing() {
    let mut s = session();
    let made = add(&mut s, 10, &["one"]);
    let before = s.document().clone();
    let revision = s.revision();
    let many: Vec<&str> = vec!["x"; 101];
    for (id, params) in [
        ("sheet.rows_add", json!({"canvas": SHEET, "seed": seed(11), "prompts": []})),
        ("sheet.rows_add", json!({"canvas": SHEET, "seed": seed(11), "prompts": many})),
        ("sheet.rows_add", json!({"canvas": SHEET, "seed": seed(11), "prompts": ["x"], "after": seed(99)})),
        ("sheet.rows_add", json!({"canvas": PLAIN, "seed": seed(11), "prompts": ["x"]})),
        ("sheet.rows_add", json!({"canvas": SHEET, "seed": seed(11), "prompts": ["x"], "extra": 1})),
        ("sheet.rows_duplicate", json!({"canvas": SHEET, "seed": seed(11), "rows": [made[0], made[0]]})),
        ("sheet.rows_remove", json!({"canvas": SHEET, "rows": [seed(99)]})),
        ("sheet.reference_unwire", json!({"canvas": SHEET, "reference": seed(99)})),
        ("sheet.create", json!({"canvas": seed(50), "title": "  "})),
    ] {
        assert!(s.enabled(id, &params).is_err(), "{id} {params}");
        assert!(s.dispatch(id, &params).is_err(), "{id} {params}");
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
    }
}

#[test]
fn a_sheet_holds_at_most_max_rows() {
    let mut s = session();
    let batch: Vec<String> = (0..100).map(|n| format!("row {n}")).collect();
    for n in 0..(MAX_ROWS / 100) {
        let seed = NodeId::from_bytes([u8::try_from(n).unwrap().saturating_add(100); 16]);
        s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed, "prompts": batch})).unwrap();
    }
    let full = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(12), "prompts": ["one more"]}));
    assert_eq!(full.unwrap_err().kind, ErrorKind::Overflow);
}

#[test]
fn a_one_row_sheets_reference_is_its_own_and_doesnt_spread() {
    let mut s = session();
    let made = add(&mut s, 10, &["one"]);
    note(&mut s);
    s.dispatch("sheet.reference_wire", &json!({"canvas": SHEET, "seed": seed(20), "reference": NOTE, "rows": made})).unwrap();
    assert_eq!(rows(&mut s)["shared"], json!([]));
    add(&mut s, 11, &["two"]);
    let listed = rows(&mut s);
    assert!(listed["rows"][1]["references"].as_array().unwrap().is_empty(), "{listed}");
}

#[test]
fn rows_move_down_and_a_cleared_sheet_starts_again() {
    let mut s = session();
    let made = add(&mut s, 10, &["one", "two", "three"]);
    s.dispatch("sheet.row_move", &json!({"canvas": SHEET, "row": made[0], "index": 2})).unwrap();
    assert_eq!(prompts(&rows(&mut s)), ["two", "three", "one"]);
    s.dispatch("sheet.rows_remove", &json!({"canvas": SHEET, "rows": made})).unwrap();
    assert!(ids(&rows(&mut s)).is_empty());
    add(&mut s, 11, &["fresh"]);
    assert_eq!(prompts(&rows(&mut s)), ["fresh"]);
}

#[test]
fn kind_only_picks_an_empty_sheets_first_row() {
    let mut s = session();
    let text = s.kinds().get(nodecraft_nodes::generation::TEXT_KIND).is_ok();
    if text {
        let first = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(10), "prompts": ["q"], "kind": "generation.text"})).unwrap();
        assert!(first["rows"].as_array().is_some_and(|rows| rows.len() == 1));
        assert_eq!(rows(&mut s)["rows"][0]["kind"], "generation.text");
    } else {
        add(&mut s, 10, &["one"]);
    }
    let refused = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(11), "prompts": ["x"], "kind": "generation.image"}));
    assert_eq!(refused.unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn a_reused_seed_is_refused_and_changes_nothing() {
    let mut s = session();
    let made = add(&mut s, 10, &["one"]);
    let before = s.document().clone();
    let revision = s.revision();
    assert!(s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(10), "prompts": ["two"]})).is_err());
    assert!(s.dispatch("sheet.rows_duplicate", &json!({"canvas": SHEET, "seed": seed(10), "rows": made})).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
}

#[test]
fn many_rows_go_in_one_remove() {
    let mut s = session();
    let batch: Vec<String> = (0..100).map(|n| format!("row {n}")).collect();
    let mut all = Vec::new();
    for n in 0..2u8 {
        let made = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(100 + n), "prompts": batch})).unwrap();
        all.extend(made["rows"].as_array().unwrap().iter().cloned());
    }
    s.dispatch("sheet.rows_remove", &json!({"canvas": SHEET, "rows": all})).unwrap();
    assert!(ids(&rows(&mut s)).is_empty());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(ids(&rows(&mut s)).len(), 200, "one undo step");
}
