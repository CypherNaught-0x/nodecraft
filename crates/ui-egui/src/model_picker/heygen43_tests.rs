//! Real builtin picker evidence, without catalog filtering or vendor media.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct Scene {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn scene(builder: egui_kittest::HarnessBuilder<Scene>, theme: crate::theme::ThemeKind, lipsync: bool) -> Harness<'static, Scene> {
    let catalog = Catalog::builtin().unwrap();
    let wiring = Wiring::new(Modality::Video).with(if lipsync { Modality::Video } else { Modality::Image }, 1).with(Modality::Audio, 1);
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("HeyGen").unwrap();
    let mut h = builder.with_size(egui::vec2(1100., 800.)).build_ui_state(
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
#[ignore = "native GPU evidence; NODECRAFT_HEYGEN43_RENDER_ROOT and NODECRAFT_HEYGEN43_STAGE"]
fn heygen43_native_picker_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_HEYGEN43_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_HEYGEN43_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(Harness::builder().wgpu(), theme, false);
        h.hover_at(egui::pos2(1090., 785.));
        h.run();
        assert_eq!(h.query_by_label("HeyGen Photo Avatar 4 (fal)").is_some(), stage == "after");
        h.render().unwrap().save(root.join(format!("{theme:?}-picker-{stage}.png"))).unwrap();
    }
}

#[test]
fn heygen43_actual_picker_fits_both_media_pairs_and_preserves_unknown_prices_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (lip, id, label) in [
            (false, "fal.heygen-avatar4", "HeyGen Photo Avatar 4 (fal)"),
            (true, "fal.heygen-lipsync-speed", "HeyGen Lip sync Speed (fal)"),
            (true, "fal.heygen-lipsync-precision", "HeyGen Lip sync Precision (fal)"),
        ] {
            let mut h = scene(Harness::builder(), theme, lip);
            assert_eq!(price_label(h.state().catalog.get(id).unwrap()), "Usage based (USD)");
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
        }
    }
}
const CANVAS: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([143; 16]);
const CARD: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([144; 16]);
fn app(theme: crate::theme::ThemeKind, photo: bool) -> crate::app::NodeCraftApp {
    use nodecraft_graph::{Canvas, Change, Position, Project};
    use nodecraft_types::{EdgeId, NodeId, ProjectId, Value};
    let mut project = Project::new(ProjectId::from_bytes([143; 16]), "Original supplied speech").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Animate") }.apply(&mut project).unwrap();
    let mut session = nodecraft_engine::Session::new(project).unwrap();
    let node = session
        .kinds()
        .get(nodecraft_nodes::generation::VIDEO_KIND)
        .unwrap()
        .instantiate_instance(
            CARD,
            Default::default(),
            &[("model".into(), Value::Text(if photo { "fal.heygen-avatar4" } else { "fal.heygen-lipsync-speed" }.into()))].into(),
        )
        .unwrap();
    session.dispatch("node.add", &serde_json::json!({"canvas":CANVAS,"node":node})).unwrap();
    let visual = NodeId::from_bytes([145; 16]);
    let speech = NodeId::from_bytes([146; 16]);
    let pixels: Vec<u8> = (0..48).flat_map(|y| (0..64).flat_map(move |x| [if x < 32 { 230 } else { 40 }, if y < 24 { 160 } else { 80 }, 110, 255])).collect();
    let png = nodecraft_media::encode_png(64, 48, &pixels, &Default::default()).unwrap();
    let clip = include_bytes!("../../../providers/src/fal/fixtures/pika29.mp4");
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&16036u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(b"\x02\0\x10\0data");
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.resize(16044, 0);
    nodecraft_engine::import::import_files(
        &mut session,
        CANVAS,
        Position { x: -360., y: 0. },
        200.,
        &[nodecraft_engine::import::Incoming { name: if photo { "portrait.png" } else { "original.mp4" }, bytes: if photo { &png } else { clip } }],
        &[visual],
    )
    .unwrap();
    nodecraft_engine::import::import_files(
        &mut session,
        CANVAS,
        Position { x: -360., y: 300. },
        200.,
        &[nodecraft_engine::import::Incoming { name: "speech.wav", bytes: &wav }],
        &[speech],
    )
    .unwrap();
    for (id, from, role) in [(147, visual, if photo { "portrait" } else { "video" }), (148, speech, "speech")] {
        session.dispatch("edge.connect",&serde_json::json!({"canvas":CANVAS,"edge":{"id":EdgeId::from_bytes([id;16]),"from":{"node":from,"port":"out"},"to":{"node":CARD,"port":role},"order":0}})).unwrap();
    }
    let mut app = crate::app::NodeCraftApp::from_session(session, CANVAS).unwrap();
    app.theme = theme;
    app
}
fn card_harness(
    builder: egui_kittest::HarnessBuilder<crate::app::NodeCraftApp>,
    theme: crate::theme::ThemeKind,
    photo: bool,
) -> Harness<'static, crate::app::NodeCraftApp> {
    // Native thumbnail completion can repaint after the default four settling frames.
    let mut h =
        builder.with_max_steps(120).with_size(egui::vec2(1100., 900.)).build_ui_state(|ui, app: &mut crate::app::NodeCraftApp| app.show(ui), app(theme, photo));
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h.state_mut().view.zoom = 1.;
    h.state_mut().view.pan = egui::vec2(520., 180.);
    h.run();
    h
}
#[test]
fn heygen43_actual_generated_lipsync_toggles_are_pointer_edits_and_each_undo_restores_the_document() {
    use nodecraft_types::Value;
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = card_harness(Harness::builder(), theme, false);
        for (label, key) in [("Remove source music", "disable_music_track"), ("Enhance replacement speech", "enable_speech_enhancement")] {
            let before = h.state().session.document().clone();
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().session.document().canvases()[&CANVAS].nodes()[&CARD].params.get(key), Some(&Value::Bool(true)));
            h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
            h.run();
            assert_eq!(h.state().session.document(), &before);
        }
    }
}
#[test]
#[ignore = "native WGPU generated media modes; NODECRAFT_HEYGEN43_RENDER_ROOT"]
fn heygen43_native_both_media_cards_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_HEYGEN43_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for photo in [true, false] {
            let mut h = card_harness(Harness::builder().wgpu(), theme, photo);
            if !photo {
                assert!(h.query_by_label("Remove source music").is_some());
            }
            h.hover_at(egui::pos2(1090., 885.));
            h.run();
            h.render().unwrap().save(root.join(format!("{theme:?}-{}-after.png", if photo { "photo-card" } else { "lipsync-card" }))).unwrap();
        }
    }
}
