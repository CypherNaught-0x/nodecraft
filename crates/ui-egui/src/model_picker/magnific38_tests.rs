//! Public UI evidence uses each source revision's actual complete builtin catalog.
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
    picker.set_query("Magnific").unwrap();
    let mut harness = builder.with_size(egui::vec2(1100., 750.)).build_ui_state(
        move |ui, scene: &mut Scene| {
            if scene.outcome == Outcome::Open {
                scene.outcome = scene.picker.show(ui.ctx(), &scene.catalog, theme.tokens());
            }
        },
        Scene { catalog, picker, outcome: Outcome::Open },
    );
    crate::theme::apply(&harness.ctx, theme);
    harness.run();
    harness
}

#[test]
#[ignore = "native GPU evidence; NODECRAFT_MAGNIFIC38_RENDER_ROOT and NODECRAFT_MAGNIFIC38_STAGE"]
fn magnific38_native_picker_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_MAGNIFIC38_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_MAGNIFIC38_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut harness = scene(Harness::builder().wgpu(), theme);
        harness.hover_at(egui::pos2(1090., 735.));
        harness.run();
        assert_eq!(harness.query_by_label("Magnific Precision V2").is_some(), stage == "after");
        harness.render().unwrap().save(root.join(format!("{theme:?}-picker-{stage}.png"))).unwrap();
    }
}

#[test]
fn magnific38_actual_picker_selects_the_model_and_shows_unknown_usage_cost() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut harness = scene(Harness::builder(), theme);
        let model = harness.state().catalog.get("magnific.precision-v2").unwrap();
        assert_eq!(price_label(model), "Usage based (USD)");
        assert_eq!(model.quote(&Default::default()).unwrap(), None);
        assert!(harness.query_all_by_label_contains("Usage based (USD)").next().is_some());
        harness.get_by_label("Magnific Precision V2").click();
        harness.run();
        assert_eq!(harness.state().outcome, Outcome::Chosen("magnific.precision-v2".into()));
    }
}

const CANVAS: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([138; 16]);
const CARD: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([139; 16]);
fn app(theme: crate::theme::ThemeKind) -> crate::app::NodeCraftApp {
    use nodecraft_graph::{Canvas, Change, Position, Project};
    use nodecraft_types::{EdgeId, NodeId, ProjectId, Value};
    let mut project = Project::new(ProjectId::from_bytes([138; 16]), "Public Magnific input").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Enhance") }.apply(&mut project).unwrap();
    let mut session = nodecraft_engine::Session::new(project).unwrap();
    let node = session
        .kinds()
        .get(nodecraft_nodes::generation::KIND)
        .unwrap()
        .instantiate_instance(CARD, Default::default(), &[("model".into(), Value::Text("magnific.precision-v2".into()))].into())
        .unwrap();
    session.dispatch("node.add", &serde_json::json!({"canvas":CANVAS,"node":node})).unwrap();
    let source = NodeId::from_bytes([140; 16]);
    let pixels: Vec<u8> = (0..6).flat_map(|y| (0..8).flat_map(move |x| [if x < 3 { 180 } else { 40 }, if y < 3 { 210 } else { 60 }, x * 20, 255])).collect();
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
    session.dispatch("edge.connect",&serde_json::json!({"canvas":CANVAS,"edge":{"id":EdgeId::from_bytes([141;16]),"from":{"node":source,"port":"out"},"to":{"node":CARD,"port":"image"},"order":0}})).unwrap();
    let mut app = crate::app::NodeCraftApp::from_session(session, CANVAS).unwrap();
    app.theme = theme;
    app
}

fn card_harness(builder: egui_kittest::HarnessBuilder<crate::app::NodeCraftApp>, theme: crate::theme::ThemeKind) -> Harness<'static, crate::app::NodeCraftApp> {
    // Native thumbnail completion can repaint after the default four settling frames.
    let mut harness =
        builder.with_max_steps(120).with_size(egui::vec2(1200., 1000.)).build_ui_state(|ui, app: &mut crate::app::NodeCraftApp| app.show(ui), app(theme));
    crate::theme::apply(&harness.ctx, theme);
    harness.run();
    harness.state_mut().view.zoom = 1.;
    harness.state_mut().view.pan = egui::vec2(520., 180.);
    harness.run();
    harness
}

#[test]
fn magnific38_generated_enhancement_controls_edit_and_undo_through_commands() {
    use egui::accesskit::Role;
    use nodecraft_types::{Unsigned, Value};
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut harness = card_harness(Harness::builder(), theme);
        let before = harness.state().session.document().clone();
        harness.get_by_label("Scale factor").click();
        harness.run();
        harness.get_by_label("4×").click();
        harness.run();
        assert_eq!(harness.state().session.document().canvases()[&CANVAS].nodes()[&CARD].params.get("scale_factor"), Some(&Value::Text("4".into())));
        harness.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
        harness.run();
        assert_eq!(harness.state().session.document(), &before);
        harness.get_by_role_and_label(Role::TextInput, "Sharpen").click();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.event(egui::Event::Text("20".into()));
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert_eq!(harness.state().session.document().canvases()[&CANVAS].nodes()[&CARD].params.get("sharpen"), Some(&Value::Unsigned(Unsigned(20))));
        harness.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
        harness.run();
        assert_eq!(harness.state().session.document(), &before);
    }
}

#[test]
#[ignore = "native GPU generated controls; NODECRAFT_MAGNIFIC38_RENDER_ROOT"]
fn magnific38_native_card_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_MAGNIFIC38_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut harness = card_harness(Harness::builder().wgpu(), theme);
        assert!(harness.query_by_label("Scale factor").is_some());
        assert!(harness.query_by_label("Sharpen").is_some());
        harness.hover_at(egui::pos2(1190., 985.));
        harness.run();
        harness.render().unwrap().save(root.join(format!("{theme:?}-card-after.png"))).unwrap();
    }
}
