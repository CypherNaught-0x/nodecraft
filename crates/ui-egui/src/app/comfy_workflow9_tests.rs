//! The ComfyUI workflow card (#9): a card placed from an imported workflow shows the ports and
//! parameters its workflow exposes, from the generated body.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_nodes::comfy_workflow::{self, Card, MappedInput, MappedOutput, MappedParam, Mapping};
use nodecraft_types::{Modality, ParamControl, ParamSpec};

const C: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([9; 16]);
const CARD: NodeId = NodeId::from_bytes([9; 16]);

/// What importing a small image-to-image workflow stores: its image loader as a port, its prompt
/// as a parameter and its Save Image node as the result.
fn card() -> Card {
    let workflow = r#"{
      "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
      "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a red fox"}},
      "3": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "NodeCraft"}}
    }"#;
    let prompt = ParamSpec {
        name: "prompt".into(),
        label: "Workflow prompt".into(),
        description: None,
        control: ParamControl::Text { multiline: true, max_bytes: 4096 },
        required: false,
        default: Some(Value::Text("a red fox".into())),
        visible_if: None,
    };
    Card {
        workflow: workflow.into(),
        mapping: Mapping {
            inputs: vec![MappedInput { node: "1".into(), input: "image".into(), port: "photo".into(), modality: Modality::Image }],
            params: vec![MappedParam { node: "2".into(), input: "text".into(), spec: prompt }],
            outputs: vec![MappedOutput { node: "3".into(), label: "Save Image".into(), modality: Modality::Image }],
            partners: Vec::new(),
            output_names: Vec::new(),
        },
    }
}

pub(super) fn app() -> NodeCraftApp {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([9; 16]), "Workflow").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds.get(comfy_workflow::ID).unwrap().instantiate_instance(CARD, card().config().unwrap(), &Default::default()).unwrap();
    node.pos = nodecraft_graph::Position { x: 150.0, y: 100.0 };
    nodecraft_graph::Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    NodeCraftApp::from_session(session, C).unwrap()
}

#[test]
fn a_workflow_card_shows_what_its_workflow_exposes() {
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    h.state_mut().selection.only(CARD);
    h.run();
    assert!(h.query_by_label("Workflow prompt").is_some(), "the exposed prompt is a parameter");
    assert!(h.query_by_label("Model").is_none(), "its model is fixed, so the card doesn't show it");
    let app = h.state();
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    assert_eq!(node.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["photo"]);
    assert_eq!(node.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["image"]);
}

#[test]
#[ignore = "manual public-safe capture; COMFY9_RENDER_DIR"]
fn comfy_workflow9_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("COMFY9_RENDER_DIR").unwrap());
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    h.state_mut().selection.only(CARD);
    h.run();
    h.render().unwrap().save(dir.join("comfy-workflow9-card.png")).unwrap();
}

fn template948_app(curated: bool) -> NodeCraftApp {
    let mut app = app();
    let mut catalog =
        nodecraft_engine::comfy_workflow_cmds::NodeCatalog::parse(include_bytes!("../../../comfy/src/templates/fixtures/catalog-facts.json")).unwrap().0;
    catalog.extend(
        nodecraft_engine::comfy_workflow_cmds::NodeCatalog::parse(include_bytes!("../../../comfy/src/templates/fixtures/mock-core-nodes.json")).unwrap().0,
    );
    nodecraft_engine::comfy_workflow_cmds::finish(&mut app.session, 1, Ok(std::sync::Arc::new(catalog).into()));
    let mut workflow: serde_json::Value = serde_json::from_str(include_str!("../../../comfy/src/templates/fixtures/image_sdxl_simple.json")).unwrap();
    if !curated {
        // An extra frontend-only note keeps the runnable graph identical while selecting
        // ordinary discovery. The capture compares generic and curated import choices.
        workflow["nodes"].as_array_mut().unwrap().push(json!({"id":100000,"type":"Note","widgets_values":[""]}));
    }
    app.session.dispatch("node.remove", &json!({"canvas":C,"id":CARD})).unwrap();
    app.session
        .dispatch(
            "comfy.workflow.import",
            &json!({"canvas":C,"id":CARD,"pos":{"x":130,"y":85},"title":"SDXL image workflow","workflow":serde_json::to_string(&workflow).unwrap()}),
        )
        .unwrap();
    app.selection.only(CARD);
    app
}

#[test]
fn templates948_imported_template_card_uses_curated_prompt_and_size_labels() {
    let mut h = Harness::builder().with_size(Vec2::new(1100.0, 950.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), template948_app(true));
    h.run();
    for label in ["Prompt", "Negative prompt", "Width", "Height", "Seed", "Steps", "Guidance"] {
        assert!(h.query_all_by_label(label).next().is_some(), "curated card field {label}");
    }
}

#[test]
#[ignore = "manual public-safe generic/curated import capture; NODECRAFT_TEMPLATE948_CAPTURE"]
fn templates948_visual_evidence() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_TEMPLATE948_CAPTURE").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "dark"), (crate::theme::ThemeKind::Light, "light")] {
        for curated in [false, true] {
            let mut app = template948_app(curated);
            app.theme = theme;
            let mut h = Harness::builder().wgpu().with_size(Vec2::new(1100.0, 950.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
            crate::theme::apply(&h.ctx, theme);
            h.run();
            h.render().unwrap().save(root.join(format!("{name}-{}.png", if curated { "curated" } else { "generic" }))).unwrap();
        }
    }
}

fn dynamic948_app() -> NodeCraftApp {
    let mut app = app();
    let info=br#"{
      "Dynamic":{"input":{"required":{"model":["COMFY_DYNAMICCOMBO_V3",{"options":[{"key":"small","inputs":{"required":{"resolution":[["64","128"],{}]}}}]}]}},"output":["IMAGE"]},
      "SaveImage":{"input":{"required":{"images":["IMAGE",{}],"filename_prefix":["STRING",{}]}},"output":[],"output_node":true}
    }"#;
    nodecraft_engine::comfy_workflow_cmds::finish(
        &mut app.session,
        1,
        Ok(std::sync::Arc::new(nodecraft_engine::comfy_workflow_cmds::NodeCatalog::parse(info).unwrap().0).into()),
    );
    app.session.dispatch("node.remove", &json!({"canvas":C,"id":CARD})).unwrap();
    let flow = r#"{"1":{"class_type":"Dynamic","inputs":{"model":"small","model.resolution":"64"}},"2":{"class_type":"SaveImage","inputs":{"images":["1",0],"filename_prefix":"NodeCraft"}}}"#;
    app.session
        .dispatch("comfy.workflow.import", &json!({"canvas":C,"id":CARD,"pos":{"x":150,"y":100},"title":"Dynamic image workflow","workflow":flow}))
        .unwrap();
    app.selection.only(CARD);
    app
}
#[test]
fn templates948_dynamic_child_is_a_generated_editable_card_control() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut state = dynamic948_app();
        state.theme = theme;
        let mut h = Harness::builder().with_size(Vec2::new(1100.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), state);
        crate::theme::apply(&h.ctx, theme);
        h.run();
        assert!(h.query_by_label("Dynamic · model.resolution").is_some());
        let node = &h.state().session.document().canvases()[&C].nodes()[&CARD];
        let mapping = comfy_workflow::mapping(node).unwrap();
        assert!(mapping.params.iter().any(|p| p.input == "model.resolution" && matches!(p.spec.control, ParamControl::Choice { .. })));
    }
}
#[test]
#[ignore = "manual public-safe dynamic input capture; NODECRAFT_TEMPLATE948_DYNAMIC_CAPTURE"]
fn templates948_dynamic_visual_evidence() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_TEMPLATE948_DYNAMIC_CAPTURE").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "dark"), (crate::theme::ThemeKind::Light, "light")] {
        let mut app = dynamic948_app();
        app.theme = theme;
        let mut h = Harness::builder().wgpu().with_size(Vec2::new(1100.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        h.run();
        h.render().unwrap().save(root.join(format!("dynamic-{name}.png"))).unwrap();
    }
}
