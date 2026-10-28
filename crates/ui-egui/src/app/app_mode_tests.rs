//! App mode (#95): a technique node's form, driven through real pointer and key events.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_types::TechniqueId;

const V1: CanvasId = CanvasId::from_bytes([0x71; 16]);
const PLACED: NodeId = NodeId::from_bytes([0x50; 16]);
const TEXT: NodeId = NodeId::from_bytes([0x61; 16]);
const SWAP: NodeId = NodeId::from_bytes([0x62; 16]);
const IMAGE: NodeId = NodeId::from_bytes([0x63; 16]);
const FLIP: NodeId = NodeId::from_bytes([0x65; 16]);

fn add(app: &mut NodeCraftApp, id: NodeId, kind: &str, params: &[(&str, &str)]) {
    let params = params.iter().map(|(name, value)| ((*name).to_owned(), Value::Text((*value).into()))).collect();
    let node = app.session.kinds().get(kind).unwrap().instantiate_instance(id, Default::default(), &params).unwrap();
    app.session.dispatch("canvas.paste", &json!({"canvas": app.canvas, "nodes": [node]})).unwrap();
}

/// The headless app with "Swap" placed as PLACED: input Brief (text 0x61 "a cat") → replace
/// (cat → dog) as output Swapped, Find exposed, and image input Photo → flip as Flipped.
fn with_app() -> Harness<'static, NodeCraftApp> {
    swap(harness())
}

fn swap(mut h: Harness<'static, NodeCraftApp>) -> Harness<'static, NodeCraftApp> {
    let app = h.state_mut();
    let canvas = app.canvas;
    add(app, TEXT, "source.text", &[("text", "a cat")]);
    add(app, SWAP, "text.replace", &[("find", "cat"), ("replacement", "dog")]);
    add(app, IMAGE, "source.image", &[]);
    add(app, FLIP, "image.flip", &[]);
    let example = nodecraft_engine::import::import_media(&mut app.session, &nodecraft_engine::import::Incoming { name: "example.png", bytes: &png() }).unwrap();
    app.session.dispatch("node.param_set", &json!({"canvas": canvas, "id": IMAGE, "name": "asset", "value": Value::Media(example)})).unwrap();
    for (id, from, to, port) in [(0x64, TEXT, SWAP, "text"), (0x66, IMAGE, FLIP, "image")] {
        let edge = json!({"id": EdgeId::from_bytes([id; 16]), "from": {"node": from, "port": "out"}, "to": {"node": to, "port": port}, "order": 0});
        app.session.dispatch("edge.connect", &json!({"canvas": canvas, "edge": edge})).unwrap();
    }
    let publish = json!({"canvas": canvas, "published": V1, "seed": NodeId::from_bytes([40; 16]), "technique": TechniqueId::from_bytes([0x7e; 16]),
        "name": "Swap", "inputs": [{"node": TEXT, "name": "Brief"}, {"node": IMAGE, "name": "Photo"}], "outputs": [{"node": SWAP, "name": "Swapped"}, {"node": FLIP, "name": "Flipped"}],
        "params": [{"node": SWAP, "param": "find", "name": "Find"}]});
    app.session.dispatch("technique.publish", &publish).unwrap();
    app.session.dispatch("technique.place", &json!({"canvas": canvas, "version": V1, "id": PLACED, "pos": {"x": 260.0, "y": -110.0}})).unwrap();
    h.run();
    h
}

fn png() -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[200; 64], &Default::default()).unwrap()
}

fn open(h: &mut Harness<'static, NodeCraftApp>) {
    h.state_mut().selection.only(PLACED);
    // The top bar grows to fit the button, then settles.
    h.run();
    h.run();
    crate::app::topbar_tests::reveal(h, "Open as app");
    h.get_by_label("Open as app").click();
    h.run();
    assert!(h.state().app_mode.is_some(), "{:?}", h.state().notice);
}

fn form(h: &mut Harness<'static, NodeCraftApp>) -> serde_json::Value {
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("app.form", &json!({"canvas": canvas, "id": PLACED})).unwrap()
}

/// Types `text` over the field labelled `label`, and commits it with Command+Enter. The form
/// is drawn over the canvas, so its field is the last one with that label.
fn retype(h: &mut Harness<'static, NodeCraftApp>, label: &str, text: &str) {
    h.query_all_by_role_and_label(egui::accesskit::Role::TextInput, label).last().unwrap().click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text(text.into()));
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    h.run();
}

#[test]
fn the_form_shows_the_examples_and_typing_fills_an_input_as_one_undo_step() {
    let mut h = with_app();
    open(&mut h);
    assert!(h.query_by_label("Swap").is_some());
    assert!(h.query_by_label("Brief").is_some());
    assert!(h.query_by_label("Settings").is_some());
    assert!(h.query_by_label("Choose a file…").is_some(), "the image input takes a file");
    assert_eq!(h.query_all_by_label("The technique's example, until you change it").count(), 2, "both inputs show their example");

    retype(&mut h, "Brief", "a fox");
    let form = form(&mut h);
    assert_eq!(form["inputs"][0]["feed"], json!("form"), "{:?}", h.state().notice);
    assert_eq!(form["inputs"][0]["values"], json!([{"type": "text", "value": "a fox"}]));
    // The source sits on the canvas too, wired to the node.
    let canvas = h.state().canvas;
    let source: NodeId = serde_json::from_value(form["inputs"][0]["sources"][0].clone()).unwrap();
    assert!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().contains_key(&source));

    // Undo from the form takes the whole fill back.
    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(self::form(&mut h)["inputs"][0]["feed"], json!("example"));
    assert!(h.state().app_mode.is_some(), "the form stays open");
}

#[test]
fn a_setting_changes_on_the_node_and_resets() {
    let mut h = with_app();
    open(&mut h);
    retype(&mut h, "Find", "fox");
    let canvas = h.state().canvas;
    let params =
        |h: &Harness<'static, NodeCraftApp>| h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&PLACED).unwrap().params.clone();
    assert_eq!(params(&h).get("find"), Some(&Value::Text("fox".into())), "{:?}", h.state().notice);
    h.get_by_label("Reset").click();
    h.run();
    assert_eq!(params(&h).get("find"), None, "back to the technique's own value");
}

#[test]
fn a_chosen_image_fills_its_input_and_the_example_comes_back() {
    let mut h = with_app();
    open(&mut h);
    let png = nodecraft_media::encode_png(2, 2, &[90; 16], &Default::default()).unwrap();
    h.state_mut().app_file_chosen("photo", None, "photo.png", &png);
    h.run();
    let form = form(&mut h);
    let photo = &form["inputs"][1];
    assert_eq!((photo["feed"].clone(), photo["values"][0]["type"].clone()), (json!("form"), json!("media")), "{:?}", h.state().notice);
    h.get_by_label("Use the example").click();
    h.run();
    assert_eq!(self::form(&mut h)["inputs"][1]["feed"], json!("example"));

    // Text is not media: nothing changes and the person is told why.
    h.state_mut().app_file_chosen("photo", None, "notes.txt", b"just words");
    h.run();
    assert_eq!(self::form(&mut h)["inputs"][1]["feed"], json!("example"));
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("notes.txt")));
}

#[test]
fn run_asks_for_the_price_and_done_or_escape_closes() {
    // A catalog, budgets and a host that takes runs, so a run can be quoted.
    use nodecraft_engine::run_cmds::{BudgetState, RunCatalog};
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([83; 16]), "App mode").unwrap();
    let canvas = CanvasId::from_bytes([81; 16]);
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(RunCatalog::new(nodecraft_catalog::Catalog::from_toml("").unwrap()).unwrap());
    resources.insert(BudgetState { budgets: Default::default(), spent: Default::default() });
    let mut services = nodecraft_engine::commands::Services::default();
    services.register_run_host(|_| Ok(json!({"accepted": true}))).unwrap();
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let session = nodecraft_engine::Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let app = NodeCraftApp::from_session(session.with_resources(resources).with_services(services), canvas).unwrap();
    let h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    let mut h = swap(h);
    open(&mut h);
    assert!(h.query_by_label("No charge").is_some(), "the form shows what a run costs");
    h.get_by_label("Run").click();
    h.run();
    assert!(h.state().run_confirm.is_some(), "the price is shown first: {:?}", h.state().notice);
    // Escape cancels the price dialog only; the form stays open under it.
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().run_confirm.is_none());
    assert!(h.state().app_mode.is_some());
    h.get_by_label("Done").click();
    h.run();
    assert!(h.state().app_mode.is_none());
    open(&mut h);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().app_mode.is_none());
}

#[test]
fn the_technique_menu_opens_the_form_and_it_closes_when_the_node_goes() {
    let mut h = with_app();
    h.state_mut().technique_action(technique_menu::TechniqueAction::App(PLACED));
    h.run();
    assert!(h.query_by_label("Brief").is_some());
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.remove", &json!({"canvas": canvas, "id": PLACED})).unwrap();
    h.run();
    assert!(h.state().app_mode.is_none());
}

#[test]
#[ignore = "manual public-safe capture; APP95_RENDER_DIR"]
fn app95_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("APP95_RENDER_DIR").unwrap());
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "app95-dark.png"), (crate::theme::ThemeKind::Light, "app95-light.png")] {
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        let h = Harness::builder().wgpu().with_size(Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let mut h = swap(h);
        open(&mut h);
        retype(&mut h, "Brief", "a fox in the snow");
        let (w, h_px) = (320u32, 200u32);
        let pixels: Vec<u8> = (0..h_px).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h_px) as u8, 170, 255])).collect();
        let png = nodecraft_media::encode_png(w, h_px, &pixels, &Default::default()).unwrap();
        h.state_mut().app_file_chosen("photo", None, "gradient.png", &png);
        // Thumbnails decode off the frame; give them time.
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            h.step();
        }
        h.render().unwrap().save(dir.join(name)).unwrap();
    }
}
