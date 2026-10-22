use super::*;

const IMAGE: NodeId = NodeId::from_bytes([65; 16]);

fn missing_image() -> Harness<'static, NodeCraftApp> {
    let (mut h, _) = scene("mock-square");
    h.state_mut().session.dispatch("node.create", &json!({"canvas": C, "id": IMAGE, "kind": "source.image", "pos": {"x": 500, "y": 120}})).unwrap();
    h.state_mut().request_run(IMAGE, super::super::generate::RunKind::Generate).unwrap();
    h.run();
    h
}

#[test]
fn the_run_confirmation_explains_a_missing_builtin_asset() {
    let h = missing_image();
    assert!(h.query_by_label_contains("Image needs a value").is_some(), "the built-in validation explanation is visible");
    assert!(h.query_by_label_contains("check node parameters, connections and installed kind").is_none());
}

#[test]
#[ignore = "manual public-safe run-refusal capture; REASONS655_RENDER_PATH"]
fn reasons655_visual() {
    let app = missing_image().into_state();
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h.render().unwrap().save(std::env::var_os("REASONS655_RENDER_PATH").unwrap()).unwrap();
}
