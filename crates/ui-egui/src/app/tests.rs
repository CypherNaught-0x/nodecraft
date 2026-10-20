use super::*;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
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
    // The Comments toolbar now occupies the top-left; this must remain bare canvas.
    let blank = h.state().viewport.min + Vec2::new(30.0, 120.0);
    click(&mut h, blank);
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
    // The Comments toolbar now occupies the top-left; this must remain bare canvas.
    let blank = h.state().viewport.min + Vec2::new(30.0, 120.0);
    click(&mut h, blank);
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
    let view = h.state().view;
    let selection = h.state().selection.clone();
    h.get_by_label("High").click();
    h.run();
    assert_eq!(node(h.state()).params.get("quality"), Some(&Value::Text("high".into())));
    assert_eq!(h.state().view, view);
    assert_eq!(h.state().selection, selection);
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
    // Scroll over bare canvas, below the Comments toolbar.
    h.hover_at(h.state().viewport.min + Vec2::new(30.0, 120.0));
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
pub(super) fn chain_harness() -> Harness<'static, NodeCraftApp> {
    chain_harness_with(|descriptor| Box::new(TestKind(descriptor)))
}
/// The chain fixture, with its test kinds wrapped by `kind`.
pub(super) fn chain_harness_with(kind: fn(NodeDescriptor) -> Box<dyn NodeKind>) -> Harness<'static, NodeCraftApp> {
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
    kinds.register(kind(relay)).unwrap();
    kinds.register(kind(collect)).unwrap();
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
pub(super) fn chain_id(n: u8) -> NodeId {
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
pub(super) fn port_point(h: &Harness<'static, NodeCraftApp>, n: u8, side: noodle::Side) -> Pos2 {
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&chain_id(n)).unwrap();
    let rect = card_geometry(&app.painted, app.session.kinds(), node, app.theme.tokens());
    app.view.to_screen(app.viewport.min, noodle::anchor(rect, side, 0, 1))
}
pub(super) fn wire_drag(h: &mut Harness<'static, NodeCraftApp>, from: Pos2, to: Pos2) {
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
pub(super) fn links(h: &Harness<'static, NodeCraftApp>) -> BTreeSet<(NodeId, NodeId)> {
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
    // Dropping on empty canvas only opens "Turn into" (a new drag closes it), and Escape
    // mid-drag does nothing.
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
    let at = Pos2::new(400.0, 300.0);
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
fn oversized_and_missing_media_pastes_are_refused() {
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
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("missing asset")));
    // The same UUID is not evidence of availability either.
    let local = clipboard::Payload { project: Some(h.state().session.document().id()), ..foreign };
    h.event(egui::Event::Paste(clipboard::to_text(&local).unwrap()));
    h.run();
    assert_eq!(h.state().session.document(), &before);
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
            if node.title == "Unavailable instance" {
                return Err(Error::new(nodecraft_types::ErrorKind::InvalidInput, "fixture instance is unavailable"));
            }
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
    first_frame_hover_captured(was_tall, None);
}

fn first_frame_hover_captured(was_tall: bool, capture: Option<&std::path::Path>) {
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
    if let Some(root) = capture {
        h.render().unwrap().save(root.join(if was_tall { "after-shrink.png" } else { "after-growth.png" })).unwrap();
    }
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
fn unavailable_instance_placeholder_owns_wheel_hover_but_not_surrounding_canvas() {
    for inside in [true, false] {
        let mut h = changing_height_harness(true);
        h.run();
        let canvas = h.state().canvas;
        let id = NodeId::from_bytes([3; 16]);
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":"Unavailable instance"})).unwrap();
        h.run();
        let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
        assert!(h.state().session.kinds().get(&node.kind).unwrap().resolve_instance(node).is_err());
        let placeholder = card::world_rect(node, None, h.state().theme.tokens());
        assert_eq!(h.state().painted.get(&id), Some(&placeholder));
        assert_eq!(h.state().navigation.geometry.rects.get(&id), Some(&placeholder));
        assert!(!h.state().navigation.geometry.sizes.contains_key(&id), "placeholder is not valid layout geometry");
        let point = if inside { placeholder.center() } else { placeholder.max + Vec2::splat(40.0) };
        let pointer = h.state().view.to_screen(h.state().viewport.min, point);
        h.hover_at(pointer);
        h.step();
        let pan = h.state().view.pan;
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            phase: egui::TouchPhase::Move,
            delta: Vec2::new(0.0, 40.0),
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        assert_eq!(h.state().view.pan == pan, inside, "painted placeholder owns wheel; surrounding canvas permits pan");
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), revision);
    }
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
        enabled: None,
        catalog: None,
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

#[test]
fn an_open_palette_keeps_search_shortcut_ownership_after_tab() {
    let mut h = harness();
    double_click(&mut h, Pos2::new(560.0, 650.0));
    h.run();
    assert!(h.state().palette.is_some());
    h.key_press(egui::Key::Tab);
    h.run();
    assert!(!canvas::text_input_focused(&h.ctx), "fixture must focus a palette row rather than its query");
    let document = h.state().session.document().clone();
    let view = h.state().view;
    let selection = h.state().selection.clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    assert!(!h.state().search.open, "search must not steal the open palette's input");
    assert!(h.state().palette.is_some());
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
    assert_eq!(h.state().selection, selection);
    assert_eq!(h.state().view, view);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().palette.is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    assert!(h.state().search.open, "normal search resumes after the palette closes");
}

#[test]
fn clipboard_paste_belongs_to_search_input_then_returns_to_canvas_after_close() {
    let mut h = harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    assert!(h.state().search.open);
    h.get_by_role_and_label(Role::TextInput, "Search canvas").click();
    h.run();
    assert!(canvas::text_input_focused(&h.ctx));
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    let selection = h.state().selection.clone();
    let view = h.state().view;
    h.event(egui::Event::Paste("clipboard query sentinel".into()));
    h.run();
    assert_eq!(h.state().search.draft, "clipboard query sentinel");
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
    assert_eq!(h.state().selection, selection);
    assert_eq!(h.state().view, view);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().search.open);
    h.hover_at(Pos2::new(800.0, 650.0));
    h.event(egui::Event::Paste("canvas payload after search".into()));
    h.run();
    let texts = nodes_of(&h, "source.text");
    assert_eq!(texts.len(), 2);
    assert!(texts.iter().any(|node| node.params.get("text") == Some(&Value::Text("canvas payload after search".into()))));
    assert_eq!(h.state().session.revision(), revision + 1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &document);
}

#[test]
fn minimap_drag_owns_quick_add_keys_until_pointer_release() {
    let mut h = harness();
    let canvas = h.state().canvas;
    let id = node(h.state()).id;
    for title in ["Before minimap", "Redo entry"] {
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":title})).unwrap();
    }
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.state_mut().selection.only(id);
    h.run();
    assert!(h.state().session.can_undo() && h.state().session.can_redo());
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let selection = h.state().selection.clone();
    let count = document.canvases().get(&canvas).unwrap().nodes().len();
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    let from = map.center() + Vec2::new(20.0, 0.0);
    h.hover_at(from);
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(h.state().navigation.drag.is_some() && h.state().navigation.pointer_owned);
    assert!(h.state().drag.is_none() && h.state().marquee.is_none() && h.state().wiring.is_none());
    for key in [egui::Key::T, egui::Key::I] {
        h.event(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
        h.run();
        assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().len(), count, "held minimap owns quick-add");
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().session.events_since(0).unwrap(), events);
        assert_eq!(h.state().selection, selection);
        assert!(h.state().session.can_undo() && h.state().session.can_redo());
        assert!(h.state().navigation.drag.is_some() && h.state().navigation.pointer_owned);
        h.event(egui::Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    // The guard must not disable the minimap's own pan intents or its frozen mapping.
    let mapping = h.state().navigation.drag.unwrap();
    let view = h.state().view;
    let to = map.right_bottom() + Vec2::new(40.0, 30.0);
    h.hover_at(to);
    h.run();
    assert_ne!(h.state().view, view);
    let center = h.state().view.visible_world(h.state().viewport).center();
    let expected = mapping.world(to);
    assert!((f64::from(center.x) - expected.0).abs() < 0.01 && (f64::from(center.y) - expected.1).abs() < 0.01);
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(h.state().navigation.drag.is_none() && !h.state().navigation.pointer_owned);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!(h.state().selection, selection);
    assert!(h.state().session.can_undo() && h.state().session.can_redo());
    h.key_press(egui::Key::T);
    h.run();
    assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().len(), count + 1);
    assert_eq!(h.state().selection.nodes().len(), 1);
    assert!(!h.state().selection.contains(id));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &document, "post-release quick-add is one undo step");
}

#[test]
fn crossing_noodles_and_culled_wiring_origin_keep_fallback_geometry() {
    let mut h = chain_harness();
    let before = h.state().session.document().clone();
    // Actual source handle starts the gesture before its card is scrolled out of view.
    let from = port_point(&h, 1, noodle::Side::Output);
    h.hover_at(from);
    h.run();
    h.drag_at(from);
    h.run();
    h.hover_at(from + Vec2::new(20.0, 0.0));
    h.run();
    assert!(h.state().wiring.is_some());
    h.state_mut().view = CanvasView { pan: Vec2::new(-360.0, 200.0), zoom: 1.0 };
    h.run();
    assert!(!h.state().painted.contains_key(&chain_id(1)));
    assert!(h.state().wiring.is_some());
    let app = h.state();
    let canvas = app.session.document().canvases().get(&app.canvas).unwrap();
    let lines = curves(canvas, &app.painted, app.session.kinds(), app.theme.tokens());
    let first = lines.iter().find(|(id, ..)| *id == EdgeId::from_bytes([10; 16])).unwrap();
    assert!(first.1.first().unwrap().x < 360.0 && first.1.last().unwrap().x > 360.0, "crossing edge still spans viewport boundary");
    let target = port_point(&h, 3, noodle::Side::Input);
    h.hover_at(target);
    h.run();
    h.drop_at(target);
    h.run();
    assert!(h.state().wiring.is_none());
    assert_eq!(links(&h), [link(1, 2), link(1, 3)].into_iter().collect());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

fn clipboard273_cut_case(failure: &str) {
    let initial = harness();
    let canvas = initial.state().canvas;
    let id = node(initial.state()).id;
    let mut document = initial.state().session.document().clone();
    let mut bad = node(initial.state()).clone();
    if failure == "ports" {
        bad.inputs.push(nodecraft_graph::Port { name: "bad".into(), port_type: PortType { modality: Modality::Text, many: false } });
    } else {
        bad.params.insert("text".into(), Value::Bool(true));
    }
    Change::SetNode { canvas, node: bad }.apply(&mut document).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(Session::new(document.clone()).unwrap(), canvas).unwrap());
    h.state_mut().selection.only(id);
    h.event(egui::Event::Cut);
    h.step();
    assert!(copied(&h).is_none(), "{failure}: must not publish rejected cut");
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), 0);
    assert!(!h.state().session.can_undo());
    assert!(h.state().session.events_since(0).unwrap().is_empty());
}
#[test]
fn clipboard273_cut_rejects_ports_before_copy_or_delete() {
    clipboard273_cut_case("ports");
}
#[test]
fn clipboard273_cut_rejects_params_before_copy_or_delete() {
    clipboard273_cut_case("params");
}
#[test]
fn clipboard273_busy_does_not_clone() {
    let mut h = harness();
    h.state_mut().keys.open = true;
    let before = h.state().session.document().clone();
    PASTE_CLONES.with(|n| n.set(0));
    h.event(egui::Event::Paste("public marker".into()));
    h.step();
    assert_eq!(PASTE_CLONES.with(|n| n.get()), 0);
    assert_eq!(h.state().session.document(), &before);
}
#[test]
fn clipboard273_final_raw_paste_event_is_the_only_copy() {
    let mut h = harness();
    PASTE_CLONES.with(|n| n.set(0));
    h.input_mut().events.extend([egui::Event::Paste("first".into()), egui::Event::Paste("last".into())]);
    h.step();
    assert_eq!(PASTE_CLONES.with(|n| n.get()), 1);
    assert_eq!(nodes_of(&h, "source.text").len(), 2);
    let id = *h.state().selection.nodes().iter().next().unwrap();
    assert_eq!(
        h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&id).unwrap().params.get("text"),
        Some(&Value::Text("last".into()))
    );
}
#[test]
fn clipboard273_final_oversized_paste_never_falls_back() {
    for oversized_last in [true, false] {
        let mut h = harness();
        let before = h.state().session.document().clone();
        PASTE_CLONES.with(|n| n.set(0));
        let big = egui::Event::Paste("x".repeat(clipboard::MAX_BYTES + 1));
        let small = egui::Event::Paste("accepted final".into());
        h.input_mut().events.extend(if oversized_last { [small, big] } else { [big, small] });
        h.step();
        assert_eq!(PASTE_CLONES.with(|n| n.get()), usize::from(!oversized_last));
        if oversized_last {
            assert_eq!(h.state().session.document(), &before);
        } else {
            assert_eq!(nodes_of(&h, "source.text").len(), 2);
        }
    }
}
#[test]
fn clipboard273_aggregate_refusal_precedes_projected_node_copies() {
    let mut h = chain_harness();
    let canvas = h.state().canvas;
    for n in [1, 4] {
        h.state_mut()
            .session
            .dispatch("node.param_set", &json!({"canvas":canvas,"id":chain_id(n),"name":"text","value":{"type":"text","value":"x".repeat(600*1024)}}))
            .unwrap();
    }
    h.run();
    h.state_mut().selection.replace(ids(&[1, 4]));
    clipboard::PROJECTED_NODES.with(|n| n.set(0));
    h.event(egui::Event::Copy);
    h.step();
    assert!(copied(&h).is_none());
    assert_eq!(clipboard::PROJECTED_NODES.with(|n| n.get()), 0);
}
#[test]
fn clipboard273_matching_uuid_does_not_prove_asset_availability() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let mut unknown = Node::new(NodeId::from_bytes([71; 16]), "missing.plugin");
    unknown.params.insert(
        "asset".into(),
        Value::Media(nodecraft_types::AssetRef {
            hash: nodecraft_types::Blake3::from_bytes([7; 32]),
            mime: "application/octet-stream".parse().unwrap(),
            meta: Default::default(),
        }),
    );
    let payload = clipboard::Payload { format: clipboard::FORMAT.into(), version: 1, project: Some(before.id()), nodes: vec![unknown], edges: vec![] };
    h.event(egui::Event::Paste(clipboard::to_text(&payload).unwrap()));
    h.step();
    assert_eq!(h.state().session.document(), &before);
}
#[test]
fn clipboard273_hovered_minimap_uses_unobscured_paste_target() {
    let mut h = harness();
    let app = h.state();
    let overlay = crate::navigation::layout(app.viewport, true, app.theme.tokens()).overlay;
    h.hover_at(overlay.center());
    h.event(egui::Event::Paste("Map placement".into()));
    h.run();
    let id = *h.state().selection.nodes().iter().next().unwrap();
    let app = h.state();
    let rect = app.view.rect_to_screen(app.viewport.min, *app.painted.get(&id).unwrap());
    assert!(!overlay.contains(rect.center()), "pasted card was hidden under navigation");
}

// One raw frame is essential: Harness::event may otherwise deliver queued input separately.
fn minimap_clipboard_action(kind: usize) -> egui::Event {
    match kind {
        0 => egui::Event::Paste("Minimap clipboard sentinel".into()),
        1 => egui::Event::Cut,
        2 => egui::Event::Copy,
        3 => egui::Event::Key { key: egui::Key::D, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
        _ => egui::Event::Key { key: egui::Key::T, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
    }
}
fn minimap_raw_frame(h: &mut Harness<'static, NodeCraftApp>, events: Vec<egui::Event>) {
    h.input_mut().events.extend(events);
    h.step();
}
fn minimap_assert_unchanged(h: &Harness<'static, NodeCraftApp>, doc: &nodecraft_graph::Project, selection: &Selection) {
    assert_eq!(h.state().session.document(), doc);
    assert_eq!(h.state().session.revision(), 0);
    assert_eq!(&h.state().selection, selection);
    assert!(!h.state().session.can_undo());
    assert!(!h.state().session.can_redo());
    assert!(h.state().session.events_since(0).unwrap().is_empty());
    assert!(copied(h).is_none());
}
#[test]
fn held_minimap_blocks_clipboard_outside_map_then_release_restores_actions() {
    for kind in 0..5 {
        let mut h = harness();
        let id = node(h.state()).id;
        h.state_mut().selection.only(id);
        let doc = h.state().session.document().clone();
        let selected = h.state().selection.clone();
        let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
        minimap_raw_frame(
            &mut h,
            vec![
                egui::Event::PointerMoved(map.center()),
                egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            ],
        );
        assert!(h.state().navigation.pointer_owned);
        let outside = Pos2::new(80.0, 120.0);
        minimap_raw_frame(&mut h, vec![egui::Event::PointerMoved(outside), minimap_clipboard_action(kind)]);
        minimap_assert_unchanged(&h, &doc, &selected);
        if kind >= 3 {
            assert!(h.ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))), "blocked key unconsumed");
        }
        minimap_raw_frame(
            &mut h,
            vec![egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
        );
        assert!(!h.state().navigation.pointer_owned);
        if let egui::Event::Key { key, physical_key, modifiers, .. } = minimap_clipboard_action(kind) {
            minimap_raw_frame(&mut h, vec![egui::Event::Key { key, physical_key, pressed: false, repeat: false, modifiers }]);
        }
        minimap_raw_frame(&mut h, vec![minimap_clipboard_action(kind)]);
        if kind == 2 {
            assert!(copied(&h).is_some());
            assert_eq!(h.state().session.document(), &doc);
        } else {
            assert_eq!(h.state().session.revision(), 1, "released action {kind}");
            assert!(h.state().session.can_undo());
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(h.state().session.document(), &doc);
        }
    }
}
#[test]
fn same_frame_minimap_press_owns_clipboard_and_quick_add_in_both_event_orders() {
    for kind in 0..5 {
        for reverse in [false, true] {
            let mut h = harness();
            let id = node(h.state()).id;
            h.state_mut().selection.only(id);
            let doc = h.state().session.document().clone();
            let selected = h.state().selection.clone();
            let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
            let press = egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
            let mut events = vec![press, minimap_clipboard_action(kind)];
            if reverse {
                events.reverse();
            }
            events.insert(0, egui::Event::PointerMoved(map.center()));
            minimap_raw_frame(&mut h, events);
            assert!(h.state().navigation.pointer_owned);
            minimap_assert_unchanged(&h, &doc, &selected);
            if kind >= 3 {
                assert!(h.ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))), "blocked key unconsumed");
            }
        }
    }
}

#[test]
fn ordinary_canvas_press_still_allows_same_frame_paste_and_quick_add() {
    for kind in [0, 3, 4] {
        let mut h = harness();
        let id = node(h.state()).id;
        h.state_mut().selection.only(id);
        let at = Pos2::new(80.0, 120.0);
        minimap_raw_frame(
            &mut h,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                minimap_clipboard_action(kind),
            ],
        );
        assert_eq!(h.state().session.revision(), 1, "ordinary canvas action {kind}");
        assert!(!h.state().navigation.pointer_owned);
    }
}

#[test]
fn independent_review291_press_inside_then_move_outside_keeps_clipboard_owned() {
    let mut h = harness();
    let id = node(h.state()).id;
    h.state_mut().selection.only(id);
    let before = h.state().session.document().clone();
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    let outside = Pos2::new(80.0, 120.0);
    minimap_raw_frame(
        &mut h,
        vec![
            egui::Event::PointerMoved(map.center()),
            egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            egui::Event::PointerMoved(outside),
            egui::Event::Paste("review291 same frame pointer travel".into()),
        ],
    );
    assert!(h.ctx.input(|i| i.pointer.press_origin().is_some_and(|p| map.contains(p))));
    assert_eq!(
        h.state().session.document(),
        &before,
        "map press origin is inside, final pointer outside, owned={}, revision={}",
        h.state().navigation.pointer_owned,
        h.state().session.revision()
    );
    assert!(h.state().navigation.pointer_owned);
}

#[test]
fn minimap_press_origin_owns_cross_boundary_frame_in_both_action_orders() {
    for inside in [true, false] {
        for kind in 0..5 {
            for action_first in [true, false] {
                let mut h = harness();
                let id = node(h.state()).id;
                h.state_mut().selection.only(id);
                let doc = h.state().session.document().clone();
                let selected = h.state().selection.clone();
                let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
                let outside = Pos2::new(80.0, 120.0);
                let (from, to) = if inside { (map.center(), outside) } else { (outside, map.center()) };
                let mut events = vec![
                    egui::Event::PointerMoved(from),
                    egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                    egui::Event::PointerMoved(to),
                ];
                if action_first {
                    events.insert(0, minimap_clipboard_action(kind));
                } else {
                    events.push(minimap_clipboard_action(kind));
                }
                minimap_raw_frame(&mut h, events);
                assert_eq!(h.ctx.input(|i| i.pointer.press_origin().is_some_and(|p| map.contains(p))), inside);
                assert_eq!(h.state().navigation.pointer_owned, inside, "origin ownership inside={inside},kind={kind},first={action_first}");
                if inside {
                    minimap_assert_unchanged(&h, &doc, &selected);
                    if kind >= 3 {
                        assert!(h.ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))));
                    }
                } else if kind == 2 {
                    assert!(copied(&h).is_some());
                } else {
                    assert_eq!(h.state().session.revision(), 1);
                }
            }
        }
    }
}
#[test]
fn minimap_press_admission_is_stable_across_later_visibility_shortcut() {
    for visible in [true, false] {
        let mut h = harness();
        h.state_mut().navigation.visible = visible;
        h.run();
        let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
        minimap_raw_frame(
            &mut h,
            vec![
                egui::Event::PointerMoved(map.center()),
                egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                egui::Event::Key { key: egui::Key::M, physical_key: Some(egui::Key::M), pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT },
                egui::Event::Paste("visibility toggle payload".into()),
            ],
        );
        assert_eq!(h.state().navigation.visible, !visible);
        assert!(!h.state().navigation.pointer_owned, "hidden initial press cannot be acquired after showing map");
        assert_eq!(h.state().session.revision(), u64::from(!visible));
    }
}
#[test]
fn minimap_first_primary_press_wins_multiple_presses_in_one_frame() {
    for first_inside in [false, true] {
        let mut h = harness();
        let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
        let outside = Pos2::new(80.0, 120.0);
        let (first, second) = if first_inside { (map.center(), outside) } else { (outside, map.center()) };
        minimap_raw_frame(
            &mut h,
            vec![
                egui::Event::PointerMoved(first),
                egui::Event::PointerButton { pos: first, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                egui::Event::PointerButton { pos: first, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
                egui::Event::PointerMoved(second),
                egui::Event::PointerButton { pos: second, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                egui::Event::Paste("multiple presses".into()),
            ],
        );
        assert_eq!(h.state().navigation.pointer_owned, first_inside);
        assert_eq!(h.state().session.revision(), u64::from(!first_inside));
    }
}
#[test]
fn minimap_admission_after_delete_blocks_paste_but_preserves_delete_history() {
    let mut h = harness();
    let id = node(h.state()).id;
    h.state_mut().selection.only(id);
    let doc = h.state().session.document().clone();
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    minimap_raw_frame(
        &mut h,
        vec![
            egui::Event::PointerMoved(map.center()),
            egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
            egui::Event::Paste("after delete".into()),
        ],
    );
    assert!(h.state().navigation.pointer_owned);
    assert_eq!(counts(&h).0, 0);
    assert_eq!(h.state().session.revision(), 1);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &doc);
}

#[test]
fn clipboard273_actual_foreign_assets_allow_paste_cut_and_undo() {
    let mut h = harness();
    let hash = h.state_mut().session.import_asset(b"public shared clipboard bytes").unwrap();
    let mut n = Node::new(NodeId::from_bytes([73; 16]), "test.asset");
    n.config.insert(
        "nested".into(),
        Value::List(vec![Value::Media(nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() })]),
    );
    let payload =
        clipboard::Payload { format: clipboard::FORMAT.into(), version: 1, project: Some(ProjectId::from_bytes([99; 16])), nodes: vec![n], edges: vec![] };
    h.event(egui::Event::Paste(clipboard::to_text(&payload).unwrap()));
    h.run();
    assert_eq!(counts(&h).0, 2);
    let beforecut = h.state().session.document().clone();
    h.event(egui::Event::Cut);
    h.step();
    let text = copied(&h).unwrap();
    assert_eq!(counts(&h).0, 1);
    h.event(egui::Event::Paste(text));
    h.run();
    assert_eq!(counts(&h).0, 2);
    for _ in 0..2 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run();
    }
    assert_eq!(h.state().session.document(), &beforecut);
}
#[test]
fn clipboard273_same_uuid_forks_require_destination_bytes() {
    let mut source = harness();
    let mut destination = harness();
    assert_eq!(source.state().session.document().id(), destination.state().session.document().id());
    let hash = source.state_mut().session.import_asset(b"fork asset").unwrap();
    assert!(destination.state().session.assets().get(&hash).is_none());
    let mut n = Node::new(NodeId::from_bytes([75; 16]), "test.asset");
    n.params.insert("asset".into(), Value::Media(nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }));
    let payload = clipboard::Payload {
        format: clipboard::FORMAT.into(),
        version: 1,
        project: Some(source.state().session.document().id()),
        nodes: vec![n],
        edges: vec![],
    };
    let text = clipboard::to_text(&payload).unwrap();
    let before = destination.state().session.document().clone();
    destination.event(egui::Event::Paste(text.clone()));
    destination.run();
    assert_eq!(destination.state().session.document(), &before);
    assert_eq!(destination.state_mut().session.import_asset(b"fork asset").unwrap(), hash);
    destination.event(egui::Event::Paste(text));
    destination.run();
    assert_eq!(counts(&destination).0, 2);
}
#[test]
fn clipboard273_plain_text_uses_resolved_card_center() {
    struct Shape(NodeDescriptor, NodeDescriptor);
    impl NodeKind for Shape {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn instance<'a>(&'a self, _: &Node) -> Result<std::borrow::Cow<'a, NodeDescriptor>, Error> {
            Ok(std::borrow::Cow::Borrowed(&self.1))
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }
    let text = spec("text", ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, Value::Text(String::new()));
    let short = NodeDescriptor {
        id: "source.text".into(),
        label: "Text".into(),
        category: Category::Source,
        inputs: vec![],
        outputs: vec![],
        params: ParamSchema { params: vec![text] },
        quick_add: None,
    };
    let mut tall = short.clone();
    for n in 0..6 {
        tall.params.params.push(spec(&format!("toggle{n}"), ParamControl::Toggle, Value::Bool(false)));
    }
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Measured text").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Shape(short, tall))).unwrap();
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
    let at = Pos2::new(400.0, 300.0);
    h.hover_at(at);
    h.event(egui::Event::Paste("Measured multiline\ntext".into()));
    h.run();
    let id = *h.state().selection.nodes().iter().next().unwrap();
    let app = h.state();
    let rect = app.view.rect_to_screen(app.viewport.min, *app.painted.get(&id).unwrap());
    assert!((rect.center() - at).length() < 0.5, "actual dynamic painted body must center on pointer");
    assert!(rect.height() > app.theme.tokens().card_min_height + 50.0);
}

#[test]
fn clipboard273_duplicate_uses_theme_world_offset_at_every_zoom() {
    for zoom in [0.3, 2.0] {
        let mut h = chain_harness();
        h.state_mut().view.zoom = zoom;
        h.state_mut().theme = ThemeKind::Light;
        h.state_mut().selection.only(chain_id(4));
        let original = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&chain_id(4)).unwrap().pos;
        let payload = h.state().selection_payload().unwrap().unwrap();
        let mut tokens = *h.state().theme.tokens();
        tokens.duplicate_offset = 96.0;
        h.state_mut().duplicate_payload(payload, &tokens);
        let id = *h.state().selection.nodes().iter().next().unwrap();
        let pos = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&id).unwrap().pos;
        assert_eq!((pos.x - original.x, pos.y - original.y), (96.0, 96.0));
        assert!(h.state().session.can_undo());
    }
}

#[test]
fn a_real_choice_popup_over_the_tag_panel_keeps_its_parameter_intent() {
    let mut h = schema_harness(vec![spec(
        "quality",
        ParamControl::Choice { options: vec![Choice { value: "low".into(), label: "Low".into() }, Choice { value: "high".into(), label: "High".into() }] },
        Value::Text("low".into()),
    )]);
    h.get_by_role_and_label(Role::ComboBox, "quality").click();
    h.run();
    let high = h.get_by_label("High").rect().center();
    h.state_mut().view.pan += Pos2::new(150.0, 180.0) - high;
    h.state_mut().tag_filter.open = true;
    h.run();
    let high = h.get_by_label("High").rect().center();
    assert!(h.state().tag_rect().contains(high));
    h.get_by_label("High").click();
    h.run();
    assert_eq!(node(h.state()).params.get("quality"), Some(&Value::Text("high".into())));
    assert_eq!(h.state().session.revision(), 1);
    assert!(!h.state().tag_filter.active());
}

#[test]
fn tag_decoration_clip_contains_visible_negative_world_cards_at_pan_and_zoom() {
    let viewport = Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(1200.0, 800.0));
    for zoom in [0.15, 1.0, 2.0] {
        let view = CanvasView { zoom, pan: Vec2::new(600.0, 400.0) };
        let clip = tag_world_clip(&view, viewport, viewport);
        let negative = Rect::from_min_size(Pos2::new(-200.0, -100.0), Vec2::new(100.0, 50.0));
        assert!(view.rect_to_screen(viewport.min, negative).intersects(viewport));
        assert!(clip.intersects(negative));
        assert!(clip.contains(Pos2::new(-194.0, -94.0)));
    }
}

#[test]
fn emitted_tag_marker_and_outline_use_world_clip_then_respect_tighter_screen_parent() {
    let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_max_steps(160).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            ui.set_clip_rect(ui.max_rect().shrink(40.0));
            app.show(ui);
        },
        NodeCraftApp::headless().unwrap(),
    );
    let id = NodeId::from_bytes([3; 16]);
    h.state_mut().selection.only(id);
    h.state_mut().apply_tag_action(super::tags::TagAction::Palette(5)).unwrap();
    h.run();
    let c = h.state().canvas;
    let tag = h.state().session.document().canvases().get(&c).unwrap().nodes().get(&id).unwrap().tag.unwrap();
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[tag],"untagged":false})).unwrap();
    for zoom in [0.15, 1.0, 2.0] {
        h.state_mut().view.zoom = zoom;
        h.run();
        let app = h.state();
        let t = app.theme.tokens();
        let world = app.painted[&id];
        assert!(world.min.x < 0.0 && world.min.y < 0.0);
        let marker = Rect::from_min_size(world.min + Vec2::splat(t.navigation_gap), Vec2::new(t.tag_marker_width, t.tag_row_height.min(world.height())));
        let expected = app.view.rect_to_screen(app.viewport.min, marker);
        let outline = app.view.rect_to_screen(app.viewport.min, world.expand(t.tag_marker_width));
        let parent = app.viewport.shrink(40.0);
        let marker_shape = h
            .output()
            .shapes
            .iter()
            .find(|shape| matches!(&shape.shape,egui::epaint::Shape::Rect(r) if r.fill==t.tag_color(Some("blue"))))
            .expect("actual painted marker");
        let egui::epaint::Shape::Rect(painted) = &marker_shape.shape else {
            panic!("rectangle marker");
        };
        assert!((painted.rect.min - expected.min).length() < 0.1);
        assert!((painted.rect.max - expected.max).length() < 0.1);
        assert!(marker_shape.clip_rect.contains(expected.center()));
        assert!((marker_shape.clip_rect.min - parent.min).length() < 0.1);
        assert!((marker_shape.clip_rect.max - parent.max).length() < 0.1);
        assert!(!marker_shape.clip_rect.contains(app.viewport.min));
        assert!(h.output().shapes.iter().any(|shape|matches!(&shape.shape,egui::epaint::Shape::Rect(r) if r.stroke.color==t.accent&&(r.rect.min-outline.min).length()<0.1&&(r.rect.max-outline.max).length()<0.1)&& (shape.clip_rect.min-parent.min).length()<0.1));
    }
}

#[test]
fn minimap_does_not_take_held_or_cancelled_discussion_pin_clipboard_and_quick_add() {
    for cancel in [false, true] {
        for kind in 0..5 {
            let mut h = harness();
            let canvas = h.state().canvas;
            let comment = nodecraft_types::CommentId::from_bytes([91; 16]);
            h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":comment,"text":"Public pin ownership","pos":{"x":0,"y":0}})).unwrap();
            h.run();
            let from = h.get_by_role_and_label(Role::Button, &format!("Discussion pin {comment}")).rect().center();
            minimap_raw_frame(
                &mut h,
                vec![
                    egui::Event::PointerMoved(from),
                    egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                ],
            );
            minimap_raw_frame(&mut h, vec![egui::Event::PointerMoved(from + Vec2::new(35.0, 15.0))]);
            assert!(h.state().comments.drag.is_some(), "a real pin gesture owns the pointer");
            if cancel {
                let node = node(h.state()).id;
                h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":node,"title":"Changed while holding pin"})).unwrap();
                h.step();
                assert!(h.state().comments.drag.is_none());
                assert!(h.state().comments.gesture_cancelled);
            }
            let document = h.state().session.document().clone();
            let revision = h.state().session.revision();
            let events = h.state().session.events_since(0).unwrap();
            let selection = h.state().selection.clone();
            let history = (h.state().session.can_undo(), h.state().session.can_redo());
            let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
            minimap_raw_frame(&mut h, vec![egui::Event::PointerMoved(map.center()), minimap_clipboard_action(kind)]);
            assert!(!h.state().navigation.pointer_owned && h.state().navigation.drag.is_none());
            let ctx = h.ctx.clone();
            assert!(!h.state_mut().minimap_input_owned(&ctx), "comment ownership must refuse navigation admission");
            assert_eq!(h.state().session.document(), &document);
            assert_eq!(h.state().session.revision(), revision);
            assert_eq!(h.state().session.events_since(0).unwrap(), events);
            assert_eq!(h.state().selection, selection);
            assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
            assert!(copied(&h).is_none());
            if kind >= 3 {
                assert!(h.ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))));
            }
            // Cancel an active pin before release so this control commits no comment movement.
            if !cancel {
                minimap_raw_frame(
                    &mut h,
                    vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
                );
            }
            minimap_raw_frame(
                &mut h,
                vec![egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
            );
            assert!(h.state().comments.drag.is_none() && !h.state().comments.gesture_cancelled);
            minimap_raw_frame(
                &mut h,
                vec![
                    egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                    egui::Event::Paste("A fresh map press still owns paste".into()),
                ],
            );
            assert!(h.state().navigation.pointer_owned, "a fresh press after pin release admits the minimap");
            assert_eq!(h.state().session.document(), &document);
        }
    }
}

#[test]
fn minimap_refused_marquee_press_stays_refused_after_escape_until_release() {
    let mut h = harness();
    let selected = h.state().selection.clone();
    h.state_mut().marquee = Some(Marquee { start: Pos2::ZERO, base: selected.clone(), cancelled: false });
    let document = h.state().session.document().clone();
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    minimap_raw_frame(
        &mut h,
        vec![
            egui::Event::PointerMoved(map.center()),
            egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
            egui::Event::Paste("Cancelled marquee payload".into()),
        ],
    );
    assert!(h.state().marquee.as_ref().is_some_and(|m| m.cancelled));
    assert!(!h.state().navigation.pointer_owned);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().selection, selected);
    minimap_raw_frame(&mut h, vec![egui::Event::PointerMoved(map.center() + Vec2::new(5.0, 0.0))]);
    assert!(!h.state().navigation.pointer_owned);
    minimap_raw_frame(
        &mut h,
        vec![egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
    );
    assert!(h.state().marquee.is_none());
    minimap_raw_frame(
        &mut h,
        vec![egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }],
    );
    assert!(h.state().navigation.pointer_owned);
}

#[test]
#[ignore = "manual public-scene capture; requires NODECRAFT_291_SCREENSHOT"]
fn minimap291_visual_current() {
    let blocked = true;
    let mut h = Harness::builder().with_step_dt(1.0 / 60.0).with_size(Vec2::new(1000.0, 700.0)).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            ui.heading("NodeCraft public minimap clipboard fixture");
            let count = app.session.document().canvases().get(&app.canvas).unwrap().nodes().len();
            ui.label(format!("Document nodes: {count}"));
            app.show(ui);
        },
        NodeCraftApp::headless().unwrap(),
    );
    let selected = node(h.state()).id;
    h.state_mut().selection.only(selected);
    let original = h.state().session.document().clone();
    let camera = h.state().view;
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    h.input_mut().events.extend([
        egui::Event::PointerMoved(map.center()),
        egui::Event::PointerButton { pos: map.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
    ]);
    h.step();
    assert!(h.state().navigation.pointer_owned);
    let outside = Pos2::new(220.0, 150.0);
    assert!(!map.contains(outside));
    h.input_mut().events.extend([egui::Event::PointerMoved(outside), egui::Event::Paste("Minimap paste public proof".into())]);
    h.step();
    assert!(h.state().navigation.pointer_owned);
    let expected = if blocked { 1 } else { 2 };
    assert_eq!(counts(&h).0, expected);
    assert_eq!(h.state().session.revision(), u64::from(!blocked));
    if blocked {
        assert_eq!(h.state().session.document(), &original);
    }
    // Capture the actual outcome after releasing, with identical original view for readability.
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: outside,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.state_mut().view = camera;
    h.run();
    assert_eq!(counts(&h).0, expected);
    h.render().unwrap().save(std::env::var("NODECRAFT_291_SCREENSHOT").unwrap()).unwrap();
    println!("Rendered actual held-map Paste outcome: blocked={blocked}, nodes={expected}, revision={}", h.state().session.revision());
}

fn bulk71_app() -> NodeCraftApp {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let mut document = app.session.document().clone();
    let original_id = node(&app).id;
    let mut first = node(&app).clone();
    first.pos = nodecraft_graph::Position { x: -360.0, y: -120.0 };
    first.params.insert("text".into(), Value::Text("A quiet mountain lake".into()));
    Change::SetNode { canvas, node: first.clone() }.apply(&mut document).unwrap();
    let second_id = NodeId::from_bytes([71; 16]);
    first.id = second_id;
    first.pos = nodecraft_graph::Position { x: 20.0, y: -120.0 };
    first.params.insert("text".into(), Value::Text("An evening forest trail".into()));
    Change::AddNode { canvas, node: first }.apply(&mut document).unwrap();
    app.session = Session::new(document).unwrap();
    app.selection.replace([original_id, second_id].into_iter().collect());
    app
}
fn bulk71_harness() -> Harness<'static, NodeCraftApp> {
    Harness::builder().with_step_dt(1.0 / 60.0).with_size(Vec2::new(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            ui.heading("NodeCraft — shared parameters");
            app.show(ui);
        },
        bulk71_app(),
    )
}
#[test]
#[ignore = "manual public-safe bulk panel capture; requires NODECRAFT_BULK71_SCREENSHOT"]
fn bulk71_visual_current() {
    let mut h = bulk71_harness();
    h.run();
    assert_eq!(h.state().selection.nodes().len(), 2);
    h.render().unwrap().save(std::env::var("NODECRAFT_BULK71_SCREENSHOT").unwrap()).unwrap();
}
#[test]
fn bulk71_mixed_text_edits_all_with_one_undo_and_owns_shortcuts() {
    let mut h = bulk71_harness();
    h.run();
    let before = h.state().session.document().clone();
    let edit = h.get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    click(&mut h, edit);
    h.event(egui::Event::Text("A shared public-safe prompt".into()));
    h.run();
    assert_eq!(h.get_by_role_and_label(Role::MultilineTextInput, "Text").value().as_deref(), Some("A shared public-safe prompt"));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    assert_eq!(canvas.nodes().len(), 2);
    assert!(
        canvas.nodes().values().all(|node| node.params.get("text") == Some(&Value::Text("A shared public-safe prompt".into()))),
        "actual nodes: {:?}, notice: {:?}, revision: {}",
        canvas.nodes(),
        h.state().notice,
        h.state().session.revision()
    );
    assert_eq!(h.state().session.revision(), 1);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
}
#[test]
fn bulk71_selection_change_never_commits_previous_draft() {
    let mut h = bulk71_harness();
    h.run();
    let before = h.state().session.document().clone();
    let edit = h.get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    click(&mut h, edit);
    h.event(egui::Event::Text("Do not apply to another selection".into()));
    h.run();
    assert_eq!(h.get_by_role_and_label(Role::MultilineTextInput, "Text").value().as_deref(), Some("Do not apply to another selection"));
    let id = *h.state().selection.nodes().first().unwrap();
    h.state_mut().selection.only(id);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().session.revision(), 0);
}

fn bulk71_custom_harness(params: Vec<ParamSpec>, second_params: BTreeMap<String, Value>) -> Harness<'static, NodeCraftApp> {
    let mut h = schema_harness(params);
    let canvas = h.state().canvas;
    let first = node(h.state()).clone();
    let descriptor = h.state().session.kinds().get(&first.kind).unwrap().descriptor().clone();
    let mut document = h.state().session.document().clone();
    let mut second = first.clone();
    second.id = NodeId::from_bytes([71; 16]);
    second.pos.x += 380.0;
    second.params.extend(second_params);
    let ids = [first.id, second.id].into_iter().collect();
    Change::AddNode { canvas, node: second }.apply(&mut document).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(TestKind(descriptor))).unwrap();
    h.state_mut().session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    h.state_mut().selection.replace(ids);
    h
}
#[test]
fn bulk71_mixed_toggle_and_choice_use_explicit_values_and_keep_owned_popup_live() {
    let mut h = schema_harness(vec![
        spec("enabled", ParamControl::Toggle, Value::Bool(false)),
        spec(
            "quality",
            ParamControl::Choice { options: vec![Choice { value: "low".into(), label: "Low".into() }, Choice { value: "high".into(), label: "High".into() }] },
            Value::Text("low".into()),
        ),
    ]);
    let canvas = h.state().canvas;
    let first = node(h.state()).clone();
    let descriptor = h.state().session.kinds().get(&first.kind).unwrap().descriptor().clone();
    let mut document = h.state().session.document().clone();
    let mut second = first.clone();
    second.id = NodeId::from_bytes([71; 16]);
    second.pos.x += 380.0;
    second.params.insert("enabled".into(), Value::Bool(true));
    second.params.insert("quality".into(), Value::Text("high".into()));
    let ids = [first.id, second.id].into_iter().collect();
    Change::AddNode { canvas, node: second }.apply(&mut document).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(TestKind(descriptor))).unwrap();
    h.state_mut().session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    h.state_mut().selection.replace(ids);
    h.run();
    let original = h.state().session.document().clone();
    let on = h.get_by_role_and_label(Role::Button, "On for all").rect().center();
    click(&mut h, on);
    assert!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().values().all(|node| node.params.get("enabled") == Some(&Value::Bool(true))));
    let choice = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::ComboBox, "quality").rect().center();
    click(&mut h, choice);
    h.run();
    let high = h.get_by_label("High").rect().center();
    click(&mut h, high);
    assert!(
        h.state()
            .session
            .document()
            .canvases()
            .get(&canvas)
            .unwrap()
            .nodes()
            .values()
            .all(|node| node.params.get("quality") == Some(&Value::Text("high".into())))
    );
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &original);
}

#[test]
fn bulk71_another_control_cannot_replace_a_blurring_text_commit() {
    let mut h = bulk71_custom_harness(
        vec![
            spec("text", ParamControl::Text { multiline: true, max_bytes: 1024 }, Value::Text("First".into())),
            spec("enabled", ParamControl::Toggle, Value::Bool(false)),
        ],
        BTreeMap::from([("text".into(), Value::Text("Second".into())), ("enabled".into(), Value::Bool(true))]),
    );
    h.run();
    let canvas = h.state().canvas;
    let original = h.state().session.document().clone();
    let edit = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    click(&mut h, edit);
    h.event(egui::Event::Text("Keep this committed draft".into()));
    h.run();
    let on = h.get_by_role_and_label(Role::Button, "On for all").rect().center();
    click(&mut h, on);
    let nodes = h.state().session.document().canvases().get(&canvas).unwrap().nodes();
    assert!(nodes.values().all(|node| node.params.get("text") == Some(&Value::Text("Keep this committed draft".into()))));
    assert_eq!(nodes.values().filter(|node| node.params.get("enabled") == Some(&Value::Bool(true))).count(), 1);
    assert_eq!(h.state().session.revision(), 1);
    let on = h.get_by_role_and_label(Role::Button, "On for all").rect().center();
    click(&mut h, on);
    assert_eq!(h.state().session.revision(), 2);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &original);
}

#[test]
fn bulk71_conditional_disabled_row_refuses_pointer_edit_until_all_targets_enable_it() {
    use nodecraft_nodes::{JobSpec, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{Condition, Error, HiddenPolicy, ParamRule, PortType};
    struct Conditional(NodeDescriptor);
    impl NodeKind for Conditional {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
        fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
            let mut c = NodeContract::derive(descriptor);
            c.params.rules.push(ParamRule {
                enabled_when: Some(Condition::Equals { param: "show".into(), value: Value::Bool(true) }),
                hidden: HiddenPolicy::Reset,
                ..ParamRule::implied("text")
            });
            c
        }
    }
    let mut h = bulk71_custom_harness(
        vec![
            spec("show", ParamControl::Toggle, Value::Bool(true)),
            spec("text", ParamControl::Text { multiline: true, max_bytes: 1024 }, Value::Text("First".into())),
        ],
        BTreeMap::from([("show".into(), Value::Bool(false)), ("text".into(), Value::Text("Second".into()))]),
    );
    let descriptor = h.state().session.kinds().get(&node(h.state()).kind).unwrap().descriptor().clone();
    let document = h.state().session.document().clone();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Conditional(descriptor))).unwrap();
    h.state_mut().session = Session::with_registries(document.clone(), nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    h.run();
    let disabled = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    click(&mut h, disabled);
    h.event(egui::Event::Text("Must not be committed".into()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), 0);
    let on = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "On for all").rect().center();
    click(&mut h, on);
    assert_eq!(h.state().session.revision(), 1);
    let edit = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    click(&mut h, edit);
    h.event(egui::Event::Text("Both enabled".into()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    assert_eq!(h.state().session.revision(), 2);
    assert!(
        h.state()
            .session
            .document()
            .canvases()
            .get(&h.state().canvas)
            .unwrap()
            .nodes()
            .values()
            .all(|node| node.params.get("text") == Some(&Value::Text("Both enabled".into())))
    );
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &document);
}

fn bulk71_real_model_harness() -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let mut document = app.session.document().clone();
    let ids = [NodeId::from_bytes([101; 16]), NodeId::from_bytes([102; 16])];
    let models = ["openai-images.sunburst", "openai-images.flare"];
    for (index, (id, model)) in ids.into_iter().zip(models).enumerate() {
        let values = BTreeMap::from([("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text(format!("Public scene {index}")))]);
        let mut node = app.session.kinds().get("generation.image").unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        node.pos = nodecraft_graph::Position { x: if index == 0 { -360.0 } else { 20.0 }, y: -120.0 };
        Change::AddNode { canvas, node }.apply(&mut document).unwrap();
    }
    app.session = Session::new(document.clone()).unwrap();
    app.selection.replace(ids.into_iter().collect());
    let mut h = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

#[test]
fn bulk71_real_registered_mixed_models_edit_prompt_through_owned_text_control() {
    let mut h = bulk71_real_model_harness();
    let canvas = h.state().canvas;
    let document = h.state().session.document().clone();
    let ids = [NodeId::from_bytes([101; 16]), NodeId::from_bytes([102; 16])];
    let models = ["openai-images.sunburst", "openai-images.flare"];
    let edit = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    click(&mut h, edit);
    h.event(egui::Event::Text("A quiet lake at sunset".into()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    assert_eq!(h.state().session.revision(), 1);
    for (id, model) in ids.into_iter().zip(models) {
        let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
        assert_eq!(node.params.get("prompt"), Some(&Value::Text("A quiet lake at sunset".into())));
        assert_eq!(node.params.get("model"), Some(&Value::Text(model.into())));
    }
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &document);
    assert!(!h.state().session.can_undo());
}

#[test]
fn bulk71_catalog_replacement_discards_old_draft_and_refusal_cache_recovers() {
    use nodecraft_engine::generation_catalog::GenerationCatalog;
    let mut h = bulk71_real_model_harness();
    let original = h.state().session.document().clone();
    let edit = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    click(&mut h, edit);
    h.event(egui::Event::Text("Never apply this obsolete draft".into()));
    h.run();
    assert_eq!(h.state().session.revision(), 0);
    let old = h.state().session.generation_catalog().unwrap().clone();
    let replacement = GenerationCatalog::builtin().unwrap();
    h.state_mut().session.swap_resource(Some(replacement.run_catalog()));
    h.state_mut().session.swap_resource(Some(replacement));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    assert_eq!(h.state().session.document(), &original);
    assert_eq!(h.state().session.revision(), 0);
    assert!(!h.state().session.can_undo());
    h.state_mut().session.swap_resource(Some(old.run_catalog()));
    h.state_mut().session.swap_resource(Some(old));
    h.run();
    let edit = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    click(&mut h, edit);
    h.event(egui::Event::Text("Fresh explicit intent".into()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    assert_eq!(h.state().session.revision(), 1);
    for id in h.state().selection.nodes() {
        assert_eq!(
            h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(id).unwrap().params.get("prompt"),
            Some(&Value::Text("Fresh explicit intent".into()))
        );
    }
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &original);
}

// A floating Area requests repaint while the pointer is held. Advance the press once,
// then require quiescence only after releasing, so this remains a complete real click.
fn bulk71_model_click(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
    h.hover_at(pos);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

fn bulk71_choose_model(h: &mut Harness<'static, NodeCraftApp>, model: &str) {
    h.event(egui::Event::Text(model.into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.query_by_label("Apply model change").is_some(), "choosing only prepares a complete review");
}
#[test]
fn bulk71_real_model_picker_previews_all_then_applies_one_undoable_transaction() {
    let mut h = bulk71_real_model_harness();
    let before = h.state().session.document().clone();
    let picker = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Change model…").rect().center();
    bulk71_model_click(&mut h, picker);
    assert!(h.state().model_choice.is_some());
    bulk71_choose_model(&mut h, "openai-images.flare");
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().session.revision(), 0);
    let apply = h.get_by_label("Apply model change").rect().center();
    bulk71_model_click(&mut h, apply);
    assert!(h.state().model_choice.is_none());
    assert_eq!(h.state().session.revision(), 1);
    let after = h.state().session.document().clone();
    for id in h.state().selection.nodes() {
        let node = after.canvases().get(&h.state().canvas).unwrap().nodes().get(id).unwrap();
        assert_eq!(node.params.get("model"), Some(&Value::Text("openai-images.flare".into())));
        assert!(node.history.is_empty());
    }
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &after);
}
#[test]
fn bulk71_blurred_invalid_shared_draft_blocks_pointer_and_programmatic_model_apply() {
    for programmatic in [false, true] {
        let mut h = bulk71_real_model_harness();
        let before = h.state().session.document().clone();
        let field = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::TextInput, "Images").rect().center();
        bulk71_model_click(&mut h, field);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("invalid".into()));
        h.run();
        let summary = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_label("2 selected nodes").rect().center();
        bulk71_model_click(&mut h, summary);
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), 0);
        if programmatic {
            let canvas = h.state().canvas;
            let ids = h.state().selection.nodes().iter().copied().collect();
            h.state_mut().open_model_picker(canvas, ids, 0).unwrap();
            h.run();
        } else {
            let picker = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Change model…").rect().center();
            bulk71_model_click(&mut h, picker);
        }
        bulk71_choose_model(&mut h, "openai-images.flare");
        let apply = h.get_by_label("Apply model change");
        assert!(apply.accesskit_node().is_disabled());
        let at = apply.rect().center();
        bulk71_model_click(&mut h, at);
        assert!(h.state().model_choice.is_some());
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), 0);
        let cancel = h.get_by_label("Cancel model change").rect().center();
        bulk71_model_click(&mut h, cancel);
        assert!(h.state().model_choice.is_none());
        let field = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::TextInput, "Images");
        assert_eq!(field.value().as_deref(), Some("invalid"));
        let at = field.rect().center();
        bulk71_model_click(&mut h, at);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("2".into()));
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.revision(), 1);
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(h.state().session.document(), &before);
    }
}

#[test]
fn bulk71_unavailable_models_use_shared_repair_picker_and_one_exact_undo() {
    let mut h = bulk71_real_model_harness();
    let mut project = h.state().session.document().clone();
    let canvas = h.state().canvas;
    let ids = [NodeId::from_bytes([101; 16]), NodeId::from_bytes([102; 16])];
    for id in ids {
        let mut node = project.canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().clone();
        node.params.insert("model".into(), Value::Text("LegacyModel:v2".into()));
        Change::SetNode { canvas, node }.apply(&mut project).unwrap();
    }
    h.state_mut().session = Session::new(project.clone()).unwrap();
    h.run();
    let panel = h.get_by_role_and_label(Role::Window, "Shared parameters");
    let picker = panel.get_by_role_and_label(Role::Button, "Change model…").rect().center();
    assert!(panel.query_by_role_and_label(Role::TextInput, "Images").is_none());
    bulk71_model_click(&mut h, picker);
    bulk71_choose_model(&mut h, "openai-images.flare");
    assert_eq!(h.state().session.document(), &project);
    assert_eq!(h.state().session.revision(), 0);
    let apply_node = h.get_by_label("Apply model change");
    assert!(!apply_node.accesskit_node().is_disabled(), "{apply_node:?}");
    let apply_rect = apply_node.rect();
    assert!(h.ctx.content_rect().contains_rect(apply_rect), "Apply {apply_rect:?} outside {:?}", h.ctx.content_rect());
    let window_rect = h.get_by_role_and_label(Role::Window, "Review model change").rect();
    assert!(window_rect.contains_rect(apply_rect), "Apply {apply_rect:?} outside review {window_rect:?}");
    let apply = apply_rect.center();
    bulk71_model_click(&mut h, apply);
    assert_eq!(h.state().session.revision(), 1, "{:?}", h.query_by_role_and_label(Role::Window, "Review model change"));
    for id in ids {
        assert_eq!(
            h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().params.get("model"),
            Some(&Value::Text("openai-images.flare".into()))
        );
    }
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &project);
    assert!(!h.state().session.can_undo());
}

#[test]
fn bulk71_model_repair_never_bypasses_future_version_or_known_port_drift() {
    for future in [false, true] {
        let h = bulk71_real_model_harness();
        let mut project = h.state().session.document().clone();
        let canvas = h.state().canvas;
        let id = NodeId::from_bytes([101; 16]);
        let mut node = project.canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().clone();
        if future {
            node.params.insert("model".into(), Value::Text("LegacyModel:v2".into()));
            node.config.insert("generation_contract_major".into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
        } else {
            node.inputs.pop();
        }
        Change::SetNode { canvas, node }.apply(&mut project).unwrap();
        let session = Session::new(project).unwrap();
        let ids = [id, NodeId::from_bytes([102; 16])].into_iter().collect();
        assert!(crate::bulk::Snapshot::read(&session, canvas, &ids).is_err());
    }
}

#[test]
#[ignore = "manual real-catalog bulk before/after capture; NODECRAFT_BULK71_MEDIA_DIR and NODECRAFT_BULK71_MEDIA_EXPECT"]
fn bulk71_real_catalog_media() {
    use egui_kittest::kittest::{NodeT, Queryable};
    let out = std::path::PathBuf::from(std::env::var_os("NODECRAFT_BULK71_MEDIA_DIR").unwrap());
    std::fs::create_dir_all(&out).unwrap();
    let expected = std::env::var("NODECRAFT_BULK71_MEDIA_EXPECT").unwrap() == "1";
    let canvas = nodecraft_types::CanvasId::from_bytes([71; 16]);
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([71; 16]), "Public shared model parameters").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let seed = nodecraft_engine::Session::new(project.clone()).unwrap();
    let ids = [nodecraft_types::NodeId::from_bytes([101; 16]), nodecraft_types::NodeId::from_bytes([102; 16])];
    for (index, (id, model)) in ids.into_iter().zip(["openai-images.sunburst", "openai-images.flare"]).enumerate() {
        let values: nodecraft_types::ParamValues = [
            ("model".into(), nodecraft_types::Value::Text(model.into())),
            ("prompt".into(), nodecraft_types::Value::Text(if index == 0 { "A quiet blue lake" } else { "Soft light above hills" }.into())),
            ("n".into(), nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(if index == 0 { 2 } else { 1 }))),
        ]
        .into();
        let mut node = seed.kinds().get("generation.image").unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        node.title = if index == 0 { "First public image" } else { "Second public image" }.into();
        node.pos = nodecraft_graph::Position { x: if index == 0 { -360.0 } else { 20.0 }, y: -120.0 };
        nodecraft_graph::Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    }
    let mut app = NodeCraftApp::from_session(nodecraft_engine::Session::new(project.clone()).unwrap(), canvas).unwrap();
    app.selection.replace(ids.into_iter().collect());
    let mut h = egui_kittest::Harness::builder()
        .wgpu()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(egui::Vec2::new(1200.0, 900.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    assert_eq!(h.query_by_role_and_label(egui::accesskit::Role::Window, "Shared parameters").is_some(), expected);
    h.render().unwrap().save(out.join(if expected { "after.png" } else { "before.png" })).unwrap();
    if expected {
        let panel = h.get_by_role_and_label(egui::accesskit::Role::Window, "Shared parameters");
        assert!(panel.query_by_label("Mixed models").is_some());
        panel.get_by_role_and_label(egui::accesskit::Role::Button, "Change model…").click();
        h.run();
        h.event(egui::Event::Text("openai-images.flare".into()));
        h.run();
        h.render().unwrap().save(out.join("picker.png")).unwrap();
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.query_by_label_contains("Drop quality").is_some());
        let apply = h.get_by_label("Apply model change");
        assert!(!apply.accesskit_node().is_disabled());
        assert!(h.get_by_role_and_label(egui::accesskit::Role::Window, "Review model change").rect().contains_rect(apply.rect()));
        h.render().unwrap().save(out.join("review.png")).unwrap();
    }
    assert_eq!(h.state().session.document(), &project);
    assert_eq!(h.state().session.revision(), 0);
}

#[test]
fn bulk71_real_owned_model_conditional_fields_refuse_then_edit_all_atomically() {
    use nodecraft_engine::{commands::Registry, generation_catalog::GenerationCatalog, run_cmds::RunCatalog};
    use nodecraft_types::{Condition, Contract, HiddenPolicy, ParamControl, ParamOwner, ParamRule, ParamSpec, Unsigned};
    let canvas = CanvasId::from_bytes([71; 16]);
    let ids = [NodeId::from_bytes([101; 16]), NodeId::from_bytes([102; 16])];
    let mut models = Vec::new();
    for model_id in ["fixture/first", "fixture/second"] {
        let mut model = nodecraft_catalog::Catalog::builtin().unwrap().get("openai-images.flare").unwrap().clone();
        model.id = model_id.into();
        model.params.params.push(ParamSpec {
            name: "detail".into(),
            label: "Fixture detail".into(),
            description: None,
            control: ParamControl::Text { multiline: true, max_bytes: 128 },
            required: false,
            default: None,
            visible_if: None,
        });
        let mut contract = Contract::from_schema(model_id, ParamOwner::Model, model.params.clone());
        contract.rules.push(ParamRule { unit: nodecraft_types::Unit::Count, meaning: Some("generated_image_count".into()), ..ParamRule::implied("n") });
        contract.rules.push(ParamRule {
            meaning: Some("fixture_detail".into()),
            enabled_when: Some(Condition::Equals { param: "n".into(), value: Value::Unsigned(Unsigned(1)) }),
            hidden: HiddenPolicy::Reset,
            ..ParamRule::implied("detail")
        });
        model.parameter_contract = Some(contract);
        models.push(model);
    }
    let catalog = GenerationCatalog::new(RunCatalog::new(nodecraft_catalog::Catalog::from_models(models).unwrap()).unwrap()).unwrap();
    let kinds = catalog.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([71; 16]), "Public conditional model fixture").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    for (index, id) in ids.into_iter().enumerate() {
        let values: nodecraft_types::ParamValues = [
            ("model".into(), Value::Text(if index == 0 { "fixture/first" } else { "fixture/second" }.into())),
            ("prompt".into(), Value::Text("Public conditional prompt".into())),
            ("n".into(), Value::Unsigned(Unsigned(if index == 0 { 1 } else { 2 }))),
            ("detail".into(), Value::Text("Original detail".into())),
        ]
        .into();
        let mut node = kinds.get("generation.image").unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        node.pos = nodecraft_graph::Position { x: if index == 0 { -360.0 } else { 20.0 }, y: -120.0 };
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    }
    let mut session = Session::with_registries(project.clone(), Registry::builtins().unwrap(), kinds).unwrap();
    session.swap_resource(Some(catalog.run_catalog()));
    session.swap_resource(Some(catalog));
    let mut app = NodeCraftApp::from_session(session, canvas).unwrap();
    app.selection.replace(ids.into_iter().collect());
    let mut h = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1200.0, 900.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    let panel = h.get_by_role_and_label(Role::Window, "Shared parameters");
    let disabled = panel.query_all_by_role_and_label(Role::Button, "Edit for all…").find(|node| node.accesskit_node().is_disabled()).unwrap().rect().center();
    bulk71_model_click(&mut h, disabled);
    assert_eq!(h.state().session.document(), &project);
    assert!(
        h.state_mut()
            .session
            .dispatch("node.params_set_bulk", &json!({"canvas":canvas,"ids":ids,"revision":0,"name":"detail","value":{"type":"text","value":"Refused detail"}}))
            .is_err()
    );
    assert_eq!(h.state().session.document(), &project);
    let edit = h
        .get_by_role_and_label(Role::Window, "Shared parameters")
        .query_all_by_role_and_label(Role::Button, "Edit for all…")
        .find(|node| !node.accesskit_node().is_disabled())
        .unwrap()
        .rect()
        .center();
    bulk71_model_click(&mut h, edit);
    h.event(egui::Event::Text("1".into()));
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(h.state().session.revision(), 1);
    let edit = h.get_by_role_and_label(Role::Window, "Shared parameters").get_by_role_and_label(Role::Button, "Edit for all…").rect().center();
    bulk71_model_click(&mut h, edit);
    h.event(egui::Event::Text("Shared detail".into()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    assert_eq!(h.state().session.revision(), 2);
    for id in ids {
        assert_eq!(
            h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().params.get("detail"),
            Some(&Value::Text("Shared detail".into()))
        );
    }
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &project);
    assert!(!h.state().session.can_undo());
}

#[test]
#[ignore = "manual public-scene capture; requires NODECRAFT_PERF281_RENDER_ROOT"]
fn perf281_visual_current() {
    let root = std::path::PathBuf::from(std::env::var("NODECRAFT_PERF281_RENDER_ROOT").unwrap());
    first_frame_hover_captured(false, Some(&root));
    first_frame_hover_captured(true, Some(&root));
}

#[test]
fn palette_closes_on_admitted_marquee_and_first_escape_cancels_it() {
    let mut h = harness();
    double_click(&mut h, Pos2::new(100.0, 100.0));
    h.run();
    assert!(h.state().palette.is_some());
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let from = Pos2::new(60.0, 500.0);
    h.hover_at(from);
    h.step();
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
    h.step();
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    h.hover_at(from + Vec2::new(40.0, 20.0));
    h.run();
    assert!(h.state().marquee.is_some(), "actual marquee must be admitted");
    assert!(h.state().palette.is_none(), "admitted gesture closes palette before Escape");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().marquee.as_ref().is_some_and(|m| m.cancelled));
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert!(!h.state().session.can_undo());
}

#[test]
fn minimap_real_double_click_never_opens_palette() {
    let mut h = harness();
    h.run();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    double_click(&mut h, map.center());
    h.run();
    assert!(h.state().palette.is_none());
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert!(!h.state().session.can_undo());
    let mut empty = harness();
    double_click(&mut empty, Pos2::new(100.0, 100.0));
    empty.run();
    assert!(empty.state().palette.is_some(), "empty canvas remains eligible");
}

#[test]
fn quick_add_yields_over_actual_keys_window_without_text_focus() {
    let mut h = harness();
    h.get_by_role_and_label(Role::Button, "API keys").click();
    h.run();
    let key_label = h.get_by_label("Account").rect();
    let at = key_label.left_center() - Vec2::new(5.0, 0.0);
    h.hover_at(at);
    h.step();
    h.ctx.memory_mut(|m| {
        if let Some(id) = m.focused() {
            m.surrender_focus(id);
        }
    });
    assert!(!canvas::text_input_focused(&h.ctx));
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let selection = h.state().selection.clone();
    let events = h.state().session.events_since(0).unwrap();
    for key in [egui::Key::T, egui::Key::I] {
        h.key_press(key);
        h.run();
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().selection, selection);
        assert_eq!(h.state().session.events_since(0).unwrap(), events);
        assert!(!h.state().session.can_undo());
    }
}

struct FloatingFixture {
    app: NodeCraftApp,
    open: bool,
    at: Pos2,
    transform: egui::emath::TSTransform,
    rect: Rect,
    blocked_keys: usize,
    extra_areas: usize,
}
fn floating_fixture() -> Harness<'static, FloatingFixture> {
    Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_step_dt(1.0 / 60.0).with_max_steps(120).build_ui_state(
        |ui, state: &mut FloatingFixture| {
            if state.open {
                let response = egui::Window::new("Public foreground fixture")
                    .fixed_pos(state.at)
                    .fixed_size(Vec2::new(240.0, 160.0))
                    .show(ui.ctx(), |ui| {
                        ui.label("No editor, key service or private data");
                    })
                    .unwrap()
                    .response;
                ui.ctx().set_transform_layer(response.layer_id, state.transform);
                state.rect = state.transform * response.rect;
            }
            for n in 0..state.extra_areas {
                egui::Area::new(egui::Id::new(("public-cap-fixture", n)))
                    .order(egui::Order::Middle)
                    .fixed_pos(Pos2::new(-500.0, -500.0))
                    .constrain(false)
                    .show(ui.ctx(), |ui| {
                        ui.label("offscreen public fixture");
                    });
            }
            state.app.show(ui);
            state.blocked_keys += ui.input(|i| i.events.iter().filter(|e| matches!(e, egui::Event::Key { key: egui::Key::T, pressed: true, .. })).count());
        },
        FloatingFixture {
            app: NodeCraftApp::headless().unwrap(),
            open: false,
            at: Pos2::new(80.0, 80.0),
            transform: egui::emath::TSTransform::IDENTITY,
            rect: Rect::NOTHING,
            blocked_keys: 0,
            extra_areas: 0,
        },
    )
}

#[test]
fn floating_window_first_open_move_transform_and_close_have_bounded_current_ownership() {
    let mut h = floating_fixture();
    h.run();
    let document = h.state().app.session.document().clone();
    let revision = h.state().app.session.revision();
    let events = h.state().app.session.events_since(0).unwrap();
    // Position first: one fresh key is the only event on the Window's first sizing frame.
    let pointer = Pos2::new(120.0, 110.0);
    h.hover_at(pointer);
    h.step();
    h.state_mut().open = true;
    h.event(egui::Event::Key { key: egui::Key::T, physical_key: Some(egui::Key::T), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.state().rect.contains(pointer));
    assert_eq!(h.state().app.session.document(), &document);
    assert_eq!(h.state().blocked_keys, 1, "blocked press remains unconsumed");
    h.event(egui::Event::Key { key: egui::Key::T, physical_key: Some(egui::Key::T), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    h.run();
    let pointer = Pos2::new(480.0, 160.0);
    h.hover_at(pointer);
    h.step();
    h.state_mut().at = Pos2::new(340.0, 90.0);
    h.state_mut().transform = egui::emath::TSTransform::from_translation(Vec2::new(100.0, 40.0));
    h.event(egui::Event::Key { key: egui::Key::T, physical_key: Some(egui::Key::T), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.state().rect.contains(pointer));
    assert_eq!(h.state().app.session.document(), &document);
    assert_eq!(h.state().blocked_keys, 2);
    assert_eq!(h.state().app.session.revision(), revision);
    assert_eq!(h.state().app.session.events_since(0).unwrap(), events);
    assert!(!h.state().app.session.can_undo());
    h.event(egui::Event::Key { key: egui::Key::T, physical_key: Some(egui::Key::T), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    h.run();
    h.state_mut().open = false;
    h.run(); // A closed area's final frame may conservatively retain ownership.
    h.key_press(egui::Key::T);
    h.run();
    assert_ne!(h.state().app.session.document(), &document);
    assert_eq!(h.state().blocked_keys, 2);
    h.state_mut().app.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().app.session.document(), &document);
}

#[test]
fn floating_window_keeps_empty_canvas_and_card_quick_add_available() {
    let mut h = floating_fixture();
    h.state_mut().open = true;
    h.run();
    let document = h.state().app.session.document().clone();
    let card = screen(&h.state().app).min + Vec2::new(5.0, 10.0);
    for pointer in [Pos2::new(800.0, 120.0), card] {
        assert!(!h.state().rect.contains(pointer));
        h.hover_at(pointer);
        h.step();
        h.key_press(egui::Key::T);
        h.run();
        assert_ne!(h.state().app.session.document(), &document);
        assert_eq!(h.state().app.selection.nodes().len(), 1);
        h.state_mut().app.session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().app.session.document(), &document);
    }
    assert_eq!(h.state().blocked_keys, 0, "allowed key presses were consumed");
}

#[test]
fn thousand_painted_cards_do_not_exhaust_floating_area_admission() {
    let app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let template = node(&app).clone();
    let mut document = app.session.document().clone();
    let changes = (10..1009u32)
        .map(|n| {
            let mut node = template.clone();
            node.id = NodeId::from_bytes(u128::from(n).to_be_bytes());
            Change::AddNode { canvas, node }
        })
        .collect();
    nodecraft_graph::Changeset::new(changes).apply(&mut document).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(Session::new(document.clone()).unwrap(), canvas).unwrap());
    h.run();
    assert_eq!(h.state().painted.len(), 1000);
    let areas = h.ctx.memory(|m| m.areas().visible_layer_ids().len());
    eprintln!("1000 painted cards: {areas} floating Area IDs");
    assert!(areas < MAX_QUICK_ADD_AREAS, "card child layers must not count as floating Areas: {areas}");
    h.hover_at(screen(h.state()).min + Vec2::new(5.0, 10.0));
    h.step();
    h.key_press(egui::Key::T);
    h.run();
    assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().len(), 1001);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &document);
}

#[test]
fn palette_closes_on_node_drag_and_late_card_pan_without_extra_history() {
    for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary] {
        let mut h = harness();
        // Open the palette clear of the card: main's palette is a foreground Area and owns every
        // press inside it, so the drag must start on visible canvas outside its rect. That is the
        // #267 case this proves: a canvas gesture admitted while the palette is open closes it.
        double_click(&mut h, Pos2::new(520.0, 70.0));
        h.run();
        assert!(h.state().palette.is_some());
        let document = h.state().session.document().clone();
        let card = screen(h.state());
        let palette = egui::AreaState::load(&h.ctx, egui::Id::new("quick_add_palette")).map(|area| area.rect()).unwrap_or(Rect::NOTHING);
        let start = card.left_center() + Vec2::new(5.0, 0.0);
        assert!(card.contains(start) && !palette.contains(start), "fixture: {start:?} must be card, not palette {palette:?}");
        h.hover_at(start);
        h.step();
        h.event(egui::Event::PointerButton { pos: start, button, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        let to = start + Vec2::new(40.0, 20.0);
        h.hover_at(to);
        h.run();
        assert!(h.state().palette.is_none());
        assert_eq!(h.state().session.document(), &document, "held drag/pan is view state");
        assert!(!h.state().session.can_undo());
        h.event(egui::Event::PointerButton { pos: to, button, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run();
        if button == egui::PointerButton::Primary {
            assert_ne!(h.state().session.document(), &document);
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(h.state().session.document(), &document);
            assert!(!h.state().session.can_undo());
        } else {
            assert_eq!(h.state().session.document(), &document);
            assert!(!h.state().session.can_undo());
        }
    }
}

#[test]
fn palette_closes_on_real_wire_drag_and_first_escape_cancels_without_edit() {
    let mut h = chain_harness();
    double_click(&mut h, Pos2::new(550.0, 600.0));
    h.run();
    assert!(h.state().palette.is_some());
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let from = port_point(&h, 1, Side::Output);
    h.drag_at(from);
    h.run();
    h.hover_at(from + Vec2::new(40.0, 20.0));
    h.run();
    assert!(h.state().wiring.is_some());
    assert!(h.state().palette.is_none());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().wiring.is_none());
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert!(!h.state().session.can_undo());
}

/// A router whose list input is fed by two text sources, so its input order badges (#64) are on
/// screen with empty canvas around them for the palette to open on.
fn reorder_harness() -> Harness<'static, NodeCraftApp> {
    let hub = NodeId::from_bytes([9; 16]);
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let canvas = CanvasId::from_bytes([7; 16]);
    let mut document = Project::new(ProjectId::from_bytes([8; 16]), "Reorder").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let mut router = kinds.get(nodecraft_nodes::router::ID).unwrap().descriptor().instantiate(hub, &BTreeMap::new()).unwrap();
    router.pos = nodecraft_graph::Position { x: 620.0, y: 240.0 };
    Change::AddNode { canvas, node: router }.apply(&mut document).unwrap();
    for n in 1..=2u8 {
        let id = NodeId::from_bytes([n; 16]);
        let mut source = kinds.get("source.text").unwrap().descriptor().instantiate(id, &BTreeMap::new()).unwrap();
        source.pos = nodecraft_graph::Position { x: 0.0, y: f32::from(n) * 300.0 };
        Change::AddNode { canvas, node: source }.apply(&mut document).unwrap();
        let wire = nodecraft_graph::Edge {
            id: EdgeId::from_bytes([20 + n; 16]),
            from: Endpoint { node: id, port: "out".into() },
            to: Endpoint { node: hub, port: "in".into() },
            order: u32::from(n) - 1,
        };
        Change::Connect { canvas, edge: wire }.apply(&mut document).unwrap();
    }
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
    h.state_mut().view = CanvasView { pan: Vec2::new(90.0, 60.0), zoom: 0.6 };
    h.run();
    h
}

/// The two noodles into the hub's list input, in document order.
fn reorder_ids(h: &Harness<'static, NodeCraftApp>) -> Vec<EdgeId> {
    let app = h.state();
    let mut edges: Vec<_> = app.session.document().canvases().get(&app.canvas).unwrap().edges().values().cloned().collect();
    edges.sort_by_key(|edge| edge.order);
    edges.into_iter().map(|edge| edge.id).collect()
}

/// The fixture: the palette is open and both badges are outside it, so a badge press is the
/// reorder and not a click inside the palette's Area.
fn palette_over_badges() -> (Harness<'static, NodeCraftApp>, Pos2, Pos2) {
    let mut h = reorder_harness();
    let open_at = Pos2::new(520.0, 640.0);
    double_click(&mut h, open_at);
    h.run();
    assert!(h.state().palette.is_some(), "double-clicking empty canvas opens the palette");
    let palette = egui::AreaState::load(&h.ctx, egui::Id::new("quick_add_palette")).map(|area| area.rect()).unwrap_or(Rect::NOTHING);
    let from = h.get_by_label("Input 2 of 2 into in of Router, drag to reorder").rect().center();
    let to = h.get_by_label("Input 1 of 2 into in of Router, drag to reorder").rect().center();
    assert!(!palette.contains(from) && !palette.contains(to), "fixture: badges {from:?} {to:?} must lie outside {palette:?}");
    (h, from, to)
}

/// #267: a port-reorder drag admitted over an open palette closed neither, so the first Escape
/// dismissed the palette and the drag needed a second one. Reordering is a canvas gesture like a
/// card drag, so it closes the palette the frame it is admitted.
#[test]
fn palette_closes_on_a_port_reorder_drag_and_first_escape_cancels_it() {
    let (mut h, from, to) = palette_over_badges();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let order = reorder_ids(&h);
    let events = h.state().session.events_since(0).unwrap();
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    h.hover_at(to);
    h.run();
    assert!(h.state().reordering.is_some(), "the held drag is the port reorder");
    assert!(h.state().palette.is_none(), "the admitted reorder owns the frame and closes the palette");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().reordering.is_none(), "the first Escape cancels the reorder, not the palette");
    h.drop_at(to);
    h.run();
    assert_eq!(h.state().session.document(), &document, "a cancelled reorder never publishes an edit");
    assert_eq!(reorder_ids(&h), order);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert!(!h.state().session.can_undo());
}

/// The same drag carried to its drop still reorders, in exactly one undo step, with the palette
/// already gone.
#[test]
fn a_port_reorder_drag_completed_over_an_open_palette_is_one_clean_undo_step() {
    let (mut h, from, to) = palette_over_badges();
    let document = h.state().session.document().clone();
    let order = reorder_ids(&h);
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    h.hover_at(to);
    h.run();
    assert!(h.state().palette.is_none(), "the palette went when the drag was admitted");
    assert_ne!(from, to);
    h.drop_at(to);
    h.run();
    assert!(h.state().reordering.is_none());
    let moved = reorder_ids(&h);
    assert_ne!(moved, order, "the badge was dropped in the other one's place");
    assert_ne!(h.state().session.document(), &document, "the reorder is one document edit");
    assert_eq!(h.state().session.revision(), 1, "one revision for the reorder (the palette opened with no edit)");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(reorder_ids(&h), order);
    assert_eq!(h.state().session.document(), &document, "one undo restores the order");
    assert!(!h.state().session.can_undo());
}

#[test]
fn floating_area_inspection_exact_cap_and_over_cap_preserve_key_admission() {
    let mut h = floating_fixture();
    h.run();
    let original = h.state().app.session.document().clone();
    let base = h.ctx.memory(|m| m.areas().visible_layer_ids().len());
    h.state_mut().extra_areas = MAX_QUICK_ADD_AREAS - base;
    h.run();
    assert_eq!(h.ctx.memory(|m| m.areas().visible_layer_ids().len()), MAX_QUICK_ADD_AREAS);
    h.hover_at(Pos2::new(800.0, 120.0));
    h.step();
    h.key_press(egui::Key::T);
    h.run();
    assert_ne!(h.state().app.session.document(), &original);
    assert_eq!(h.state().blocked_keys, 0);
    h.state_mut().app.session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    h.state_mut().extra_areas += 1;
    h.run();
    assert_eq!(h.ctx.memory(|m| m.areas().visible_layer_ids().len()), MAX_QUICK_ADD_AREAS + 1);
    let revision = h.state().app.session.revision();
    let events = h.state().app.session.events_since(0).unwrap();
    h.key_press(egui::Key::T);
    h.run();
    assert_eq!(h.state().app.session.document(), &original);
    assert_eq!(h.state().app.session.revision(), revision);
    assert_eq!(h.state().app.session.events_since(0).unwrap(), events);
    assert_eq!(h.state().blocked_keys, 1, "over-cap admission leaves the key unconsumed");
    assert!(h.state().app.session.can_redo());
}

fn palette_row_focus_with_history() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let canvas = h.state().canvas;
    let id = node(h.state()).id;
    for title in ["Before palette", "Redo entry"] {
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":title})).unwrap();
    }
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    double_click(&mut h, Pos2::new(100.0, 100.0));
    h.run();
    assert!(h.state().palette.is_some());
    h.key_press(egui::Key::Tab);
    h.run();
    assert!(h.ctx.memory(|m| m.focused().is_some()), "Tab focuses an actual palette row");
    assert!(!canvas::text_input_focused(&h.ctx));
    h.hover_at(Pos2::new(800.0, 120.0));
    h.run();
    assert!(h.state().palette.is_some());
    assert!(!canvas::text_input_focused(&h.ctx));
    assert!(h.state().session.can_undo() && h.state().session.can_redo());
    h
}
fn raw_key(key: egui::Key, pressed: bool) -> egui::Event {
    egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE }
}

#[test]
fn palette_same_frame_escape_and_metadata_key_preserve_ownership() {
    let mut h = palette_row_focus_with_history();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let selection = h.state().selection.clone();
    // input_mut injects ONE RawInput frame. Queued Harness.event calls would each run a frame.
    h.input_mut().events.extend([raw_key(egui::Key::Escape, true), raw_key(egui::Key::T, true)]);
    h.step();
    assert!(h.state().palette.is_none());
    assert!(h.ctx.input(|i| i.key_pressed(egui::Key::T)), "refused T stays unconsumed");
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!(h.state().selection, selection);
    assert!(h.state().session.can_undo() && h.state().session.can_redo());
    h.input_mut().events.extend([raw_key(egui::Key::Escape, false), raw_key(egui::Key::T, false)]);
    h.step();
    h.key_press(egui::Key::T);
    h.run();
    let canvas = h.state().canvas;
    assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().len(), document.canvases().get(&canvas).unwrap().nodes().len() + 1);
    assert_eq!(h.state().session.revision(), revision + 1);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &document, "next fresh T is exactly one intended undo step");
}

#[test]
fn palette_same_frame_row_choice_and_metadata_key_create_only_chosen_kind() {
    let mut h = palette_row_focus_with_history();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    assert_eq!(h.state().palette.as_ref().unwrap().highlighted, 0);
    // #191 and #194 added kinds since this test was written, so the chosen kind is read from the
    // real palette rather than hardcoded to the row that used to be first. What is being proved
    // is #267's guarantee: the row choice is the only node this frame creates.
    let chosen = match &h.state().palette_entries().first().map(|entry| entry.target.clone()) {
        Some(palette::Target::Kind(kind)) => kind.clone(),
        other => panic!("the first palette row must offer a kind, got {other:?}"),
    };
    h.input_mut().events.extend([raw_key(egui::Key::Enter, true), raw_key(egui::Key::T, true)]);
    h.step();
    assert!(h.state().palette.is_none());
    assert!(h.ctx.input(|i| i.key_pressed(egui::Key::T)), "palette frame does not consume unrelated T");
    assert_eq!(nodes_of(&h, &chosen).len(), 1, "the real focused first row chooses {chosen}");
    assert_ne!(chosen, "source.text", "the fixture must not prove the guarantee with its own key");
    assert_eq!(nodes_of(&h, "source.text").len(), 1, "unrelated T must not add Text");
    assert_eq!(h.state().session.revision(), revision + 1);
    assert_eq!(h.state().session.events_since(0).unwrap().len(), events.len() + 1);
    assert_eq!(h.state().selection.nodes().len(), 1);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &document, "row choice is exactly one intended undo step");
}

#[cfg(not(target_arch = "wasm32"))]
mod clipboard_native273;

mod clipboard_capture273;
