//! #1116: questions are modal (Enter answers, Escape cancels) and tools are floating panels, all
//! on screen in a 640 x 400 window.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_types::FieldId;

fn small() -> Harness<'static, NodeCraftApp> {
    Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(640.0, 400.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap())
}

fn on_screen(h: &Harness<'static, NodeCraftApp>, title: &str) {
    let rect = h.get_by_role_and_label(Role::Window, title).rect();
    let content = h.ctx.content_rect();
    assert!(content.contains_rect(rect), "{title} {rect:?} inside {content:?}");
    // The dialog itself, not a full-window backdrop.
    assert!(rect.width() < content.width() && rect.height() < content.height(), "{title} {rect:?} is the dialog's own rect");
}

fn field(h: &mut Harness<'static, NodeCraftApp>) -> FieldId {
    let canvas = h.state().canvas;
    let node = super::tests::node(h.state()).id;
    let id = FieldId::from_bytes([0x16; 16]);
    let field = json!({"id": id, "title": "Field", "rect": {"pos": {"x": 0.0, "y": 0.0}, "width": 240, "height": 160}});
    h.state_mut().session.dispatch("field.create", &json!({"canvas": canvas, "field": field, "nodes": [node]})).unwrap();
    h.run();
    id
}

fn title(h: &Harness<'static, NodeCraftApp>, id: FieldId) -> String {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().fields().get(&id).unwrap().title.clone()
}

#[test]
fn renaming_a_field_is_a_question_enter_saves_and_escape_cancels() {
    let mut h = small();
    h.run();
    let id = field(&mut h);
    let before = title(&h, id);
    h.state_mut().fields.rename = Some((id, "Hero shots".into()));
    h.run();
    on_screen(&h, "Rename field");
    assert!(h.ctx.memory(|m| m.top_modal_layer()).is_some(), "the question blocks the canvas");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().fields.rename.is_none(), "Escape cancels");
    assert_eq!(title(&h, id), before);
    h.state_mut().fields.rename = Some((id, "Hero shots".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(title(&h, id), "Hero shots", "Enter saves");
    assert!(h.state().fields.rename.is_none());
}

#[test]
fn the_project_files_panel_floats_on_screen_and_escape_closes_it() {
    let mut h = small();
    h.run();
    h.state_mut().files.menu_open = true;
    h.run();
    h.run();
    on_screen(&h, "Project files");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().files.menu_open, "Escape closes it");
    assert!(h.query_by_role_and_label(Role::Window, "Project files").is_none());
}
