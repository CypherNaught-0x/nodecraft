//! Public authored model cards, using the actual catalog before and after #27.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_types::{Modality, PortType};

fn scene(theme: ThemeKind) -> NodeCraftApp {
    let c = CanvasId::from_bytes([27; 16]);
    let mut p = Project::new(ProjectId::from_bytes([27; 16]), "MiniMax video, voice and music").unwrap();
    Change::AddCanvas { canvas: Canvas::new(c, "Creative studies") }.apply(&mut p).unwrap();
    let kinds = nodecraft_engine::generation_catalog::GenerationCatalog::builtin().unwrap().registry().unwrap();
    for (index, model, title, prompt, modality) in [
        (0u8, "fal.minimax-h3", "Motion study", "A paper boat turns gently on a quiet pond", Modality::Video),
        (1, "fal.minimax-speech28-hd", "Voice study", "A small idea can become a whole creative world.", Modality::Audio),
        (2, "fal.minimax-music3", "Music study", "Warm acoustic guitar and soft piano, a gentle instrumental", Modality::Audio),
    ] {
        let id = NodeId::from_bytes([index + 1; 16]);
        let kind = if modality == Modality::Video { nodecraft_nodes::generation::VIDEO_KIND } else { nodecraft_nodes::generation::AUDIO_KIND };
        let params = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text(prompt.into()))].into();
        let mut n = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &params).unwrap_or_else(|_| {
            let mut n = nodecraft_graph::Node::new(id, kind);
            n.inputs = vec![nodecraft_graph::Port { name: "prompt".into(), port_type: PortType { modality: Modality::Text, many: true } }];
            n.outputs = vec![nodecraft_graph::Port { name: "out".into(), port_type: PortType { modality, many: true } }];
            n.params = params;
            n
        });
        n.title = title.into();
        n.pos = nodecraft_graph::Position { x: -470. + f32::from(index) * 340., y: -240. };
        Change::AddNode { canvas: c, node: n }.apply(&mut p).unwrap();
    }
    let mut app = NodeCraftApp::from_session(Session::new(p).unwrap(), c).unwrap();
    app.theme = theme;
    app
}

#[test]
#[ignore = "manual native GPU before/after evidence; NODECRAFT_MINIMAX27_RENDER_ROOT and _PHASE"]
fn minimax27_render_cards() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_MINIMAX27_RENDER_ROOT").unwrap());
    let phase = std::env::var("NODECRAFT_MINIMAX27_RENDER_PHASE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h = Harness::builder().wgpu().with_size(Vec2::new(1400., 900.)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), scene(theme));
        theme::apply(&h.ctx, theme);
        h.hover_at(egui::pos2(1390., 890.));
        h.run();
        if let Some(button) = h.query_by_label("Hide map") {
            button.click();
            h.run();
        }
        assert_eq!(h.state().session.document().canvases().values().next().unwrap().nodes().len(), 3);
        h.render().unwrap().save(root.join(format!("{theme:?}-{phase}.png"))).unwrap();
        if phase == "after" {
            h.query_all_by_label("Change model…").nth(1).unwrap().click();
            h.run();
            h.event(egui::Event::Text("MiniMax".into()));
            h.run();
            assert!(h.state().model_choice.is_some());
            h.render().unwrap().save(root.join(format!("{theme:?}-model-picker.png"))).unwrap();
        }
    }
}

#[test]
fn minimax27_actual_cards_model_picker_apply_and_undo_use_shared_commands_in_both_themes() {
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h =
            Harness::builder().with_size(Vec2::new(1400., 900.)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), scene(theme));
        theme::apply(&h.ctx, theme);
        h.run();
        let before = h.state().session.document().clone();
        h.query_all_by_label("Change model…").nth(1).unwrap().click();
        h.run();
        assert!(h.state().model_choice.is_some());
        h.event(egui::Event::Text("fal.minimax-music3".into()));
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.document(), &before, "picker review does not mutate or generate");
        h.get_by_label("Apply model change").click();
        h.run();
        let c = CanvasId::from_bytes([27; 16]);
        let id = NodeId::from_bytes([2; 16]);
        let after = h.state().session.document().clone();
        let node = after.canvases().get(&c).unwrap().nodes().get(&id).unwrap();
        assert_eq!(node.params.get("model"), Some(&Value::Text("fal.minimax-music3".into())));
        assert!(node.history.is_empty());
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run();
        assert_eq!(h.state().session.document(), &before);
        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
        h.run();
        assert_eq!(h.state().session.document(), &after);
    }
}
