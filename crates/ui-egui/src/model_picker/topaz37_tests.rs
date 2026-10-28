//! Topaz UI evidence uses the actual catalog at each source revision.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

struct Scene {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}

fn scene(builder: egui_kittest::HarnessBuilder<Scene>, theme: crate::theme::ThemeKind) -> Harness<'static, Scene> {
    let catalog = Catalog::builtin().unwrap();
    let wiring = Wiring::new(Modality::Image).with(Modality::Image, 1).with_inline_prompt();
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("Topaz").unwrap();
    let mut h = builder.with_size(egui::vec2(1100., 750.)).build_ui_state(
        move |ui, scene: &mut Scene| {
            if scene.outcome == Outcome::Open {
                scene.outcome = scene.picker.show(ui.ctx(), &scene.catalog, theme.tokens());
            }
        },
        Scene { catalog, picker, outcome: Outcome::Open },
    );
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h
}

#[test]
#[ignore = "native GPU evidence; NODECRAFT_TOPAZ37_RENDER_ROOT and NODECRAFT_TOPAZ37_STAGE"]
fn topaz37_native_picker_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_TOPAZ37_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_TOPAZ37_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(Harness::builder().wgpu(), theme);
        h.hover_at(egui::pos2(1090., 735.));
        h.run();
        assert_eq!(h.query_by_label("Topaz Precision Standard V2 (fal)").is_some(), stage == "after");
        h.render().unwrap().save(root.join(format!("{theme:?}-picker-{stage}.png"))).unwrap();
    }
}

#[test]
fn topaz37_actual_picker_has_the_one_block_rate_and_selects_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(Harness::builder(), theme);
        assert_eq!(price_label(h.state().catalog.get("fal.topaz-precision").unwrap()), "0.08 USD / call");
        assert!(h.query_all_by_label_contains("0.08 USD / call").next().is_some());
        h.get_by_label("Topaz Precision Standard V2 (fal)").click();
        h.run();
        assert_eq!(h.state().outcome, Outcome::Chosen("fal.topaz-precision".into()));
    }
}

const CANVAS: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([137; 16]);
const CARD: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([138; 16]);
fn app(theme: crate::theme::ThemeKind) -> crate::app::NodeCraftApp {
    use nodecraft_graph::{Canvas, Change, Position, Project};
    use nodecraft_types::{EdgeId, NodeId, ProjectId, Unsigned, Value};
    let mut project = Project::new(ProjectId::from_bytes([137; 16]), "Original Topaz input").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Upscale") }.apply(&mut project).unwrap();
    let mut session = nodecraft_engine::Session::new(project).unwrap();
    let node = session
        .kinds()
        .get(nodecraft_nodes::generation::KIND)
        .unwrap()
        .instantiate_instance(
            CARD,
            Default::default(),
            &[("model".into(), Value::Text("fal.topaz-precision".into())), ("upscale_factor".into(), Value::Unsigned(Unsigned(2)))].into(),
        )
        .unwrap();
    session.dispatch("node.add", &serde_json::json!({"canvas":CANVAS,"node":node})).unwrap();
    let source = NodeId::from_bytes([139; 16]);
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
    session.dispatch("edge.connect",&serde_json::json!({"canvas":CANVAS,"edge":{"id":EdgeId::from_bytes([140;16]),"from":{"node":source,"port":"out"},"to":{"node":CARD,"port":"image"},"order":0}})).unwrap();
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
fn topaz37_generated_card_factor_control_edits_through_one_undoable_command() {
    use egui::accesskit::Role;
    use nodecraft_types::{Unsigned, Value};
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = card_harness(Harness::builder(), theme);
        assert_eq!(h.state().session.document().canvases()[&CANVAS].nodes()[&CARD].params.get("upscale_factor"), Some(&Value::Unsigned(Unsigned(2))));
        let before = h.state().session.document().clone();
        h.get_by_role_and_label(Role::TextInput, "Upscale factor").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("3".into()));
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.document().canvases()[&CANVAS].nodes()[&CARD].params.get("upscale_factor"), Some(&Value::Unsigned(Unsigned(3))));
        h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
        h.run();
        assert_eq!(h.state().session.document(), &before);
    }
}

#[test]
#[ignore = "native GPU generated controls; NODECRAFT_TOPAZ37_RENDER_ROOT"]
fn topaz37_native_generated_card_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_TOPAZ37_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = card_harness(Harness::builder().wgpu(), theme);
        assert!(h.query_by_label("Upscale factor").is_some());
        h.hover_at(egui::pos2(1090., 885.));
        h.run();
        h.render().unwrap().save(root.join(format!("{theme:?}-card-after.png"))).unwrap();
    }
}
