use super::*;
use egui_kittest::Harness;
use std::collections::BTreeSet;

pub(super) fn harness() -> Harness<'static, NodeCraftApp> {
    Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap())
}
fn node(app: &NodeCraftApp) -> &nodecraft_graph::Node {
    app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().next().unwrap()
}
fn screen(app: &NodeCraftApp) -> Rect {
    app.view.rect_to_screen(app.viewport.min, card::world_rect(node(app), None, app.theme.tokens()))
}
fn click(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
    h.hover_at(pos);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

fn double_click(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
    h.hover_at(pos);
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
        }
    }
}

#[test]
fn first_frame_centres_graph_at_100_percent() {
    let h = harness();
    assert_eq!(h.state().view.zoom, 1.0);
    assert!((screen(h.state()).center() - h.state().viewport.center()).length() < 1.0);
}

#[test]
fn background_pan_and_right_drag_preserve_document() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let pan = h.state().view.pan;
    h.drag_at(Pos2::new(50.0, 50.0));
    h.run();
    h.hover_at(Pos2::new(150.0, 90.0));
    h.run();
    h.drop_at(Pos2::new(150.0, 90.0));
    h.run();
    assert!((h.state().view.pan - pan - Vec2::new(100.0, 40.0)).length() < 1.0);
    let pan = h.state().view.pan;
    let p = screen(h.state()).left_center() + Vec2::new(5.0, 0.0);
    h.hover_at(p);
    h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Secondary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.hover_at(p + Vec2::new(30.0, 20.0));
    h.run();
    h.event(egui::Event::PointerButton {
        pos: p + Vec2::new(30.0, 20.0),
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!((h.state().view.pan - pan - Vec2::new(30.0, 20.0)).length() < 1.0);
}

#[test]
fn zoomed_card_drag_is_one_world_space_undo_step() {
    for zoom in [0.5, 1.0, 2.0] {
        let mut h = harness();
        h.state_mut().view.zoom = zoom;
        h.run();
        let before = h.state().session.document().clone();
        let pos = node(h.state()).pos;
        let p = screen(h.state()).left_center() + Vec2::new(5.0 * zoom, 0.0);
        h.drag_at(p);
        h.run();
        h.hover_at(p + Vec2::new(20.0, -10.0));
        h.run();
        h.hover_at(p + Vec2::new(60.0, -20.0));
        h.run();
        h.drop_at(p + Vec2::new(60.0, -20.0));
        h.run();
        let moved = node(h.state()).pos;
        assert!((moved.x - pos.x - 60.0 / zoom).abs() < 1.0, "zoom {zoom}: {moved:?}");
        assert!((moved.y - pos.y + 20.0 / zoom).abs() < 1.0);
        assert!(h.state().selection.contains(node(h.state()).id));
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run();
        assert_eq!(h.state().session.document(), &before);
        assert!(!h.state().session.can_undo());
        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
        h.run();
        assert_eq!(node(h.state()).pos, moved);
    }
}

#[test]
fn text_edit_commits_without_moving_card_and_can_be_undone() {
    let mut h = harness();
    let before = node(h.state()).clone();
    let rect = screen(h.state());
    click(&mut h, rect.min + Vec2::new(50.0, 55.0));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("Moonlit mountains".into()));
    h.run();
    // Canvas reset must not steal a text editing shortcut.
    let pan = h.state().view.pan;
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num0);
    h.run();
    assert_eq!(h.state().view.pan, pan);
    click(&mut h, Pos2::new(50.0, 50.0));
    assert_eq!(node(h.state()).params.get("text"), Some(&Value::Text("Moonlit mountains".into())));
    assert_eq!(node(h.state()).pos, before.pos);
    assert_eq!(card::title(node(h.state()), None), "Moonlit mountains");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(node(h.state()), &before);
}

#[test]
fn title_edit_and_escape_use_view_drafts() {
    let mut h = harness();
    let before = node(h.state()).clone();
    let p = screen(h.state()).left_top() + Vec2::new(50.0, -12.0);
    // Two clicks with a short interval produce the native double-click path.
    double_click(&mut h, p);
    h.run();
    h.event(egui::Event::Text("Custom name".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(node(h.state()).title, "Custom name");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(node(h.state()), &before);
    double_click(&mut h, p);
    h.run();
    h.event(egui::Event::Text("Cancelled".into()));
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(node(h.state()), &before);
}

#[test]
fn low_zoom_hides_body_controls_and_offscreen_nodes_are_culled() {
    let mut h = harness();
    h.state_mut().view.zoom = 0.2;
    h.run();
    assert!(h.query_by_role_and_label(Role::TextInput, "Text").is_none());
    assert!(h.query_by_role_and_label(Role::Button, "Rename A QUIET FOREST AT DAWN, SOFT LIGHT THROUGH THE TREES.").is_some());
    h.state_mut().view.pan = Vec2::splat(100_000.0);
    h.run();
    assert!(h.query_by_role_and_label(Role::TextInput, "Text").is_none());
    assert!(h.query_by_role_and_label(Role::Button, "Rename A QUIET FOREST AT DAWN, SOFT LIGHT THROUGH THE TREES.").is_none());
}

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{Choice, Integer, Number, ParamControl, ParamSchema, ParamSpec, PortType, Unsigned, VisibleIf};

struct TestKind(NodeDescriptor);
impl NodeKind for TestKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Err(Error::new(ErrorKind::Unsupported, "test-only kind"))
    }
}
fn spec(name: &str, control: ParamControl, default: Value) -> ParamSpec {
    ParamSpec { name: name.into(), label: name.into(), description: None, control, required: false, default: Some(default), visible_if: None }
}
fn schema_harness(params: Vec<ParamSpec>) -> Harness<'static, NodeCraftApp> {
    let descriptor = NodeDescriptor {
        id: "test.controls".into(),
        label: "Controls".into(),
        category: Category::Source,
        inputs: vec![],
        outputs: vec![],
        params: ParamSchema { params },
        quick_add: None,
    };
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Controls").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    Change::AddNode { canvas, node: descriptor.instantiate(NodeId::from_bytes([3; 16]), &BTreeMap::new()).unwrap() }.apply(&mut document).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(TestKind(descriptor))).unwrap();
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap())
}
fn enter_text(h: &mut Harness<'static, NodeCraftApp>, label: &str, text: &str) {
    h.get_by_role_and_label(Role::TextInput, label).click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text(text.into()));
    h.run();
}
#[test]
fn typed_controls_preserve_full_width_values_and_invalid_drafts() {
    let mut h = schema_harness(vec![
        spec("count", ParamControl::Integer { min: Some(Integer(i64::MIN)), max: Some(Integer(i64::MAX)), step: Unsigned(1) }, Value::Integer(Integer(0))),
        spec("seed", ParamControl::Seed { max: None }, Value::Unsigned(Unsigned(0))),
        spec("enabled", ParamControl::Toggle, Value::Bool(false)),
    ]);
    enter_text(&mut h, "count", &i64::MIN.to_string());
    h.key_press(egui::Key::Enter);
    h.run();
    enter_text(&mut h, "seed", &u64::MAX.to_string());
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(node(h.state()).params.get("count"), Some(&Value::Integer(Integer(i64::MIN))));
    assert_eq!(node(h.state()).params.get("seed"), Some(&Value::Unsigned(Unsigned(u64::MAX))));
    enter_text(&mut h, "count", "invalid");
    click(&mut h, Pos2::new(50.0, 50.0));
    h.get_by_role_and_label(Role::CheckBox, "enabled").click();
    h.run();
    assert_eq!(h.get_by_role_and_label(Role::TextInput, "count").value().as_deref(), Some("invalid"));
    assert!(h.query_by_label("Enter a value within this parameter's allowed range and step.").is_some());
    assert_eq!(node(h.state()).params.get("count"), Some(&Value::Integer(Integer(i64::MIN))));
    assert_eq!(node(h.state()).params.get("enabled"), Some(&Value::Bool(true)));
}
#[test]
fn conditional_controls_and_choice_labels_follow_schema() {
    let mut conditional = spec("detail", ParamControl::Text { multiline: false, max_bytes: 100 }, Value::Text("Detail".into()));
    conditional.visible_if = Some(VisibleIf { parameter: "enabled".into(), equals: Value::Bool(true) });
    let mut h = schema_harness(vec![
        spec("enabled", ParamControl::Toggle, Value::Bool(false)),
        conditional,
        spec(
            "quality",
            ParamControl::Choice { options: vec![Choice { value: "low".into(), label: "Low".into() }, Choice { value: "high".into(), label: "High".into() }] },
            Value::Text("low".into()),
        ),
    ]);
    assert!(h.query_by_role_and_label(Role::TextInput, "detail").is_none());
    h.get_by_role_and_label(Role::CheckBox, "enabled").click();
    h.run();
    assert!(h.query_by_role_and_label(Role::TextInput, "detail").is_some());
    h.get_by_role_and_label(Role::ComboBox, "quality").click();
    h.run();
    h.get_by_label("High").click();
    h.run();
    assert_eq!(node(h.state()).params.get("quality"), Some(&Value::Text("high".into())));
}
#[test]
fn slider_previews_then_commits_one_undo_step() {
    let mut h = schema_harness(vec![spec(
        "strength",
        ParamControl::Slider { min: Some(Number::new(0.0).unwrap()), max: Some(Number::new(1.0).unwrap()), step: Number::new(0.01).unwrap() },
        Value::Number(Number::new(0.5).unwrap()),
    )]);
    let before = h.state().session.document().clone();
    let rect = h.get_by_role_and_label(Role::Slider, "strength").rect();
    let from = Pos2::new(rect.center().x as f32, rect.center().y as f32);
    h.drag_at(from);
    h.run();
    h.hover_at(from + Vec2::new(10.0, 0.0));
    h.run();
    h.hover_at(from + Vec2::new(25.0, 0.0));
    h.run();
    assert_eq!(h.state().session.document(), &before);
    h.drop_at(from + Vec2::new(25.0, 0.0));
    h.run();
    assert_ne!(h.state().session.document(), &before);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
}
#[test]
fn scrolling_over_collapsed_card_pans_canvas() {
    let mut h = harness();
    h.state_mut().view.zoom = 0.2;
    h.run();
    let p = screen(h.state()).center();
    let pan = h.state().view.pan;
    h.hover_at(p);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: Vec2::new(0.0, 20.0),
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    assert!(h.state().view.pan.y > pan.y);
}

#[test]
fn focused_zoom_button_does_not_block_document_shortcuts() {
    let mut h = harness();
    let before = node(h.state()).clone();
    let rect = screen(h.state());
    click(&mut h, rect.min + Vec2::new(50.0, 55.0));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("Mountain".into()));
    h.run();
    h.key_press(egui::Key::Tab);
    h.run();
    assert_ne!(node(h.state()), &before);
    h.get_by_role_and_label(Role::Button, "100%").focus();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(node(h.state()), &before);
    h.state_mut().view.zoom = 2.0;
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num0);
    h.run();
    assert_eq!(h.state().view.zoom, 1.0);
}

#[test]
fn escape_parameter_draft_preserves_document_and_undo_through_next_frame() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    enter_text(&mut h, "Text", "This draft must be cancelled");
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
    h.run();
    click(&mut h, Pos2::new(50.0, 50.0));
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
}

#[test]
fn low_zoom_15_percent_keeps_named_labels_but_no_body_controls() {
    let mut h = harness();
    h.state_mut().view.zoom = 0.15;
    h.run();
    assert!(h.query_by_role_and_label(Role::Button, "Rename A QUIET FOREST AT DAWN, SOFT LIGHT THROUGH THE TREES.").is_some());
    assert!(h.query_by_role_and_label(Role::TextInput, "Text").is_none());
}

#[test]
fn accessibility_rect_at_double_zoom_targets_the_actual_control() {
    let mut h = schema_harness(vec![spec("enabled", ParamControl::Toggle, Value::Bool(false))]);
    h.state_mut().view.zoom = 2.0;
    h.state_mut().view.pan = Vec2::new(200.0, 150.0);
    h.run();
    let rect = h.get_by_role_and_label(Role::CheckBox, "enabled").rect();
    click(&mut h, Pos2::new(rect.center().x as f32, rect.center().y as f32));
    assert_eq!(node(h.state()).params.get("enabled"), Some(&Value::Bool(true)));
}

#[test]
fn keyboard_card_rename_and_whitespace_title_restore_automatic_label() {
    let mut h = harness();
    h.get_by_role_and_label(Role::Button, "Node A quiet forest at dawn, soft light through the trees.").focus();
    h.run();
    h.key_press(egui::Key::F2);
    h.run();
    h.event(egui::Event::Text("Keyboard name".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(node(h.state()).title, "Keyboard name");
    let canvas = h.state().canvas;
    let id = node(h.state()).id;
    h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":"  \n  "})).unwrap();
    assert!(node(h.state()).title.is_empty());
    assert_eq!(card::title(node(h.state()), None), "A quiet forest at dawn, soft light through the trees.");
}

#[test]
fn invalid_unfocused_draft_survives_unrelated_undo_and_redo() {
    let mut h = schema_harness(vec![
        spec("count", ParamControl::Unsigned { min: None, max: Some(Unsigned(u64::MAX)), step: Unsigned(8) }, Value::Unsigned(Unsigned(0))),
        spec("enabled", ParamControl::Toggle, Value::Bool(false)),
    ]);
    enter_text(&mut h, "count", "9");
    click(&mut h, Pos2::new(50.0, 50.0));
    h.get_by_role_and_label(Role::CheckBox, "enabled").click();
    h.run();
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    h.run();
    assert_eq!(h.get_by_role_and_label(Role::TextInput, "count").value().as_deref(), Some("9"));
    h.state_mut().session.dispatch("edit.redo", &serde_json::Value::Null).unwrap();
    h.run();
    assert_eq!(h.get_by_role_and_label(Role::TextInput, "count").value().as_deref(), Some("9"));
    assert_eq!(node(h.state()).params.get("count"), Some(&Value::Unsigned(Unsigned(0))));
}

#[test]
fn hiding_dirty_prompt_marks_unsaved_and_retains_full_draft() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    enter_text(&mut h, "Text", "Unsaved until restored");
    h.state_mut().view.zoom = 0.15;
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(h.state().editors.values().any(|editor| editor.hidden_dirty));
    assert!(h.query_by_role_and_label(Role::Button, "Discard drafts").is_some());
    h.state_mut().view.zoom = 1.0;
    h.run();
    assert_eq!(h.get_by_role_and_label(Role::MultilineTextInput, "Text").value().as_deref(), Some("Unsaved until restored"));
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn unbounded_numbers_unsigned_steps_and_seed_max_remain_exact() {
    let mut h = schema_harness(vec![
        spec("number", ParamControl::Slider { min: None, max: None, step: Number::new(0.1).unwrap() }, Value::Number(Number::new(0.0).unwrap())),
        spec("count", ParamControl::Unsigned { min: Some(Unsigned(0)), max: Some(Unsigned(u64::MAX)), step: Unsigned(3) }, Value::Unsigned(Unsigned(0))),
        spec("seed", ParamControl::Seed { max: Some(Unsigned(u64::from(u32::MAX))) }, Value::Unsigned(Unsigned(0))),
    ]);
    enter_text(&mut h, "number", "51.248178375505404");
    h.key_press(egui::Key::Enter);
    h.run();
    enter_text(&mut h, "count", &u64::MAX.to_string());
    h.key_press(egui::Key::Enter);
    h.run();
    enter_text(&mut h, "seed", &(u64::from(u32::MAX) + 1).to_string());
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(node(h.state()).params.get("number"), Some(&Value::Number(Number::new(51.248178375505404).unwrap())));
    assert_eq!(node(h.state()).params.get("count"), Some(&Value::Unsigned(Unsigned(u64::MAX))));
    assert_eq!(node(h.state()).params.get("seed"), Some(&Value::Unsigned(Unsigned(0))));
}

fn large_prompt_harness() -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let first = app.session.document().canvases().get(&canvas).unwrap().nodes().values().next().unwrap().id;
    let mut changes = Vec::new();
    for index in 0..4u8 {
        let mut item = nodecraft_nodes::text::kind()
            .descriptor()
            .instantiate(if index == 0 { first } else { NodeId::from_bytes([index + 3; 16]) }, &BTreeMap::new())
            .unwrap();
        item.pos = nodecraft_graph::Position { x: f32::from(index % 2) * 360.0, y: f32::from(index / 2) * 260.0 };
        let text = format!("{}TAIL-{index}", "x".repeat(1024 * 1024 - 4096));
        item.params.insert("text".into(), Value::Text(text));
        changes.push(if index == 0 { Change::SetNode { canvas, node: item } } else { Change::AddNode { canvas, node: item } });
    }
    // Setup through a fresh session keeps the edit/undo assertions independent of fixture creation.
    let mut document = app.session.document().clone();
    nodecraft_graph::Changeset::new(changes).apply(&mut document).unwrap();
    app.session = Session::new(document).unwrap();
    Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}

#[test]
fn four_large_prompts_use_lazy_drafts_and_keep_unseen_tail_on_cancel_and_commit() {
    let mut h = large_prompt_harness();
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(h.state().editors.values().map(EditorState::draft_count).sum::<usize>(), 0);
    let before = node(h.state()).params.get("text").unwrap().clone();
    let p = screen(h.state()).min + Vec2::new(50.0, 55.0);
    click(&mut h, p);
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text("cancelled".into()));
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(node(h.state()).params.get("text"), Some(&before));
    assert!(!h.state().session.can_undo());
    click(&mut h, p);
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text("!".into()));
    h.run();
    h.key_press(egui::Key::Tab);
    h.run();
    let Value::Text(before) = before else { panic!("text fixture") };
    assert_eq!(node(h.state()).params.get("text"), Some(&Value::Text(format!("{before}!"))));
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    assert_eq!(node(h.state()).params.get("text"), Some(&Value::Text(before)));
    assert!(!h.state().session.can_undo());
}

#[test]
fn typing_slider_value_commits_once_after_enter() {
    let mut h = schema_harness(vec![spec(
        "strength",
        ParamControl::Slider { min: Some(Number::new(0.0).unwrap()), max: Some(Number::new(1.0).unwrap()), step: Number::new(0.01).unwrap() },
        Value::Number(Number::new(0.5).unwrap()),
    )]);
    let before = h.state().session.document().clone();
    let rect = h.get_by_role_and_label(Role::Slider, "strength").rect();
    double_click(&mut h, Pos2::new(rect.max.x + 25.0, rect.center().y as f32));
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    for character in ["0", ".", "7", "5"] {
        h.event(egui::Event::Text(character.into()));
        h.run();
        assert_eq!(h.state().session.document(), &before);
    }
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(node(h.state()).params.get("strength"), Some(&Value::Number(Number::new(0.75).unwrap())));
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
}

#[test]
fn nondefault_canvas_settings_apply_to_graph_backed_shell() {
    let mut h = harness();
    let mut settings = NodeCraftSettings::default();
    settings.appearance.theme = Theme::Light;
    settings.canvas.scroll = nodecraft_engine::settings::ScrollAction::Zoom;
    settings.canvas.zoom_speed = 2.0;
    h.state_mut().theme = ThemeKind::Light;
    h.state_mut().canvas_prefs = settings.canvas.clone();
    assert_eq!(h.state().theme, ThemeKind::Light);
    h.hover_at(Pos2::new(50.0, 50.0));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: Vec2::new(0.0, 60.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    for _ in 0..30 {
        h.run();
    }
    assert!(h.state().view.zoom > 1.5);
}

#[test]
#[ignore = "manual release benchmark for four near-1MiB prompts; no timing assertion"]
fn large_prompt_idle_release_benchmark() {
    let mut h = large_prompt_harness();
    for _ in 0..5 {
        h.step();
    }
    let start = std::time::Instant::now();
    for _ in 0..120 {
        h.step();
    }
    eprintln!("four near-1MiB prompts, 120 warm idle UI frames: {:?}, mean {:?}", start.elapsed(), start.elapsed() / 120);
    assert_eq!(h.state().editors.values().map(EditorState::draft_count).sum::<usize>(), 0);
}

#[test]
fn exact_one_mib_prompt_is_not_truncated_by_preview_focus_or_cancel() {
    let mut h = large_prompt_harness();
    let canvas = h.state().canvas;
    let mut item = node(h.state()).clone();
    let text = format!("{}UNSEEN-TAIL", "x".repeat(1024 * 1024 - 11));
    assert_eq!(text.len(), 1024 * 1024);
    item.params.insert("text".into(), Value::Text(text.clone()));
    let mut document = h.state().session.document().clone();
    Change::SetNode { canvas, node: item }.apply(&mut document).unwrap();
    h.state_mut().session = Session::new(document).unwrap();
    h.run();
    let p = screen(h.state()).min + Vec2::new(50.0, 55.0);
    click(&mut h, p);
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(node(h.state()).params.get("text"), Some(&Value::Text(text)));
    assert!(!h.state().session.can_undo());
    assert_eq!(h.state().editors.values().map(EditorState::draft_count).sum::<usize>(), 0);
}

#[test]
fn separate_drag_gestures_remain_separate_undo_steps() {
    let mut h = harness();
    let before = node(h.state()).pos;
    for _ in 0..2 {
        let p = screen(h.state()).left_center() + Vec2::new(5.0, 0.0);
        h.drag_at(p);
        h.run();
        h.hover_at(p + Vec2::new(30.0, 0.0));
        h.run();
        h.drop_at(p + Vec2::new(30.0, 0.0));
        h.run();
    }
    assert_eq!(node(h.state()).pos.x, before.x + 60.0);
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    assert_eq!(node(h.state()).pos.x, before.x + 30.0);
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    assert_eq!(node(h.state()).pos, before);
    assert!(!h.state().session.can_undo());
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn settings_constructor_and_exit_preserve_native_store_lifecycle() {
    let path = std::env::temp_dir().join(format!(
        "nodecraft-ui-settings-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let (mut store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(path.clone()));
    store.update(|settings| {
        settings.appearance.theme = Theme::Light;
        settings.canvas.scroll = nodecraft_engine::settings::ScrollAction::Zoom;
        settings.canvas.zoom_speed = 2.5;
    });
    let ctx = egui::Context::default();
    let mut app = NodeCraftApp::with_settings(&ctx, store).unwrap();
    assert_eq!(app.theme, ThemeKind::Light);
    assert_eq!(app.canvas_prefs.zoom_speed(), 2.5);
    assert_eq!(app.canvas_prefs.scroll, nodecraft_engine::settings::ScrollAction::Zoom);
    eframe::App::on_exit(&mut app);
    let (restored, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(path.clone()));
    assert_eq!(restored.get().appearance.theme, Theme::Light);
    assert_eq!(restored.get().canvas.zoom_speed(), 2.5);
    drop(restored);
    drop(app);
    std::fs::remove_dir_all(path).unwrap();
}

fn large_schema_harness(count: u32, dynamic: bool) -> Harness<'static, NodeCraftApp> {
    use std::borrow::Cow;
    struct DynamicLarge(NodeDescriptor);
    impl NodeKind for DynamicLarge {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn instance<'a>(&'a self, _: &nodecraft_graph::Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            Ok(Cow::Owned(self.0.clone()))
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }
    let descriptor = NodeDescriptor {
        id: "test.large_schema".into(),
        label: "Model".into(),
        category: Category::Source,
        inputs: vec![],
        outputs: vec![],
        quick_add: None,
        params: ParamSchema {
            params: vec![ParamSpec {
                name: "model".into(),
                label: "Model".into(),
                description: None,
                required: false,
                default: None,
                visible_if: None,
                control: ParamControl::Choice {
                    options: (0..1200).map(|index| Choice { value: format!("{index}{}", "x".repeat(400)), label: "y".repeat(400) }).collect(),
                },
            }],
        },
    };
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Schema cache stress").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let changes = (0..count)
        .map(|index| {
            let mut node = nodecraft_graph::Node::new(NodeId::from_bytes(u128::from(index).to_be_bytes()), "test.large_schema");
            if !dynamic {
                node.pos.x = index as f32 * 400.0;
            }
            Change::AddNode { canvas, node }
        })
        .collect();
    nodecraft_graph::Changeset::new(changes).apply(&mut document).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    if dynamic {
        kinds.register(Box::new(DynamicLarge(descriptor))).unwrap();
    } else {
        kinds.register(Box::new(TestKind(descriptor))).unwrap();
    }
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap())
}

#[test]
fn ten_thousand_small_nodes_share_large_static_schema_and_culled_nodes_stay_lazy() {
    let h = large_schema_harness(10_000, false);
    assert!(h.state().schemas.len() > 1);
    assert!(h.state().schemas.len() < 20, "only visible candidates should resolve");
    let first = h.state().schemas.values().next().unwrap();
    assert!(h.state().schemas.values().all(|entry| Arc::ptr_eq(&entry.descriptor, &first.descriptor)));
    assert!(cache_bytes(&h.state().schemas) < 5 * 1024 * 1024);
}

#[test]
fn owned_dynamic_schema_cache_obeys_aggregate_lru_budget() {
    let mut h = large_schema_harness(24, true);
    for _ in 0..3 {
        h.step();
    }
    assert!(cache_bytes(&h.state().schemas) <= MAX_SCHEMA_CACHE_BYTES);
    assert!(h.state().schemas.len() < 24, "dynamic descriptors must be evicted rather than multiplied without bound");
    assert!(h.state().schemas.values().all(|entry| entry.borrowed_identity.is_none()));
}

#[test]
fn title_at_four_times_zoom_remains_when_body_is_below_viewport() {
    let mut h = harness();
    let position = node(h.state()).pos;
    h.state_mut().view.zoom = 4.0;
    h.state_mut().view.pan = Vec2::new(300.0 - position.x * 4.0, 740.0 - position.y * 4.0);
    h.run();
    assert!(screen(h.state()).min.y > h.state().viewport.max.y);
    assert!(h.query_by_role_and_label(Role::Button, "Rename A QUIET FOREST AT DAWN, SOFT LIGHT THROUGH THE TREES.").is_some());
}

/// Text A → relay B → relay C, and a lone Text D, at 30% zoom so cards have no body controls.
fn chain_harness() -> Harness<'static, NodeCraftApp> {
    let text = PortType { modality: nodecraft_types::Modality::Text, many: false };
    let port = |name: &str| nodecraft_graph::Port { name: name.into(), port_type: text };
    let relay = NodeDescriptor {
        id: "test.relay".into(),
        label: "Relay".into(),
        category: Category::Action,
        inputs: vec![port("in")],
        outputs: vec![port("out")],
        params: ParamSchema { params: vec![] },
        quick_add: None,
    };
    let collect = NodeDescriptor {
        id: "test.collect".into(),
        label: "Collect".into(),
        category: Category::Action,
        inputs: vec![nodecraft_graph::Port { name: "items".into(), port_type: PortType { many: true, ..text } }],
        outputs: vec![],
        params: ParamSchema { params: vec![] },
        quick_add: None,
    };
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds.register(Box::new(TestKind(relay))).unwrap();
    kinds.register(Box::new(TestKind(collect))).unwrap();
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Chain").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    for (n, kind, x, y) in [(1, "source.text", 0.0, 0.0), (2, "test.relay", 400.0, 0.0), (3, "test.relay", 800.0, 0.0), (4, "source.text", 0.0, 400.0)] {
        let mut node = kinds.get(kind).unwrap().descriptor().instantiate(NodeId::from_bytes([n; 16]), &BTreeMap::new()).unwrap();
        node.pos = nodecraft_graph::Position { x, y };
        Change::AddNode { canvas, node }.apply(&mut document).unwrap();
    }
    for (e, from) in [(10u8, 1u8), (11, 2)] {
        let edge = nodecraft_graph::Edge {
            id: nodecraft_types::EdgeId::from_bytes([e; 16]),
            from: nodecraft_graph::Endpoint { node: NodeId::from_bytes([from; 16]), port: "out".into() },
            to: nodecraft_graph::Endpoint { node: NodeId::from_bytes([from + 1; 16]), port: "in".into() },
            order: 0,
        };
        Change::Connect { canvas, edge }.apply(&mut document).unwrap();
    }
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
    h.state_mut().view = CanvasView { pan: Vec2::new(100.0, 100.0), zoom: 0.3 };
    h.run();
    h
}
fn chain_id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn ids(ns: &[u8]) -> BTreeSet<NodeId> {
    ns.iter().map(|n| chain_id(*n)).collect()
}
/// Screen point just inside the top-left of card `n`.
fn card_point(h: &Harness<'static, NodeCraftApp>, n: u8) -> Pos2 {
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&chain_id(n)).unwrap();
    let descriptor = app.session.kinds().get(&node.kind).ok().map(|k| k.descriptor());
    app.view.rect_to_screen(app.viewport.min, card::world_rect(node, descriptor, app.theme.tokens())).min + Vec2::splat(6.0)
}
fn press(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2, button: egui::PointerButton, modifiers: egui::Modifiers) {
    h.event(egui::Event::ModifiersChanged(modifiers));
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button, pressed, modifiers });
        h.run();
    }
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run();
}
fn press_card(h: &mut Harness<'static, NodeCraftApp>, n: u8, button: egui::PointerButton, modifiers: egui::Modifiers) {
    let pos = card_point(h, n);
    press(h, pos, button, modifiers);
}
fn box_drag(h: &mut Harness<'static, NodeCraftApp>, from: Pos2, to: Pos2, modifiers: egui::Modifiers) {
    h.event(egui::Event::ModifiersChanged(modifiers));
    h.hover_at(from);
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers });
    h.run();
    h.hover_at(from + (to - from) * 0.5);
    h.run();
    h.hover_at(to);
    h.run();
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers });
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run();
}

#[test]
fn click_shift_click_select_all_and_escape() {
    let mut h = chain_harness();
    let before = h.state().session.document().clone();
    press_card(&mut h, 1, egui::PointerButton::Primary, egui::Modifiers::NONE);
    assert_eq!(h.state().selection.nodes(), &ids(&[1]));
    press_card(&mut h, 2, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2]));
    press_card(&mut h, 1, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    assert_eq!(h.state().selection.nodes(), &ids(&[2]));
    // Shift-clicking empty canvas keeps the selection; a plain click clears it.
    press(&mut h, Pos2::new(600.0, 650.0), egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    assert_eq!(h.state().selection.nodes(), &ids(&[2]));
    press(&mut h, Pos2::new(600.0, 650.0), egui::PointerButton::Primary, egui::Modifiers::NONE);
    assert!(h.state().selection.is_empty());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2, 3, 4]));
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().selection.is_empty());
    // Selection is view state: no document change and nothing to undo.
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
}

#[test]
fn shift_drag_box_selects_without_panning_and_plain_drag_still_pans() {
    let mut h = chain_harness();
    let pan = h.state().view.pan;
    let (origin, view) = (h.state().viewport.min, h.state().view);
    let world = |x: f32, y: f32| view.to_screen(origin, Pos2::new(x, y));
    let (from, to) = (world(-60.0, -60.0), world(500.0, 50.0));
    box_drag(&mut h, from, to, egui::Modifiers::SHIFT);
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2]));
    assert_eq!(h.state().view.pan, pan);
    // A second Shift-box adds to the selection.
    let (from, to) = (world(-60.0, 380.0), world(50.0, 450.0));
    box_drag(&mut h, from, to, egui::Modifiers::SHIFT);
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2, 4]));
    let from = Pos2::new(600.0, 650.0);
    box_drag(&mut h, from, from + Vec2::new(-40.0, -30.0), egui::Modifiers::NONE);
    assert!((h.state().view.pan - pan - Vec2::new(-40.0, -30.0)).length() < 1.0);
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2, 4]));
}

#[test]
fn context_menu_selects_chains_and_kinds() {
    let mut h = chain_harness();
    press_card(&mut h, 3, egui::PointerButton::Secondary, egui::Modifiers::NONE);
    assert_eq!(h.state().selection.nodes(), &ids(&[3]));
    assert!(h.query_by_label("Select upstream").is_some());
    // Escape closes the menu but keeps the selection.
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.query_by_label("Select upstream").is_none());
    assert_eq!(h.state().selection.nodes(), &ids(&[3]));
    press_card(&mut h, 3, egui::PointerButton::Secondary, egui::Modifiers::NONE);
    h.get_by_label("Select upstream").click();
    h.run();
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2, 3]));
    press(&mut h, Pos2::new(600.0, 650.0), egui::PointerButton::Primary, egui::Modifiers::NONE);
    press_card(&mut h, 2, egui::PointerButton::Secondary, egui::Modifiers::NONE);
    h.get_by_label("Select downstream").click();
    h.run();
    assert_eq!(h.state().selection.nodes(), &ids(&[2, 3]));
    press_card(&mut h, 4, egui::PointerButton::Secondary, egui::Modifiers::NONE);
    h.get_by_label("Select all Text nodes").click();
    h.run();
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 4]));
}

#[test]
fn selection_drops_removed_nodes() {
    let mut h = chain_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    assert_eq!(h.state().selection.len(), 4);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.remove", &json!({"canvas": canvas, "id": chain_id(3)})).unwrap();
    h.run();
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2, 4]));
}

#[test]
fn grabbing_or_right_clicking_a_selected_card_keeps_the_selection() {
    let mut h = chain_harness();
    press_card(&mut h, 1, egui::PointerButton::Primary, egui::Modifiers::NONE);
    press_card(&mut h, 2, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    press_card(&mut h, 2, egui::PointerButton::Secondary, egui::Modifiers::NONE);
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2]));
    h.key_press(egui::Key::Escape);
    h.run();
    let before = h.state().session.document().clone();
    let from = card_point(&h, 1);
    box_drag(&mut h, from, from + Vec2::new(30.0, 15.0), egui::Modifiers::NONE);
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 2]));
    // The drag moved only the grabbed card, as one undo step.
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    let moved =
        |n: u8| canvas.nodes().get(&chain_id(n)).unwrap().pos != before.canvases().get(&h.state().canvas).unwrap().nodes().get(&chain_id(n)).unwrap().pos;
    assert!(moved(1) && !moved(2));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn box_selection_is_live_and_escape_cancels_it() {
    let mut h = chain_harness();
    press_card(&mut h, 4, egui::PointerButton::Primary, egui::Modifiers::NONE);
    let pan = h.state().view.pan;
    let (origin, view) = (h.state().viewport.min, h.state().view);
    let (from, to) = (view.to_screen(origin, Pos2::new(-60.0, -60.0)), view.to_screen(origin, Pos2::new(100.0, 50.0)));
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    h.hover_at(from);
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
    h.run();
    h.hover_at(to);
    h.run();
    assert_eq!(h.state().selection.nodes(), &ids(&[1, 4]));
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().selection.nodes(), &ids(&[4]));
    // A cancelled box stays cancelled and does not turn into a pan for the rest of the gesture.
    h.hover_at(to + Vec2::new(200.0, 0.0));
    h.run();
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::SHIFT });
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run();
    assert_eq!(h.state().selection.nodes(), &ids(&[4]));
    assert_eq!(h.state().view.pan, pan);
}

#[test]
fn selection_shortcuts_yield_to_text_fields() {
    let mut h = harness();
    let id = node(h.state()).id;
    let rect = screen(h.state());
    click(&mut h, rect.min + Vec2::splat(4.0));
    assert_eq!(h.state().selection.nodes(), &[id].into_iter().collect());
    // Start empty so a leaked ⌘A would visibly select the node.
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().selection.is_empty());
    click(&mut h, rect.min + Vec2::new(50.0, 55.0));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    assert!(h.state().selection.is_empty());
    click(&mut h, rect.min + Vec2::splat(4.0));
    click(&mut h, rect.min + Vec2::new(50.0, 55.0));
    h.key_press(egui::Key::Escape);
    h.run();
    // ⌘A went to the text field, and Escape only left it (egui drops focus before the frame).
    assert_eq!(h.state().selection.nodes(), &[id].into_iter().collect());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().selection.is_empty());
}

#[test]
fn box_select_uses_painted_geometry_not_the_evictable_schema_cache() {
    use std::borrow::Cow;
    // Static palette metadata has one parameter; each instance resolves six, so the painted card
    // is taller than anything the static descriptor implies.
    struct Tall(NodeDescriptor, NodeDescriptor);
    impl NodeKind for Tall {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn instance<'a>(&'a self, _: &nodecraft_graph::Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            Ok(Cow::Borrowed(&self.1))
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }
    let toggle = |n: u32| spec(&format!("p{n}"), ParamControl::Toggle, Value::Bool(false));
    let short = NodeDescriptor {
        id: "test.tall".into(),
        label: "Tall".into(),
        category: Category::Source,
        inputs: vec![],
        outputs: vec![],
        params: ParamSchema { params: vec![toggle(0)] },
        quick_add: None,
    };
    let tall = NodeDescriptor { params: ParamSchema { params: (0..6).map(toggle).collect() }, ..short.clone() };
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Tall").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let mut card = nodecraft_graph::Node::new(NodeId::from_bytes([3; 16]), "test.tall");
    card.params.insert("p0".into(), Value::Bool(false));
    Change::AddNode { canvas, node: card }.apply(&mut document).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Tall(short, tall))).unwrap();
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
    h.run();
    let id = NodeId::from_bytes([3; 16]);
    let painted = *h.state().painted.get(&id).unwrap();
    let static_rect = card::world_rect(node(h.state()), h.state().session.kinds().get("test.tall").ok().map(|k| k.descriptor()), h.state().theme.tokens());
    assert!(painted.height() > static_rect.height() + 10.0, "{painted:?} vs {static_rect:?}");
    // Simulate eviction, then box only the part of the card below the static height.
    h.state_mut().schemas.clear();
    let (origin, view) = (h.state().viewport.min, h.state().view);
    let lower = Rect::from_min_max(Pos2::new(painted.min.x - 20.0, static_rect.max.y + 4.0), Pos2::new(painted.max.x + 20.0, painted.max.y - 4.0));
    let geometry = card_geometry(&h.state().painted, h.state().session.kinds(), node(h.state()), h.state().theme.tokens());
    assert_eq!(geometry, painted);
    box_drag(&mut h, view.to_screen(origin, lower.min), view.to_screen(origin, lower.max), egui::Modifiers::SHIFT);
    assert_eq!(h.state().selection.nodes(), &[id].into_iter().collect());
}

/// Screen position of node `n`'s single input or output port.
fn port_point(h: &Harness<'static, NodeCraftApp>, n: u8, side: noodle::Side) -> Pos2 {
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&chain_id(n)).unwrap();
    let rect = card_geometry(&app.painted, app.session.kinds(), node, app.theme.tokens());
    app.view.to_screen(app.viewport.min, noodle::anchor(rect, side, 0, 1))
}
fn wire_drag(h: &mut Harness<'static, NodeCraftApp>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.run();
    h.drag_at(from);
    h.run();
    h.hover_at(from + (to - from) * 0.5);
    h.run();
    h.hover_at(to);
    h.run();
    h.drop_at(to);
    h.run();
}
/// Edges as (from node, to node) pairs.
fn links(h: &Harness<'static, NodeCraftApp>) -> BTreeSet<(NodeId, NodeId)> {
    let app = h.state();
    app.session.document().canvases().get(&app.canvas).unwrap().edges().values().map(|e| (e.from.node, e.to.node)).collect()
}
fn link(a: u8, b: u8) -> (NodeId, NodeId) {
    (chain_id(a), chain_id(b))
}

#[test]
fn dragging_an_output_fans_out_and_replaces_a_single_input_in_one_step() {
    let mut h = chain_harness();
    let before = h.state().session.document().clone();
    let (from, to) = (port_point(&h, 1, noodle::Side::Output), port_point(&h, 3, noodle::Side::Input));
    wire_drag(&mut h, from, to);
    // A now feeds B and C; C's single input dropped B.
    assert_eq!(links(&h), [link(1, 2), link(1, 3)].into_iter().collect());
    assert!(h.state().notice.is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
}

#[test]
fn invalid_drops_leave_the_document_alone_and_explain_why() {
    let mut h = chain_harness();
    let canvas = h.state().canvas;
    let mut image = h.state().session.kinds().get("source.image").unwrap().descriptor().instantiate(chain_id(5), &BTreeMap::new()).unwrap();
    image.pos = nodecraft_graph::Position { x: 400.0, y: 400.0 };
    h.state_mut().session.dispatch("node.add", &json!({"canvas": canvas, "node": image})).unwrap();
    h.run();
    let before = h.state().session.document().clone();
    for (a, b, message) in
        [(5, 3, "An image output can't connect to a text input."), (3, 2, "That connection would create a loop."), (3, 3, "A node can't connect to itself.")]
    {
        let (from, to) = (port_point(&h, a, noodle::Side::Output), port_point(&h, b, noodle::Side::Input));
        wire_drag(&mut h, from, to);
        assert_eq!(h.state().session.document(), &before, "{a}->{b}");
        assert_eq!(h.state().notice.as_deref(), Some(message));
    }
    // Dropping on empty canvas, or Escape mid-drag, does nothing.
    let from = port_point(&h, 1, noodle::Side::Output);
    wire_drag(&mut h, from, Pos2::new(900.0, 600.0));
    let to = port_point(&h, 3, noodle::Side::Input);
    h.hover_at(from);
    h.run();
    h.drag_at(from);
    h.run();
    h.hover_at(to);
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.drop_at(to);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn rerouting_an_input_end_moves_or_disconnects_the_noodle() {
    let mut h = chain_harness();
    let before = h.state().session.document().clone();
    // Pick up A → B by B's input and drop it on C's input: A → C replaces B → C.
    let (from, to) = (port_point(&h, 2, noodle::Side::Input), port_point(&h, 3, noodle::Side::Input));
    wire_drag(&mut h, from, to);
    assert_eq!(links(&h), [link(1, 3)].into_iter().collect());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    // Dropping a picked-up end on empty canvas disconnects it.
    let from = port_point(&h, 3, noodle::Side::Input);
    wire_drag(&mut h, from, Pos2::new(900.0, 600.0));
    assert_eq!(links(&h), [link(1, 2)].into_iter().collect());
}

#[test]
fn noodles_are_clickable_box_selectable_and_deletable() {
    let mut h = chain_harness();
    // At 100% the gaps between cards are wider than the port handles' pointer slop.
    h.state_mut().view = CanvasView { pan: Vec2::new(-100.0, 100.0), zoom: 1.0 };
    h.run();
    let (a, b) = (port_point(&h, 1, noodle::Side::Output), port_point(&h, 2, noodle::Side::Input));
    let edge = |h: &Harness<'static, NodeCraftApp>, from: u8| {
        h.state().session.document().canvases().get(&h.state().canvas).unwrap().edges().values().find(|e| e.from.node == chain_id(from)).unwrap().id
    };
    let (ab, bc) = (edge(&h, 1), edge(&h, 2));
    press(&mut h, a + (b - a) * 0.5, egui::PointerButton::Primary, egui::Modifiers::NONE);
    assert_eq!(h.state().selection.edges(), &[ab].into_iter().collect());
    assert!(h.state().selection.nodes().is_empty());
    let before = h.state().session.document().clone();
    h.key_press(egui::Key::Delete);
    h.run();
    assert_eq!(links(&h), [link(2, 3)].into_iter().collect());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    // A box over the gap between B and C catches only their noodle; then delete C with it.
    let (c_in, b_out) = (port_point(&h, 3, noodle::Side::Input), port_point(&h, 2, noodle::Side::Output));
    let mid = b_out + (c_in - b_out) * 0.5;
    box_drag(&mut h, mid - Vec2::splat(4.0), mid + Vec2::splat(4.0), egui::Modifiers::SHIFT);
    assert_eq!(h.state().selection.edges(), &[bc].into_iter().collect());
    press_card(&mut h, 3, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    h.key_press(egui::Key::Backspace);
    h.run();
    assert_eq!(links(&h), [link(1, 2)].into_iter().collect());
    assert!(!h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().contains_key(&chain_id(3)));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn list_inputs_collect_in_order_reject_duplicates_and_accept_reroutes() {
    let mut h = chain_harness();
    let canvas = h.state().canvas;
    let mut collect = h.state().session.kinds().get("test.collect").unwrap().descriptor().instantiate(chain_id(6), &BTreeMap::new()).unwrap();
    collect.pos = nodecraft_graph::Position { x: 800.0, y: 400.0 };
    h.state_mut().session.dispatch("node.add", &json!({"canvas": canvas, "node": collect})).unwrap();
    h.run();
    let into = port_point(&h, 6, noodle::Side::Input);
    for n in [1, 4] {
        let from = port_point(&h, n, noodle::Side::Output);
        wire_drag(&mut h, from, into);
    }
    let orders = |h: &Harness<'static, NodeCraftApp>| -> Vec<(NodeId, u32)> {
        let edges = noodle::into_port(
            h.state().session.document().canvases().get(&canvas).unwrap(),
            &nodecraft_graph::Endpoint { node: chain_id(6), port: "items".into() },
        );
        edges.into_iter().map(|e| (e.from.node, e.order)).collect()
    };
    assert_eq!(orders(&h), vec![(chain_id(1), 0), (chain_id(4), 1)]);
    // The same output twice into one list is refused.
    let before = h.state().session.document().clone();
    let from = port_point(&h, 1, noodle::Side::Output);
    wire_drag(&mut h, from, into);
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().notice.as_deref(), Some("These ports are already connected."));
    // Rerouting B → C onto the list appends it after the existing items.
    let from = port_point(&h, 3, noodle::Side::Input);
    wire_drag(&mut h, from, into);
    assert_eq!(orders(&h), vec![(chain_id(1), 0), (chain_id(4), 1), (chain_id(2), 2)]);
    assert!(!links(&h).contains(&link(2, 3)));
}

#[test]
fn delete_keys_yield_to_text_fields() {
    let mut h = harness();
    let id = node(h.state()).id;
    let rect = screen(h.state());
    click(&mut h, rect.min + Vec2::splat(4.0));
    click(&mut h, rect.min + Vec2::new(50.0, 55.0));
    for key in [egui::Key::Backspace, egui::Key::Delete] {
        h.key_press(key);
        h.run();
    }
    assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().contains_key(&id));
    assert_eq!(h.state().selection.nodes(), &[id].into_iter().collect());
    // Outside the text field the same key deletes the selected node, as one undo step.
    h.key_press(egui::Key::Escape);
    h.run();
    click(&mut h, rect.min + Vec2::splat(4.0));
    h.key_press(egui::Key::Delete);
    h.run();
    assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().is_empty());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().contains_key(&id));
}

#[test]
fn box_select_ignores_the_static_shape_of_an_undrawn_short_card() {
    use std::borrow::Cow;
    // The reverse of the tall case: static metadata is tall (six rows), each instance resolves to
    // one row. Placed just above the viewport, the real card is culled while the static shape
    // would reach into view.
    struct Short(NodeDescriptor, NodeDescriptor);
    impl NodeKind for Short {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn instance<'a>(&'a self, _: &nodecraft_graph::Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            Ok(Cow::Borrowed(&self.1))
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }
    let toggle = |n: u32| spec(&format!("p{n}"), ParamControl::Toggle, Value::Bool(false));
    let tall = NodeDescriptor {
        id: "test.short".into(),
        label: "Short".into(),
        category: Category::Source,
        inputs: vec![],
        outputs: vec![],
        params: ParamSchema { params: (0..6).map(toggle).collect() },
        quick_add: None,
    };
    let short = NodeDescriptor { params: ParamSchema { params: vec![toggle(0)] }, ..tall.clone() };
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Short").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let mut card = nodecraft_graph::Node::new(NodeId::from_bytes([3; 16]), "test.short");
    card.params.insert("p0".into(), Value::Bool(false));
    Change::AddNode { canvas, node: card }.apply(&mut document).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Short(tall, short))).unwrap();
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
    h.run();
    let t = *h.state().theme.tokens();
    let static_rect = card::world_rect(node(h.state()), h.state().session.kinds().get("test.short").ok().map(|k| k.descriptor()), &t);
    let real = card::world_rect(
        node(h.state()),
        h.state().session.kinds().get("test.short").ok().and_then(|k| k.resolve_instance(node(h.state())).ok()).as_deref(),
        &t,
    );
    assert!(static_rect.height() > real.height() + 100.0);
    // Scroll so the real card (and its label and cull margin) is above the top edge, while the
    // static shape would still reach well into the viewport.
    let top = real.max.y + 120.0;
    h.state_mut().view = CanvasView { pan: Vec2::new(300.0, -top), zoom: 1.0 };
    h.run();
    assert!(h.state().painted.is_empty(), "the short card must be culled for this test");
    let (origin, view) = (h.state().viewport.min, h.state().view);
    let from = view.to_screen(origin, Pos2::new(static_rect.min.x - 10.0, top + 10.0));
    let to = view.to_screen(origin, Pos2::new(static_rect.max.x + 10.0, (static_rect.max.y - 10.0).max(top + 40.0)));
    assert!(static_rect.intersects(Rect::from_two_pos(view.to_world(origin, from), view.to_world(origin, to))));
    box_drag(&mut h, from, to, egui::Modifiers::SHIFT);
    assert!(h.state().selection.is_empty());
}

#[test]
fn the_keys_button_opens_the_keys_panel() {
    let mut h = harness();
    assert!(!h.state().keys.open);
    assert!(h.query_by_label("API keys").is_some());
    h.get_by_role_and_label(Role::Button, "API keys").click();
    h.run();
    assert!(h.state().keys.open);
    assert!(h.query_by_label("No keys yet.").is_some(), "headless: no accounts in settings");
    // No keychain service in the headless app: checking or saving says so instead of failing silently.
    h.get_by_label("openai").click();
    h.run();
    h.get_by_role_and_label(Role::PasswordInput, "Key").click();
    h.run();
    h.event(egui::Event::Text("sk-proj-abcdefghij1234".into()));
    h.run();
    h.get_by_label("Save").click();
    h.run();
    assert!(h.state().keys.notice.as_deref().is_some_and(|n| n.contains("unavailable")), "{:?}", h.state().keys.notice);
}

#[test]
fn noodles_follow_a_card_while_it_is_dragged_and_commit_on_release() {
    let mut h = chain_harness();
    let canvas = h.state().canvas;
    let pos = |h: &Harness<'static, NodeCraftApp>| h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&chain_id(2)).unwrap().pos;
    let start = |h: &Harness<'static, NodeCraftApp>| {
        let app = h.state();
        let canvas = app.session.document().canvases().get(&app.canvas).unwrap();
        let curves = curves(canvas, &app.painted, app.session.kinds(), app.theme.tokens());
        let edge = canvas.edges().values().find(|e| e.from.node == chain_id(2)).unwrap().id;
        curves.iter().find(|(id, ..)| *id == edge).map(|(_, curve, _)| curve[0]).unwrap()
    };
    let (before_pos, before_start) = (pos(&h), start(&h));
    let grab = card_point(&h, 2);
    h.drag_at(grab);
    h.run();
    h.hover_at(grab + Vec2::new(30.0, 45.0));
    h.run();
    h.hover_at(grab + Vec2::new(60.0, 90.0));
    h.run();
    // Mid-drag: the document hasn't moved, but B's outgoing noodle starts where B is painted.
    assert_eq!(pos(&h), before_pos);
    let moved = start(&h) - before_start;
    let zoom = h.state().view.zoom;
    assert!((moved - Vec2::new(60.0, 90.0) / zoom).length() < 1.0, "{moved:?}");
    h.drop_at(grab + Vec2::new(60.0, 90.0));
    h.run();
    assert!((pos(&h).x - before_pos.x - 60.0 / zoom).abs() < 1.0);
}

#[test]
fn only_cards_drawn_this_frame_are_noodle_drop_targets() {
    // A card off screen isn't painted, so it can't take a drop wherever its static size would
    // put its inputs.
    let mut h = harness();
    h.run();
    let canvas_id = h.state().canvas;
    let painted = h.state().painted.clone();
    let doc = h.state().session.document().clone();
    let canvas = doc.canvases().get(&canvas_id).unwrap();
    assert!(canvas.nodes().len() > painted.len() || !painted.is_empty());
    let targets: Vec<NodeId> = super::drop_targets(canvas, &painted).map(|(node, _)| node.id).collect();
    assert_eq!(targets, painted.keys().copied().collect::<Vec<_>>());
    // With nothing painted (every card culled), nothing can be dropped on.
    assert_eq!(super::drop_targets(canvas, &BTreeMap::new()).count(), 0);
    // A painted id whose node is gone is skipped.
    let mut stale = painted.clone();
    stale.insert(NodeId::from_bytes([0xee; 16]), Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)));
    assert_eq!(super::drop_targets(canvas, &stale).count(), painted.len());
}

#[test]
fn navigation_overlay_preserves_choice_popup_pointer_commit_and_undo() {
    let mut h = schema_harness(vec![spec(
        "quality",
        ParamControl::Choice { options: vec![Choice { value: "low".into(), label: "Low".into() }, Choice { value: "high".into(), label: "High".into() }] },
        Value::Text("low".into()),
    )]);
    h.get_by_role_and_label(Role::ComboBox, "quality").click();
    h.run();
    assert!(egui::Popup::is_any_open(&h.ctx));
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    let option = h.get_by_label("High").rect().center();
    h.state_mut().view.pan += map.center() - option;
    h.run();
    let option = h.get_by_label("High").rect().center();
    assert!(map.contains(option), "real Choice option must overlap the minimap: {option:?} vs {map:?}");
    let camera = h.state().view;
    let document = h.state().session.document().clone();
    let selection = h.state().selection.clone();
    // Separate real pointer down/up frames; no accessibility action or fabricated intent.
    h.hover_at(option);
    h.event(egui::Event::PointerButton { pos: option, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos: option, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(!egui::Popup::is_any_open(&h.ctx));
    assert_eq!(node(h.state()).params.get("quality"), Some(&Value::Text("high".into())));
    assert_eq!(h.state().session.revision(), 1, "one parameter commit");
    assert_eq!(h.state().view, camera);
    assert_eq!(h.state().selection, selection);
    assert!(h.state().navigation.drag.is_none());
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &document);
    assert!(!h.state().session.can_undo());
    assert_eq!(h.state().view, camera);
}

#[test]
fn secret_editor_owns_shifted_navigation_events_and_key_service_survives_navigation_host() {
    use nodecraft_engine::secrets::{Account, MemoryStore, SecretStore, register_key_service};
    let store = Arc::new(MemoryStore::default());
    let mut services = nodecraft_engine::commands::Services::default();
    register_key_service(&mut services, Arc::clone(&store) as Arc<dyn SecretStore>).unwrap();
    let app = NodeCraftApp::with_services(services).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1100.0, 800.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    h.run();
    h.get_by_role_and_label(Role::Button, "API keys").click();
    h.run();
    h.get_by_label("openai").click();
    h.run();
    h.get_by_role_and_label(Role::PasswordInput, "Key").click();
    h.run();
    h.event(egui::Event::Text("sk-proj-integration-fixture1234".into()));
    h.run();
    let view = h.state().view;
    let visible = h.state().navigation.visible;
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let selection = h.state().selection.clone();
    for (logical, physical, text) in
        [(egui::Key::Exclamationmark, egui::Key::Num1, "!"), (egui::Key::Num2, egui::Key::Num2, "@"), (egui::Key::M, egui::Key::M, "M")]
    {
        h.event(egui::Event::Key { key: logical, physical_key: Some(physical), pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT });
        h.event(egui::Event::Text(text.into()));
        h.run();
        h.event(egui::Event::Key { key: logical, physical_key: Some(physical), pressed: false, repeat: false, modifiers: egui::Modifiers::SHIFT });
        h.run();
        assert_eq!(h.state().view, view);
        assert_eq!(h.state().navigation.visible, visible);
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().selection, selection);
        assert!(!h.state().session.can_undo() && !h.state().session.can_redo());
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_all", &json!({})).is_err());
    }
    h.get_by_label("Save").click();
    h.run();
    let stored = store.get(&Account::new("openai").unwrap()).unwrap().unwrap();
    assert!(stored.expose() == "sk-proj-integration-fixture1234!@M", "secret editor retained its text while navigation stayed inactive");
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
}

#[test]
fn active_wire_gesture_crosses_minimap_without_navigation_and_commits_once() {
    let mut h = chain_harness();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let view = h.state().view;
    let from = port_point(&h, 1, noodle::Side::Output);
    let target = port_point(&h, 3, noodle::Side::Input);
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map.center();
    h.hover_at(from);
    h.run();
    h.drag_at(from);
    h.run();
    h.hover_at(from + Vec2::new(20.0, 0.0));
    h.run();
    assert!(h.state().wiring.is_some());
    let ctx = h.ctx.clone();
    for (id, params) in
        [("canvas.fit_all", json!({})), ("canvas.fit_selection", json!({})), ("canvas.minimap_toggle", json!({})), ("canvas.pan_to", json!({"x":0,"y":0}))]
    {
        assert!(h.state_mut().navigation_enabled(&ctx, id, &params).is_err());
        assert!(h.state_mut().dispatch_navigation(&ctx, id, &params).is_err());
    }
    h.event(egui::Event::Key {
        key: egui::Key::Exclamationmark,
        physical_key: Some(egui::Key::Num1),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    h.run();
    h.event(egui::Event::Key {
        key: egui::Key::Exclamationmark,
        physical_key: Some(egui::Key::Num1),
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    h.run();
    for point in [map, target] {
        h.hover_at(point);
        h.run();
        assert!(h.state().wiring.is_some());
        assert!(!h.state().navigation.pointer_owned && h.state().navigation.drag.is_none());
        assert_eq!(h.state().view, view);
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().session.revision(), revision);
    }
    h.drop_at(target);
    h.run();
    assert!(h.state().wiring.is_none());
    assert_eq!(links(&h), [link(1, 2), link(1, 3)].into_iter().collect());
    assert_eq!(h.state().session.revision(), revision + 1);
    let connected = h.state().session.document().clone();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert_eq!(h.state().session.document(), &document);
    h.state_mut().session.dispatch("edit.redo", &json!({})).unwrap();
    h.run();
    assert_eq!(h.state().session.document(), &connected);
}

#[test]
fn minimap_owns_drag_started_over_a_covered_output_port() {
    let mut h = chain_harness();
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map.center();
    let port = port_point(&h, 4, noodle::Side::Output);
    let delta = (map - port) / h.state().view.zoom;
    let canvas = h.state().canvas;
    let position = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&chain_id(4)).unwrap().pos;
    h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas,"id":chain_id(4),"pos":{"x":position.x+delta.x,"y":position.y+delta.y}})).unwrap();
    h.run();
    assert!(port_point(&h, 4, noodle::Side::Output).distance(map) < 1.0, "fixture must cover a real painted port");
    assert!(h.state().painted.contains_key(&chain_id(4)));
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let selection = h.state().selection.clone();
    h.hover_at(map);
    h.run();
    h.drag_at(map);
    h.run();
    h.hover_at(map + Vec2::new(20.0, 10.0));
    h.run();
    assert!(h.state().navigation.pointer_owned);
    assert!(h.state().wiring.is_none());
    let target = port_point(&h, 3, noodle::Side::Input);
    h.hover_at(target);
    h.run();
    h.drop_at(target);
    h.run();
    assert!(h.state().wiring.is_none());
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().selection, selection);
}

/// Let half a second pass, so the next clicks start a new click sequence (egui counts clicks
/// within 0.3 s as one double- or triple-click).
fn idle(h: &mut Harness<'static, NodeCraftApp>) {
    for _ in 0..30 {
        h.step();
    }
}

fn nodes_of(h: &Harness<'static, NodeCraftApp>, kind: &str) -> Vec<nodecraft_graph::Node> {
    let app = h.state();
    app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().filter(|n| n.kind == kind).cloned().collect()
}

#[test]
fn quick_add_keys_create_kinds_under_the_pointer_as_one_step() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let at = Pos2::new(800.0, 560.0);
    h.hover_at(at);
    h.run();
    h.key_press(egui::Key::I);
    h.run();
    let images = nodes_of(&h, "source.image");
    assert_eq!(images.len(), 1);
    let image = images.first().unwrap();
    // Centred on the pointer, and selected.
    let world = h.state().view.to_world(h.state().viewport.min, at);
    let t = h.state().theme.tokens();
    assert!((image.pos.x - (world.x - t.card_width * 0.5)).abs() < 0.5);
    assert!((image.pos.y - (world.y - t.card_min_height * 0.5)).abs() < 0.5);
    assert_eq!(h.state().selection.nodes(), &[image.id].into_iter().collect());
    // With a modifier held the key is not a quick-add.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    h.run();
    assert_eq!(nodes_of(&h, "source.text").len(), 1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    // Typing in a text field never quick-adds.
    let rect = screen(h.state());
    click(&mut h, rect.min + Vec2::new(50.0, 55.0));
    h.key_press(egui::Key::V);
    h.event(egui::Event::Text("v".into()));
    h.run();
    assert!(nodes_of(&h, "source.video").is_empty());
}

#[test]
fn double_click_opens_a_searchable_palette_of_kinds_and_commands() {
    let mut h = harness();
    let at = Pos2::new(560.0, 650.0);
    double_click(&mut h, at);
    h.run();
    assert!(h.state().palette.is_some());
    h.event(egui::Event::Text("aud".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    let audio = nodes_of(&h, "source.audio");
    assert_eq!(audio.len(), 1);
    assert!(h.state().palette.is_none());
    let world = h.state().view.to_world(h.state().viewport.min, at);
    assert!((audio.first().unwrap().pos.x + h.state().theme.tokens().card_width * 0.5 - world.x).abs() < 0.5);

    // Commands that can run now are listed too: undo the creation from the palette. (The new
    // card sits under `at`, so open the palette on empty canvas elsewhere.)
    let empty = Pos2::new(150.0, 600.0);
    idle(&mut h);
    double_click(&mut h, empty);
    h.run();
    h.event(egui::Event::Text("undo".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(nodes_of(&h, "source.audio").is_empty());

    // Arrow keys move the highlight; Escape closes without creating anything.
    idle(&mut h);
    double_click(&mut h, empty);
    h.run();
    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(h.state().palette.as_ref().map(|p| p.highlighted), Some(1));
    let before = h.state().session.document().clone();
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().palette.is_none());
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn quick_add_bindings_are_single_letters_or_digits() {
    assert_eq!(quick_add_key("T"), Some((egui::Modifiers::NONE, egui::Key::T)));
    assert_eq!(quick_add_key("t"), Some((egui::Modifiers::NONE, egui::Key::T)));
    assert_eq!(quick_add_key("Shift+T"), Some((egui::Modifiers::SHIFT, egui::Key::T)));
    assert_eq!(quick_add_key("3"), Some((egui::Modifiers::NONE, egui::Key::Num3)));
    for bad in ["Escape", "Enter", "Space", "TT", "Á", "Ctrl+T", "Shift+", "", "+"] {
        assert_eq!(quick_add_key(bad), None, "{bad}");
    }
}

#[test]
fn held_quick_add_keys_do_not_repeat() {
    let mut h = harness();
    h.hover_at(Pos2::new(800.0, 560.0));
    h.run();
    for repeat in [false, true, true, true] {
        h.event(egui::Event::Key { key: egui::Key::T, physical_key: None, pressed: true, repeat, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    // The first press made one node; the repeats made none.
    assert_eq!(nodes_of(&h, "source.text").len(), 2);
}

#[test]
fn palette_keys_never_reach_the_canvas_and_it_closes_when_the_view_moves() {
    let mut h = harness();
    let id = node(h.state()).id;
    double_click(&mut h, Pos2::new(150.0, 600.0));
    h.run();
    // (Double-clicking empty canvas clears the selection, so select while the palette is open.)
    h.state_mut().selection.only(id);
    let corner = h.state().palette.as_ref().map(|p| p.screen).unwrap();
    // A click on the popup's padding takes focus off the query, but the palette stays open...
    click(&mut h, corner + Vec2::splat(3.0));
    assert!(h.state().palette.is_some());
    // ...and Backspace, Delete, ⌘A or Escape still don't touch the canvas selection or nodes.
    for key in [egui::Key::Backspace, egui::Key::Delete] {
        h.key_press(key);
        h.run();
    }
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().contains_key(&id));
    assert_eq!(h.state().selection.nodes(), &[id].into_iter().collect());
    // Scrolling the canvas moves the view, so the palette closes rather than create elsewhere.
    h.hover_at(Pos2::new(600.0, 100.0));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: Vec2::new(0.0, 40.0),
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    assert!(h.state().palette.is_none());
}

#[test]
fn double_clicks_on_noodles_or_cards_do_not_open_the_palette_and_rows_are_clickable() {
    let mut h = chain_harness();
    h.state_mut().view = CanvasView { pan: Vec2::new(-100.0, 100.0), zoom: 1.0 };
    h.run();
    let (a, b) = (port_point(&h, 1, noodle::Side::Output), port_point(&h, 2, noodle::Side::Input));
    double_click(&mut h, a + (b - a) * 0.5);
    h.run();
    assert!(h.state().palette.is_none());
    idle(&mut h);
    let on_card = card_point(&h, 2);
    double_click(&mut h, on_card);
    h.run();
    assert!(h.state().palette.is_none());
    idle(&mut h);
    double_click(&mut h, Pos2::new(600.0, 660.0));
    h.run();
    assert!(h.state().palette.is_some());
    h.event(egui::Event::Text("vid".into()));
    h.run();
    h.get_by_label_contains("Video").click();
    h.run();
    assert_eq!(nodes_of(&h, "source.video").len(), 1);
    assert!(h.state().palette.is_none());
}

#[test]
fn enter_right_after_typing_chooses_from_the_typed_query() {
    let mut h = harness();
    let texts = nodes_of(&h, "source.text").len();
    double_click(&mut h, Pos2::new(560.0, 650.0));
    h.run();
    assert!(h.state().palette.is_some());
    // Queued together (the harness may still deliver them a frame apart): ↵ chooses from "vid".
    h.event(egui::Event::Text("vid".into()));
    h.event(egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert_eq!(nodes_of(&h, "source.video").len(), 1);
    assert_eq!(nodes_of(&h, "source.text").len(), texts, "not the empty query's first row");
    assert!(h.state().palette.is_none());
}

#[test]
fn a_fit_while_the_palette_is_open_closes_it() {
    let mut h = harness();
    // Away from the nodes, so a fit has to move the view.
    h.state_mut().view.pan += Vec2::new(600.0, 400.0);
    h.run();
    double_click(&mut h, Pos2::new(560.0, 650.0));
    h.run();
    assert!(h.state().palette.is_some());
    // Focus leaves the query (Tab), so the fit shortcut reaches navigation and moves the view.
    h.key_press(egui::Key::Tab);
    h.run();
    let before = h.state().view;
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Num1);
    h.run();
    assert_ne!(h.state().view, before, "the fit moved the view");
    assert!(h.state().palette.is_none(), "the palette's world point no longer matches the view");
}

/// The text the last frames asked the platform to copy, if any.
fn copied(h: &Harness<'static, NodeCraftApp>) -> Option<String> {
    h.output().platform_output.commands.iter().find_map(|command| match command {
        egui::OutputCommand::CopyText(text) => Some(text.clone()),
        _ => None,
    })
}
fn counts(h: &Harness<'static, NodeCraftApp>) -> (usize, usize) {
    let app = h.state();
    let canvas = app.session.document().canvases().get(&app.canvas).unwrap();
    (canvas.nodes().len(), canvas.edges().len())
}

#[test]
fn copy_paste_cut_and_duplicate_move_nodes_with_their_noodles() {
    let mut h = chain_harness();
    // Select A and B (with the A → B noodle between them) and copy.
    press_card(&mut h, 1, egui::PointerButton::Primary, egui::Modifiers::NONE);
    press_card(&mut h, 2, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    h.event(egui::Event::Copy);
    h.step();
    let text = copied(&h).expect("a copy");
    assert!(text.contains("nodecraft.nodes"));
    h.run();
    let before = h.state().session.document().clone();
    // Paste under the pointer: two new nodes and their inner noodle, selected, one undo step.
    let at = Pos2::new(800.0, 600.0);
    h.hover_at(at);
    h.event(egui::Event::Paste(text.clone()));
    h.run();
    assert_eq!(counts(&h), (6, 3));
    let pasted = h.state().selection.nodes().clone();
    assert_eq!(pasted.len(), 2);
    assert!(pasted.iter().all(|id| !before.canvases().get(&h.state().canvas).unwrap().nodes().contains_key(id)));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    // Duplicate: the same, offset from the originals, without touching the clipboard.
    press_card(&mut h, 1, egui::PointerButton::Primary, egui::Modifiers::NONE);
    press_card(&mut h, 2, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    assert_eq!(counts(&h), (6, 3));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    // Cut: copies, then removes the nodes (and every noodle touching them) in one step.
    press_card(&mut h, 3, egui::PointerButton::Primary, egui::Modifiers::NONE);
    h.event(egui::Event::Cut);
    h.step();
    assert!(copied(&h).is_some_and(|t| t.contains("nodecraft.nodes")));
    h.run();
    assert_eq!(counts(&h), (3, 1));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn plain_text_pastes_as_a_text_node_but_not_into_a_focused_field() {
    let mut h = harness();
    h.hover_at(Pos2::new(800.0, 600.0));
    h.event(egui::Event::Paste("A lighthouse in fog".into()));
    h.run();
    let texts = nodes_of(&h, "source.text");
    assert_eq!(texts.len(), 2);
    assert!(texts.iter().any(|n| n.params.get("text") == Some(&Value::Text("A lighthouse in fog".into()))));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    // With a text field focused, the paste goes into the field, not onto the canvas.
    let rect = screen(h.state());
    click(&mut h, rect.min + Vec2::new(50.0, 55.0));
    h.event(egui::Event::Paste("into the field".into()));
    h.run();
    assert_eq!(nodes_of(&h, "source.text").len(), 1);
    // Blank text pastes nothing.
    click(&mut h, Pos2::new(900.0, 650.0));
    h.event(egui::Event::Paste("   ".into()));
    h.run();
    assert_eq!(nodes_of(&h, "source.text").len(), 1);
}

#[test]
fn paste_lands_centred_duplicate_offsets_and_unreadable_copies_explain() {
    let mut h = chain_harness();
    let t = *h.state().theme.tokens();
    press_card(&mut h, 4, egui::PointerButton::Primary, egui::Modifiers::NONE);
    let original = nodes_of(&h, "source.text").into_iter().find(|n| n.id == chain_id(4)).unwrap();
    // Duplicate: 40 points right and down of the original.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    let copy = nodes_of(&h, "source.text").into_iter().find(|n| n.id != chain_id(4) && n.id != chain_id(1)).unwrap();
    assert_eq!((copy.pos.x - original.pos.x, copy.pos.y - original.pos.y), (40.0, 40.0));
    // Paste: the card's centre lands on the pointer.
    h.event(egui::Event::Copy);
    h.step();
    let text = copied(&h).unwrap();
    h.run();
    let at = Pos2::new(800.0, 600.0);
    h.hover_at(at);
    h.event(egui::Event::Paste(text.clone()));
    h.run();
    let pasted = h.state().selection.nodes().iter().next().copied().unwrap();
    let node = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&pasted).unwrap().clone();
    let world = h.state().view.to_world(h.state().viewport.min, at);
    assert!((node.pos.x + t.card_width / 2.0 - world.x).abs() < 0.5);
    assert!((node.pos.y + t.card_min_height / 2.0 - world.y).abs() < 0.5);
    // A NodeCraft copy this version can't read gives a notice, not a Text node full of JSON.
    let before = h.state().session.document().clone();
    h.event(egui::Event::Paste(text.replace("\"version\":1", "\"version\":9")));
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(h.state().notice.as_deref().is_some_and(|n| n.starts_with("Can't paste these nodes")));
}

#[test]
fn cut_with_a_stale_selection_still_removes_what_remains() {
    let mut h = chain_harness();
    press_card(&mut h, 3, egui::PointerButton::Primary, egui::Modifiers::NONE);
    press_card(&mut h, 4, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    // Node 3 disappears behind the selection's back (e.g. undo in another view).
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.remove", &json!({"canvas": canvas, "id": chain_id(3)})).unwrap();
    h.event(egui::Event::Cut);
    h.run();
    assert!(h.state().notice.is_none(), "{:?}", h.state().notice);
    assert!(!h.state().session.document().canvases().get(&canvas).unwrap().nodes().contains_key(&chain_id(4)));
}

#[test]
fn copies_that_could_not_paste_back_are_refused_and_cut_keeps_them() {
    let mut h = chain_harness();
    let canvas = h.state().canvas;
    // Two Text nodes with 600 KiB prompts: fine in the graph, too big for one paste command.
    for n in [1u8, 4] {
        let value = json!({"type": "text", "value": "x".repeat(600 * 1024)});
        h.state_mut().session.dispatch("node.param_set", &json!({"canvas": canvas, "id": chain_id(n), "name": "text", "value": value})).unwrap();
    }
    h.run();
    press_card(&mut h, 1, egui::PointerButton::Primary, egui::Modifiers::NONE);
    press_card(&mut h, 4, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    let before = h.state().session.document().clone();
    h.event(egui::Event::Cut);
    h.step();
    assert!(copied(&h).is_none());
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("too large to copy")));
}

#[test]
fn oversized_and_foreign_media_pastes_are_refused() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    h.hover_at(Pos2::new(800.0, 600.0));
    h.event(egui::Event::Paste("x".repeat(clipboard::MAX_BYTES + 1)));
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("16 MiB")));
    // An Image node referencing an asset, copied from another project.
    let mut image = h.state().session.kinds().get("source.image").unwrap().descriptor().instantiate(NodeId::from_bytes([9; 16]), &BTreeMap::new()).unwrap();
    let asset = nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([1; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() };
    image.params.insert("asset".into(), Value::Media(asset));
    let foreign = clipboard::Payload {
        format: clipboard::FORMAT.into(),
        version: clipboard::VERSION,
        project: Some(ProjectId::from_bytes([77; 16])),
        nodes: vec![image.clone()],
        edges: vec![],
    };
    h.event(egui::Event::Paste(clipboard::to_text(&foreign).unwrap()));
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("another project")));
    // From this project, the same node pastes.
    let local = clipboard::Payload { project: Some(h.state().session.document().id()), ..foreign };
    h.event(egui::Event::Paste(clipboard::to_text(&local).unwrap()));
    h.run();
    assert_eq!(nodes_of(&h, "source.image").len(), 1);
}

#[test]
fn a_paste_while_the_keys_panel_is_open_never_reaches_the_document() {
    let mut h = harness();
    h.get_by_role_and_label(Role::Button, "API keys").click();
    h.run();
    assert!(h.state().keys.open);
    let before = h.state().session.document().clone();
    // The key field isn't focused yet: the paste must not become a Text node.
    h.hover_at(Pos2::new(400.0, 300.0));
    h.event(egui::Event::Paste("sk-proj-not-a-real-key-1234".into()));
    h.run();
    assert_eq!(h.state().session.document(), &before);
    // Copy and cut are held back too.
    h.event(egui::Event::Copy);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

fn changing_height_harness(tall: bool) -> Harness<'static, NodeCraftApp> {
    struct Height(NodeDescriptor, NodeDescriptor);
    impl NodeKind for Height {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn instance<'a>(&'a self, node: &Node) -> Result<std::borrow::Cow<'a, NodeDescriptor>, Error> {
            Ok(std::borrow::Cow::Borrowed(if node.title == "Tall" { &self.1 } else { &self.0 }))
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }
    let descriptor = |count| NodeDescriptor {
        id: "test.height".into(),
        label: "Height".into(),
        category: Category::Source,
        inputs: vec![],
        outputs: vec![],
        quick_add: None,
        params: ParamSchema { params: (0..count).map(|n| spec(&format!("p{n}"), ParamControl::Toggle, Value::Bool(false))).collect() },
    };
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Public height regression").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let mut node = Node::new(NodeId::from_bytes([3; 16]), "test.height");
    node.title = if tall { "Tall" } else { "Short" }.into();
    node.pos = nodecraft_graph::Position { x: 60.0, y: 80.0 };
    Change::AddNode { canvas, node }.apply(&mut document).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Height(descriptor(1), descriptor(6)))).unwrap();
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap())
}

fn first_frame_hover(was_tall: bool) {
    let mut h = changing_height_harness(was_tall);
    h.run();
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([3; 16]);
    let old_revision = h.state().session.revision();
    // Choose a point inside only the tall body, away from the minimap and controls.
    let pointer = h.state().view.to_screen(h.state().viewport.min, Pos2::new(70.0, 350.0));
    let short_height = h.state().theme.tokens().card_min_height;
    assert!(350.0 > 80.0 + short_height, "pointer must be outside the short body");
    h.hover_at(pointer);
    h.step(); // Kittest processes queued events as separate frames: position before the edit.
    h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":if was_tall {"Short"} else {"Tall"}})).unwrap();
    assert_eq!(h.state().schemas.get(&id).unwrap().revision, old_revision);
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let undo = h.state().session.can_undo();
    let redo = h.state().session.can_redo();
    let events = h.state().session.events_since(0).unwrap();
    let pan = h.state().view.pan;
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: Vec2::new(0.0, 40.0),
        modifiers: egui::Modifiers::NONE,
    });
    h.step(); // Deliberately the first frame: running to quiescence hides stale hover ownership.
    assert_eq!(h.state().view.pan != pan, was_tall, "shrunk card permits canvas pan; grown card owns scroll");
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.can_undo(), undo);
    assert_eq!(h.state().session.can_redo(), redo);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
}
#[test]
fn first_frame_hover_after_growth() {
    first_frame_hover(false);
}
#[test]
fn first_frame_hover_after_shrink() {
    first_frame_hover(true);
}

#[test]
fn last_shared_descriptor_removal_and_replacement_release_accounting() {
    let mut h = changing_height_harness(false);
    h.run();
    let id = NodeId::from_bytes([3; 16]);
    let first = h.state().schemas.get(&id).unwrap();
    let descriptor = Arc::clone(&first.descriptor);
    let bytes = first.descriptor_bytes;
    let identity = first.borrowed_identity.unwrap();
    let entry = |descriptor: Arc<NodeDescriptor>, identity, descriptor_bytes| CachedSchema {
        descriptor,
        visible: BTreeSet::new(),
        borrowed_identity: Some(identity),
        descriptor_bytes,
        revision: 0,
        last_used: 0,
    };
    let other = NodeId::from_bytes([4; 16]);
    let mut cache = SchemaCache::default();
    cache.insert(id, entry(Arc::clone(&descriptor), identity, bytes));
    cache.insert(other, entry(Arc::clone(&descriptor), identity, bytes));
    assert_eq!(cache_bytes(&cache), 512 + bytes);
    cache.remove(&id);
    assert_eq!(cache_bytes(&cache), 256 + bytes);
    assert_eq!(cache.shared.get(&identity).unwrap().2, 1);
    cache.remove(&other);
    assert_eq!(cache_bytes(&cache), 0);
    assert!(cache.shared.is_empty());
    cache.insert(id, entry(Arc::clone(&descriptor), identity, bytes));
    cache.insert(id, entry(Arc::clone(&descriptor), identity, bytes));
    assert_eq!(cache_bytes(&cache), 256 + bytes);
    assert_eq!(cache.shared.get(&identity).unwrap().2, 1);
    let mut replacement = (*descriptor).clone();
    replacement.label = "Different descriptor".into();
    let different = Arc::new(replacement);
    let different_bytes = descriptor_bytes(&different).unwrap();
    let new_identity = Arc::as_ptr(&different) as usize;
    assert_ne!(identity, new_identity);
    cache.insert(id, entry(Arc::clone(&different), new_identity, different_bytes));
    assert!(!cache.shared.contains_key(&identity));
    assert!(Arc::ptr_eq(&cache.get(&id).unwrap().descriptor, &different));
    assert_eq!(cache_bytes(&cache), 256 + different_bytes);
    cache.remove(&id);
    assert_eq!(cache_bytes(&cache), 0);
    assert!(cache.shared.is_empty());
}
