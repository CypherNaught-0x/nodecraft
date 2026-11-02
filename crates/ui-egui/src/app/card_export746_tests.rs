//! Export to folder cards (#746): a person chooses the folder on the card, and Export writes the
//! wired results there off the UI thread. Pointer events drive the real card buttons.
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};

const IMAGE: NodeId = NodeId::from_bytes([0x74; 16]);
const CARD: NodeId = NodeId::from_bytes([0x75; 16]);
const RESULT: nodecraft_types::GenerationId = nodecraft_types::GenerationId::from_bytes([0x76; 16]);

fn png() -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[90; 64], &Default::default()).unwrap()
}

/// An image card with one result, wired into an Export to folder card.
fn scene() -> Harness<'static, NodeCraftApp> {
    let canvas = CanvasId::from_bytes([0x77; 16]);
    let bytes = png();
    let hash = Session::new(Project::new(ProjectId::from_bytes([0x78; 16]), "Scratch").unwrap()).unwrap().import_asset(&bytes).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0x79; 16]), "Export card").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let kinds = Session::new(project.clone()).unwrap().kinds().clone();
    let mut image = kinds.get("generation.image").unwrap().descriptor().instantiate(IMAGE, &BTreeMap::new()).unwrap();
    image.title = "hero".into();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    image.history.push(nodecraft_graph::Generation::new(RESULT, vec![Value::Media(asset)]));
    image.active = Some(RESULT);
    let out = image.outputs.first().unwrap().name.clone();
    image.pos = nodecraft_graph::Position { x: -420.0, y: -200.0 };
    let mut card = kinds.get(nodecraft_nodes::export::ID).unwrap().instantiate_instance(CARD, Default::default(), &Default::default()).unwrap();
    card.pos = nodecraft_graph::Position { x: 40.0, y: -200.0 };
    Change::AddNode { canvas, node: image }.apply(&mut project).unwrap();
    Change::AddNode { canvas, node: card }.apply(&mut project).unwrap();
    let edge = nodecraft_graph::Edge {
        id: nodecraft_types::EdgeId::from_bytes([0x7a; 16]),
        from: nodecraft_graph::Endpoint { node: IMAGE, port: out },
        to: nodecraft_graph::Endpoint { node: CARD, port: "image".into() },
        order: 0,
    };
    Change::Connect { canvas, edge }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    session.import_asset(&bytes).unwrap();
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let builder = Harness::builder().with_size(Vec2::new(1200.0, 800.0));
    let builder = if std::env::var_os("NODECRAFT_CARD_EXPORT746_EVIDENCE").is_some() { builder.wgpu() } else { builder };
    let mut h = builder.build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

/// A real pointer hovers, presses and releases the button labelled `label`.
fn press(h: &mut Harness<'static, NodeCraftApp>, label: &str) {
    let pos = h.get_by_label(label).rect().center();
    h.hover_at(pos);
    h.run();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    // A progress window repaints until its export ends, so step rather than run to idle.
    h.run_steps(2);
}

fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn evidence(h: &mut Harness<'static, NodeCraftApp>, name: &str) {
    if let Some(dir) = std::env::var_os("NODECRAFT_CARD_EXPORT746_EVIDENCE") {
        h.run();
        let image = h.render().unwrap();
        image.save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
    }
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
#[test]
fn card746_a_person_chooses_the_folder_and_export_writes_there() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let mut h = scene();
    let before = h.state().session.document().clone();
    h.get_by_label("No folder chosen yet");
    assert!(h.get_by_label("Export").accesskit_node().is_disabled(), "nothing to export into yet");
    evidence(&mut h, "1-no-folder");

    // Dismissing the folder dialog grants nothing.
    h.state_mut().exports.answer = None;
    press(&mut h, "Choose folder…");
    export_tests::finish(&mut h);
    assert!(h.get_by_label("Export").accesskit_node().is_disabled());

    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Renders");
    std::fs::create_dir(&folder).unwrap();
    h.state_mut().exports.answer = Some(folder.clone());
    press(&mut h, "Choose folder…");
    export_tests::finish(&mut h);
    h.get_by_label("Folder: Renders");
    assert!(!h.get_by_label("Export").accesskit_node().is_disabled());
    evidence(&mut h, "2-folder-chosen");
    // The grant is view state: the project holds no path and no edit was made.
    assert_eq!(h.state().session.document(), &before);
    let saved = serde_json::to_string(h.state().session.document()).unwrap();
    assert!(!saved.contains("Renders"), "the chosen folder never reaches the project");

    press(&mut h, "Export");
    export_tests::finish(&mut h);
    assert_eq!(files_in(&folder), ["hero-out-1.png"], "{:?}", h.state().notice);
    assert_eq!(std::fs::read(folder.join("hero-out-1.png")).unwrap(), png());
    assert_eq!(h.state().notice.as_deref(), Some("Exported 1 file to Renders."));
    assert_eq!(h.state().session.document(), &before, "an export is no edit");
    assert!(!h.state().session.can_undo(), "and no undo step");
    evidence(&mut h, "3-exported");

    // Again: the default policy keeps the file there, and says so.
    press(&mut h, "Export");
    export_tests::finish(&mut h);
    assert_eq!(files_in(&folder), ["hero-out-1.png"]);
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("1 kept")), "{:?}", h.state().notice);
}

#[test]
fn card746_programmatic_callers_cannot_grant_or_open_a_dialog() {
    let mut h = scene();
    let canvas = h.state().canvas;
    let dir = tempfile::tempdir().unwrap();
    // What the CLI, MCP or the agent would send: no host, so nothing is written and no dialog opens.
    let error = h.state_mut().session.dispatch("export.node_run", &json!({"canvas": canvas, "node": CARD})).unwrap_err();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::Unsupported);
    let error = h.state_mut().session.dispatch("export.node_run", &json!({"canvas": canvas, "node": CARD, "destination": dir.path()})).unwrap_err();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::InvalidInput);
    // A parameter edit cannot plant a folder either: the card has no destination parameter.
    let error = h
        .state_mut()
        .session
        .dispatch("node.param_set", &json!({"canvas": canvas, "id": CARD, "name": "destination", "value": {"type":"text","value": dir.path()}}));
    assert!(error.is_err());
    h.run();
    assert!(!h.state().export_pending(), "no dialog or export was started");
    assert!(h.state().card_export_folder(CARD).is_none());
    assert!(files_in(dir.path()).is_empty());
}

#[test]
fn card746_the_card_draws_its_schema_parameters() {
    let mut h = scene();
    for label in ["File names", "If a file exists", "Format", "Subfolder"] {
        h.get_by_label(label);
    }
    // The run planner never offers to run it.
    let canvas = h.state().canvas;
    let error = h.state_mut().session.dispatch("run.quote", &json!({"canvas": canvas, "mode": "selected", "nodes": [CARD]}));
    assert!(error.is_err() || error.as_ref().is_ok_and(|quote| !quote.to_string().contains(&CARD.to_string())), "{error:?}");
    h.run();
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
#[test]
fn card746_replacing_existing_files_needs_the_persons_yes() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let mut h = scene();
    let canvas = h.state().canvas;
    // Anyone who edits the project (an agent too) can set the card to replace files.
    h.state_mut()
        .session
        .dispatch("node.param_set", &json!({"canvas": canvas, "id": CARD, "name": "on_collision", "value": {"type":"text","value":"overwrite"}}))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Renders");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("hero-out-1.png"), b"the person's own file").unwrap();
    h.state_mut().exports.answer = Some(folder.clone());
    press(&mut h, "Choose folder…");
    export_tests::finish(&mut h);

    press(&mut h, "Export");
    h.get_by_label("Replace existing files?");
    h.get_by_label("1 file in Renders will be replaced by this export.");
    evidence(&mut h, "4-confirm-replace");
    assert_eq!(std::fs::read(folder.join("hero-out-1.png")).unwrap(), b"the person's own file", "nothing is written before the answer");
    press(&mut h, "Cancel");
    export_tests::finish(&mut h);
    assert!(h.query_by_label("Replace existing files?").is_none());
    assert_eq!(std::fs::read(folder.join("hero-out-1.png")).unwrap(), b"the person's own file", "Cancel keeps the file");

    press(&mut h, "Export");
    press(&mut h, "Replace files");
    export_tests::finish(&mut h);
    assert_eq!(std::fs::read(folder.join("hero-out-1.png")).unwrap(), png(), "{:?}", h.state().notice);
    assert_eq!(files_in(&folder), ["hero-out-1.png"]);
}
