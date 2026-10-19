//! Public command controls use the production generation kind and the shipped catalog.
use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, ErrorKind, NodeId, ParamValues, ProjectId, Value};
use serde_json::json;
use std::collections::BTreeMap;

const C: CanvasId = CanvasId::from_bytes([157; 16]);
const A: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: &str = "openai-images.sunburst";
const DESTINATION: &str = "openai-images.flare";

fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([157; 16]), "Model choice").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let supplied: ParamValues = [("model".into(), Value::Text(SOURCE.into())), ("prompt".into(), Value::Text("Public-safe example".into()))].into();
    let node = session.kinds().get("generation.image").unwrap().instantiate_instance(A, BTreeMap::new(), &supplied).unwrap();
    session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
    session
}

#[test]
fn real_generation_model_preview_and_atomic_swap_are_public_commands() {
    let mut session = session();
    let before = session.document().clone();
    let revision = session.revision();
    let preview = session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[A],"model":DESTINATION}));
    assert!(preview.is_ok(), "production model choice needs a source-aware preview command: {preview:?}");
    let preview = preview.unwrap();
    assert_eq!(session.document(), &before, "preview is read-only");
    assert_eq!(session.revision(), revision);
    let commit = json!({"canvas":C,"nodes":[A],"model":DESTINATION,"expected_revision":revision,"preview_binding":preview["binding"]});
    session.dispatch("node.set_model", &commit).unwrap();
    let changed = session.document().clone();
    assert_eq!(changed.canvases().get(&C).unwrap().nodes().get(&A).unwrap().params.get("model"), Some(&Value::Text(DESTINATION.into())));
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &before);
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(session.document(), &changed);
}

#[test]
fn raw_selector_parameter_cannot_bypass_source_aware_model_preview() {
    let mut session = session();
    let before = session.document().clone();
    let revision = session.revision();
    let result = session.dispatch("node.param_set", &json!({"canvas":C,"id":A,"name":"model","value":Value::Text(DESTINATION.into())}));
    assert_eq!(result.unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
}

fn preview(session: &mut Session, nodes: &[NodeId], model: &str) -> serde_json::Value {
    session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":nodes,"model":model})).unwrap()
}
fn apply(session: &mut Session, nodes: &[NodeId], model: &str, preview: &serde_json::Value) -> Result<serde_json::Value, nodecraft_types::Error> {
    session.dispatch(
        "node.set_model",
        &json!({"canvas":C,"nodes":nodes,"model":model,"expected_revision":preview["revision"],"preview_binding":preview["binding"]}),
    )
}

#[test]
fn preview_staleness_duplicate_targets_and_atomic_mixed_kind_refuse_without_changes() {
    let mut s = session();
    let p = preview(&mut s, &[A], DESTINATION);
    s.dispatch("node.title", &json!({"canvas":C,"id":A,"title":"An unrelated title edit"})).unwrap();
    let before = s.document().clone();
    assert_eq!(apply(&mut s, &[A], DESTINATION, &p).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(s.document(), &before);
    let wrong = NodeId::from_bytes([2; 16]);
    s.dispatch("node.create", &json!({"canvas":C,"id":wrong,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    for ids in [vec![A, A], vec![A, wrong], vec![]] {
        assert!(s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":ids,"model":DESTINATION})).is_err());
    }
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
}

#[test]
fn unchanged_model_is_noop_and_same_revision_catalog_replacement_refuses_old_preview() {
    use nodecraft_engine::{generation_catalog::GenerationCatalog, run_cmds::RunCatalog};
    let mut s = session();
    let before = s.document().clone();
    let revision = s.revision();
    let p = preview(&mut s, &[A], SOURCE);
    assert_eq!(p["targets"][0]["changed"], false);
    apply(&mut s, &[A], SOURCE, &p).unwrap();
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    let p = preview(&mut s, &[A], DESTINATION);
    let next = GenerationCatalog::new(RunCatalog::new(nodecraft_catalog::Catalog::builtin().unwrap()).unwrap()).unwrap();
    s.swap_resource(Some(next.run_catalog()));
    s.swap_resource(Some(next));
    assert_eq!(s.revision(), revision);
    assert!(apply(&mut s, &[A], DESTINATION, &p).is_err());
    assert!(s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[A],"model":DESTINATION})).is_err());
    assert_eq!(s.document(), &before);
}

fn semantic_catalog() -> nodecraft_catalog::Catalog {
    use nodecraft_types::{Choice, Condition, Contract, HiddenPolicy, Integer, ParamControl, ParamOwner, ParamRule, ParamSchema, ParamSpec, Unit, Unsigned};
    let text = |name: &str, default: Option<&str>| ParamSpec {
        name: name.into(),
        label: name.into(),
        description: None,
        required: false,
        visible_if: None,
        control: ParamControl::Text { multiline: false, max_bytes: 64 },
        default: default.map(|s| Value::Text(s.into())),
    };
    let choice = |name: &str, options: &[&str]| ParamSpec {
        name: name.into(),
        label: name.into(),
        description: None,
        required: false,
        visible_if: None,
        control: ParamControl::Choice { options: options.iter().map(|s| Choice { value: (*s).into(), label: (*s).into() }).collect() },
        default: Some(Value::Text(options[0].into())),
    };
    let mut source = nodecraft_catalog::Catalog::builtin().unwrap().get(SOURCE).unwrap().clone();
    source.id = "fixture/source".into();
    source.params = ParamSchema {
        params: vec![
            ParamSpec {
                name: "count".into(),
                label: "Count".into(),
                description: None,
                required: false,
                visible_if: None,
                control: ParamControl::Unsigned { min: Some(Unsigned(0)), max: Some(Unsigned(10)), step: Unsigned(1) },
                default: Some(Value::Unsigned(Unsigned(1))),
            },
            ParamSpec {
                name: "duration".into(),
                label: "Duration".into(),
                description: None,
                required: false,
                visible_if: None,
                control: ParamControl::Integer { min: Some(Integer(1)), max: Some(Integer(100)), step: Unsigned(1) },
                default: Some(Value::Integer(Integer(1))),
            },
            choice("voice", &["soft", "bright"]),
            choice("style", &["calm", "bold"]),
            text("unverified", Some("original")),
            ParamSpec {
                name: "detail_enabled".into(),
                label: "Detail".into(),
                description: None,
                required: false,
                visible_if: None,
                control: ParamControl::Toggle,
                default: Some(Value::Bool(false)),
            },
            text("detail", None),
        ],
    };
    let mut from = Contract::from_schema(source.id.clone(), ParamOwner::Model, source.params.clone());
    for (name, meaning, unit) in [
        ("count", "output_count", Unit::Count),
        ("duration", "duration", Unit::Seconds),
        ("voice", "voice", Unit::None),
        ("style", "image_style", Unit::None),
        ("detail_enabled", "detail_enabled", Unit::None),
        ("detail", "detail", Unit::None),
    ] {
        from.rules.push(ParamRule { meaning: Some(meaning.into()), unit, ..ParamRule::implied(name) });
    }
    source.parameter_contract = Some(from.clone());
    let mut destination = source.clone();
    destination.id = "fixture/destination".into();
    destination.params.params.iter_mut().find(|p| p.name == "count").unwrap().control =
        ParamControl::Unsigned { min: Some(Unsigned(0)), max: Some(Unsigned(5)), step: Unsigned(1) };
    destination.params.params.iter_mut().find(|p| p.name == "voice").unwrap().control =
        ParamControl::Choice { options: vec![Choice { value: "bright".into(), label: "Bright".into() }] };
    destination.params.params.iter_mut().find(|p| p.name == "voice").unwrap().default = Some(Value::Text("bright".into()));
    destination.params.params.push(text("new_setting", Some("fresh")));
    let mut to = Contract { namespace: destination.id.clone(), schema: destination.params.clone(), ..from };
    to.rules.iter_mut().find(|r| r.param == "duration").unwrap().unit = Unit::Milliseconds;
    to.rules.iter_mut().find(|r| r.param == "style").unwrap().meaning = Some("audio_style".into());
    let detail = to.rules.iter_mut().find(|r| r.param == "detail").unwrap();
    detail.enabled_when = Some(Condition::Equals { param: "detail_enabled".into(), value: Value::Bool(true) });
    detail.hidden = HiddenPolicy::Reset;
    destination.parameter_contract = Some(to);
    nodecraft_catalog::Catalog::from_models([source, destination]).unwrap()
}
fn semantic_session(count: u64) -> Session {
    use nodecraft_engine::{
        commands::{Registry, Resources},
        generation_catalog::GenerationCatalog,
        run_cmds::RunCatalog,
    };
    use nodecraft_types::{Integer, Unsigned};
    let catalog = GenerationCatalog::new(RunCatalog::new(semantic_catalog()).unwrap()).unwrap();
    let kinds = catalog.registry().unwrap();
    let params: ParamValues = [
        ("model".into(), Value::Text("fixture/source".into())),
        ("prompt".into(), Value::Text("Private draft must not appear in reports".into())),
        ("count".into(), Value::Unsigned(Unsigned(count))),
        ("duration".into(), Value::Integer(Integer(15))),
        ("voice".into(), Value::Text("soft".into())),
        ("style".into(), Value::Text("bold".into())),
        ("unverified".into(), Value::Text("do not infer semantics".into())),
        ("detail".into(), Value::Text("recoverable detail".into())),
    ]
    .into();
    let mut project = Project::new(ProjectId::from_bytes([158; 16]), "Shared model contract").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds.get("generation.image").unwrap().instantiate_instance(A, Default::default(), &params).unwrap();
    node.title = "Retain title".into();
    node.pos.x = 71.0;
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(catalog.run_catalog());
    resources.insert(catalog);
    Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources)
}

#[test]
fn shared_contract_migration_reports_units_meaning_options_defaults_and_resets_without_values() {
    let mut s = semantic_session(4);
    let before = s.document().clone();
    let p = preview(&mut s, &[A], "fixture/destination");
    let target = &p["targets"][0];
    assert!(target["kept"].as_array().unwrap().contains(&json!("count")));
    for (name, reason) in [("duration", "unit_changed"), ("voice", "invalid_in_target"), ("style", "meaning_changed"), ("unverified", "unverified_meaning")] {
        assert!(target["dropped"].as_array().unwrap().iter().any(|v| v["param"] == name && v["reason"] == reason));
    }
    assert!(target["defaults"].as_array().unwrap().contains(&json!("new_setting")));
    assert_eq!(target["reset"], json!(["detail"]));
    for private in ["Private draft", "do not infer semantics", "recoverable detail"] {
        assert!(!p.to_string().contains(private));
    }
    apply(&mut s, &[A], "fixture/destination", &p).unwrap();
    let node = s.document().canvases().get(&C).unwrap().nodes().get(&A).unwrap();
    assert_eq!(node.params.get("count"), Some(&Value::Unsigned(nodecraft_types::Unsigned(4))));
    assert!(!node.params.contains_key("detail"));
    assert_eq!(node.title, "Retain title");
    assert_eq!(node.pos.x, 71.0);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
    let mut s = semantic_session(7);
    let p = preview(&mut s, &[A], "fixture/destination");
    assert!(p["targets"][0]["dropped"].as_array().unwrap().iter().any(|v| v["param"] == "count" && v["reason"] == "invalid_in_target"));
}

#[test]
fn composed_text_wires_remain_selectable_and_connected_image_role_cannot_be_dropped() {
    use nodecraft_graph::{Edge, Endpoint};
    use nodecraft_types::EdgeId;
    let mut s = session();
    for n in [10u8, 11] {
        let id = NodeId::from_bytes([n; 16]);
        s.dispatch("node.create", &json!({"canvas":C,"id":id,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([n; 16]),
            from: Endpoint { node: id, port: "out".into() },
            to: Endpoint { node: A, port: "prompt".into() },
            order: u32::from(n),
        };
        s.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    }
    let p = preview(&mut s, &[A], DESTINATION);
    apply(&mut s, &[A], DESTINATION, &p).unwrap();
    assert_eq!(s.document().canvases().get(&C).unwrap().edges().len(), 2);
    let image = NodeId::from_bytes([12; 16]);
    s.dispatch("node.create", &json!({"canvas":C,"id":image,"kind":"source.image","pos":{"x":0,"y":0}})).unwrap();
    s.dispatch("edge.connect",&json!({"canvas":C,"edge":Edge{id:EdgeId::from_bytes([12;16]),from:Endpoint{node:image,port:"out".into()},to:Endpoint{node:A,port:"image".into()},order:0}})).unwrap();
    let before = s.document().clone();
    assert!(s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[A],"model":"fal.flux-schnell"})).is_err());
    assert_eq!(s.document(), &before);
}

#[test]
fn failed_persistent_append_keeps_complete_source_document_history_and_event_state() {
    use nodecraft_format::{AssetStore, MemoryStorage, ProjectStorage, Snapshot, StorageLocation};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    struct Store {
        memory: Rc<RefCell<MemoryStorage>>,
        fail: Rc<Cell<bool>>,
    }
    impl ProjectStorage for Store {
        fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, nodecraft_types::Error> {
            self.memory.borrow_mut().load(l)
        }
        fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), nodecraft_types::Error> {
            self.memory.borrow_mut().save(l, b)
        }
        fn checkpoint(&mut self, b: &[u8]) -> Result<(), nodecraft_types::Error> {
            self.memory.borrow_mut().checkpoint(b)
        }
        fn append(&mut self, b: &[u8]) -> Result<(), nodecraft_types::Error> {
            if self.fail.get() {
                return Err(nodecraft_types::Error::new(ErrorKind::Conflict, "injected append failure"));
            }
            self.memory.borrow_mut().append(b)
        }
    }
    let original = session();
    let memory = Rc::new(RefCell::new(MemoryStorage::default()));
    let fail = Rc::new(Cell::new(false));
    let mut s = Session::with_storage(
        Snapshot { project: original.document().clone(), assets: AssetStore::default(), epoch: 0 },
        Box::new(Store { memory: memory.clone(), fail: fail.clone() }),
    )
    .unwrap();
    let p = preview(&mut s, &[A], DESTINATION);
    let before = s.document().clone();
    let revision = s.revision();
    let events = s.events_since(0).unwrap();
    let journal = memory.borrow().recovery.clone();
    fail.set(true);
    assert!(apply(&mut s, &[A], DESTINATION, &p).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert!(!s.can_undo());
    assert!(!s.can_redo());
    assert_eq!(memory.borrow().recovery, journal);
    assert!(s.file_status().recovery_required);
}

#[test]
fn real_bulk_proposal_has_one_commit_and_undo_for_all_targets() {
    let mut s = session();
    let b = NodeId::from_bytes([2; 16]);
    let mut second = s.document().canvases().get(&C).unwrap().nodes().get(&A).unwrap().clone();
    second.id = b;
    s.dispatch("node.add", &json!({"canvas":C,"node":second})).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    let p = preview(&mut s, &[b, A], DESTINATION);
    let reordered = preview(&mut s, &[A, b], DESTINATION);
    assert_eq!(p["binding"], reordered["binding"]);
    apply(&mut s, &[A, b], DESTINATION, &p).unwrap();
    assert_eq!(s.revision(), revision + 1);
    for id in [A, b] {
        assert_eq!(s.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap().params.get("model"), Some(&Value::Text(DESTINATION.into())));
    }
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
}

#[test]
fn aggregate_source_payload_refuses_before_retaining_a_multi_node_candidate() {
    let seed = session();
    let mut project = Project::new(ProjectId::from_bytes([159; 16]), "Bounded selected payload").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut ids = Vec::new();
    for n in 1u8..=17 {
        let id = NodeId::from_bytes([n; 16]);
        ids.push(id);
        let supplied: ParamValues = [("model".into(), Value::Text(SOURCE.into())), ("prompt".into(), Value::Text("x".repeat(512 * 1024)))].into();
        let node = seed.kinds().get("generation.image").unwrap().instantiate_instance(id, Default::default(), &supplied).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut s = Session::new(project).unwrap();
    let revision = s.revision();
    let result = s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":ids,"model":DESTINATION}));
    assert_eq!(result.unwrap_err().kind, ErrorKind::Budget);
    assert_eq!(s.revision(), revision);
    assert!(!s.can_undo());
    assert!(s.document().canvases().get(&C).unwrap().nodes().values().all(|n| n.params.get("model") == Some(&Value::Text(SOURCE.into()))));
}

#[test]
fn unavailable_saved_source_has_explicit_drop_report_and_reversible_known_destination() {
    let seed = session();
    let mut project = seed.document().clone();
    let mut node = project.canvases().get(&C).unwrap().nodes().get(&A).unwrap().clone();
    node.params.insert("model".into(), Value::Text("removed/model".into()));
    Change::SetNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let before = s.document().clone();
    let p = preview(&mut s, &[A], DESTINATION);
    assert_eq!(p["targets"][0]["previous_model"], serde_json::Value::Null);
    assert_eq!(p["targets"][0]["source_unavailable"], true);
    assert!(!p.to_string().contains("removed/model"));
    assert!(p["targets"][0]["dropped"].as_array().unwrap().iter().all(|v| v["reason"] == "source_unavailable"));
    apply(&mut s, &[A], DESTINATION, &p).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
}

#[test]
fn schema_valid_legacy_selector_recovers_without_public_id_leak_and_undo_restores_source() {
    for legacy in ["LegacyModel:v2".to_owned(), "legacy".repeat(80)] {
        let seed = session();
        let mut project = seed.document().clone();
        let mut node = project.canvases().get(&C).unwrap().nodes().get(&A).unwrap().clone();
        let saved = Value::Text(legacy.clone());
        nodecraft_types::ParamControl::Model.validate_value(&saved).unwrap();
        assert!(!nodecraft_types::model_namespace(&legacy));
        node.params.insert("model".into(), saved);
        Change::SetNode { canvas: C, node }.apply(&mut project).unwrap();
        let mut s = Session::new(project).unwrap();
        let before = s.document().clone();
        let revision = s.revision();
        let p = preview(&mut s, &[A], DESTINATION);
        assert_eq!(p["targets"][0]["previous_model"], serde_json::Value::Null);
        assert_eq!(p["targets"][0]["source_unavailable"], true);
        assert!(!p.to_string().contains(&legacy));
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
        // Source recovery must not weaken the destination's installable catalog ID syntax.
        assert_eq!(s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[A],"model":legacy})).unwrap_err().kind, ErrorKind::InvalidInput);
        assert_eq!(s.document(), &before);
        apply(&mut s, &[A], DESTINATION, &p).unwrap();
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
    }
}

#[test]
fn invalid_stored_selector_shapes_and_future_versions_refuse_without_source_changes() {
    for saved in [Value::Bool(false), Value::Text(String::new()), Value::Text("x".repeat(513)), Value::Text("contains spaces".into())] {
        let seed = session();
        let mut project = seed.document().clone();
        let mut node = project.canvases().get(&C).unwrap().nodes().get(&A).unwrap().clone();
        node.params.insert("model".into(), saved);
        Change::SetNode { canvas: C, node }.apply(&mut project).unwrap();
        let mut s = Session::new(project).unwrap();
        let before = s.document().clone();
        let revision = s.revision();
        assert_eq!(s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[A],"model":DESTINATION})).unwrap_err().kind, ErrorKind::InvalidInput);
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
        assert!(!s.can_undo());
    }
    let seed = session();
    let mut project = seed.document().clone();
    let mut node = project.canvases().get(&C).unwrap().nodes().get(&A).unwrap().clone();
    node.params.insert("model".into(), Value::Text("LegacyModel:v2".into()));
    node.config.insert("generation_contract_major".into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
    Change::SetNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let before = s.document().clone();
    assert_eq!(s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[A],"model":DESTINATION})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(s.document(), &before);
    assert!(!s.can_undo());
}
