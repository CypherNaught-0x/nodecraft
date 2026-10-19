//! The rest of the M1 exit criterion (#65): a 200-node graph built through commands is saved to a
//! `.ncraft` file, every step of building it undoes back to the start, and the file reopens
//! identical.
use super::super::storage::StorageSetup;
use super::*;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn storage_app(dir: &std::path::Path, recovery: &str, open: Option<std::path::PathBuf>) -> NodeCraftApp {
    NodeCraftApp::with_services_and_storage(Default::default(), StorageSetup { recovery: dir.join(recovery), open }).unwrap()
}

fn storage_harness(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    Harness::builder().with_size(Vec2::new(1600.0, 1000.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}

#[test]
fn a_200_node_graph_saves_undoes_to_the_start_and_reopens_identical() {
    let dir = std::env::temp_dir().join(format!("nodecraft-perf-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let scratch = Scratch(dir);
    let file = scratch.0.join("two-hundred.ncraft");
    let mut h = storage_harness(storage_app(&scratch.0, "recovery", None));
    let canvas = h.state().canvas;
    let original = h.state().session.document().clone();
    let id = |n: u32| NodeId::from_bytes(u128::from(n + 1000).to_be_bytes());
    // 20 rows of a Text node feeding a chain of 9 routers: 200 creates and 180 wires.
    let dispatch = |h: &mut Harness<'static, NodeCraftApp>, command: &str, params: serde_json::Value| {
        h.state_mut().session.dispatch(command, &params).unwrap();
    };
    for n in 0..200u32 {
        let kind = if n % 10 == 0 { "source.text" } else { "structure.router" };
        let pos = json!({"x": (n % 10) as f32 * 400.0, "y": (n / 10) as f32 * 320.0});
        dispatch(&mut h, "node.create", json!({"canvas": canvas, "id": id(n), "kind": kind, "pos": pos}));
        if n % 10 != 0 {
            let edge = nodecraft_graph::Edge {
                id: nodecraft_types::EdgeId::from_bytes(u128::from(n + 5000).to_be_bytes()),
                from: nodecraft_graph::Endpoint { node: id(n - 1), port: "out".into() },
                to: nodecraft_graph::Endpoint { node: id(n), port: "in".into() },
                order: 0,
            };
            dispatch(&mut h, "edge.connect", json!({"canvas": canvas, "edge": edge}));
        }
    }
    for n in (0..200u32).step_by(10) {
        let value = json!({"type": "text", "value": format!("Row {}: a quiet forest at dawn.", n / 10)});
        dispatch(&mut h, "node.param_set", json!({"canvas": canvas, "id": id(n), "name": "text", "value": value}));
    }
    h.run();
    let built = h.state().session.document().canvases().get(&canvas).unwrap();
    assert_eq!((built.nodes().len(), built.edges().len()), (original.canvases().get(&canvas).unwrap().nodes().len() + 200, 180));

    let started = Instant::now();
    dispatch(&mut h, "file.save_as", json!({"location": file.to_str().unwrap()}));
    let save_as = started.elapsed();
    assert!(!h.state().session.file_status().dirty);
    // One more edit, saved with the shortcut as a user would.
    let pos = json!({"x": 40.0, "y": 0.0});
    dispatch(&mut h, "node.move", json!({"canvas": canvas, "id": id(0), "pos": pos}));
    h.run();
    assert!(h.state().session.file_status().dirty);
    let started = Instant::now();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    h.step();
    // The frame only snapshots the document; the file work finishes on its own thread (#461).
    let save = started.elapsed();
    let mut frames = 0;
    while h.state().session.file_status().saving {
        std::thread::sleep(std::time::Duration::from_millis(1));
        h.step();
        frames += 1;
        assert!(frames <= 10_000, "the background save never finishes");
    }
    let written = started.elapsed();
    assert!(h.state().notice.is_none(), "the save went through: {:?}", h.state().notice);
    assert!(!h.state().session.file_status().dirty, "Cmd+S saves to the same file");
    let saved = h.state().session.document().clone();

    // 200 creates + 180 wires + 20 edits + 1 move.
    let mut steps = 0;
    while h.state().session.can_undo() {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.step();
        steps += 1;
        assert!(steps <= 1000, "undo never ends");
    }
    h.run();
    assert_eq!(steps, 401);
    assert_eq!(h.state().session.document(), &original);

    // The file still holds the whole graph. The project is single-writer, so close this window
    // first, as a user would before opening it elsewhere.
    drop(h);
    let (_, _, _, open_error) = NodeCraftApp::storage_session(StorageSetup { recovery: scratch.0.join("recovery-check"), open: Some(file.clone()) }).unwrap();
    assert!(open_error.is_none(), "the saved file reopens: {open_error:?}");
    let started = Instant::now();
    let reopened = storage_harness(storage_app(&scratch.0, "recovery-reopen", Some(file.clone())));
    let open = started.elapsed();
    assert_eq!(reopened.state().session.document(), &saved, "the file holds the whole graph");
    eprintln!(
        "200 nodes, 180 wires: .ncraft {} bytes; save as {save_as:?}, Cmd+S frame {save:?} (written after {written:?}), open {open:?}",
        std::fs::metadata(&file).unwrap().len()
    );
}
