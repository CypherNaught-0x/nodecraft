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
    crate::app::panel_check::check_question(&h, egui::Id::new("nodecraft.model_review"), "Review model change");
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
        crate::app::panel_check::check_question(&h, egui::Id::new("nodecraft.model_stale"), "Model preview changed");
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

#[test]
fn router949_model_picker_matches_all_direct_candidate_states_and_headless_checks() {
    const MODELS: [&str; 3] = ["fal.grok-image2-edit", "fal.qwen-image3-edit", "fal.grok-image2"];
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut expected = None;
        for routed in [false, true] {
            let builtin = nodecraft_catalog::Catalog::builtin().unwrap();
            let catalog = nodecraft_catalog::Catalog::from_models(MODELS.iter().map(|id| builtin.get(id).unwrap().clone())).unwrap();
            let snapshot = GenerationCatalog::new(nodecraft_engine::run_cmds::RunCatalog::new(catalog).unwrap()).unwrap();
            let kinds = snapshot.registry().unwrap();
            let mut project = Project::new(ProjectId::from_bytes([149; 16]), "Public Router model choices").unwrap();
            Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
            let params = [("model".into(), Value::Text(MODELS[0].into())), ("prompt".into(), Value::Text("Public sample".into()))].into();
            let node = kinds.get("generation.image").unwrap().instantiate_instance(A, Default::default(), &params).unwrap();
            Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
            let mut resources = nodecraft_engine::commands::Resources::default();
            resources.insert(snapshot.clone());
            resources.insert(snapshot.run_catalog());
            let mut session =
                Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
            let hub = NodeId::from_bytes([40; 16]);
            if routed {
                let node = session
                    .kinds()
                    .get(nodecraft_nodes::router::ID)
                    .unwrap()
                    .instantiate_instance(hub, nodecraft_nodes::router::config(nodecraft_types::Modality::Image), &Default::default())
                    .unwrap();
                session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
                session.dispatch("edge.connect",&json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([40;16]),"from":{"node":hub,"port":"out"},"to":{"node":A,"port":"images"},"order":0}})).unwrap();
            }
            for n in 10..14 {
                let id = NodeId::from_bytes([n; 16]);
                session.dispatch("node.create", &json!({"canvas":C,"id":id,"kind":"source.image","pos":{"x":0,"y":0}})).unwrap();
                session.dispatch("edge.connect",&json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([n;16]),"from":{"node":id,"port":"out"},"to":{"node":if routed {hub}else{A},"port":if routed {"in"}else{"images"}},"order":n}})).unwrap();
            }
            let before = session.document().clone();
            let refused = session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[A],"model":MODELS[1]}));
            assert!(refused.is_err(), "four images cannot fit Qwen's three-image maximum through either path");
            assert_eq!(session.document(), &before);
            let mut app = NodeCraftApp::from_session(session, C).unwrap();
            app.theme = theme;
            let revision = app.session.revision();
            app.open_model_picker(C, vec![A], revision).unwrap();
            let mut h = Harness::builder().with_size(Vec2::new(1200.0, 900.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
            theme::apply(&h.ctx, theme);
            h.run();
            let states: Vec<_> = MODELS
                .iter()
                .map(|id| {
                    let label = &builtin.get(id).unwrap().label;
                    ((*id).to_owned(), h.get_by_label(label).accesskit_node().is_disabled())
                })
                .collect();
            assert_eq!(states.iter().map(|s| s.1).collect::<Vec<_>>(), vec![false, true, true]);
            if let Some(expected) = &expected {
                assert_eq!(&states, expected);
            } else {
                expected = Some(states);
            }
        }
    }
}

/// A picker-stage failure shows at the top of the picker panel (#1116; its own window before),
/// which keeps its layout in the floating panel area (#959).
#[test]
fn a_picker_failure_shows_in_the_picker_panel_that_keeps_its_layout() {
    let mut h = harness();
    h.get_by_label("Change model…").click();
    h.run();
    h.state_mut().model_choice.as_mut().unwrap().error = Some("The model catalog could not be read. Nothing was changed.".into());
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Window, "Choose a model").get_by_label("The model catalog could not be read. Nothing was changed.");
    crate::app::panel_check::check_open_panels(&h, "Choose a model, with a failure");
}

/// The desktop app's smallest window (#1115), where every dialog must stay on the screen.
fn small() -> Harness<'static, NodeCraftApp> {
    let mut h = Harness::builder().with_size(Vec2::new(640.0, 400.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    h.run();
    h
}
/// Opens the picker on the selected card, as its Change model… does.
fn open_small(h: &mut Harness<'static, NodeCraftApp>) {
    h.state_mut().selection.only(A);
    let revision = h.state().session.revision();
    h.state_mut().open_model_picker(C, vec![A], revision).unwrap();
    h.run();
}
fn selected(h: &Harness<'static, NodeCraftApp>) -> bool {
    h.state().selection.nodes().contains(&A)
}

/// #1116: Choose a model is a tool, a floating panel kept in the panel area at 640 × 400; Escape
/// closes it and doesn't also clear the canvas selection.
#[test]
fn dialogs1116_the_picker_is_a_panel_on_screen_and_escape_closes_only_it() {
    let mut h = small();
    open_small(&mut h);
    h.run_steps(4);
    crate::app::panel_check::check_open_panels(&h, "Choose a model at 640 × 400");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().model_choice.is_none());
    assert!(selected(&h), "Escape closed the picker without reaching the canvas");
}

/// #1116: Review model change is a question at 640 × 400: Escape cancels (nothing changes, the
/// canvas keeps its selection) and Enter applies.
#[test]
fn dialogs1116_review_is_a_question_enter_applies_escape_cancels() {
    let mut h = small();
    let before = h.state().session.document().clone();
    open_small(&mut h);
    choose(&mut h);
    h.run_steps(4);
    crate::app::panel_check::check_question(&h, egui::Id::new("nodecraft.model_review"), "Review model change at 640 × 400");
    let apply = h.get_by_label("Apply model change").rect();
    assert!(h.ctx.content_rect().contains_rect(apply), "Apply {apply:?} is on the screen");
    h.get_by_role_and_label(egui::accesskit::Role::Dialog, "Review model change");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().model_choice.is_none());
    assert_eq!(h.state().session.document(), &before);
    assert!(selected(&h), "Escape cancelled without reaching the canvas");

    open_small(&mut h);
    choose(&mut h);
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().model_choice.is_none());
    let node = h.state().session.document().canvases().get(&C).unwrap().nodes().get(&A).unwrap().clone();
    assert_eq!(node.params.get("model"), Some(&Value::Text(DESTINATION.into())));
}

/// #1116: Model preview changed is a question at 640 × 400: Enter refreshes the models, Escape
/// cancels the change.
#[test]
fn dialogs1116_preview_changed_is_a_question_enter_refreshes_escape_cancels() {
    for escape in [true, false] {
        let mut h = small();
        open_small(&mut h);
        choose(&mut h);
        h.state_mut().session.dispatch("node.title", &json!({"canvas":C,"id":A,"title":"External change"})).unwrap();
        h.run();
        assert!(h.state().model_choice.as_ref().unwrap().stale);
        crate::app::panel_check::check_question(&h, egui::Id::new("nodecraft.model_stale"), "Model preview changed at 640 × 400");
        let before = h.state().session.document().clone();
        h.key_press(if escape { egui::Key::Escape } else { egui::Key::Enter });
        h.run();
        assert_eq!(h.state().session.document(), &before);
        if escape {
            assert!(h.state().model_choice.is_none());
            assert!(selected(&h), "Escape cancelled without reaching the canvas");
        } else {
            let choice = h.state().model_choice.as_ref().unwrap();
            assert!(!choice.stale && matches!(choice.step, Step::Picking(_)), "Enter refreshed the models");
        }
    }
}
