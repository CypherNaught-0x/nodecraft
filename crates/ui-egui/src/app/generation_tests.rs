//! Real schema-generated production generation card. No paid UI host is installed.
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
const C: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([65; 16]);
const N: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([66; 16]);
fn public_scene(unknown: bool) -> NodeCraftApp {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([67; 16]), "Public image generation").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    // Authored persisted graph fixture, identical in before/after captures. Before this kind
    // was registered the actual default Session could retain it but could not describe it.
    let mut node = nodecraft_graph::Node::new(N, "generation.image");
    node.title = "Public image generation".into();
    node.pos = nodecraft_graph::Position { x: 150.0, y: 100.0 };
    node.inputs = vec![
        nodecraft_graph::Port { name: "prompt".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: true } },
        nodecraft_graph::Port { name: "image".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: true } },
        nodecraft_graph::Port { name: "mask".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: true } },
    ];
    node.outputs =
        vec![nodecraft_graph::Port { name: "out".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: true } }];
    node.params.insert("model".into(), nodecraft_types::Value::Text(if unknown { "unavailable/model" } else { "openai-images.flare" }.into()));
    node.params.insert("prompt".into(), nodecraft_types::Value::Text("A public blue circle".into()));
    node.params.insert("n".into(), nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(2)));
    nodecraft_graph::Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    NodeCraftApp::from_session(Session::new(project).unwrap(), C).unwrap()
}
#[test]
fn default_generation_card_uses_registered_model_schema_and_actual_describe() {
    let app = public_scene(false);
    assert!(app.session.kinds().get("generation.image").is_ok());
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.step();
    assert!(h.query_by_label_contains("Images").is_some());
    assert!(h.query_by_label_contains("Prompt").is_some());
    let description = h.state_mut().session.dispatch("node.describe", &json!({"canvas":C,"id":N})).unwrap();
    assert_eq!(description["contract"]["params"]["namespace"], "generation.image");
    assert_eq!(description["contract"]["owned_groups"][0]["namespace"], "openai-images.flare");
    assert_eq!(
        h.state().session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().params["model"],
        nodecraft_types::Value::Text("openai-images.flare".into())
    );
}
#[test]
fn unknown_saved_generation_model_remains_readable_without_substitution() {
    let app = public_scene(true);
    let before = app.session.document().clone();
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.step();
    assert_eq!(h.state().session.document(), &before);
    assert!(h.query_by_label_contains("Public image generation").is_some());
}
#[test]
#[ignore = "manual public-safe before/after capture; GENERATION465_RENDER_PATH and GENERATION465_EXPECT_CONTROLS"]
fn generation465_visual() {
    let path = std::path::PathBuf::from(std::env::var_os("GENERATION465_RENDER_PATH").unwrap());
    let expected = std::env::var("GENERATION465_EXPECT_CONTROLS").unwrap() == "1";
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), public_scene(false));
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.step();
    h.step();
    assert_eq!(h.query_by_label_contains("Images").is_some(), expected);
    h.render().unwrap().save(path).unwrap();
}

fn conditional_scene() -> NodeCraftApp {
    let mut model = nodecraft_catalog::Catalog::builtin().unwrap().get("openai-images.flare").unwrap().clone();
    let mut contract = model.parameter_contract.take().unwrap();
    let index = model.params.params.iter().position(|p| p.name == "n").unwrap();
    let n = model.params.params.remove(index);
    model.params.params.insert(0, n);
    contract.schema = model.params.clone();
    contract.rules.push(nodecraft_types::ParamRule {
        hidden: nodecraft_types::HiddenPolicy::Reset,
        visible_when: Some(nodecraft_types::Condition::Equals { param: "n".into(), value: nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(1)) }),
        ..nodecraft_types::ParamRule::implied("quality")
    });
    contract.rules.push(nodecraft_types::ParamRule {
        enabled_when: Some(nodecraft_types::Condition::Equals { param: "n".into(), value: nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(1)) }),
        ..nodecraft_types::ParamRule::implied("size")
    });
    model.parameter_contract = Some(contract);
    let catalog = nodecraft_engine::generation_catalog::GenerationCatalog::new(
        nodecraft_engine::run_cmds::RunCatalog::new(nodecraft_catalog::Catalog::from_models([model]).unwrap()).unwrap(),
    )
    .unwrap();
    let mut app = public_scene(false);
    // Install matching registry/resources through the public custom-session constructor.
    let session =
        Session::with_registries(app.session.document().clone(), nodecraft_engine::commands::Registry::builtins().unwrap(), catalog.registry().unwrap())
            .unwrap();
    app = NodeCraftApp::from_session(session, C).unwrap();
    app.session.swap_resource(Some(catalog.run_catalog()));
    app.session.swap_resource(Some(catalog));
    app
}
#[test]
fn generation_cache_honors_owned_visibility_before_card_render() {
    let app = conditional_scene();
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    let mut cache = SchemaCache::default();
    let schema = cached_schema(&mut cache, &app.session, node, 1).unwrap();
    assert!(!schema.visible.contains("quality"), "model-owned hidden rule must reach generated card");
    assert!(schema.visible.contains("size"));
    assert!(!schema.enabled.as_ref().unwrap().contains("size"), "visible disabled model field must remain non-editable");
}
#[test]
fn generation_cache_rejects_same_revision_catalog_replacement() {
    let mut app = public_scene(false);
    let mut cache = SchemaCache::default();
    let revision = app.session.revision();
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    assert!(cached_schema(&mut cache, &app.session, node, 1).is_some());
    let replacement = nodecraft_engine::generation_catalog::GenerationCatalog::builtin().unwrap();
    app.session.swap_resource(Some(replacement.run_catalog()));
    app.session.swap_resource(Some(replacement));
    assert_eq!(revision, app.session.revision());
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    cache.refresh_generation(&app.session);
    assert!(cache.get(&N).is_none(), "early geometry must not retain stale descriptor");
    assert!(cached_schema(&mut cache, &app.session, node, 2).is_none(), "old definition cannot render a replaced catalog as current");
}

#[test]
fn generation_card_conditional_visibility_changes_through_real_parameter_command() {
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), conditional_scene());
    h.step();
    assert!(h.query_by_label("Quality").is_none());
    assert!(h.query_by_label("Size").is_some());
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas":C,"id":N,"name":"n","value":{"type":"unsigned","value":"1"}})).unwrap();
    h.step();
    assert!(h.query_by_label("Quality").is_some());
}

#[test]
fn generation_parameter_controller_publishes_owned_reset_atomically() {
    let mut app = conditional_scene();
    let set = |app: &mut NodeCraftApp, name: &str, value: serde_json::Value| {
        app.session.dispatch("node.param_set", &json!({"canvas":C,"id":N,"name":name,"value":value}))
    };
    set(&mut app, "n", json!({"type":"unsigned","value":"1"})).unwrap();
    set(&mut app, "quality", json!({"type":"text","value":"high"})).unwrap();
    let before = app.session.document().clone();
    set(&mut app, "n", json!({"type":"unsigned","value":"2"})).unwrap();
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    assert!(!node.params.contains_key("quality"), "Reset must publish in same command as controller edit");
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(app.session.document(), &before);
    let revision = app.session.revision();
    assert!(set(&mut app, "quality", json!({"type":"text","value":"not-valid"})).is_err());
    assert_eq!(revision, app.session.revision());
}
#[test]
fn generation_parameter_rejects_replaced_catalog_without_mutation() {
    let mut app = public_scene(false);
    let before = app.session.document().clone();
    let revision = app.session.revision();
    let replacement = nodecraft_engine::generation_catalog::GenerationCatalog::builtin().unwrap();
    app.session.swap_resource(Some(replacement.run_catalog()));
    app.session.swap_resource(Some(replacement));
    assert!(app.session.dispatch("node.param_set", &json!({"canvas":C,"id":N,"name":"prompt","value":{"type":"text","value":"new prompt"}})).is_err());
    assert_eq!(app.session.document(), &before);
    assert_eq!(app.session.revision(), revision);
}

fn policy_scene() -> NodeCraftApp {
    let mut model = nodecraft_catalog::Catalog::builtin().unwrap().get("openai-images.flare").unwrap().clone();
    model.params = nodecraft_types::ParamSchema {
        params: [
            ("unset_caption", "Unset caption", "fabricated unset schema"),
            ("auto_caption", "Auto caption", "fabricated auto schema"),
            ("literal_caption", "Literal caption", "fabricated literal schema"),
        ]
        .into_iter()
        .map(|(name, label, _default)| nodecraft_types::ParamSpec {
            name: name.into(),
            label: label.into(),
            description: None,
            control: nodecraft_types::ParamControl::Text { multiline: false, max_bytes: 1024 },
            required: false,
            default: (name == "literal_caption").then(|| nodecraft_types::Value::Text("resolved literal".into())),
            visible_if: None,
        })
        .collect(),
    };
    let mut contract = nodecraft_types::Contract::from_schema(&model.id, nodecraft_types::ParamOwner::Model, model.params.clone());
    for (name, default) in [
        ("unset_caption", nodecraft_types::DefaultPolicy::Unset),
        ("auto_caption", nodecraft_types::DefaultPolicy::Auto),
        ("literal_caption", nodecraft_types::DefaultPolicy::Literal),
    ] {
        contract.rules.push(nodecraft_types::ParamRule { default: Some(default), ..nodecraft_types::ParamRule::implied(name) });
    }
    model.parameter_contract = Some(contract);
    model.pricing = nodecraft_catalog::Pricing::Free;
    let catalog = nodecraft_engine::generation_catalog::GenerationCatalog::new(
        nodecraft_engine::run_cmds::RunCatalog::new(nodecraft_catalog::Catalog::from_models([model]).unwrap()).unwrap(),
    )
    .unwrap();
    let mut project = public_scene(false).session.document().clone();
    let mut node = project.canvases().get(&C).unwrap().nodes().get(&N).unwrap().clone();
    node.params.remove("n");
    nodecraft_graph::Change::SetNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), catalog.registry().unwrap()).unwrap();
    session.swap_resource(Some(catalog.run_catalog()));
    session.swap_resource(Some(catalog));
    NodeCraftApp::from_session(session, C).unwrap()
}
#[test]
fn generation_policy_unset_does_not_display_schema_literal() {
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), policy_scene());
    h.step();
    assert!(h.query_by_value("fabricated unset schema").is_none());
    h.get_by_label("Unset caption").click();
    h.step();
    h.step();
    assert_eq!(h.get_by_label("Unset caption").accesskit_node().value().as_deref(), Some(""));
}
#[test]
fn generation_policy_auto_does_not_display_schema_literal() {
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), policy_scene());
    h.step();
    assert!(h.query_by_value("fabricated auto schema").is_none());
    h.get_by_label("Auto caption").click();
    h.step();
    h.step();
    assert_eq!(h.get_by_label("Auto caption").accesskit_node().value().as_deref(), Some(""));
}
#[test]
fn generation_policy_literal_displays_resolved_default() {
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), policy_scene());
    h.step();
    h.get_by_label("Literal caption").click();
    h.step();
    h.step();
    assert!(h.query_all_by_value("resolved literal").next().is_some());
    assert!(h.query_by_value("fabricated literal schema").is_none());
}

#[test]
fn generation_marquee_admission_uses_the_same_owned_conditional_states_as_the_card() {
    let app = conditional_scene();
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    let admitted = admission::admit(&app.session, node, None).unwrap();
    assert!(!admitted.visible.contains("quality"));
    assert!(admitted.visible.contains("size"));
    assert!(!admitted.enabled.as_ref().unwrap().contains("size"));
    let mut schemas = SchemaCache::default();
    let rendered = cached_schema(&mut schemas, &app.session, node, 1).unwrap();
    assert_eq!(admitted.visible, rendered.visible);
    assert_eq!(admitted.enabled, rendered.enabled);
}

#[test]
fn generation_marquee_geometry_rejects_same_revision_replaced_catalog_like_the_renderer() {
    let mut app = conditional_scene();
    let mut geometry = admission::GeometryCache::default();
    geometry.prepare(&mut app.session, C, app.theme);
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().clone();
    geometry.rect(&app.session, &node, app.theme.tokens());
    assert!(!geometry.placeholder(N));
    assert_eq!(geometry.len(), 1);
    let revision = app.session.revision();
    let replacement = nodecraft_engine::generation_catalog::GenerationCatalog::builtin().unwrap();
    app.session.swap_resource(Some(replacement.run_catalog()));
    app.session.swap_resource(Some(replacement));
    assert_eq!(app.session.revision(), revision);
    assert!(geometry.prepare(&mut app.session, C, app.theme));
    assert_eq!(geometry.len(), 0);
    geometry.rect(&app.session, &node, app.theme.tokens());
    assert!(geometry.placeholder(N));
    assert!(admission::admit(&app.session, &node, None).is_none());
    let mut schemas = SchemaCache::default();
    assert!(cached_schema(&mut schemas, &app.session, &node, 1).is_none());
}

#[test]
fn generation_shared_admission_charges_enabled_names_within_the_existing_cache_budget() {
    let app = conditional_scene();
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    let admitted = admission::admit(&app.session, node, None).unwrap();
    let names =
        admitted.visible.iter().chain(admitted.enabled.iter().flatten()).fold(256usize, |total, name| total.saturating_add(name.len().saturating_add(96)));
    assert!(admitted.bytes.checked_add(names).unwrap() <= MAX_SCHEMA_CACHE_BYTES);
    let mut cache = SchemaCache::default();
    cached_schema(&mut cache, &app.session, node, 1).unwrap();
    assert_eq!(cache_bytes(&cache), cache.bytes);
    assert_eq!(cache.bytes, admitted.bytes + names);
    assert!(cache.bytes <= MAX_SCHEMA_CACHE_BYTES);
    assert!(std::mem::size_of::<admission::GeometryCache>() <= 128);
}
