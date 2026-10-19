use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_types::ParamValues;
const C: CanvasId = CanvasId::from_bytes([157; 16]);
const A: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: &str = "openai-images.sunburst";
const DESTINATION: &str = "openai-images.flare";
fn app() -> NodeCraftApp {
    let mut project = Project::new(ProjectId::from_bytes([157; 16]), "Public model choice").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let values: ParamValues = [("model".into(), Value::Text(SOURCE.into())), ("prompt".into(), Value::Text("Two circles".into()))].into();
    let node = session.kinds().get("generation.image").unwrap().instantiate_instance(A, Default::default(), &values).unwrap();
    session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
    NodeCraftApp::from_session(session, C).unwrap()
}
fn harness() -> Harness<'static, NodeCraftApp> {
    Harness::builder().with_size(Vec2::new(1200.0, 900.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app())
}
fn choose(h: &mut Harness<'static, NodeCraftApp>) {
    h.event(egui::Event::Text(DESTINATION.into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(matches!(h.state().model_choice.as_ref().unwrap().step, Step::Review(_)));
}
#[test]
fn actual_card_pointer_preview_apply_is_one_atomic_undo_without_run_consent() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    h.get_by_label("Change model…").click();
    h.run();
    assert!(h.state().model_choice.is_some());
    choose(&mut h);
    assert_eq!(h.state().session.document(), &before);
    h.get_by_label("Apply model change").click();
    h.run();
    assert!(h.state().model_choice.is_none());
    let after = h.state().session.document().clone();
    let node = after.canvases().get(&C).unwrap().nodes().get(&A).unwrap();
    assert_eq!(node.params.get("model"), Some(&Value::Text(DESTINATION.into())));
    assert_eq!(node.params.get("prompt"), Some(&Value::Text("Two circles".into())));
    assert!(node.history.is_empty());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &after);
}
#[test]
fn cancel_closing_frame_owns_undo_delete_and_navigation() {
    let mut h = harness();
    h.get_by_label("Change model…").click();
    h.run();
    let before = h.state().session.document().clone();
    let view = h.state().view;
    let ctx = h.ctx.clone();
    assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_all", &json!({})).is_err());
    h.input_mut().events.extend([
        egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
    ]);
    h.run();
    assert!(h.state().model_choice.is_none());
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().view, view);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert!(!h.state().session.document().canvases().get(&C).unwrap().nodes().contains_key(&A), "normal Undo works after the closing frame");
}
#[test]
fn external_document_or_same_revision_catalog_replacement_keeps_preview_stale() {
    for catalog in [false, true] {
        let mut h = harness();
        h.get_by_label("Change model…").click();
        h.run();
        choose(&mut h);
        if catalog {
            let next = GenerationCatalog::builtin().unwrap();
            h.state_mut().session.swap_resource(Some(next.run_catalog()));
            h.state_mut().session.swap_resource(Some(next));
        } else {
            h.state_mut().session.dispatch("node.title", &json!({"canvas":C,"id":A,"title":"External change"})).unwrap();
        }
        let before = h.state().session.document().clone();
        h.run();
        assert!(h.state().model_choice.as_ref().unwrap().stale);
        assert!(h.query_by_label("Apply model change").is_none());
        h.get_by_label("Cancel model change").click();
        h.run();
        assert_eq!(h.state().session.document(), &before);
    }
}
#[test]
fn opening_picker_preserves_real_unsaved_prompt_and_apply_stays_disabled() {
    let mut h = harness();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Prompt").click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    h.event(egui::Event::Text("Recover this private draft".into()));
    h.run();
    assert!(h.state().editors.get(&A).unwrap().has_uncommitted());
    let before = h.state().session.document().clone();
    h.get_by_label("Change model…").click();
    h.run();
    assert_eq!(h.state().session.document(), &before, "opening does not submit a lost-focus draft");
    choose(&mut h);
    let apply = h.get_by_label("Apply model change");
    assert!(apply.accesskit_node().is_disabled());
    h.get_by_label("Cancel model change").click();
    h.run();
    assert!(h.state().editors.get(&A).unwrap().has_uncommitted());
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn actual_role_filter_keeps_incompatible_models_visible_but_disabled() {
    use nodecraft_graph::Edge;
    let mut h = harness();
    let image = NodeId::from_bytes([12; 16]);
    h.state_mut().session.dispatch("node.create", &json!({"canvas":C,"id":image,"kind":"source.image","pos":{"x":0,"y":0}})).unwrap();
    h.state_mut().session.dispatch("edge.connect",&json!({"canvas":C,"edge":Edge{id:EdgeId::from_bytes([12;16]),from:Endpoint{node:image,port:"out".into()},to:Endpoint{node:A,port:"image".into()},order:0}})).unwrap();
    let revision = h.state().session.revision();
    h.state_mut().open_model_picker(C, vec![A], revision).unwrap();
    h.run();
    h.event(egui::Event::Text("fal.flux-schnell".into()));
    h.run();
    let label = nodecraft_catalog::Catalog::builtin().unwrap().get("fal.flux-schnell").unwrap().label.clone();
    assert!(h.get_by_label(&label).accesskit_node().is_disabled());
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(matches!(h.state().model_choice.as_ref().unwrap().step, Step::Picking(_)));
    assert_eq!(h.state().session.revision(), revision);
}

#[test]
fn unavailable_saved_model_card_uses_explicit_recovery_picker_not_raw_parameter_edit() {
    let original = app();
    let mut project = original.session.document().clone();
    let mut node = project.canvases().get(&C).unwrap().nodes().get(&A).unwrap().clone();
    let saved = Value::Text("LegacyModel:v2".into());
    nodecraft_types::ParamControl::Model.validate_value(&saved).unwrap();
    node.params.insert("model".into(), saved);
    Change::SetNode { canvas: C, node }.apply(&mut project).unwrap();
    let before = project.clone();
    let app = NodeCraftApp::from_session(Session::new(project).unwrap(), C).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 900.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.get_by_label("Choose supported model…").click();
    h.run();
    choose(&mut h);
    let Step::Review(preview) = &h.state().model_choice.as_ref().unwrap().step else { panic!() };
    assert!(preview.targets.iter().all(|target| target.dropped.iter().all(|drop| drop.reason == "source_unavailable")));
    h.get_by_label("Apply model change").click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}
