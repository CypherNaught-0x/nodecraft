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
[[model.modes]]
id = "image-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }, { role = "image", modality = "image", min = 1, max = 1 }]
"#;

fn making() -> Make {
    let mock = MockProvider::new("mock", [("mock-image".to_owned(), MockModel::new(MockOutput::Image))]);
    Box::new(move |adapter, _| if adapter == "mock" { Ok(Arc::new(mock.clone())) } else { Err(Readiness::NotSetUp("not in this test".into())) })
}

/// A project with a plain canvas, opened on it; `sheet` also adds sheet `S` with these prompts.
fn scene(sheet: &[&str]) -> Harness<'static, NodeCraftApp> {
    scene_of(&sheet.iter().map(|prompt| (*prompt).to_owned()).collect::<Vec<_>>())
}

fn scene_of(sheet: &[String]) -> Harness<'static, NodeCraftApp> {
    scene_in(&[], sheet)
}

/// [`scene_of`] with more plain canvases.
fn scene_in(canvases: &[(CanvasId, &str)], sheet: &[String]) -> Harness<'static, NodeCraftApp> {
    scene_with(MOCK_IMAGE, "mock-square", making(), canvases, sheet)
}

/// [`scene_in`] over `catalog`, the rows on `model`, runs made by `make`.
fn scene_with(catalog: &str, model: &str, make: Make, canvases: &[(CanvasId, &str)], sheet: &[String]) -> Harness<'static, NodeCraftApp> {
    let app = app_with(catalog, model, make, canvases, sheet);
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

/// The app [`scene_with`] shows, opened on the plain canvas.
pub(super) fn app_with(catalog: &str, model: &str, make: Make, canvases: &[(CanvasId, &str)], sheet: &[String]) -> NodeCraftApp {
    let catalog = nodecraft_catalog::Catalog::from_toml(catalog).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([103; 16]), "Sheets").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, title) in canvases {
        Change::AddCanvas { canvas: Canvas::new(*id, *title) }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut services = Services::default();
    let runs = Runs::with_maker(&mut services, make).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    runs.install(&mut session);
    if !sheet.is_empty() {
        session.dispatch("sheet.create", &json!({"canvas": S, "title": "Foxes"})).unwrap();
        // The row commands add at most 100 at a time.
        for (n, chunk) in sheet.chunks(100).enumerate() {
            let seed = NodeId::from_bytes([104u8.wrapping_add(u8::try_from(n).unwrap()); 16]);
            session.dispatch("sheet.rows_add", &json!({"canvas": S, "seed": seed, "prompts": chunk})).unwrap();
        }
        // And a model swap reports on at most so many cards at once.
        for rows in rows_of(&session).chunks(100) {
            let preview = session.dispatch("node.model_preview", &json!({"canvas": S, "nodes": rows, "model": model})).unwrap();
            let revision = session.revision();
            session
                .dispatch(
                    "node.set_model",
                    &json!({"canvas": S, "nodes": rows, "model": model, "expected_revision": revision, "preview_binding": preview["binding"]}),
                )
                .unwrap();
        }
    }
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.runs = Some(runs);
    app
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
    crate::app::topbar_tests::reveal(h, label);
    settle(h);
    let pos = h.get_by_role_and_label(role, label).rect().center();
    click(h, pos);
}

/// A few frames for layouts that measure over two (the grid, a growing panel), so a control is
/// read where it will be clicked. A worker may keep repainting, so a fixed number is stepped.
fn settle(h: &mut Harness<'static, NodeCraftApp>) {
    for _ in 0..4 {
        let ctx = h.ctx.clone();
        h.state_mut().tick_runs(&ctx);
        h.step();
    }
}

fn click(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
    h.hover_at(pos);
    h.run();
    // Press and release one frame apart: `run` can step many frames while something repaints, and
    // a press held past egui's longest click no longer counts as a click.
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
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
        // A worker (a run, a save) may keep repainting; the deadline bounds the wait instead.
        let _ = h.try_run();
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
fn disabled_narrow_sheet_more_preserves_prompt_focus_and_uncommitted_text() {
    let mut h = scene(&["a fox"]);
    h.state_mut().open_canvas(S);
    h.set_size(Vec2::new(480.0, 700.0));
    h.run();
    let prompt = h.query_all_by_role(Role::MultilineTextInput).next().unwrap();
    prompt.focus();
    h.run();
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text(" at dusk".into()));
    h.run();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let focus = h.ctx.memory(|m| m.focused());
    let draft = h.query_all_by_role(Role::MultilineTextInput).next().unwrap().value().unwrap();
    assert!(h.state().sheet_drafts_pending());
    assert!(h.get_by_role_and_label(Role::Button, "More").accesskit_node().is_disabled());
    let at = h.get_by_role_and_label(Role::Button, "More").rect().center();
    click(&mut h, at);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.query_all_by_role(Role::MultilineTextInput).next().unwrap().value().unwrap(), draft);
    assert!(h.state().sheet_drafts_pending());
    assert_eq!(h.ctx.memory(|m| m.focused()), focus);
    assert!(!egui::Popup::is_id_open(&h.ctx, topbar::popup_id()));
}

/// Clicks into the prompt field showing `prompt` and types `more` at its end.
fn type_into(h: &mut Harness<'static, NodeCraftApp>, prompt: &str, more: &str) {
    let field = h.query_all_by_role(Role::MultilineTextInput).find(|node| node.value().as_deref() == Some(prompt)).unwrap().rect().center();
    click(h, field);
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text(more.into()));
    h.run();
}

fn on_sheet(prompts: &[&str]) -> Harness<'static, NodeCraftApp> {
    let mut h = scene(prompts);
    h.state_mut().open_canvas(S);
    h.run();
    h
}

#[test]
fn undoing_the_sheet_that_is_open_goes_back_to_the_canvas() {
    let mut h = scene(&[]);
    open_sheets(&mut h);
    h.get_by_role_and_label(Role::TextInput, "Title").focus();
    h.run();
    h.event(egui::Event::Text("Owls".into()));
    h.run();
    press(&mut h, Role::Button, "New sheet");
    assert!(h.state().on_sheet());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document().canvases().len(), 1, "the sheet is undone");
    assert_eq!(h.state().canvas, C, "and the view is back on the canvas, not on nothing");
    assert!(!h.state().on_sheet());
}

#[test]
fn a_project_opens_on_its_canvas_even_when_a_sheet_sorts_first() {
    let mut project = Project::new(ProjectId::from_bytes([103; 16]), "Sheets").unwrap();
    Change::AddCanvas { canvas: Canvas::sheet(CanvasId::from_bytes([1; 16]), "Sheet") }.apply(&mut project).unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    assert_eq!(super::sheet_view::home_canvas(&project), Some(C));
}

#[test]
fn a_published_technique_is_neither_home_nor_listed_with_the_sheets() {
    let mut h = scene(&[]);
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let supplied: nodecraft_types::ParamValues = [("text".to_owned(), Value::Text("a fox".into()))].into_iter().collect();
    let node = kinds.get("source.text").unwrap().instantiate_instance(NodeId::from_bytes([41; 16]), Default::default(), &supplied).unwrap();
    h.state_mut().session.dispatch("node.add", &json!({"canvas": C, "node": node})).unwrap();
    // Its canvas id sorts before the working canvas's.
    let published = CanvasId::from_bytes([1; 16]);
    let technique = nodecraft_types::TechniqueId::from_bytes([2; 16]);
    h.state_mut()
        .session
        .dispatch(
            "technique.publish",
            &json!({"canvas": C, "published": published, "seed": NodeId::from_bytes([3; 16]), "technique": technique, "name": "Fox text",
                "outputs": [{"node": NodeId::from_bytes([41; 16])}]}),
        )
        .unwrap();
    assert_eq!(super::sheet_view::home_canvas(h.state().session.document()), Some(C));
    h.run();
    open_sheets(&mut h);
    assert!(h.query_by_label_contains("Fox text").is_none(), "a published version isn't a canvas to open");
    assert!(h.query_by_label("Canvas").is_some());
}

#[test]
fn a_prompt_still_being_typed_is_kept_when_a_dialog_takes_the_input() {
    let mut h = on_sheet(&["a fox", "an owl"]);
    type_into(&mut h, "a fox", " at dusk");
    let owl = rows_of(&h.state().session)[1];
    // A quote opening (here for the other row) blocks the table while the field still has focus.
    h.state_mut().request_runs(vec![owl], super::generate::RunKind::Generate).unwrap();
    frames_until(&mut h, |app| app.run_confirm.is_some());
    h.run();
    assert_eq!(prompts(h.state(), S), ["a fox at dusk", "an owl"]);
}

#[test]
fn the_save_key_commits_a_prompt_being_typed_first() {
    let mut h = on_sheet(&["a fox"]);
    type_into(&mut h, "a fox", " at dusk");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    h.run();
    assert_eq!(prompts(h.state(), S), ["a fox at dusk"]);
    assert!(!h.state().sheet_drafts_pending());
}

#[test]
fn the_undo_key_is_the_text_fields_while_a_prompt_has_focus() {
    let mut h = on_sheet(&["a fox"]);
    type_into(&mut h, "a fox", " at dusk");
    let revision = h.state().session.revision();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.revision(), revision, "the document's undo didn't run");
}

#[test]
fn a_rows_run_button_includes_the_prompt_typed_into_it() {
    let mut h = on_sheet(&["a fox", "an owl"]);
    type_into(&mut h, "a fox", " at dusk");
    let fox = rows_of(&h.state().session)[0];
    let run = h.query_all_by_role_and_label(Role::Button, "Run").next().unwrap().rect().center();
    click(&mut h, run);
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(prompts(h.state(), S), ["a fox at dusk", "an owl"], "leaving the field for Run committed it");
    let confirm = h.state().run_confirm.as_ref().unwrap();
    assert_eq!(confirm.nodes(), [fox]);
    assert!(!confirm.drafts(), "nothing typed is left out of the run");
}

#[test]
fn run_selected_quotes_only_the_checked_rows_and_up_moves_a_row() {
    let mut h = on_sheet(&["a fox", "an owl"]);
    let up = h.query_all_by_role_and_label(Role::Button, "Up").nth(1).unwrap().rect().center();
    click(&mut h, up);
    assert_eq!(prompts(h.state(), S), ["an owl", "a fox"]);
    let owl = rows_of(&h.state().session)[0];
    let first = h.query_all_by_role(Role::CheckBox).next().unwrap().rect().center();
    click(&mut h, first);
    press(&mut h, Role::Button, "Run 1 selected");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(h.state().run_confirm.as_ref().unwrap().nodes(), [owl]);
}

#[test]
fn a_full_sheet_lays_out_only_the_rows_in_view() {
    let prompts: Vec<String> = (0..1000).map(|n| format!("prompt {n}")).collect();
    let mut h = scene_of(&prompts);
    h.state_mut().open_canvas(S);
    h.run();
    let fields = h.query_all_by_role(Role::MultilineTextInput).count();
    assert!(fields > 0 && fields < 30, "{fields} prompt fields laid out");
    let frames = 60;
    let start = Instant::now();
    for _ in 0..frames {
        h.step();
    }
    println!("1000-row sheet: {:?} per frame", start.elapsed() / frames);
    // An edit reads the rows again, once.
    let first = rows_of(&h.state().session)[0];
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": S, "id": first, "name": "prompt", "value": Value::Text("edited".into())})).unwrap();
    let start = Instant::now();
    h.step();
    println!("1000-row sheet: {:?} for the frame after an edit", start.elapsed());
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

/// Focuses the text field labelled `label` (the `nth` of them), clears it and types `text`.
fn retype(h: &mut Harness<'static, NodeCraftApp>, label: &str, nth: usize, text: &str) {
    let field = h.query_all_by_label(label).filter(|node| matches!(node.accesskit_node().role(), Role::TextInput | Role::MultilineTextInput)).nth(nth).unwrap();
    let at = field.rect().center();
    click(h, at);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Backspace);
    h.run();
    for (n, part) in text.split('\n').enumerate() {
        if n > 0 {
            h.key_press(egui::Key::Enter);
        }
        h.event(egui::Event::Text(part.into()));
        h.run();
    }
    h.run();
}

fn open_template(h: &mut Harness<'static, NodeCraftApp>) {
    let at = h.get_by_label("From template").rect().center();
    click(h, at);
    assert!(h.state().sheet.template.is_some(), "the template panel is open");
}

#[test]
fn a_template_previews_its_count_then_expands_into_rows_as_one_undo_step() {
    let mut h = on_sheet(&["a fox"]);
    open_template(&mut h);
    assert_eq!(h.state().sheet.template.as_ref().unwrap().text, "a fox", "it starts from the last row's prompt");
    retype(&mut h, "Template", 0, "a {{colour}} fox at {{time}}");
    retype(&mut h, "Variable", 0, "colour");
    retype(&mut h, "Values", 0, "red\nblue");
    // An undefined variable is reported before anything is made.
    assert!(h.query_by_label_contains("which no variable defines").is_some());
    assert!(h.get_by_role_and_label(Role::Button, "Expand").accesskit_node().is_disabled());
    press(&mut h, Role::Button, "Add variable");
    retype(&mut h, "Variable", 1, "time");
    retype(&mut h, "Values", 1, "dawn\ndusk\nnight");
    assert!(h.query_by_label_contains("6 new rows (2 colour × 3 time)").is_some(), "the count shows before the rows are made");
    assert!(h.query_by_label("a red fox at dawn").is_some(), "and the first prompts");
    assert_eq!(prompts(h.state(), S), ["a fox"], "a preview makes nothing");
    press(&mut h, Role::Button, "Expand into 6 rows");
    assert_eq!(
        prompts(h.state(), S),
        ["a fox", "a red fox at dawn", "a red fox at dusk", "a red fox at night", "a blue fox at dawn", "a blue fox at dusk", "a blue fox at night"]
    );
    assert!(h.state().sheet.template.is_none(), "the panel closes once the rows are made");
    assert!(h.state().run_confirm.is_none(), "expanding runs nothing");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(prompts(h.state(), S), ["a fox"], "one undo removes them all");
}

#[test]
fn a_template_too_big_to_expand_says_how_many_and_stays_disabled() {
    let mut h = on_sheet(&["a fox"]);
    open_template(&mut h);
    retype(&mut h, "Template", 0, "{{n}}");
    retype(&mut h, "Variable", 0, "n");
    let values: Vec<String> = (0..11).map(|n| n.to_string()).collect();
    retype(&mut h, "Values", 0, &values.join("\n"));
    h.state_mut().sheet.template.as_mut().unwrap().copies = 10;
    h.run();
    h.run();
    assert!(h.query_by_label_contains("makes 110 rows").is_some());
    assert!(h.get_by_role_and_label(Role::Button, "Expand").accesskit_node().is_disabled());
    // Copies without variables repeat the template.
    retype(&mut h, "Template", 0, "an owl");
    retype(&mut h, "Variable", 0, "");
    retype(&mut h, "Values", 0, "");
    h.state_mut().sheet.template.as_mut().unwrap().copies = 3;
    h.run();
    h.run();
    press(&mut h, Role::Button, "Expand into 3 rows");
    assert_eq!(prompts(h.state(), S), ["a fox", "an owl", "an owl", "an owl"]);
}

#[test]
fn the_prompt_and_status_filters_narrow_the_table_and_select_what_they_show() {
    let mut h = on_sheet(&["a red fox", "an owl", "a fox at dusk"]);
    retype(&mut h, "Filter", 0, "FOX");
    let shown: Vec<String> = h.query_all_by_role(Role::MultilineTextInput).filter_map(|node| node.value()).collect();
    assert_eq!(shown, ["a red fox", "a fox at dusk"], "the filter ignores case");
    assert!(h.query_by_label("Showing 2 of 3").is_some());
    press(&mut h, Role::Button, "Select shown");
    let rows = rows_of(&h.state().session);
    assert!(h.state().selection.contains(rows[0]) && h.state().selection.contains(rows[2]) && !h.state().selection.contains(rows[1]));
    assert!(h.query_by_label("Run 2 selected").is_some());
    // Row numbers stay the sheet's own.
    assert!(h.query_by_label("3").is_some());

    retype(&mut h, "Filter", 0, "");
    let combo = h.query_all_by_role(Role::ComboBox).next().unwrap().rect().center();
    click(&mut h, combo);
    let done = h.get_by_label("Done").rect().center();
    click(&mut h, done);
    assert_eq!(h.state().sheet.status, super::sheet_view::StatusFilter::Done);
    assert!(h.query_by_label("No rows match the filter.").is_some(), "nothing has run yet");
    h.state_mut().sheet.status = super::sheet_view::StatusFilter::NotRun;
    h.run();
    assert_eq!(h.query_all_by_role(Role::MultilineTextInput).count(), 3);

    // Opening another canvas forgets the filter.
    h.state_mut().sheet.filter = "owl".into();
    h.state_mut().open_canvas(C);
    assert!(h.state().sheet.filter.is_empty() && h.state().sheet.status == super::sheet_view::StatusFilter::All);
}

/// Runs `rows` through the price confirmation and waits for each to have a new result.
fn run_rows(h: &mut Harness<'static, NodeCraftApp>, rows: &[NodeId]) {
    let before: Vec<usize> = rows.iter().map(|row| history_of(h.state(), *row).len()).collect();
    h.state_mut().request_runs(rows.to_vec(), super::generate::RunKind::Generate).unwrap();
    frames_until(h, |app| app.run_confirm.is_some());
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(h, Role::Button, &label);
    frames_until(h, |app| {
        rows.iter().zip(&before).all(|(row, count)| {
            history_of(app, *row).len() > *count && app.session.document().canvases().get(&S).unwrap().nodes().get(row).unwrap().active.is_some()
        })
    });
    // And until the runs are done with them, so nothing recorded late changes the active result.
    frames_until(h, |app| app.runs.as_ref().is_none_or(|runs| rows.iter().all(|row| runs.card_status(S, *row).is_none())));
    h.run();
}

fn history_of(app: &NodeCraftApp, row: NodeId) -> Vec<nodecraft_types::GenerationId> {
    app.session.document().canvases().get(&S).unwrap().nodes().get(&row).unwrap().history.iter().map(|g| g.id).collect()
}

fn show_grid(h: &mut Harness<'static, NodeCraftApp>) {
    settle(h);
    let at = h.get_by_label("Grid").rect().center();
    click(h, at);
    assert_eq!(h.state().sheet.mode, super::sheet_view::ViewMode::Grid);
    // The grid lays its tiles out over two frames: read them where they settle.
    settle(h);
}

#[test]
fn the_grid_shows_rows_with_results_badged_and_selects_and_sends_them_to_the_canvas() {
    let mut h = on_sheet(&["a fox", "an owl", "a hare"]);
    let rows = rows_of(&h.state().session);
    run_rows(&mut h, &rows[..2]);
    show_grid(&mut h);
    assert!(h.query_by_label("Row 1").is_some() && h.query_by_label("Row 2").is_some());
    assert!(h.query_by_label("Row 3").is_none(), "a row with no result has no tile");
    assert!(h.query_by_label("1 row shown has no result yet.").is_some());
    assert_eq!(h.query_all_by_label("Done").count(), 2, "each tile is badged with its row's status");
    assert_eq!(h.query_all_by_role(Role::MultilineTextInput).count(), 0, "the grid replaces the table");

    // The filters narrow the grid as they do the table.
    retype(&mut h, "Filter", 0, "owl");
    assert!(h.query_by_label("Row 1").is_none() && h.query_by_label("Row 2").is_some());
    retype(&mut h, "Filter", 0, "");
    h.state_mut().sheet.status = super::sheet_view::StatusFilter::NotRun;
    h.run();
    assert!(h.query_by_label("Row 1").is_none() && h.query_by_label("Row 2").is_none());
    h.state_mut().sheet.status = super::sheet_view::StatusFilter::All;
    h.run();

    // Clicking a tile selects its row, as its checkbox does in the table.
    settle(&mut h);
    let owl = h.get_by_label("Row 2").rect().center();
    click(&mut h, owl);
    assert!(h.state().selection.contains(rows[1]) && !h.state().selection.contains(rows[0]));
    press(&mut h, Role::Button, "Send 1 to canvas");
    let placed: Vec<&Node> = h.state().session.document().canvases().get(&C).unwrap().nodes().values().collect();
    assert_eq!(placed.len(), 1, "one source card on the canvas");
    assert_eq!(placed[0].kind, "source.image");
    let Some(Value::Text(recorded)) = placed[0].config.get(nodecraft_engine::sheet_cmds::place::RESULT_CONFIG) else { panic!("no provenance") };
    let recorded: serde_json::Value = serde_json::from_str(recorded).unwrap();
    assert_eq!(recorded["row"], json!(rows[1]));
    assert_eq!(recorded["generation"], json!(history_of(h.state(), rows[1])[0]));
    assert_eq!(h.state().notice.as_deref(), Some("Sent 1 result to \"Canvas\"."));
    assert!(h.state().on_sheet(), "the sheet stays open");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert!(h.state().session.document().canvases().get(&C).unwrap().nodes().is_empty(), "one undo takes the card back");
}

#[test]
fn a_row_run_again_keeps_its_older_result_and_the_grid_steps_through_them() {
    let mut h = on_sheet(&["a fox"]);
    let row = rows_of(&h.state().session)[0];
    run_rows(&mut h, &[row]);
    run_rows(&mut h, &[row]);
    let history = history_of(h.state(), row);
    assert_eq!(history.len(), 2, "a run adds a result and keeps the one before");
    let active = |h: &Harness<'static, NodeCraftApp>| h.state().session.document().canvases().get(&S).unwrap().nodes().get(&row).unwrap().active;
    assert_eq!(active(&h), Some(history[1]), "the newest is active");
    assert!(h.query_by_label("Done 2/2").is_some(), "the table says which result of how many");
    show_grid(&mut h);
    assert!(h.query_by_label("2 of 2").is_some());
    assert!(h.get_by_role_and_label(Role::Button, "Next result, row 1").accesskit_node().is_disabled());
    press(&mut h, Role::Button, "Previous result, row 1");
    assert_eq!(
        active(&h),
        Some(history[0]),
        "the older result is active again; history now {:?}, was {history:?}; notice {:?}; 1 of 2 shown {}",
        history_of(h.state(), row),
        h.state().notice,
        h.query_by_label("1 of 2").is_some()
    );
    assert!(h.query_by_label("1 of 2").is_some());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(active(&h), Some(history[1]), "stepping is one undo step");
}

#[cfg(unix)]
#[test]
fn export_asks_for_a_folder_and_saves_the_chosen_rows_active_results() {
    let mut h = on_sheet(&["a fox", "an owl"]);
    let rows = rows_of(&h.state().session);
    run_rows(&mut h, &rows);
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().exports.answer = Some(dir.path().to_path_buf());
    assert!(h.query_by_label("Export all").is_some(), "with nothing selected every result is offered");
    let first = h.query_all_by_role(Role::CheckBox).next().unwrap().rect().center();
    click(&mut h, first);
    press(&mut h, Role::Button, "Export 1");
    frames_until(&mut h, |app| app.export_pending() && app.notice.is_none());
    h.run();
    assert!(h.query_by_label_contains("1 media files; 1 ready").is_some(), "only the selected row's active result");
    // The save repaints while its worker writes, so this click is stepped, not settled.
    let pos = h.get_by_role_and_label(Role::Button, "Save files").rect().center();
    h.hover_at(pos);
    h.step();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    let names_in = |dir: &std::path::Path| {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    };
    // The save stages its files in a hidden folder first, so wait for the finished set rather
    // than an entry count (the staging folder alone used to satisfy `>= 2`).
    frames_until(&mut h, |_| {
        let names = names_in(dir.path());
        names.iter().any(|name| name == "workflow-results.json") && !names.iter().any(|name| name.starts_with(".nodecraft-results-"))
    });
    let names = names_in(dir.path());
    assert!(names.iter().any(|name| name == "workflow-results.json"), "{names:?}");
    assert_eq!(names.iter().filter(|name| name.ends_with(".png")).count(), 1, "{names:?}");
}

#[test]
fn a_template_takes_an_empty_value_and_an_image_variable_from_the_panel() {
    let mut h = on_sheet(&["a fox"]);
    let snow = NodeId::from_bytes([61; 16]);
    let mut node = h.state().session.kinds().get("source.image").unwrap().descriptor().instantiate(snow, &Default::default()).unwrap();
    node.title = "Snow".into();
    node.pos = nodecraft_graph::Position { x: -480.0, y: 0.0 };
    h.state_mut().session.dispatch("node.add", &json!({"canvas": S, "node": node})).unwrap();
    h.run();
    open_template(&mut h);
    retype(&mut h, "Template", 0, "a {{colour}} fox");
    retype(&mut h, "Variable", 0, "colour");
    retype(&mut h, "Values", 0, "red");
    let empty = h.get_by_label("Empty value").rect().center();
    click(&mut h, empty);
    assert!(h.query_by_label_contains("2 new rows").is_some(), "red, and nothing");
    press(&mut h, Role::Button, "Add variable");
    retype(&mut h, "Variable", 1, "backdrop");
    let images = h.query_all_by_label("Images").nth(1).unwrap().rect().center();
    click(&mut h, images);
    assert!(h.query_by_label_contains("needs 1 to 100 values").is_some(), "an image variable needs an image");
    assert!(h.query_by_label_contains("Expand into").is_none());
    let tick = h.get_by_label("Snow").rect().center();
    click(&mut h, tick);
    assert!(h.query_by_label_contains("2 new rows").is_some(), "the image is wired into each row: still 2");
    press(&mut h, Role::Button, "Expand into 2 rows");
    assert_eq!(prompts(h.state(), S), ["a fox", "a red fox", "a  fox"]);
    let listed = h.state_mut().session.dispatch("sheet.rows", &json!({"canvas": S})).unwrap();
    for row in 1..3 {
        assert_eq!(listed["rows"][row]["references"][0]["node"], json!(snow), "each new row has the image wired in");
    }
}

#[test]
fn export_offers_the_selected_results_else_all_and_refuses_too_many() {
    use super::sheet_view::{MAX_EXPORT_ROWS, export_choice};
    let ids: Vec<NodeId> = (0..=MAX_EXPORT_ROWS)
        .map(|n| NodeId::from_bytes([u8::try_from(n % 250).unwrap(), u8::try_from(n / 250).unwrap(), 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]))
        .collect();
    let all = export_choice(Vec::new(), ids[..3].to_vec());
    assert_eq!((all.label.as_str(), all.rows.len(), all.problem.is_none()), ("Export all", 3, true), "selected rows without results fall back to all");
    let some = export_choice(vec![ids[1]], ids[..3].to_vec());
    assert_eq!((some.label.as_str(), some.rows.clone()), ("Export 1", vec![ids[1]]));
    let none = export_choice(Vec::new(), Vec::new());
    assert!(none.problem.is_some());
    let many = export_choice(Vec::new(), ids.clone());
    assert!(many.problem.unwrap().contains("at most 256 rows"), "the shared save takes at most 256 nodes");
    assert!(export_choice(Vec::new(), ids[..MAX_EXPORT_ROWS].to_vec()).problem.is_none());
}

#[test]
fn a_text_result_shows_its_start_in_the_grid() {
    use super::sheet_view::{Preview, preview_of};
    let mut node = Node::new(NodeId::from_bytes([71; 16]), "generation.text");
    node.outputs = vec![nodecraft_graph::Port { name: "text".into(), port_type: PortType { modality: Modality::Text, many: false } }];
    assert_eq!(preview_of(&node, 8), Preview::Nothing);
    let made = nodecraft_types::GenerationId::from_bytes([72; 16]);
    node.history.push(nodecraft_graph::Generation::new(made, vec![Value::Text("a quiet fox in the snow".into())]));
    node.active = Some(made);
    assert_eq!(preview_of(&node, 8), Preview::Text("a quiet ".into()));
}

#[test]
fn results_go_to_the_canvas_open_before_the_sheet_or_the_one_picked() {
    // Its id sorts after the first canvas's, so it is not the project's home canvas.
    let other = CanvasId::from_bytes([109; 16]);
    let mut h = scene_in(&[(other, "Moodboard")], &["a fox".to_owned()]);
    assert_eq!(h.state().canvas, C);
    h.state_mut().open_canvas(other);
    h.state_mut().open_canvas(S);
    h.run();
    let row = rows_of(&h.state().session)[0];
    run_rows(&mut h, &[row]);
    assert_eq!(h.state().place_target(), Some(other), "the canvas open before the sheet");
    let first = h.query_all_by_role(Role::CheckBox).next().unwrap().rect().center();
    click(&mut h, first);
    press(&mut h, Role::Button, "Send 1 to canvas");
    let on = |h: &Harness<'static, NodeCraftApp>, id: CanvasId| h.state().session.document().canvases().get(&id).unwrap().nodes().len();
    assert_eq!((on(&h, other), on(&h, C)), (1, 0));
    // The picker sends to another.
    let picker = h
        .query_all_by_role(Role::ComboBox)
        .find(|node| {
            node.value().is_some_and(|value| value.contains("Moodboard")) || node.accesskit_node().label().is_some_and(|label| label.contains("Moodboard"))
        })
        .unwrap()
        .rect()
        .center();
    click(&mut h, picker);
    let canvas = h.get_by_label("Canvas").rect().center();
    click(&mut h, canvas);
    assert_eq!(h.state().place_target(), Some(C));
    press(&mut h, Role::Button, "Send 1 to canvas");
    assert_eq!((on(&h, other), on(&h, C)), (1, 1));
}

#[test]
fn a_document_this_session_can_only_view_takes_no_results() {
    let mut h = on_sheet(&["a fox"]);
    let row = rows_of(&h.state().session)[0];
    run_rows(&mut h, &[row]);
    let first = h.query_all_by_role(Role::CheckBox).next().unwrap().rect().center();
    click(&mut h, first);
    h.state_mut().session.set_access(nodecraft_engine::access::Access::new(nodecraft_sync::Role::View, None).unwrap());
    h.run();
    press(&mut h, Role::Button, "Send 1 to canvas");
    assert!(h.state().session.document().canvases().get(&C).unwrap().nodes().is_empty());
    assert!(h.state().notice.is_some(), "the refusal is shown");
}

pub(super) mod batch;

/// #1113: a disabled sheet control still says why when hovered, instead of losing its tooltip.
#[test]
fn disabled_run_selected_and_cancel_explain_themselves() {
    let mut h = on_sheet(&["a fox", "an owl"]);
    for (label, why) in [("Run selected", "Select at least one row to run"), ("Cancel", "No rows are queued or running")] {
        let button = h.get_by_role_and_label(Role::Button, label);
        assert!(button.accesskit_node().is_disabled(), "{label} starts disabled");
        button.hover();
        for _ in 0..90 {
            h.step();
        }
        assert!(h.query_by_label(why).is_some(), "{label} explains: {why}");
        h.hover_at(egui::Pos2::ZERO);
        h.run();
    }
}

/// #1118: a sheet's controls all have spoken names, not bare glyphs.
#[test]
fn a_sheets_controls_have_names() {
    let h = on_sheet(&["a fox", "an owl"]);
    assert_eq!(crate::a11y_check::unnamed_controls(&h), Vec::<String>::new());
}
