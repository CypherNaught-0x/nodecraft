//! The sheet's batch controls (#163): a row's model and aspect from the table, the model and
//! settings of many rows at once with Mixed values, references, retrying only the failed rows,
//! cancelling running ones, naming a batch, and the grid at 1000 rows. The mock provider only.
use super::*;
use serde_json::Value as Json;

/// The sheet's catalog plus a model with an aspect ratio, one the mock refuses and one that
/// never finishes.
const EXTRA: &str = r#"
[[model]]
id = "mock-wide"
provider = "mock"
provider_model = "mock-image"
label = "Mock wide"
description = "Public deterministic test fixture with an aspect ratio"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.modes]]
id = "image-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }, { role = "image", modality = "image", min = 1, max = 1 }]
[[model.params.params]]
name = "aspect"
label = "Aspect"
control = { type = "choice", options = [{ value = "1:1", label = "1:1" }, { value = "16:9", label = "16:9" }] }
default = { type = "text", value = "1:1" }

[[model]]
id = "mock-broken"
provider = "mock"
provider_model = "mock-broken"
label = "Mock broken"
description = "Public deterministic test fixture the mock refuses"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "mock-slow"
provider = "mock"
provider_model = "mock-slow"
label = "Mock slow"
description = "Public deterministic test fixture that never finishes"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

fn models() -> String {
    format!("{MOCK_IMAGE}{EXTRA}")
}

fn batch_making() -> Make {
    let mut broken = MockModel::new(MockOutput::Image);
    broken.fail = Some(nodecraft_provider_testkit::mock::MockFailure::Submit(nodecraft_providers::ErrorKind::UnsupportedInput));
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mock = MockProvider::new(
        "mock",
        [("mock-image".to_owned(), MockModel::new(MockOutput::Image)), ("mock-broken".to_owned(), broken), ("mock-slow".to_owned(), slow)],
    );
    Box::new(move |adapter, _| if adapter == "mock" { Ok(Arc::new(mock.clone())) } else { Err(Readiness::NotSetUp("not in this test".into())) })
}

/// Sheet `S` of `prompts` on `model`, open.
fn batch(model: &str, prompts: &[&str]) -> Harness<'static, NodeCraftApp> {
    let prompts: Vec<String> = prompts.iter().map(|prompt| (*prompt).to_owned()).collect();
    let mut h = scene_with(&models(), model, batch_making(), &[], &prompts);
    h.state_mut().open_canvas(S);
    h.run();
    h
}

fn row(h: &Harness<'static, NodeCraftApp>, id: NodeId) -> Node {
    h.state().session.document().canvases().get(&S).unwrap().nodes().get(&id).unwrap().clone()
}

fn param(h: &Harness<'static, NodeCraftApp>, id: NodeId, name: &str) -> Option<Value> {
    row(h, id).params.get(name).cloned()
}

fn set_model(h: &mut Harness<'static, NodeCraftApp>, rows: &[NodeId], model: &str) {
    let session = &mut h.state_mut().session;
    let preview = session.dispatch("node.model_preview", &json!({"canvas": S, "nodes": rows, "model": model})).unwrap();
    let revision = session.revision();
    session
        .dispatch("node.set_model", &json!({"canvas": S, "nodes": rows, "model": model, "expected_revision": revision, "preview_binding": preview["binding"]}))
        .unwrap();
    h.run();
}

/// Opens the combo box labelled `label` and picks `option` from it.
fn pick(h: &mut Harness<'static, NodeCraftApp>, label: &str, option: &str) {
    let at = h.get_by_role_and_label(Role::ComboBox, label).rect().center();
    click(h, at);
    let at = h.get_all_by_label(option).last().unwrap().rect().center();
    click(h, at);
}

/// A click whose frames keep repainting (a window fading out), stepped rather than settled.
fn click_steps(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
    h.hover_at(pos);
    h.run_steps(2);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run_steps(2);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(10);
}

fn choose_model(h: &mut Harness<'static, NodeCraftApp>, model: &str) {
    h.event(egui::Event::Text(model.into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.query_by_label("Apply model change").is_some(), "choosing a model shows its review first");
}

#[test]
fn a_rows_aspect_and_model_are_set_from_the_table_and_a_swap_says_what_it_drops() {
    let mut h = batch("mock-wide", &["a fox", "an owl"]);
    let rows = rows_of(&h.state().session);
    pick(&mut h, "Aspect of row 1", "16:9");
    assert_eq!(param(&h, rows[0], "aspect"), Some(Value::Text("16:9".into())), "the aspect comes from the row's parameter contract");
    assert_ne!(param(&h, rows[1], "aspect"), Some(Value::Text("16:9".into())), "the other row keeps its own");

    // A row's own model button opens the picker for that row alone.
    press(&mut h, Role::Button, "Model of row 2: mock-wide");
    assert_eq!(h.state().model_choice.as_ref().unwrap().nodes(), [rows[1]]);
    h.key_press(egui::Key::Escape);
    h.run();
    h.state_mut().model_choice = None;
    h.state_mut().selection.clear();
    h.run();

    // With nothing selected, Model… is for every row, and the review names what is dropped.
    press(&mut h, Role::Button, "Model…");
    let mut all = rows.clone();
    all.sort_unstable();
    assert_eq!(h.state().model_choice.as_ref().unwrap().nodes(), all);
    choose_model(&mut h, "mock-square");
    assert_eq!(h.query_all_by_label_contains("Drop aspect").count(), 2, "the swap reports the setting the new model doesn't have, per row");
    let at = h.get_by_label("Apply model change").rect().center();
    click_steps(&mut h, at);
    for id in &rows {
        assert_eq!(param(&h, *id, "model"), Some(Value::Text("mock-square".into())));
        assert_eq!(param(&h, *id, "aspect"), None);
    }
    assert_eq!(h.query_all_by_label("Not offered").count(), 2, "an aspect the model doesn't have is said, not implied");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(param(&h, rows[0], "model"), Some(Value::Text("mock-wide".into())), "the swap is one undo step");
    assert_eq!(param(&h, rows[0], "aspect"), Some(Value::Text("16:9".into())));
}

#[test]
fn settings_for_selected_rows_show_mixed_values_and_set_them_for_every_row() {
    let mut h = batch("mock-wide", &["a fox", "an owl", "a hare"]);
    let rows = rows_of(&h.state().session);
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": S, "id": rows[0], "name": "aspect", "value": Value::Text("16:9".into())})).unwrap();
    assert!(h.get_by_role_and_label(Role::Button, "Settings").accesskit_node().is_disabled(), "settings need a selection");
    press(&mut h, Role::Button, "Select all");
    press(&mut h, Role::Button, "Settings");
    let window = h.get_by_role_and_label(Role::Window, "Row settings");
    assert!(window.query_by_label("3 selected rows").is_some());
    assert!(window.query_all_by_label("Mixed").next().is_some(), "rows that differ say so");
    let at = window.get_by_role_and_label(Role::ComboBox, "Aspect").rect().center();
    click(&mut h, at);
    let at = h.get_all_by_label("16:9").last().unwrap().rect().center();
    click(&mut h, at);
    for id in &rows {
        assert_eq!(param(&h, *id, "aspect"), Some(Value::Text("16:9".into())));
    }
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_ne!(param(&h, rows[1], "aspect"), Some(Value::Text("16:9".into())), "every row's change is one undo step");
    assert_eq!(param(&h, rows[0], "aspect"), Some(Value::Text("16:9".into())));

    // One row has no card to edit it on, so the same panel edits it alone.
    h.state_mut().selection.clear();
    h.state_mut().selection.toggle(rows[2]);
    h.run();
    let window = h.get_by_role_and_label(Role::Window, "Row settings");
    assert!(window.query_by_label("1 selected row").is_some());
    let at = window.get_by_role_and_label(Role::ComboBox, "Aspect").rect().center();
    click(&mut h, at);
    let at = h.get_all_by_label("16:9").last().unwrap().rect().center();
    click(&mut h, at);
    assert_eq!(param(&h, rows[2], "aspect"), Some(Value::Text("16:9".into())));
    assert_ne!(param(&h, rows[1], "aspect"), Some(Value::Text("16:9".into())));
}

fn png() -> Vec<u8> {
    nodecraft_media::encode_png(8, 8, &[180; 8 * 8 * 4], &Default::default()).unwrap()
}

/// Imports image files onto the sheet, left of the rows, as a drop would.
fn images(h: &mut Harness<'static, NodeCraftApp>, first: u8, names: &[&str]) -> Vec<NodeId> {
    let bytes = png();
    let ids: Vec<NodeId> = (0..names.len()).map(|n| NodeId::from_bytes([first.wrapping_add(u8::try_from(n).unwrap()); 16])).collect();
    let files: Vec<nodecraft_engine::import::Incoming<'_>> = names.iter().map(|name| nodecraft_engine::import::Incoming { name, bytes: &bytes }).collect();
    nodecraft_engine::import::import_files(&mut h.state_mut().session, S, nodecraft_graph::Position { x: -480.0, y: 0.0 }, 200.0, &files, &ids).unwrap();
    h.run();
    ids
}

fn references(h: &mut Harness<'static, NodeCraftApp>) -> Vec<Vec<Json>> {
    let listed = h.state_mut().session.dispatch("sheet.rows", &json!({"canvas": S})).unwrap();
    listed["rows"].as_array().unwrap().iter().map(|row| row["references"].as_array().unwrap().iter().map(|wire| wire["node"].clone()).collect()).collect()
}

fn menu(h: &mut Harness<'static, NodeCraftApp>, item: &str) {
    press(h, Role::Button, "References");
    let at = h.get_by_label_contains(item).rect().center();
    click(h, at);
}

#[test]
fn references_are_wired_into_every_row_or_the_selected_ones_and_checked_against_the_model() {
    let mut h = batch("mock-square", &["a fox", "an owl"]);
    let rows = rows_of(&h.state().session);
    let [picture] = images(&mut h, 150, &["pic.png"])[..] else { panic!("one image") };
    menu(&mut h, "into every row");
    assert_eq!(references(&mut h), [vec![json!(picture)], vec![json!(picture)]], "a shared reference");
    assert_eq!(h.query_all_by_label_contains("(shared)").count(), 2, "the table marks it shared");

    // A row's references are checked together against its model when it is quoted: this model
    // takes one image, so a second one wired in blocks the row, with the reason, before any cost.
    let [second] = images(&mut h, 160, &["two.png"])[..] else { panic!("one image") };
    h.state_mut().selection.toggle(rows[0]);
    h.run();
    menu(&mut h, "Wire \"");
    assert_eq!(references(&mut h)[0], [json!(picture), json!(second)], "per-row references follow the shared one");
    let quoted = h.state_mut().session.dispatch("run.quote", &json!({"canvas": S, "mode": "selected", "nodes": [rows[0]]})).unwrap();
    let reasons = format!("{} {}", quoted["blocked"], quoted["blocks"]);
    assert_eq!(quoted["allowed"], false, "{quoted}");
    assert!(reasons.contains("no mode of this node accepts these inputs"), "the quote says why: {quoted}");
    h.state_mut().session.dispatch("sheet.reference_unwire", &json!({"canvas": S, "reference": second})).unwrap();
    h.run();
    menu(&mut h, "Unwire \"");
    assert_eq!(references(&mut h), [vec![], vec![json!(picture)]], "unwired from the selected row only");
}

#[test]
fn retry_failed_runs_only_the_failed_rows_and_cancel_stops_a_running_one() {
    let mut h = batch("mock-square", &["a fox", "an owl", "a hare"]);
    let rows = rows_of(&h.state().session);
    set_model(&mut h, &rows[1..2], "mock-broken");
    assert!(h.get_by_role_and_label(Role::Button, "Retry failed").accesskit_node().is_disabled());
    run_all(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.failure(S, rows[1]).is_some() && !runs.is_busy()));
    assert_eq!((history_of(h.state(), rows[0]).len(), history_of(h.state(), rows[2]).len()), (1, 1), "the others kept their results");
    h.state_mut().sheet.status = super::super::sheet_view::StatusFilter::Failed;
    h.run();
    assert_eq!(h.query_all_by_role(Role::MultilineTextInput).count(), 1, "the failed row is filtered as failed");
    h.state_mut().sheet.status = super::super::sheet_view::StatusFilter::All;
    h.run();
    press(&mut h, Role::Button, "Retry 1 failed");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(h.state().run_confirm.as_ref().unwrap().nodes(), [rows[1]], "only the failed row is quoted again");
    h.state_mut().run_confirm = None;
    h.run();

    set_model(&mut h, &rows[2..3], "mock-slow");
    h.state_mut().request_runs(vec![rows[2]], super::super::generate::RunKind::Generate).unwrap();
    frames_until(&mut h, |app| app.run_confirm.is_some());
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(&mut h, Role::Button, &label);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.card_status(S, rows[2]).is_some()));
    press(&mut h, Role::Button, "Cancel 1");
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.cancelled(S, rows[2])));
    h.run();
    assert!(h.query_by_label("Cancelled").is_some(), "the row says it was cancelled");
    assert_eq!(history_of(h.state(), rows[2]).len(), 1, "the earlier result stays");
    h.state_mut().sheet.status = super::super::sheet_view::StatusFilter::Cancelled;
    h.run();
    assert_eq!(h.query_all_by_role(Role::MultilineTextInput).count(), 1);
}

fn run_all(h: &mut Harness<'static, NodeCraftApp>) {
    press(h, Role::Button, "Run all");
    frames_until(h, |app| app.run_confirm.is_some());
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(h, Role::Button, &label);
}

#[test]
fn a_batch_is_named_from_its_header_and_reopened_by_name_from_the_sheets_panel() {
    let mut h = batch("mock-square", &["a fox"]);
    press(&mut h, Role::Button, "Rename");
    retype(&mut h, "Name", 0, "Foxes at dusk");
    press(&mut h, Role::Button, "Save name");
    assert_eq!(h.state().session.document().canvases().get(&S).unwrap().title(), "Foxes at dusk");
    assert!(h.query_by_label("Foxes at dusk").is_some(), "the header shows the new name");
    open_sheets(&mut h);
    press(&mut h, Role::Button, "Canvas");
    assert!(!h.state().on_sheet());
    open_sheets(&mut h);
    press(&mut h, Role::Button, "Foxes at dusk (sheet)");
    assert_eq!(h.state().canvas, S, "reopened by its name");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document().canvases().get(&S).unwrap().title(), "Foxes", "renaming is one undo step");
}

/// `count` rows on `model`, each with one recorded image result (one shared asset), and the app.
pub(in crate::app) fn full_sheet(count: usize) -> NodeCraftApp {
    let prompts: Vec<String> = (0..count).map(|n| format!("prompt {n}")).collect();
    let mut app = app_with(&models(), "mock-square", batch_making(), &[], &prompts);
    let session = &mut app.session;
    let hash = session.import_asset(&png()).unwrap();
    let image = Value::Media(nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() });
    let rows: Vec<Node> = nodecraft_engine::sheet_cmds::rows(session.document().canvases().get(&S).unwrap()).into_iter().cloned().collect();
    for (chunk_index, chunk) in rows.chunks(100).enumerate() {
        let results: Vec<Json> = chunk
            .iter()
            .enumerate()
            .map(|(n, row)| {
                let outputs: Vec<Value> = row
                    .outputs
                    .iter()
                    .map(|port| match (port.port_type.modality, port.port_type.many) {
                        (Modality::Image, true) => Value::List(vec![image.clone()]),
                        (Modality::Image, false) => image.clone(),
                        _ => Value::List(Vec::new()),
                    })
                    .collect();
                let id = nodecraft_types::GenerationId::from_bytes(u128::try_from(chunk_index * 100 + n + 1).unwrap().to_be_bytes());
                json!({"node": row.id, "generation": nodecraft_graph::Generation::new(id, outputs)})
            })
            .collect();
        session.dispatch("run.record", &json!({"canvas": S, "results": results})).unwrap();
    }
    app.open_canvas(S);
    app.sheet.mode = super::super::sheet_view::ViewMode::Grid;
    app
}

#[test]
fn a_full_sheets_grid_lays_out_only_the_tiles_in_view() {
    let mut h =
        Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), full_sheet(1000));
    h.run();
    let tiles = h.query_all_by_label_contains("Row ").filter(|node| node.accesskit_node().role() == Role::CheckBox).count();
    assert!(tiles > 0 && tiles < 120, "{tiles} tiles laid out of 1000");
    assert!(h.query_by_label("Row 1").is_some() && h.query_by_label("Row 1000").is_none());
    let frames = 30;
    let start = Instant::now();
    for _ in 0..frames {
        h.step();
    }
    println!("1000-row grid: {:?} per frame", start.elapsed() / frames);
    // Scrolled to the end, the last tiles are laid out instead of the first.
    for _ in 0..200 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Page,
            phase: egui::TouchPhase::Move,
            delta: Vec2::new(0.0, -1.0),
            modifiers: egui::Modifiers::NONE,
        });
        h.hover_at(Pos2::new(600.0, 500.0));
        h.step();
    }
    h.run();
    assert!(h.query_by_label("Row 1000").is_some() && h.query_by_label("Row 1").is_none(), "only the rows in view, at the end too");
}
