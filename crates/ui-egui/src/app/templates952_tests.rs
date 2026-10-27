//! Actual library descriptions, pointer-driven starter insertion, and native theme evidence.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
fn app() -> NodeCraftApp {
    let mut app = NodeCraftApp::headless().unwrap();
    app.library.open = true;
    app.library.template_query = "Generate text".into();
    app
}
fn harness(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    Harness::builder().with_size(Vec2::new(1100.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}
#[test]
fn templates952_library_shows_descriptions_and_pointer_add_preserves_work_and_undo() {
    let mut h = harness(app());
    h.run();
    h.get_by_label("Starter templates");
    let entry = nodecraft_engine::templates::entries().unwrap().into_iter().find(|e| e.id == "generate-text").unwrap();
    assert!(h.query_by_label(&entry.description).is_some());
    let before = h.state().session.document().clone();
    h.get_by_label("Add template").click();
    h.run();
    assert!(h.state().session.document().canvases().contains_key(&entry.canvas), "{:?}", h.state().notice);
    assert_eq!(h.state().canvas, entry.canvas);
    assert!(h.query_by_label("In project").is_some());
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert_eq!(h.state().session.document(), &before);
}
#[test]
fn templates952_published_and_library_versions_show_their_saved_short_description() {
    let mut app = app();
    let canvas = app.canvas;
    let source = app.session.document().canvases().get(&canvas).unwrap().nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    let version = CanvasId::from_bytes([95; 16]);
    app.session.dispatch("technique.publish",&json!({"canvas":canvas,"published":version,"seed":NodeId::from_bytes([94;16]),"technique":nodecraft_types::TechniqueId::from_bytes([93;16]),"name":"Example text","description":"Make a short reusable caption.","outputs":[{"node":source,"name":"Caption"}]})).unwrap();
    app.session.dispatch("library.add", &json!({"canvas":version})).unwrap();
    let mut h = harness(app);
    h.run();
    assert_eq!(h.get_all_by_label("Make a short reusable caption.").count(), 2);
}
#[test]
fn templates952_shipped_app_form_opens_through_the_actual_selected_technique_button() {
    let entry = nodecraft_engine::templates::entries().unwrap().into_iter().find(|e| e.id == "app-form").unwrap();
    let snapshot = nodecraft_engine::templates::load(&entry.id).unwrap();
    let placed = snapshot.project.canvases().get(&entry.canvas).unwrap().nodes().values().find(|n| n.kind == "structure.technique").unwrap().id;
    let session = Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap();
    let mut app = NodeCraftApp::from_session(session, entry.canvas).unwrap();
    app.selection.only(placed);
    let mut h = harness(app);
    h.run();
    h.run();
    crate::app::topbar_tests::reveal(&mut h, "Open as app");
    h.get_by_label("Open as app").click();
    h.run();
    assert!(h.state().app_mode.is_some(), "{:?}", h.state().notice);
    assert!(h.query_by_label("Subject").is_some());
    let form = h.state_mut().session.dispatch("app.form", &json!({"canvas":entry.canvas,"id":placed})).unwrap();
    assert_eq!(form["inputs"][0]["feed"], json!("example"));
    assert_eq!(form["inputs"][0]["example"], json!({"type":"text","value":"A paper boat"}));
    assert_eq!(form["inputs"][0]["fillable"], json!(true));
}
#[test]
#[ignore = "native GPU before/after evidence; TEMPLATE952_RENDER_DIR"]
fn templates952_library_render() {
    let dir = std::path::PathBuf::from(std::env::var_os("TEMPLATE952_RENDER_DIR").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut app = app();
        app.theme = theme;
        let mut h = Harness::builder().wgpu().with_size(Vec2::new(1100.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        h.run();
        assert!(h.query_by_label("Starter templates").is_some());
        h.render().unwrap().save(dir.join(format!("{theme:?}-after.png"))).unwrap();
    }
}
