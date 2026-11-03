//! Pika evidence uses the unfiltered builtin catalog at each source revision.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct Scene {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn scene(builder: egui_kittest::HarnessBuilder<Scene>, theme: crate::theme::ThemeKind) -> Harness<'static, Scene> {
    let catalog = Catalog::builtin().unwrap();
    let wiring = Wiring::new(Modality::Video).with(Modality::Image, 1).with_inline_prompt();
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("Pika").unwrap();
    let mut h = builder.with_size(egui::vec2(1100., 750.)).build_ui_state(
        move |ui, state: &mut Scene| {
            if state.outcome == Outcome::Open {
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, theme.tokens());
            }
        },
        Scene { catalog, picker, outcome: Outcome::Open },
    );
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h
}
#[test]
#[ignore = "native GPU evidence; NODECRAFT_PIKA29_RENDER_ROOT and NODECRAFT_PIKA29_STAGE"]
fn pika29_native_picker_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_PIKA29_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_PIKA29_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(Harness::builder().wgpu(), theme);
        h.hover_at(egui::pos2(1090., 735.));
        h.run();
        assert_eq!(h.query_by_label("Pika 2.5 image to video (fal)").is_some(), stage == "after");
        h.render().unwrap().save(root.join(format!("{theme:?}-picker-{stage}.png"))).unwrap();
    }
}

#[test]
fn pika29_actual_picker_selects_the_first_frame_model_and_shows_its_rate_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(Harness::builder(), theme);
        assert_eq!(price_label(h.state().catalog.get("fal.pika25-i2v").unwrap()), "0.05 USD / second");
        h.get_by_label("Pika 2.5 image to video (fal)").click();
        h.run();
        assert_eq!(h.state().outcome, Outcome::Chosen("fal.pika25-i2v".into()));
    }
}
const CANVAS: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([129; 16]);
const CARD: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([130; 16]);
fn app(theme: crate::theme::ThemeKind) -> crate::app::NodeCraftApp {
    use nodecraft_graph::{Canvas, Change, Position, Project};
    use nodecraft_types::{EdgeId, NodeId, ProjectId, Unsigned, Value};
    let mut project = Project::new(ProjectId::from_bytes([129; 16]), "Original paper kite").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Animate") }.apply(&mut project).unwrap();
    let mut session = nodecraft_engine::Session::new(project).unwrap();
    let node = session
        .kinds()
        .get(nodecraft_nodes::generation::VIDEO_KIND)
        .unwrap()
        .instantiate_instance(
            CARD,
            Default::default(),
            &[
                ("model".into(), Value::Text("fal.pika25-i2v".into())),
                ("prompt".into(), Value::Text("A paper kite rises above an empty meadow".into())),
                ("duration_seconds".into(), Value::Unsigned(Unsigned(5))),
            ]
            .into(),
        )
        .unwrap();
    session.dispatch("node.add", &serde_json::json!({"canvas":CANVAS,"node":node})).unwrap();
    let source = NodeId::from_bytes([131; 16]);
    let pixels: Vec<u8> = (0..6).flat_map(|y| (0..8).flat_map(move |x| [if x < 3 { 230 } else { 30 }, if y < 3 { 190 } else { 40 }, x * 20, 255])).collect();
    let png = nodecraft_media::encode_png(8, 6, &pixels, &Default::default()).unwrap();
    nodecraft_engine::import::import_files(
        &mut session,
        CANVAS,
        Position { x: -420., y: 0. },
        200.,
        &[nodecraft_engine::import::Incoming { name: "original.png", bytes: &png }],
        &[source],
    )
    .unwrap();
    session.dispatch("edge.connect",&serde_json::json!({"canvas":CANVAS,"edge":{"id":EdgeId::from_bytes([132;16]),"from":{"node":source,"port":"out"},"to":{"node":CARD,"port":"first_frame"},"order":0}})).unwrap();
    let mut app = crate::app::NodeCraftApp::from_session(session, CANVAS).unwrap();
    app.theme = theme;
    app
}
fn card_harness(builder: egui_kittest::HarnessBuilder<crate::app::NodeCraftApp>, theme: crate::theme::ThemeKind) -> Harness<'static, crate::app::NodeCraftApp> {
    // Native thumbnail completion can repaint after the default four settling frames.
    let mut h =
        builder.with_max_steps(120).with_size(egui::vec2(1100., 900.)).build_ui_state(|ui, app: &mut crate::app::NodeCraftApp| app.show(ui), app(theme));
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h.state_mut().view.zoom = 1.;
    h.state_mut().view.pan = egui::vec2(520., 180.);
    h.run();
    h
}
#[test]
fn pika29_generated_duration_control_is_a_pointer_edit_and_one_undoable_command() {
    use egui::accesskit::Role;
    use nodecraft_types::{Unsigned, Value};
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        // The app harness's step budget (app/tests.rs): a focused text field blinks its cursor
        // and the source card's preview decodes on a worker thread, so how many 1/4 s default
        // steps they need depends on the runner's speed (slow Windows/macOS CI ran out at 4).
        let mut h = card_harness(Harness::builder().with_step_dt(1.0 / 60.0).with_max_steps(120), theme);
        let before = h.state().session.document().clone();
        h.get_by_role_and_label(Role::TextInput, "Seconds").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("10".into()));
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.document().canvases()[&CANVAS].nodes()[&CARD].params.get("duration_seconds"), Some(&Value::Unsigned(Unsigned(10))));
        h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
        h.run();
        assert_eq!(h.state().session.document(), &before);
    }
}
#[test]
#[ignore = "native GPU generated controls; NODECRAFT_PIKA29_RENDER_ROOT"]
fn pika29_native_generated_card_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_PIKA29_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = card_harness(Harness::builder().wgpu(), theme);
        assert!(h.query_by_label("Seconds").is_some());
        h.hover_at(egui::pos2(1090., 885.));
        h.run();
        h.render().unwrap().save(root.join(format!("{theme:?}-card-after.png"))).unwrap();
    }
}
