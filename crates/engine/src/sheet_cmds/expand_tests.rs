use super::{MAX_COPIES, MAX_VARIABLES};
use crate::sheet_cmds::{MAX_ADD, MAX_ROWS};
use crate::{Session, commands::Registry, generation_catalog::GenerationCatalog};
use nodecraft_exec::staleness::{PROVENANCE_CONFIG, input_hash};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, ErrorKind, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

const SHEET: CanvasId = CanvasId::from_bytes([1; 16]);
const NOTE: NodeId = NodeId::from_bytes([4; 16]);

fn session() -> Session {
    let kinds = GenerationCatalog::builtin().unwrap().registry().unwrap();
    let project = Project::new(ProjectId::from_bytes([9; 16]), "Sheets").unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    s.dispatch("sheet.create", &json!({"canvas": SHEET, "title": "Mugs"})).unwrap();
    s
}

fn seed(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn expand(s: &mut Session, params: Json) -> Result<Json, nodecraft_types::Error> {
    let mut params = params;
    params["canvas"] = json!(SHEET);
    s.dispatch("sheet.expand", &params)
}

fn listed(s: &mut Session) -> Json {
    s.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap()
}

fn prompts(s: &mut Session) -> Vec<String> {
    listed(s)["rows"].as_array().unwrap().iter().map(|row| row["prompt"].as_str().unwrap_or_default().to_owned()).collect()
}

fn colours_and_sizes() -> Json {
    json!([{"name": "colour", "text": ["red", "blue"]}, {"name": "size", "text": ["small", "large", ""]}])
}

#[test]
fn a_template_expands_into_its_cartesian_product_as_one_undo_step() {
    let mut s = session();
    s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(5), "prompts": ["first"]})).unwrap();
    let made = expand(&mut s, json!({"seed": seed(10), "template": "a {{size}} {{ colour }} mug", "variables": colours_and_sizes()})).unwrap();
    assert_eq!(made["count"], 6);
    // The first variable varies slowest; an empty value inserts nothing.
    assert_eq!(prompts(&mut s), ["first", "a small red mug", "a large red mug", "a  red mug", "a small blue mug", "a large blue mug", "a  blue mug"]);
    let rows = listed(&mut s);
    assert_eq!(rows["rows"][0]["expansion"], Json::Null, "rows it didn't make carry no provenance");
    assert_eq!(rows["rows"][5]["expansion"], json!({"values": {"colour": "blue", "size": "large"}, "copy": null}));
    assert_eq!(rows["rows"][1]["model"], rows["rows"][0]["model"], "new rows copy the row they follow");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(prompts(&mut s), ["first"], "one undo removes every row it made");
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(prompts(&mut s).len(), 7);
}

#[test]
fn a_dry_run_counts_and_previews_without_editing() {
    let mut s = session();
    let before = s.document().clone();
    let revision = s.revision();
    let preview =
        expand(&mut s, json!({"seed": seed(10), "template": "a {{colour}} {{size}} mug", "variables": colours_and_sizes(), "copies": 2, "dry_run": true}))
            .unwrap();
    assert_eq!(preview["count"], 12);
    assert_eq!(preview["combinations"], 6);
    assert_eq!(preview["fits"], true);
    assert_eq!(preview["factors"], "2 colour × 3 size × 2 copies");
    assert_eq!(preview["prompts"], json!(["a red small mug", "a red small mug", "a red large mug", "a red large mug", "a red  mug"]));
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
}

#[test]
fn copies_without_variables_repeat_the_template_and_number_each_copy() {
    let mut s = session();
    let made = expand(&mut s, json!({"seed": seed(10), "template": "a fox {{{{in braces}}}}", "copies": 3})).unwrap();
    assert_eq!(made["count"], 3);
    assert_eq!(prompts(&mut s), ["a fox {{in braces}}"; 3]);
    let rows = listed(&mut s);
    let copies: Vec<&Json> = rows["rows"].as_array().unwrap().iter().map(|row| &row["expansion"]["copy"]).collect();
    assert_eq!(copies, [&json!(1), &json!(2), &json!(3)]);
    // Provenance says where a row came from, not what it makes: identical copies hash alike.
    let canvas = s.document().canvases().get(&SHEET).unwrap();
    let ids: Vec<NodeId> = super::rows(canvas).iter().map(|row| row.id).collect();
    assert!(canvas.nodes().get(&ids[0]).unwrap().config.contains_key(PROVENANCE_CONFIG));
    // (This catalog's rows have no model to hash, so a text source stands in for one.)
    let mut note = s.kinds().get("source.text").unwrap().descriptor().instantiate(NOTE, &BTreeMap::new()).unwrap();
    let hash = |node: nodecraft_graph::Node| {
        let plain = CanvasId::from_bytes([3; 16]);
        let mut project = Project::new(ProjectId::from_bytes([8; 16]), "Plain").unwrap();
        Change::AddCanvas { canvas: Canvas::new(plain, "Plain") }.apply(&mut project).unwrap();
        Change::AddNode { canvas: plain, node }.apply(&mut project).unwrap();
        input_hash(project.canvases().get(&plain).unwrap(), s.kinds(), NOTE).unwrap()
    };
    let without = hash(note.clone());
    note.config.insert(PROVENANCE_CONFIG.into(), Value::Text(r#"{"copy":2}"#.into()));
    assert_eq!(hash(note), without);
}

#[test]
fn too_many_rows_are_refused_with_the_count_and_the_fix() {
    let mut s = session();
    let hundred: Vec<String> = (0..10).map(|n| format!("v{n}")).collect();
    let variables = json!([{"name": "a", "text": hundred}, {"name": "b", "text": ["x", "y"]}]);
    let template = "{{a}} {{b}}";
    let preview = expand(&mut s, json!({"seed": seed(10), "template": template, "variables": variables, "copies": 6, "dry_run": true})).unwrap();
    assert_eq!(preview["count"], 120);
    assert_eq!(preview["fits"], false);
    assert!(preview["problem"].as_str().unwrap().contains("120 rows"), "{preview}");
    assert_eq!(preview["prompts"].as_array().unwrap().len(), 5, "the first prompts still show");
    let refused = expand(&mut s, json!({"seed": seed(10), "template": template, "variables": variables, "copies": 6})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Overflow);
    assert!(refused.message.contains("10 a × 2 b × 6 copies makes 120 rows") && refused.message.contains("lower copies"), "{}", refused.message);
    assert!(prompts(&mut s).is_empty());

    // Eight variables of 100 values overflow nothing: the count is checked, not wrapped.
    let values: Vec<String> = (0..100).map(|n| format!("v{n}")).collect();
    let variables: Vec<Json> = (0..MAX_VARIABLES).map(|n| json!({"name": format!("v{n}"), "text": values})).collect();
    let template: String = (0..MAX_VARIABLES).map(|n| format!("{{{{v{n}}}}}")).collect();
    let preview = expand(&mut s, json!({"seed": seed(10), "template": template, "variables": variables, "copies": MAX_COPIES, "dry_run": true})).unwrap();
    assert_eq!(preview["fits"], false);
    assert_eq!(preview["count"], json!(10u64.pow(18)), "counted exactly, without wrapping");
    assert!(preview["problem"].as_str().unwrap().contains("makes 1000000000000000000 rows"));
}

#[test]
fn an_expansion_respects_the_sheets_room() {
    let mut s = session();
    let batch: Vec<String> = (0..100).map(|n| format!("row {n}")).collect();
    for n in 0..(MAX_ROWS / 100) - 1 {
        s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(100 + u8::try_from(n).unwrap()), "prompts": batch})).unwrap();
    }
    s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(120), "prompts": ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]})).unwrap();
    let preview = expand(&mut s, json!({"seed": seed(10), "template": "x", "copies": MAX_ADD, "dry_run": true})).unwrap();
    assert_eq!(preview["room"], 90);
    assert_eq!(preview["limit"], 90);
    assert!(preview["problem"].as_str().unwrap().contains("room for 90"), "{preview}");
    assert!(expand(&mut s, json!({"seed": seed(10), "template": "x", "copies": 90})).is_ok());
}

#[test]
fn bad_variables_and_templates_are_refused_with_what_to_do() {
    let mut s = session();
    for (variables, template, says) in [
        (json!([{"name": "c", "text": ["red"]}]), "{{colour}}", "no variable defines"),
        (json!([{"name": "c", "text": ["red"]}]), "a mug", "doesn't use `{{c}}`"),
        (json!([{"name": "c", "text": ["red"]}, {"name": "C", "text": ["x"]}]), "{{c}} {{C}}", "differ by more than case"),
        (json!([{"name": "c", "text": ["red", "red"]}]), "{{c}}", "twice"),
        (json!([{"name": "c", "text": []}]), "{{c}}", "1 to 100 values"),
        (json!([{"name": "c"}]), "{{c}}", "either `text`"),
        (json!([{"name": "c", "text": ["a"], "images": [NOTE]}]), "{{c}}", "either `text`"),
        (json!([{"name": "two words", "text": ["a"]}]), "x", "can't name"),
        (json!([{"name": "c", "text": ["a"]}]), "{{c", "no closing"),
        (json!([{"name": "c", "text": ["a".repeat(4097)]}]), "{{c}}", "longer than"),
        (json!([{"name": "c", "images": [NOTE]}]), "x", "isn't on this sheet"),
    ] {
        let error = expand(&mut s, json!({"seed": seed(10), "template": template, "variables": variables})).unwrap_err();
        assert!(error.message.contains(says), "{template} {variables}: {}", error.message);
        assert_ne!(error.kind, ErrorKind::Internal);
    }
    for copies in [0, MAX_COPIES + 1] {
        assert!(expand(&mut s, json!({"seed": seed(10), "template": "x", "copies": copies})).is_err());
    }
    assert!(prompts(&mut s).is_empty());
}

const REFERENCE_MODEL: &str = r#"
[[model]]
id = "mock-reference"
provider = "mock"
provider_model = "mock-image"
label = "Mock reference image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "image-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }, { role = "image", modality = "image", min = 0, max = 1 }]
"#;

/// A sheet whose first row uses a model taking one reference image.
fn reference_session() -> (Session, NodeId) {
    let catalog = nodecraft_catalog::Catalog::from_toml(REFERENCE_MODEL).unwrap();
    let snapshot = GenerationCatalog::new(crate::run_cmds::RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let project = Project::new(ProjectId::from_bytes([9; 16]), "Sheets").unwrap();
    let mut resources = crate::commands::Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    s.dispatch("sheet.create", &json!({"canvas": SHEET, "title": "Mugs"})).unwrap();
    let made = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(5), "prompts": ["first"]})).unwrap();
    let row: NodeId = serde_json::from_value(made["rows"][0].clone()).unwrap();
    let preview = s.dispatch("node.model_preview", &json!({"canvas": SHEET, "nodes": [row], "model": "mock-reference"})).unwrap();
    let revision = s.revision();
    s.dispatch(
        "node.set_model",
        &json!({"canvas": SHEET, "nodes": [row], "model": "mock-reference", "expected_revision": revision, "preview_binding": preview["binding"]}),
    )
    .unwrap();
    (s, row)
}

fn image_sources(s: &mut Session, n: u8) -> Vec<NodeId> {
    let kinds = s.kinds().clone();
    (0..n)
        .map(|n| {
            let id = NodeId::from_bytes([50 + n; 16]);
            let node = kinds.get("source.image").unwrap().descriptor().instantiate(id, &BTreeMap::new()).unwrap();
            s.dispatch("node.add", &json!({"canvas": SHEET, "node": node})).unwrap();
            id
        })
        .collect()
}

#[test]
fn image_variables_wire_one_reference_per_row_and_stay_out_of_the_prompt() {
    let (mut s, _) = reference_session();
    let images = image_sources(&mut s, 2);
    let variables = json!([{"name": "colour", "text": ["red", "blue"]}, {"name": "look", "images": images}]);
    let in_prompt = expand(&mut s, json!({"seed": seed(10), "template": "{{colour}} {{look}}", "variables": variables})).unwrap_err();
    assert!(in_prompt.message.contains("image variable"), "{}", in_prompt.message);
    let made = expand(&mut s, json!({"seed": seed(10), "template": "a {{colour}} mug", "variables": variables})).unwrap();
    assert_eq!(made["count"], 4);
    let rows = listed(&mut s);
    let wired: Vec<Json> = rows["rows"].as_array().unwrap().iter().skip(1).map(|row| row["references"][0]["node"].clone()).collect();
    assert_eq!(wired, [json!(images[0]), json!(images[1]), json!(images[0]), json!(images[1])]);
    assert_eq!(rows["rows"][2]["expansion"]["values"], json!({"colour": "red", "look": {"node": images[1]}}));
    assert_eq!(rows["rows"][2]["prompt"], "a red mug");
}

#[test]
fn an_image_variable_needs_a_free_input_and_cant_be_a_shared_reference() {
    let (mut s, _) = reference_session();
    let images = image_sources(&mut s, 2);
    // With a second row, the first image wired into both is a shared reference.
    s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(6), "prompts": ["second"]})).unwrap();
    s.dispatch("sheet.reference_wire", &json!({"canvas": SHEET, "seed": seed(7), "reference": images[0]})).unwrap();
    assert_eq!(listed(&mut s)["shared"], json!([images[0]]));
    let shared = expand(&mut s, json!({"seed": seed(10), "template": "x", "variables": [{"name": "look", "images": [images[0]]}]})).unwrap_err();
    assert!(shared.message.contains("already a shared reference"), "{}", shared.message);
    // A model with no image input has nowhere to put one; a dry run says so too.
    let mut plain = session();
    let images = image_sources(&mut plain, 1);
    let full = expand(&mut plain, json!({"seed": seed(10), "template": "x", "variables": [{"name": "look", "images": images}], "dry_run": true})).unwrap_err();
    assert!(full.message.contains("no free Image input for `look`"), "{}", full.message);
    assert_eq!(full.kind, ErrorKind::Unsupported);
}

#[test]
fn a_text_source_is_not_an_image_value_and_a_reused_seed_is_refused() {
    let mut s = session();
    let note = s.kinds().get("source.text").unwrap().descriptor().instantiate(NOTE, &BTreeMap::new()).unwrap();
    s.dispatch("node.add", &json!({"canvas": SHEET, "node": note})).unwrap();
    let error = expand(&mut s, json!({"seed": seed(10), "template": "x", "variables": [{"name": "i", "images": [NOTE]}]})).unwrap_err();
    assert!(error.message.contains("no single image output"), "{}", error.message);
    expand(&mut s, json!({"seed": seed(10), "template": "x"})).unwrap();
    let before = s.document().clone();
    let again = expand(&mut s, json!({"seed": seed(10), "template": "x", "dry_run": true})).unwrap_err();
    assert_eq!(again.kind, ErrorKind::Conflict);
    assert!(expand(&mut s, json!({"seed": seed(10), "template": "x"})).is_err());
    assert_eq!(s.document(), &before);
}

#[test]
fn rows_added_after_an_expanded_row_carry_no_provenance_but_duplicates_keep_it() {
    let mut s = session();
    let made = expand(&mut s, json!({"seed": seed(10), "template": "{{c}}", "variables": [{"name": "c", "text": ["red"]}]})).unwrap();
    s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(11), "prompts": ["plain"]})).unwrap();
    s.dispatch("sheet.rows_duplicate", &json!({"canvas": SHEET, "seed": seed(12), "rows": made["rows"]})).unwrap();
    let rows = listed(&mut s);
    let provenance: Vec<&Json> = rows["rows"].as_array().unwrap().iter().map(|row| &row["expansion"]).collect();
    assert_eq!(provenance, [&json!({"values": {"c": "red"}, "copy": null}), &json!({"values": {"c": "red"}, "copy": null}), &Json::Null]);
}
