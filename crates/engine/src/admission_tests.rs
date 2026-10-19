//! Prepared admission identity, semantic coverage and deterministic work evidence.
use super::*;
use crate::commands::CommandSpec;
use nodecraft_format::{MemoryStorage, StorageLocation, journal, take_validation_metrics};
use nodecraft_graph::{Canvas, Generation, Node, Position};
use nodecraft_types::{AssetRef, Blake3, GenerationId, MediaMeta, ProjectId, Value as Wire};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 6, 0, 0, 0, 185, 234, 222, 129, 0, 0, 0, 17, 73, 68, 65, 84, 120,
    156, 99, 248, 207, 192, 240, 31, 132, 25, 48, 24, 0, 161, 121, 11, 245, 77, 196, 154, 7, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
#[derive(Default)]
struct State {
    memory: MemoryStorage,
    appends: usize,
    fail_append: bool,
}
#[derive(Clone, Default)]
struct Storage(Rc<RefCell<State>>);
impl ProjectStorage for Storage {
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.borrow_mut().memory.load(l)
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().memory.save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().memory.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.appends += 1;
        if s.fail_append {
            return Err(invalid("injected append failure"));
        }
        s.memory.append(b)
    }
}
fn canvas() -> CanvasId {
    CanvasId::from_bytes([1; 16])
}
fn id(n: u128) -> NodeId {
    NodeId::from_bytes(n.to_be_bytes())
}
fn image(hash: Blake3) -> Wire {
    Wire::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(2), height: Some(3), ..Default::default() } })
}
fn setup(nodes: u128) -> (Session, Storage, Blake3) {
    let mut assets = AssetStore::default();
    let hash = assets.insert(PNG).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Admission").unwrap();
    let mut changes = vec![Change::AddCanvas { canvas: Canvas::new(canvas(), "C") }];
    for n in 1..=nodes {
        let mut node = Node::new(id(n), "source.image");
        node.params.insert("image".into(), image(hash));
        changes.push(Change::AddNode { canvas: canvas(), node });
    }
    Changeset::new(changes).apply(&mut project).unwrap();
    let storage = Storage::default();
    let mut session = Session::with_storage(Snapshot { project, assets, epoch: 0 }, Box::new(storage.clone())).unwrap();
    session
        .registry
        .register(CommandSpec {
            id: "test.edit",
            label: "Test",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || json!({}),
            params_doc: "changeset",
            enabled: |_, _| Ok(()),
            run: |_, value| Ok(Effect::Edit { changes: crate::commands::params(value)?, result: Value::Null, gesture: None }),
        })
        .unwrap();
    (session, storage, hash)
}
fn effect(x: f32) -> Effect {
    Effect::Edit {
        changes: Changeset::new(vec![Change::MoveNode { canvas: canvas(), id: id(1), pos: Position { x, y: 0.0 } }]),
        result: Value::Null,
        gesture: None,
    }
}
fn movement(x: i32) -> Value {
    json!({"canvas":canvas(),"id":id(1),"pos":{"x":x,"y":0}})
}
fn unchanged(session: &Session, storage: &Storage, revision: u64, appends: usize, recovery: &[u8]) {
    assert_eq!(session.revision(), revision);
    assert_eq!(storage.0.borrow().appends, appends);
    assert_eq!(storage.0.borrow().memory.recovery, recovery);
}
#[test]
fn prepared_capability_cannot_cross_session_assets_revision_or_save_gc() {
    let (mut source, storage, _) = setup(2);
    let proof = source.prepare(effect(10.0)).unwrap();
    let valid = source.prepare(effect(11.0)).unwrap();
    let (mut other, other_storage, _) = setup(2);
    let before = other.document.clone();
    let recovery = other_storage.0.borrow().memory.recovery.clone();
    assert!(other.commit_prepared(proof, "test.edit").is_err());
    assert_eq!(other.document, before);
    unchanged(&other, &other_storage, 0, 0, &recovery);
    source.commit_prepared(valid, "test.edit").unwrap();
    let proof = source.prepare(effect(12.0)).unwrap();
    let revision = source.revision();
    source.import_asset(b"new immutable bytes").unwrap();
    assert_eq!(source.revision(), revision);
    let recovery = storage.0.borrow().memory.recovery.clone();
    let appends = storage.0.borrow().appends;
    assert!(source.commit_prepared(proof, "test.edit").is_err());
    unchanged(&source, &storage, revision, appends, &recovery);
    let proof = source.prepare(effect(13.0)).unwrap();
    source.dispatch("node.move", &movement(14)).unwrap();
    let recovery = storage.0.borrow().memory.recovery.clone();
    let appends = storage.0.borrow().appends;
    let revision = source.revision();
    assert!(source.commit_prepared(proof, "test.edit").is_err());
    unchanged(&source, &storage, revision, appends, &recovery);
    let proof = source.prepare(effect(15.0)).unwrap();
    source.dispatch("file.save_as", &json!({"location":"saved.ncraft"})).unwrap();
    assert_eq!(source.revision(), revision);
    let recovery = storage.0.borrow().memory.recovery.clone();
    assert!(source.commit_prepared(proof, "test.edit").is_err());
    unchanged(&source, &storage, revision, appends, &recovery);
}
#[test]
fn failed_prepare_import_and_append_keep_current_source_identity() {
    let (mut session, storage, _) = setup(2);
    let identity = Arc::clone(&session.admission_identity);
    let valid = session.prepare(effect(10.0)).unwrap();
    assert!(
        session
            .prepare(Effect::Edit { changes: Changeset::new(vec![Change::RemoveNode { canvas: canvas(), id: id(999) }]), result: Value::Null, gesture: None })
            .is_err()
    );
    assert!(Arc::ptr_eq(&identity, &session.admission_identity));
    assert!(session.import_asset(&vec![0; nodecraft_format::MAX_ASSET_BYTES + 1]).is_err());
    assert!(Arc::ptr_eq(&identity, &session.admission_identity));
    storage.0.borrow_mut().fail_append = true;
    let document = session.document.clone();
    let recovery = storage.0.borrow().memory.recovery.clone();
    assert!(session.commit_prepared(valid, "test.edit").is_err());
    assert!(Arc::ptr_eq(&identity, &session.admission_identity));
    assert_eq!(session.document, document);
    assert_eq!(session.revision(), 0);
    assert!(!session.can_undo());
    assert_eq!(storage.0.borrow().memory.recovery, recovery);
    assert!(session.file_status().recovery_required);
}
#[test]
fn move_and_availability_do_not_visit_or_sniff_unchanged_images() {
    let (mut session, storage, _) = setup(2000);
    take_validation_metrics();
    session.enabled("node.move", &movement(7)).unwrap();
    session.dispatch("node.move", &movement(7)).unwrap();
    let metrics = take_validation_metrics();
    eprintln!("2000-image-node admitted move availability+dispatch: {metrics:?}");
    assert_eq!(metrics.full_asset_checks, 0);
    assert_eq!(metrics.full_v1_checks, 0);
    assert_eq!(metrics.asset_node_visits, 0);
    assert_eq!(metrics.sniffs, 0);
    assert_eq!(metrics.image_probes, 0);
    assert_eq!(metrics.replay_preflights, 4);
    session.enabled("keys.status", &json!({"account":"openai"})).unwrap();
    assert_eq!(take_validation_metrics(), Default::default());
    assert_eq!(journal::recover(&storage.0.borrow().memory.recovery).unwrap().snapshot.project, *session.document());
}
fn asset_canvas(node: Node) -> Canvas {
    let c = CanvasId::from_bytes([3; 16]);
    let mut p = Project::new(ProjectId::from_bytes([4; 16]), "Payload").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(c, "C") }, Change::AddNode { canvas: c, node }]).apply(&mut p).unwrap();
    p.canvases().get(&c).unwrap().clone()
}
#[test]
fn all_forward_asset_roles_fail_before_append_and_do_not_invalidate_a_valid_preparation() {
    let (mut session, storage, hash) = setup(2);
    let valid = session.prepare(effect(20.0)).unwrap();
    let before = session.document.clone();
    let identity = Arc::clone(&session.admission_identity);
    let recovery = storage.0.borrow().memory.recovery.clone();
    let missing = image(Blake3::from_bytes([99; 32]));
    let mut wrong_mime = image(hash);
    let mut wrong_dimensions = image(hash);
    if let Wire::Media(asset) = &mut wrong_mime {
        asset.mime = "image/jpeg".parse().unwrap();
    }
    if let Wire::Media(asset) = &mut wrong_dimensions {
        asset.meta.width = Some(99);
    }
    let mut changes = Vec::new();
    for value in [missing, wrong_mime, wrong_dimensions] {
        let nested = Wire::List(vec![value]);
        let mut node = Node::new(id(99), "source.image");
        node.params.insert("image".into(), nested.clone());
        let mut replacement = node.clone();
        replacement.id = id(1);
        changes.extend([
            Change::SetParam { canvas: canvas(), id: id(1), name: "nested".into(), value: Some(nested.clone()) },
            Change::AddGeneration {
                canvas: canvas(),
                id: id(1),
                index: 0,
                generation: Generation {
                    id: GenerationId::from_bytes([9; 16]),
                    outputs: vec![nested],
                    input_hash: None,
                    items: Vec::new(),
                    settings: Vec::new(),
                },
            },
            Change::AddNode { canvas: canvas(), node: node.clone() },
            Change::RestoreNode { canvas: canvas(), node: node.clone(), edges: vec![], pins: vec![] },
            Change::AddCanvas { canvas: asset_canvas(node) },
            Change::SetNode { canvas: canvas(), node: replacement },
        ]);
    }
    let mut config = Node::new(id(1), "source.image");
    config.config.insert("asset".into(), image(Blake3::from_bytes([99; 32])));
    changes.push(Change::SetNode { canvas: canvas(), node: config });
    for change in changes {
        let params = serde_json::to_value(Changeset::new(vec![change])).unwrap();
        assert!(session.enabled("test.edit", &params).is_err());
        assert!(session.dispatch("test.edit", &params).is_err());
        assert_eq!(session.document, before);
        assert!(Arc::ptr_eq(&identity, &session.admission_identity));
        unchanged(&session, &storage, 0, 0, &recovery);
        assert!(!session.file_status().recovery_required);
    }
    session.commit_prepared(valid, "test.edit").unwrap();
    assert_eq!(storage.0.borrow().appends, 1);
}
#[test]
fn touched_nodes_use_the_complete_source_store_and_valid_final_removal_remains_valid() {
    let (mut session, storage, hash) = setup(2);
    let extra = session.import_asset(b"extra text").unwrap();
    let edit = Changeset::new(vec![Change::SetParam {
        canvas: canvas(),
        id: id(1),
        name: "extra".into(),
        value: Some(Wire::Media(AssetRef { hash: extra, mime: "text/plain".parse().unwrap(), meta: Default::default() })),
    }]);
    session.dispatch("test.edit", &serde_json::to_value(edit).unwrap()).unwrap();
    let missing = Changeset::new(vec![
        Change::SetParam { canvas: canvas(), id: id(2), name: "temporary".into(), value: Some(image(Blake3::from_bytes([99; 32]))) },
        Change::RemoveNode { canvas: canvas(), id: id(2) },
    ]);
    let before = session.document.clone();
    let recovery = storage.0.borrow().memory.recovery.clone();
    let appends = storage.0.borrow().appends;
    let params = serde_json::to_value(missing).unwrap();
    assert!(session.enabled("test.edit", &params).is_err());
    assert!(session.dispatch("test.edit", &params).is_err());
    assert_eq!(session.document, before);
    unchanged(&session, &storage, 1, appends, &recovery);
    let mut wrong = image(hash);
    if let Wire::Media(asset) = &mut wrong {
        asset.meta.width = Some(123);
    }
    let edit = Changeset::new(vec![
        Change::SetParam { canvas: canvas(), id: id(2), name: "bad".into(), value: Some(wrong) },
        Change::RemoveNode { canvas: canvas(), id: id(2) },
    ]);
    session.enabled("test.edit", &serde_json::to_value(&edit).unwrap()).unwrap();
    session.dispatch("test.edit", &serde_json::to_value(edit).unwrap()).unwrap();
    // The ordered inverse restores the temporary value, then removes that parameter;
    // its final graph must remain valid through undo and canonical redo.
    session.dispatch("edit.undo", &Value::Null).unwrap();
    session.dispatch("edit.redo", &Value::Null).unwrap();
    assert_eq!(journal::recover(&storage.0.borrow().memory.recovery).unwrap().snapshot.project, *session.document());
}

/// The actual main executor owns completed results; this adapter models a host recording
/// one through the same command boundary, without inventing a production run command.
fn runtime_record(c: &Context<'_>, params: &Value) -> Result<Effect, Error> {
    let generation: Generation = crate::commands::params(params)?;
    let node = c.document.canvases().get(&canvas()).and_then(|c| c.nodes().get(&id(1))).ok_or_else(|| invalid("missing run node"))?;
    let changes = nodecraft_exec::run::record_changes(canvas(), node, generation)?;
    Ok(Effect::Edit { changes: Changeset::new(changes), result: Value::Null, gesture: None })
}
fn register_runtime(session: &mut Session) {
    session
        .registry
        .register(CommandSpec {
            id: "test.record_run",
            label: "Record run",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || json!({}),
            params_doc: "generation",
            enabled: |_, _| Ok(()),
            run: runtime_record,
        })
        .unwrap();
}
#[test]
fn actual_run_result_remains_owned_and_persists_stamps_through_v2_admission() {
    use nodecraft_exec::{
        planner::{Planned, RunMode, plan},
        run::{Run, RunLimits, RunOptions},
        scheduler::RetryPolicy,
    };
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([8; 16]), "Run recording").unwrap();
    let node = kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &[("text".into(), Wire::Text("completed result".into()))].into()).unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas(), "C") }, Change::AddNode { canvas: canvas(), node }]).apply(&mut project).unwrap();
    let graph = project.canvases().get(&canvas()).unwrap();
    let Planned::Ready(plan) = plan(graph, &kinds, &RunMode::Selected, &[id(1)].into(), &Default::default()).unwrap() else { panic!("source must plan") };
    let (mut run, progress) =
        Run::start(&plan, canvas(), graph, &kinds, RunOptions { id: [7; 16], first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER }).unwrap();
    let job = progress.jobs.first().unwrap();
    let nodecraft_nodes::JobSpec::Source { output, value, .. } = run.work(job.id).unwrap().clone() else { panic!("expected source work") };
    let completed = run.job_done(&kinds, job.id, vec![(output, value)]);
    assert!(run.is_finished());
    assert!(completed.failed.is_empty());
    let result = completed.finished.first().unwrap().clone();
    assert!(result.generation.input_hash.is_some());
    assert_eq!(result.generation.outputs, vec![Wire::Text("completed result".into())]);
    let params = serde_json::to_value(&result.generation).unwrap();
    let storage = Storage::default();
    let mut persistent = Session::with_storage(Snapshot { project: project.clone(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    register_runtime(&mut persistent);
    let proof = persistent
        .prepare(Effect::Edit {
            changes: Changeset::new(vec![Change::MoveNode { canvas: canvas(), id: id(1), pos: Position { x: 4.0, y: 0.0 } }]),
            result: Value::Null,
            gesture: None,
        })
        .unwrap();
    let identity = Arc::clone(&persistent.admission_identity);
    let recovery = storage.0.borrow().memory.recovery.clone();
    let status = persistent.file_status();
    persistent.enabled("test.record_run", &params).unwrap();
    assert!(Arc::ptr_eq(&identity, &persistent.admission_identity));
    assert_eq!(persistent.file_status(), status);
    unchanged(&persistent, &storage, 0, 0, &recovery);
    persistent.dispatch("test.record_run", &params).unwrap();
    assert!(!Arc::ptr_eq(&identity, &persistent.admission_identity));
    assert_eq!(serde_json::to_value(&result.generation).unwrap(), params, "the caller still owns the completed stamped result");
    assert!(persistent.commit_prepared(proof, "node.move").is_err(), "publication invalidates the older legitimate proof");
    let recorded = persistent.document().clone();
    assert_eq!(recorded.canvases().get(&canvas()).unwrap().nodes().get(&id(1)).unwrap().history.first(), Some(&result.generation));
    assert_eq!(journal::recover(&storage.0.borrow().memory.recovery).unwrap().snapshot.project, recorded);
    persistent.dispatch("edit.undo", &Value::Null).unwrap();
    assert_eq!(persistent.document(), &project);
    persistent.dispatch("edit.redo", &Value::Null).unwrap();
    assert_eq!(persistent.document(), &recorded);
    persistent.dispatch("file.save_as", &json!({"location":"stamped-v2.ncraft"})).unwrap();
    assert_eq!(journal::recover(&storage.0.borrow().memory.recovery).unwrap().snapshot.project, recorded);
    let mut memory = Session::new(project.clone()).unwrap();
    register_runtime(&mut memory);
    let identity = Arc::clone(&memory.admission_identity);
    memory.enabled("test.record_run", &params).unwrap();
    assert!(Arc::ptr_eq(&identity, &memory.admission_identity));
    memory.dispatch("test.record_run", &params).unwrap();
    assert!(!Arc::ptr_eq(&identity, &memory.admission_identity));
    let recorded = memory.document().clone();
    assert_eq!(recorded.canvases().get(&canvas()).unwrap().nodes().get(&id(1)).unwrap().active, Some(result.generation.id));
    memory.dispatch("edit.undo", &Value::Null).unwrap();
    assert_eq!(memory.document(), &project);
    memory.dispatch("edit.redo", &Value::Null).unwrap();
    assert_eq!(memory.document(), &recorded);
}

#[test]
fn current_main_card_and_multi_edit_commands_use_the_admitted_publication_boundary() {
    use nodecraft_graph::{Edge, Endpoint, Port};
    use nodecraft_types::{EdgeId, Modality, PortType};
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([8; 16]), "New commands").unwrap();
    let mut changes = vec![Change::AddCanvas { canvas: Canvas::new(canvas(), "C") }];
    for n in 1..=3 {
        let mut node = kinds.get("source.text").unwrap().descriptor().instantiate(id(n), &Default::default()).unwrap();
        if n > 1 {
            node.inputs.push(Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } });
        }
        changes.push(Change::AddNode { canvas: canvas(), node });
    }
    let edge = |n, to| Edge {
        id: EdgeId::from_bytes([n; 16]),
        from: Endpoint { node: id(1), port: "out".into() },
        to: Endpoint { node: id(to), port: "in".into() },
        order: 0,
    };
    changes.push(Change::Connect { canvas: canvas(), edge: edge(1, 2) });
    Changeset::new(changes).apply(&mut project).unwrap();
    let storage = Storage::default();
    let mut session = Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    for (command, params) in [
        ("node.title", json!({"canvas":canvas(),"id":id(1),"title":"renamed"})),
        ("node.param_set", json!({"canvas":canvas(),"id":id(1),"name":"text","value":{"type":"text","value":"changed"}})),
        ("edge.rewire", json!({"canvas":canvas(),"edge":edge(1,3),"replace":[EdgeId::from_bytes([1;16])]})),
        ("canvas.delete", json!({"canvas":canvas(),"nodes":[id(3)]})),
        (
            "canvas.paste",
            json!({"canvas":canvas(),"nodes":[kinds.get("source.text").unwrap().descriptor().instantiate(id(4), &Default::default()).unwrap()],"edges":[]}),
        ),
    ] {
        let stale = session.prepare(effect(50.0)).unwrap();
        let before = session.document.clone();
        let identity = Arc::clone(&session.admission_identity);
        session.enabled(command, &params).unwrap();
        assert!(Arc::ptr_eq(&identity, &session.admission_identity));
        session.dispatch(command, &params).unwrap();
        let after = session.document.clone();
        assert!(!Arc::ptr_eq(&identity, &session.admission_identity));
        assert!(session.commit_prepared(stale, "test.edit").is_err());
        session.dispatch("edit.undo", &Value::Null).unwrap();
        assert_eq!(session.document(), &before);
        session.dispatch("edit.redo", &Value::Null).unwrap();
        assert_eq!(session.document(), &after);
        assert_eq!(journal::recover(&storage.0.borrow().memory.recovery).unwrap().snapshot.project, after);
    }
}
