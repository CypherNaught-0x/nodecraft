//! Public, installed-list picker fixtures and real pointer/typing regressions for #678.
use super::NodeCraftApp;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_types::{CanvasId, Choice, NodeId, OptionKey, OptionLists, ParamValues, ProjectId, Value};
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([167; 16]);
const N: NodeId = NodeId::from_bytes([168; 16]);

fn choices() -> Vec<Choice> {
    (0..5000).map(|n| Choice { value: format!("checkpoint-{n:04}.safetensors"), label: format!("Studio model {n:04}") }).collect()
}

fn install(app: &mut NodeCraftApp, choices: Vec<Choice>) {
    let mut lists = OptionLists::default();
    lists.set(OptionKey { owner: "comfyui.sdxl", source: "comfyui.checkpoints", scope: None }, choices).unwrap();
    let _ = app.session.swap_resource(Some(lists));
}

fn scene(checkpoint: Option<&str>, choices: Vec<Choice>) -> NodeCraftApp {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([169; 16]), "Public option picker").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let (kinds, _) = nodecraft_engine::generation_catalog::default_registry().unwrap();
    let mut values: ParamValues = [("model".into(), Value::Text("comfyui.sdxl".into())), ("prompt".into(), Value::Text("A public sample".into()))].into();
    if let Some(checkpoint) = checkpoint {
        values.insert("checkpoint".into(), Value::Text(checkpoint.into()));
    }
    let mut node = kinds.get(nodecraft_nodes::generation::KIND).unwrap().instantiate_instance(N, Default::default(), &values).unwrap();
    node.title = "Public checkpoint picker".into();
    nodecraft_graph::Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut app = NodeCraftApp::from_session(nodecraft_engine::Session::new(project).unwrap(), C).unwrap();
    install(&mut app, choices);
    app
}

fn harness(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    Harness::builder()
        .with_size(egui::Vec2::new(1400.0, 1000.0))
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}

fn stored(app: &NodeCraftApp) -> Option<Value> {
    app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().params.get("checkpoint").cloned()
}

fn open(h: &mut Harness<'static, NodeCraftApp>) {
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Checkpoint").click();
    h.run();
}

fn type_filter(h: &mut Harness<'static, NodeCraftApp>, query: &str) {
    assert!(h.get_by_role_and_label(Role::TextInput, "Filter options").is_focused());
    h.get_by_role_and_label(Role::TextInput, "Filter options").type_text(query);
    h.run();
    assert_eq!(h.get_by_role_and_label(Role::TextInput, "Filter options").value().as_deref(), Some(query));
}

#[test]
fn typing_a_label_finds_one_of_five_thousand_choices_and_stores_its_value() {
    let mut h = harness(scene(None, choices()));
    let before = h.state().session.document().clone();
    open(&mut h);
    type_filter(&mut h, "MODEL 4999");
    assert_eq!(h.state().session.document(), &before, "filtering is presentation state");
    h.get_by_label("Studio model 4999").click();
    h.run();
    assert_eq!(stored(h.state()), Some(Value::Text("checkpoint-4999.safetensors".into())));
    assert_eq!(h.get_by_role_and_label(Role::ComboBox, "Checkpoint").accesskit_node().value().as_deref(), Some("Studio model 4999"));
}

#[test]
fn typing_the_distinct_value_also_finds_its_label() {
    let mut h = harness(scene(None, choices()));
    open(&mut h);
    type_filter(&mut h, "checkpoint-4242");
    h.get_by_label("Studio model 4242").click();
    h.run();
    assert_eq!(stored(h.state()), Some(Value::Text("checkpoint-4242.safetensors".into())));
}

#[test]
fn opening_reveals_the_current_choice_and_reopening_drops_the_old_filter() {
    let mut h = harness(scene(Some("checkpoint-4999.safetensors"), choices()));
    open(&mut h);
    assert!(h.query_by_label("Studio model 4999").is_some());
    assert!(h.query_by_label("Studio model 0000").is_none());
    type_filter(&mut h, "model 0001");
    h.get_by_label("Studio model 0001").click();
    h.run();
    open(&mut h);
    assert!(h.get_by_role_and_label(Role::TextInput, "Filter options").accesskit_node().value().unwrap_or_default().is_empty());
    assert!(h.query_by_label("Studio model 0000").is_some(), "reopening shows the unfiltered list near the choice");
    assert_eq!(stored(h.state()), Some(Value::Text("checkpoint-0001.safetensors".into())));
}

#[test]
fn default_clears_a_stale_override_and_the_same_parameter_edit_is_undoable() {
    let mut h = harness(scene(Some("removed.safetensors"), choices()));
    let before = h.state().session.document().clone();
    open(&mut h);
    h.get_by_label("Use default").click();
    h.run();
    assert_eq!(stored(h.state()), None, "clearing restores the optional parameter's default");
    assert_eq!(h.get_by_role_and_label(Role::ComboBox, "Checkpoint").accesskit_node().value().as_deref(), Some("Choose…"));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn replacing_a_list_while_filtering_keeps_the_query_and_replaces_its_matches() {
    let mut h = harness(scene(None, choices()));
    let before = h.state().session.document().clone();
    open(&mut h);
    type_filter(&mut h, "model 4999");
    assert!(h.query_by_label("Studio model 4999").is_some());
    let mut replacement = choices();
    replacement.get_mut(4999).unwrap().label = "Renamed checkpoint".into();
    replacement.get_mut(2000).unwrap().label = "Studio model 4999".into();
    install(h.state_mut(), replacement);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.get_by_role_and_label(Role::TextInput, "Filter options").accesskit_node().value().as_deref(), Some("model 4999"));
    h.get_by_label("Studio model 4999").click();
    h.run();
    assert_eq!(stored(h.state()), Some(Value::Text("checkpoint-2000.safetensors".into())));
}

#[test]
fn a_long_newly_visible_label_does_not_change_popup_width() {
    let mut options = choices();
    options.last_mut().unwrap().label = format!("Special {}", "W".repeat(1024));
    let mut h = harness(scene(None, options));
    open(&mut h);
    let before = h.get_by_role_and_label(Role::TextInput, "Filter options").rect().width();
    type_filter(&mut h, "Special");
    let after = h.get_by_role_and_label(Role::TextInput, "Filter options").rect().width();
    assert!((before - after).abs() <= 0.5, "popup width changed from {before} to {after}");
}

#[test]
fn an_empty_known_list_can_clear_a_stale_value() {
    let mut h = harness(scene(Some("removed.safetensors"), Vec::new()));
    open(&mut h);
    h.get_by_label("Use default").click();
    h.run();
    assert_eq!(stored(h.state()), None);
}

#[test]
fn default_without_an_override_is_disabled_and_a_choice_named_default_is_distinct() {
    let mut h = harness(scene(None, vec![Choice { value: "provider-default".into(), label: "Default".into() }]));
    let before = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let can_undo = h.state().session.can_undo();
    open(&mut h);
    let reset = h.get_by_label("Use default");
    assert!(reset.accesskit_node().is_disabled());
    reset.click();
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().session.revision(), revision, "a disabled reset never dirties the document");
    assert_eq!(h.state().session.can_undo(), can_undo, "no empty undo step");
    h.get_by_label("Default").click();
    h.run();
    assert_eq!(stored(h.state()), Some(Value::Text("provider-default".into())));
    open(&mut h);
    assert!(!h.get_by_label("Use default").accesskit_node().is_disabled());
    h.get_by_label("Use default").click();
    h.run();
    assert_eq!(stored(h.state()), None);
}

#[test]
#[ignore = "manual public-safe before/after capture; NODECRAFT_PICKER_EVIDENCE and optional NODECRAFT_PICKER_EVIDENCE_FILTER"]
fn render_option_picker_evidence() {
    let path = std::path::PathBuf::from(std::env::var_os("NODECRAFT_PICKER_EVIDENCE").unwrap());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut h = Harness::builder()
        .wgpu()
        .with_size(egui::Vec2::new(1400.0, 1000.0))
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), scene(Some("checkpoint-4999.safetensors"), choices()));
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    open(&mut h);
    if let Ok(query) = std::env::var("NODECRAFT_PICKER_EVIDENCE_FILTER") {
        type_filter(&mut h, &query);
    }
    h.render().unwrap().save(path).unwrap();
}
