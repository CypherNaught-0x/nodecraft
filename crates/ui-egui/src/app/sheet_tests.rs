//! The sheet view (#163): the Sheets panel makes and opens sheets, the table edits rows through
//! the `sheet.*` commands, and running rows quotes them together first. Runs use the mock
//! provider; nothing here calls a real one.
use super::runs::{Make, Runs};
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::{
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    provider_setup::Readiness,
    run_cmds::RunCatalog,
};
use nodecraft_provider_testkit::mock::{MockModel, MockOutput, MockProvider};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([101; 16]);
const S: CanvasId = CanvasId::from_bytes([102; 16]);

const MOCK_IMAGE: &str = r#"
[[model]]
id = "mock-square"
provider = "mock"
provider_model = "mock-image"
label = "Mock image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

fn making() -> Make {
    let mock = MockProvider::new("mock", [("mock-image".to_owned(), MockModel::new(MockOutput::Image))]);
    Box::new(move |adapter, _| if adapter == "mock" { Ok(Arc::new(mock.clone())) } else { Err(Readiness::NotSetUp("not in this test".into())) })
}

/// A project with a plain canvas, opened on it; `sheet` also adds sheet `S` with these prompts.
fn scene(sheet: &[&str]) -> Harness<'static, NodeCraftApp> {
    let catalog = nodecraft_catalog::Catalog::from_toml(MOCK_IMAGE).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([103; 16]), "Sheets").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut services = Services::default();
    let runs = Runs::with_maker(&mut services, making()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    runs.install(&mut session);
    if !sheet.is_empty() {
        session.dispatch("sheet.create", &json!({"canvas": S, "title": "Foxes"})).unwrap();
        session.dispatch("sheet.rows_add", &json!({"canvas": S, "seed": NodeId::from_bytes([104; 16]), "prompts": sheet})).unwrap();
        let rows = rows_of(&session);
        let preview = session.dispatch("node.model_preview", &json!({"canvas": S, "nodes": rows, "model": "mock-square"})).unwrap();
        let revision = session.revision();
        session
            .dispatch(
                "node.set_model",
                &json!({"canvas": S, "nodes": rows, "model": "mock-square", "expected_revision": revision, "preview_binding": preview["binding"]}),
            )
            .unwrap();
    }
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.runs = Some(runs);
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

fn rows_of(session: &Session) -> Vec<NodeId> {
    let canvas = session.document().canvases().get(&S).unwrap();
    nodecraft_engine::sheet_cmds::rows(canvas).iter().map(|row| row.id).collect()
}

/// The rows' prompts, top to bottom.
fn prompts(app: &NodeCraftApp, canvas: CanvasId) -> Vec<String> {
    let canvas = app.session.document().canvases().get(&canvas).unwrap();
    nodecraft_engine::sheet_cmds::rows(canvas)
        .iter()
        .map(|row| match row.params.get("prompt") {
            Some(Value::Text(text)) => text.clone(),
            _ => String::new(),
        })
        .collect()
}

fn press(h: &mut Harness<'static, NodeCraftApp>, role: Role, label: &str) {
    let pos = h.get_by_role_and_label(role, label).rect().center();
    click(h, pos);
}

fn click(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

fn open_sheets(h: &mut Harness<'static, NodeCraftApp>) {
    press(h, Role::Button, "Sheets");
    assert!(h.state().sheet.open);
}

fn frames_until(h: &mut Harness<'static, NodeCraftApp>, done: impl Fn(&NodeCraftApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let ctx = h.ctx.clone();
        h.state_mut().tick_runs(&ctx);
        h.run();
        if done(h.state()) {
            return;
        }
        assert!(Instant::now() < deadline, "never got there; notice: {:?}", h.state().notice);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_sheets_panel_makes_a_sheet_opens_it_as_a_table_and_goes_back() {
    let mut h = scene(&[]);
    open_sheets(&mut h);
    h.get_by_role_and_label(Role::TextInput, "Title").focus();
    h.run();
    h.event(egui::Event::Text("Owls".into()));
    h.run();
    press(&mut h, Role::Button, "New sheet");
    assert!(h.state().on_sheet(), "the new sheet is open");
    let sheet = h.state().canvas;
    assert_eq!(h.state().session.document().canvases().get(&sheet).unwrap().title(), "Owls");
    assert!(h.query_by_label_contains("No rows yet").is_some());

    press(&mut h, Role::Button, "Add row");
    assert_eq!(prompts(h.state(), sheet), vec![String::new()]);
    assert!(h.query_by_label("Not run").is_some(), "the row shows it has no result");

    // The undo key removes the row as one step, like any edit.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert!(prompts(h.state(), sheet).is_empty());

    open_sheets(&mut h);
    press(&mut h, Role::Button, "Canvas");
    assert_eq!(h.state().canvas, C);
    assert!(!h.state().on_sheet());
}

#[test]
fn a_prompt_typed_into_a_row_is_committed_and_rows_move_and_go() {
    let mut h = scene(&["a fox", "an owl"]);
    open_sheets(&mut h);
    press(&mut h, Role::Button, "Foxes (sheet)");
    assert_eq!(h.state().canvas, S);

    let field = h.query_all_by_role(Role::MultilineTextInput).find(|node| node.value().as_deref() == Some("a fox")).unwrap().rect().center();
    click(&mut h, field);
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text(" at dusk".into()));
    h.run();
    assert_eq!(prompts(h.state(), S), ["a fox", "an owl"], "nothing is committed while typing");
    // Leaving the field commits it.
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(prompts(h.state(), S), ["a fox at dusk", "an owl"]);

    let down = h.query_all_by_role_and_label(Role::Button, "Down").next().unwrap().rect().center();
    click(&mut h, down);
    assert_eq!(prompts(h.state(), S), ["an owl", "a fox at dusk"]);

    let first = h.query_all_by_role(Role::CheckBox).next().unwrap().rect().center();
    click(&mut h, first);
    press(&mut h, Role::Button, "Duplicate");
    assert_eq!(prompts(h.state(), S), ["an owl", "an owl", "a fox at dusk"]);
    press(&mut h, Role::Button, "Remove");
    assert_eq!(prompts(h.state(), S), ["an owl", "a fox at dusk"]);
}

#[test]
fn run_all_quotes_every_row_in_one_price_then_records_each() {
    let mut h = scene(&["a fox", "an owl"]);
    h.state_mut().open_canvas(S);
    h.run();
    press(&mut h, Role::Button, "Run all");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert!(h.query_all_by_label_contains("0.08").next().is_some(), "both rows are in one price");
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(&mut h, Role::Button, &label);
    frames_until(&mut h, |app| {
        let canvas = app.session.document().canvases().get(&S).unwrap();
        canvas.nodes().values().all(|row| row.active.is_some())
    });
    h.run();
    assert_eq!(h.query_all_by_label("Done").count(), 2);
}

#[test]
#[ignore = "manual public-safe capture; SHEET163_RENDER_DIR"]
fn sheet163_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("SHEET163_RENDER_DIR").unwrap());
    let mut h = scene(&["A red fox in fresh snow", "A barn owl at dusk", "A hare in tall grass"]);
    h.state_mut().open_canvas(S);
    h.run();
    press(&mut h, Role::Button, "Run all");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(&mut h, Role::Button, &label);
    frames_until(&mut h, |app| app.session.document().canvases().get(&S).unwrap().nodes().values().all(|row| row.active.is_some()));
    let first = h.query_all_by_role(Role::CheckBox).next().unwrap().rect().center();
    click(&mut h, first);
    h.state_mut().sheet.open = true;
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "sheet163-dark.png"), (crate::theme::ThemeKind::Light, "sheet163-light.png")] {
        h.state_mut().theme = theme;
        let mut shot = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        shot.render().unwrap().save(dir.join(name)).unwrap();
        h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), shot.into_state());
    }
}
