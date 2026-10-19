use super::*;

#[test]
fn stale_palette_action_is_available_without_empty_params() {
    let h = super::tests::harness();
    let app = h.state();
    assert!(app.session.enabled("canvas.stale", &json!({"canvas":app.canvas})).is_ok());
    assert!(
        app.palette_entries().iter().any(|entry| entry.target == palette::Target::Command("canvas.stale")),
        "the existing command must be available with active canvas context"
    );
}

use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_graph::{Edge, Endpoint, Generation, Port};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{Blake3, EdgeId, GenerationId, Modality, PortType};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn ui(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}
fn open(h: &mut Harness<'static, NodeCraftApp>, query: &str) {
    h.run();
    for _ in 0..40 {
        h.step();
    }
    let pos = Pos2::new(850.0, 100.0);
    h.hover_at(pos);
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
        }
    }
    h.run();
    assert!(h.state().palette.is_some(), "opening {query}; snapshot={} notice={:?}", h.state().stale_snapshot.is_some(), h.state().notice);
    h.event(egui::Event::Text(query.into()));
    h.run();
}
fn check(h: &mut Harness<'static, NodeCraftApp>) {
    open(h, "List stale results");
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().palette.is_none());
}
fn visible(h: &Harness<'static, NodeCraftApp>) -> bool {
    h.query_by_label_contains("Last staleness check:").is_some()
}
struct Kind {
    descriptor: NodeDescriptor,
    source: bool,
    rejected: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}
impl NodeKind for Kind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn validate_instance(&self, node: &nodecraft_graph::Node, _: bool) -> Result<nodecraft_nodes::ResolvedParams, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.source && self.rejected.load(Ordering::SeqCst) {
            Err(Error::new(ErrorKind::Unsupported, "Choose a supported source"))
        } else {
            self.descriptor.params.resolve(&node.params, false)
        }
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}
fn diagnostic() -> (NodeCraftApp, Arc<AtomicUsize>, Arc<AtomicBool>) {
    diagnostic_with_downstream(false)
}
fn diagnostic_with_downstream(downstream: bool) -> (NodeCraftApp, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let rejected = Arc::new(AtomicBool::new(!downstream));
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let port = |name: &str| Port { name: name.into(), port_type: PortType { modality: Modality::Text, many: false } };
    for (name, source) in [("test.stale_source", true), ("test.stale_consumer", false)] {
        kinds
            .register(Box::new(Kind {
                descriptor: NodeDescriptor {
                    id: name.into(),
                    label: "Public fixture".into(),
                    category: if source { Category::Source } else { Category::Generation },
                    inputs: if source { vec![] } else { vec![port("in")] },
                    outputs: vec![port("out")],
                    params: Default::default(),
                    quick_add: None,
                },
                source,
                rejected: rejected.clone(),
                calls: calls.clone(),
            }))
            .unwrap();
    }
    let mut project = Project::new(ProjectId::from_bytes([4; 16]), "Public diagnostics").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let source = kinds.get("test.stale_source").unwrap().descriptor().instantiate(id(1), &Default::default()).unwrap();
    Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
    for n in [20, 21] {
        let mut node = kinds.get("test.stale_consumer").unwrap().descriptor().instantiate(id(n), &Default::default()).unwrap();
        node.pos = nodecraft_graph::Position { x: f32::from(n) * 30.0, y: 200.0 };
        let generation = GenerationId::from_bytes([n; 16]);
        node.history.push(Generation {
            id: generation,
            outputs: vec![Value::Text("Result".into())],
            input_hash: if downstream && n == 21 { None } else { Some(Blake3::from_bytes([0; 32])) },
        });
        node.active = Some(generation);
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        Change::Connect {
            canvas: C,
            edge: Edge {
                id: EdgeId::from_bytes([n; 16]),
                from: Endpoint { node: if downstream && n == 21 { id(20) } else { id(1) }, port: "out".into() },
                to: Endpoint { node: id(n), port: "in".into() },
                order: 0,
            },
        }
        .apply(&mut project)
        .unwrap();
    }
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    (NodeCraftApp::from_session(session, C).unwrap(), calls, rejected)
}
#[test]
fn actual_palette_report_preserves_state_and_shared_cause_count_without_rehashing() {
    let (app, calls, rejected) = diagnostic();
    let mut h = ui(app);
    h.run();
    calls.store(0, Ordering::SeqCst);
    for _ in 0..20 {
        let _ = h.state().palette_entries();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let report = h.state_mut().session.dispatch("canvas.stale", &json!({"canvas":C})).unwrap();
    assert_eq!(report["unknown_total"], 2);
    let query_calls = calls.swap(0, Ordering::SeqCst);
    assert_eq!(query_calls, 3);
    let document = h.state().session.document().clone();
    let rev = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    let file_status = h.state().session.file_status();
    let status_revision = h.state().session.status_revision();
    let selection = h.state().selection.clone();
    let view = h.state().view;
    h.state_mut().notice = Some("Existing notice".into());
    open(&mut h, "List stale results");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "opening/filtering must not hash");
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(calls.load(Ordering::SeqCst), query_calls, "one query, no formatting hooks");
    assert!(visible(&h));
    h.get_by_label_contains("0 stale results; 2 checked results");
    h.get_by_label_contains(&format!("First invalid checked result: {}", id(20)));
    h.get_by_label_contains(&format!("Failing input node: {}", id(1)));
    h.get_by_label_contains("unsupported:");
    h.get_by_label("Existing notice");
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), rev);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
    assert_eq!(h.state().session.file_status(), file_status);
    assert_eq!(h.state().session.status_revision(), status_revision);
    assert_eq!(h.state().selection, selection);
    assert_eq!(h.state().view, view);
    for _ in 0..5 {
        h.step();
    }
    h.state_mut().view.pan += Vec2::new(20.0, 0.0);
    h.run();
    assert!(visible(&h));
    assert_eq!(calls.load(Ordering::SeqCst), query_calls);
    rejected.store(false, Ordering::SeqCst);
    h.step();
    assert!(visible(&h), "historical check is not a registry certification");
    check(&mut h);
    h.get_by_label_contains("2 stale results; 0 checked results");
}
#[test]
fn same_frame_palette_undo_and_real_drop_do_not_paint_previous_summary() {
    let mut h = super::tests::harness();
    let canvas = h.state().canvas;
    let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().values().next().unwrap().id;
    h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":node,"title":"Changed"})).unwrap();
    h.run();
    check(&mut h);
    assert!(visible(&h));
    open(&mut h, "Undo");
    h.key_press(egui::Key::Enter);
    h.step();
    assert!(!visible(&h), "old text must never be submitted in the Undo frame");
    assert!(h.state().session.can_redo());
    h.run();
    check(&mut h);
    assert!(visible(&h));
    let before = h.state().session.document().clone();
    let revision = h.state().session.revision();
    #[derive(Debug)]
    struct Dropped(std::path::PathBuf);
    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            panic!("must use bounded path reader")
        }
    }
    let path = std::env::temp_dir().join(format!("nodecraft-stale-{}-public.txt", std::process::id()));
    std::fs::write(&path, b"Public imported text").unwrap();
    h.hover_at(Pos2::new(700.0, 150.0));
    h.run();
    h.input_mut().dropped_files.push(Arc::new(Dropped(path.clone())));
    h.step();
    let _ = std::fs::remove_file(path);
    assert!(h.state().session.revision() > revision);
    assert!(!visible(&h), "old text must never be submitted in the import frame");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before);
}
#[test]
fn snapshot_rejects_replacement_canvas_and_revision_but_retains_view_only_changes() {
    let mut h = super::tests::harness();
    check(&mut h);
    assert!(visible(&h));
    h.state_mut().view.pan += Vec2::new(5.0, 10.0);
    h.run();
    assert!(visible(&h));
    let document = h.state().session.document().clone();
    h.state_mut().session = Session::new(document).unwrap();
    h.step();
    assert!(!visible(&h), "same IDs/revision cannot alias a different Session");
    check(&mut h);
    let canvas = h.state().canvas;
    let other = CanvasId::from_bytes([99; 16]);
    let mut project = h.state().session.document().clone();
    Change::AddCanvas { canvas: Canvas::new(other, "Other") }.apply(&mut project).unwrap();
    h.state_mut().session = Session::new(project).unwrap();
    h.step();
    assert!(!visible(&h));
    check(&mut h);
    h.state_mut().canvas = other;
    h.step();
    assert!(!visible(&h));
    h.state_mut().canvas = canvas;
    h.step();
    assert!(!visible(&h));
}

fn recorded_project() -> Project {
    serde_json::from_str(include_str!("stale/recorded-project.json")).unwrap()
}
#[test]
fn actual_recorded_fresh_changed_and_unrecorded_reports() {
    let project = recorded_project();
    let mut h = ui(NodeCraftApp::from_session(Session::new(project).unwrap(), C).unwrap());
    check(&mut h);
    h.get_by_label_contains("0 stale results; 0 checked results");
    h.get_by_label_contains("Unrecorded results are not verified fresh");
    h.state_mut()
        .session
        .dispatch("node.param_set", &json!({"canvas":C,"id":id(2),"name":"text","value":{"type":"text","value":"Changed public input"}}))
        .unwrap();
    h.step();
    assert!(!visible(&h));
    check(&mut h);
    h.get_by_label_contains("1 stale results; 0 checked results");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.step();
    assert!(!visible(&h));
    check(&mut h);
    h.get_by_label_contains("0 stale results; 0 checked results");
    h.state_mut().session.dispatch("edit.redo", &json!({})).unwrap();
    h.step();
    assert!(!visible(&h));
    let mut unrecorded = super::tests::harness();
    check(&mut unrecorded);
    unrecorded.get_by_label_contains("No stale or locally invalid checked results reported");
}
#[derive(Clone)]
struct SharedStorage(std::rc::Rc<std::cell::RefCell<nodecraft_format::MemoryStorage>>);
impl nodecraft_format::ProjectStorage for SharedStorage {
    fn load(&mut self, l: &nodecraft_format::StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.borrow_mut().load(l)
    }
    fn save(&mut self, l: &nodecraft_format::StorageLocation, b: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().append(b)
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().discard()
    }
}
#[test]
fn real_ui_session_open_same_ids_invalidates_and_save_retains_historical_check() {
    use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation};
    let project = NodeCraftApp::headless().unwrap().session.document().clone();
    let canvas = *project.canvases().keys().next().unwrap();
    let snapshot = Snapshot { project: project.clone(), assets: Default::default(), epoch: 0 };
    let shared = SharedStorage(std::rc::Rc::new(std::cell::RefCell::new(MemoryStorage::default())));
    let location = StorageLocation::from_command("public-example.ncraft".into()).unwrap();
    shared.0.borrow_mut().save(&location, &nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    let session = Session::with_storage(snapshot, Box::new(shared.clone())).unwrap();
    let mut h = ui(NodeCraftApp::from_session(session, canvas).unwrap());
    check(&mut h);
    let revision = h.state().session.revision();
    h.state_mut().session.dispatch("file.save_as", &json!({"location":"saved-public-example.ncraft"})).unwrap();
    h.step();
    assert_eq!(h.state().session.revision(), revision);
    assert!(visible(&h));
    assert!(shared.0.borrow().files.contains_key(&StorageLocation::from_command("saved-public-example.ncraft".into()).unwrap()));
    h.state_mut().session.dispatch("file.save", &json!({})).unwrap();
    h.step();
    assert!(visible(&h));
    h.state_mut().session.dispatch("file.open", &json!({"location":"public-example.ncraft","discard_dirty":true})).unwrap();
    h.step();
    assert_eq!(h.state().session.document(), &project);
    assert!(!visible(&h), "actual Open must invalidate even for same project/canvas IDs");
}

#[test]
fn actual_engine_router_budget_failure_is_visible_instead_of_zero_success() {
    let canvas = CanvasId::from_bytes([1; 16]);
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Router limit").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let numbered = |prefix: u8, index: u16| {
        let mut bytes = [prefix; 16];
        bytes[..2].copy_from_slice(&index.to_le_bytes());
        bytes
    };
    let router_id = |index| NodeId::from_bytes(numbered(2, index));
    let source_id = NodeId::from_bytes([3; 16]);
    let mut changes = vec![Change::AddNode {
        canvas,
        node: kinds.get("source.text").unwrap().instantiate_instance(source_id, Default::default(), &Default::default()).unwrap(),
    }];
    let mut edge_index = 0u16;
    for index in 0..1014u16 {
        changes.push(Change::AddNode {
            canvas,
            node: kinds.get(nodecraft_nodes::router::ID).unwrap().instantiate_instance(router_id(index), Default::default(), &Default::default()).unwrap(),
        });
        let upstream = if index == 0 {
            source_id
        } else if index < 14 {
            router_id(index - 1)
        } else {
            router_id(13)
        };
        for order in 0..if index > 0 && index < 14 { 2 } else { 1 } {
            changes.push(Change::Connect {
                canvas,
                edge: Edge {
                    id: EdgeId::from_bytes(numbered(4, edge_index)),
                    from: Endpoint { node: upstream, port: "out".into() },
                    to: Endpoint { node: router_id(index), port: "in".into() },
                    order,
                },
            });
            edge_index += 1;
        }
    }
    nodecraft_graph::Changeset::new(changes).apply(&mut project).unwrap();

    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut app = NodeCraftApp::from_session(session, canvas).unwrap();
    let before = app.session.document().clone();
    let revision = app.session.revision();
    assert!(app.palette_entries().iter().any(|entry| entry.target == palette::Target::Command("canvas.stale")));
    // Avoid rendering this deliberately large router fixture; exercise the same chosen-command handler.
    app.dispatch_palette_command("canvas.stale");
    assert!(app.stale_snapshot.is_none());
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("invalid_input"));
    assert!(notice.contains("routers"));
    assert!(!notice.contains("0 stale"));
    assert!(notice.len() <= 1536);
    assert_eq!(app.session.document(), &before);
    assert_eq!(app.session.revision(), revision);
}
#[test]
fn contextual_availability_missing_canvas_is_cheap_and_generic_commands_stay_empty() {
    let (mut app, calls, _) = diagnostic();
    calls.store(0, Ordering::SeqCst);
    assert_eq!(app.palette_command_params("edit.undo"), json!({}));
    app.canvas = CanvasId::from_bytes([99; 16]);
    assert!(!app.palette_entries().iter().any(|entry| entry.target == palette::Target::Command("canvas.stale")));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn newly_chosen_query_installs_in_activation_frame_and_preserves_other_notices() {
    let mut h = super::tests::harness();
    open(&mut h, "List stale results");
    h.key_press(egui::Key::Enter);
    h.step();
    assert!(h.state().stale_snapshot.is_some());
    // egui may perform the first Area sizing pass before exposing its accessibility children.
    h.run();
    assert!(visible(&h));
    h.state_mut().notice = Some("Unrelated failure".into());
    h.run();
    h.get_by_label("Unrelated failure");
    assert!(visible(&h));
    // Deferred notices must not prevent another actual palette selection.
    open(&mut h, "List stale results");
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().palette.is_none());
    assert!(visible(&h));
}

#[test]
fn actual_downstream_unrecorded_active_result_is_counted_as_engine_reports() {
    let (app, _, _) = diagnostic_with_downstream(true);
    let mut h = ui(app);
    check(&mut h);
    h.get_by_label_contains("2 stale results; 0 checked results");
    h.get_by_label_contains("stale status can propagate downstream");
}

#[test]
fn performance_palette_report_keeps_main_clipboard_behavior() {
    let mut h = super::tests::harness();
    let before = h.state().session.document().clone();
    open(&mut h, "Performance report");
    h.event(egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.state().palette.is_none());
    assert_eq!(h.state().notice.as_deref(), Some("Performance report copied to the clipboard."));
    let copied = h
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text),
            _ => None,
        })
        .expect("the actual palette action must publish clipboard output");
    let report: serde_json::Value = serde_json::from_str(copied).unwrap();
    assert!(report.is_object());
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().session.revision(), 0);
}

/// Explicit local screenshots; ordinary CI keeps this renderer unselected.
#[test]
#[ignore = "manual local screenshot capture; requires STALE339_RENDER_ROOT"]
fn stale339_visual_current() {
    let root = std::path::PathBuf::from(std::env::var("STALE339_RENDER_ROOT").unwrap());
    let (app, _, _) = diagnostic();
    let mut h = ui(app);
    check(&mut h);
    assert!(visible(&h));
    h.render().unwrap().save(root.join("invalid-checked-results.png")).unwrap();
    let mut h = ui(NodeCraftApp::from_session(Session::new(recorded_project()).unwrap(), C).unwrap());
    check(&mut h);
    assert!(visible(&h));
    h.render().unwrap().save(root.join("zero-recorded.png")).unwrap();
}

#[test]
fn dismissing_staleness_check_preserves_document_history_notices_and_hidden_draft() {
    let mut h = super::tests::harness();
    let canvas = h.state().canvas;
    let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().values().next().unwrap().id;
    for title in ["First public title", "Second public title"] {
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":node,"title":title})).unwrap();
    }
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.state_mut().selection.only(node);
    h.run();
    check(&mut h);
    assert!(visible(&h));
    // A real hidden editor draft must outlive this unrelated view-only dismissal.
    h.get_by_role_and_label(Role::TextInput, "Text").click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("Retain this public unsaved draft".into()));
    h.run();
    h.state_mut().view.zoom = 0.15;
    h.state_mut().notice = Some("Keep this unrelated notice".into());
    h.run();
    assert!(h.state().editors.values().any(|editor| editor.hidden_dirty));
    let drafts: Vec<_> = h.state().editors.iter().map(|(id, editor)| (*id, editor.draft_count(), editor.hidden_dirty)).collect();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let selection = h.state().selection.clone();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    let view = h.state().view;
    assert_eq!(history, (true, true));
    assert!(h.ctx.content_rect().contains_rect(h.get_by_role_and_label(Role::Button, "Dismiss check").rect()));
    h.get_by_role_and_label(Role::Button, "Dismiss check").click();
    h.run();
    assert!(h.state().stale_snapshot.is_none());
    assert!(!visible(&h));
    assert!(h.query_by_role_and_label(Role::Button, "Dismiss check").is_none());
    h.get_by_label("Keep this unrelated notice");
    h.get_by_role_and_label(Role::Button, "Discard drafts");
    assert_eq!(h.state().notice.as_deref(), Some("Keep this unrelated notice"));
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!(h.state().selection, selection, "the pointer must not activate the underlying canvas");
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
    assert_eq!(h.state().view, view);
    assert_eq!(h.state().editors.iter().map(|(id, editor)| (*id, editor.draft_count(), editor.hidden_dirty)).collect::<Vec<_>>(), drafts);
    assert!(h.state().palette.is_none() && h.state().drag.is_none() && h.state().marquee.is_none() && h.state().wiring.is_none());
    h.state_mut().view.zoom = 1.0;
    h.run();
    assert_eq!(h.get_by_role_and_label(Role::MultilineTextInput, "Text").value().as_deref(), Some("Retain this public unsaved draft"));
}
