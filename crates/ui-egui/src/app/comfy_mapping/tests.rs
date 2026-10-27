use super::*;
use egui_kittest::{Harness, kittest::Queryable};
const C: CanvasId = CanvasId::from_bytes([9; 16]);
const N: NodeId = NodeId::from_bytes([9; 16]);
const INFO: &str = r#"{
 "LoadImage":{"input":{"required":{"image":[["example.png"],{"image_upload":true}]}},"output":["IMAGE"]},
 "CLIPTextEncode":{"input":{"required":{"text":["STRING",{"multiline":true}],"strength":["FLOAT",{"min":0.0,"max":1.0,"default":0.5}]}},"output":["CONDITIONING"]},
 "SaveImage":{"input":{"required":{"images":["IMAGE",{}],"filename_prefix":["STRING",{}]}},"output":[],"output_node":true}
}"#;
fn app() -> NodeCraftApp {
    let mut app = super::super::comfy_workflow9_tests::app();
    nodecraft_engine::comfy_workflow_cmds::finish(
        &mut app.session,
        1,
        Ok(std::sync::Arc::new(nodecraft_engine::comfy_workflow_cmds::NodeCatalog::parse(INFO.as_bytes()).unwrap().0).into()),
    );
    app.open_comfy_mapping(N);
    app
}
#[test]
fn mapping947_real_checkbox_apply_and_undo_change_an_existing_card_without_reimport() {
    let mut h = Harness::builder().with_size(egui::Vec2::new(1100.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    h.run();
    assert!(h.query_by_label("Workflow fields").is_some());
    h.get_by_label("Show 2.strength").click();
    h.run();
    h.get_by_label("Apply fields").click();
    h.run();
    assert!(h.state().comfy_mapping.is_none());
    let node = &h.state().session.document().canvases()[&C].nodes()[&N];
    assert!(comfy_workflow::mapping(node).unwrap().params.iter().any(|p| p.node == "2" && p.input == "strength"));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    let node = &h.state().session.document().canvases()[&C].nodes()[&N];
    assert!(!comfy_workflow::mapping(node).unwrap().params.iter().any(|p| p.input == "strength"));
}
#[test]
fn mapping947_cancel_keeps_document_and_missing_catalog_has_a_clear_action() {
    let app = app();
    let before = app.session.document().clone();
    let mut h = Harness::builder().with_size(egui::Vec2::new(1100.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h.get_by_label("Show 2.strength").click();
    h.run();
    h.get_by_label("Cancel").click();
    h.run();
    assert_eq!(*h.state().session.document(), before);
    let mut app = super::super::comfy_workflow9_tests::app();
    app.open_comfy_mapping(N);
    let mut h = Harness::builder().with_size(egui::Vec2::new(800.0, 650.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    assert!(h.query_by_label("Read server nodes").is_some());
    assert!(h.state().comfy_mapping.as_ref().unwrap().fields.is_none());
}

#[test]
fn mapping947_real_reorder_rename_and_document_replacement_keep_the_editor_bound_to_its_card() {
    use egui::accesskit::Role;
    for (theme, width) in [(crate::theme::ThemeKind::Dark, 1100.0), (crate::theme::ThemeKind::Light, 800.0)] {
        let mut state = app();
        state.theme = theme;
        let mut h = Harness::builder().with_size(egui::Vec2::new(width, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), state);
        crate::theme::apply(&h.ctx, theme);
        h.run();
        h.get_by_label("Show 2.strength").click();
        h.run();
        h.get_by_label("Move 2.strength up").click();
        h.run();
        h.get_by_label("Results").click();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Image list port").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Image list port").type_text("textures");
        h.run();
        h.get_by_label("Apply fields").click();
        h.run();
        let node = &h.state().session.document().canvases()[&C].nodes()[&N];
        let mapping = comfy_workflow::mapping(node).unwrap();
        assert_eq!(mapping.params.first().unwrap().input, "strength");
        assert_eq!(node.outputs[0].name, "textures");
        h.state_mut().open_comfy_mapping(N);
        h.run();
        let replacement = Session::new(h.state().session.document().clone()).unwrap();
        h.state_mut().session = replacement;
        h.run();
        assert!(h.state().comfy_mapping.is_none(), "opening the same ids as a new document closes an old draft");
    }
}
#[test]
#[ignore = "manual public-safe GPU evidence; NODECRAFT_MAPPING947_CAPTURE"]
fn mapping947_visual_evidence() {
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_MAPPING947_CAPTURE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "dark"), (crate::theme::ThemeKind::Light, "light")] {
        let mut state = app();
        state.comfy_mapping = None;
        state.theme = theme;
        state.selection.only(N);
        let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1100.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), state);
        crate::theme::apply(&h.ctx, theme);
        h.run();
        h.render().unwrap().save(folder.join(format!("before-{name}.png"))).unwrap();
        h.state_mut().open_comfy_mapping(N);
        crate::theme::apply(&h.ctx, theme);
        h.run();
        h.get_by_label("Show 2.strength").click();
        h.run();
        h.render().unwrap().save(folder.join(format!("editor-{name}.png"))).unwrap();
        h.get_by_label("Apply fields").click();
        h.run();
        h.render().unwrap().save(folder.join(format!("after-{name}.png"))).unwrap();
    }
}
