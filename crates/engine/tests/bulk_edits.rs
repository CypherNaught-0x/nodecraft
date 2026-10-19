use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

fn setup() -> (Session, CanvasId, [NodeId; 2]) {
    let canvas = CanvasId::from_bytes([1; 16]);
    let ids = [NodeId::from_bytes([2; 16]), NodeId::from_bytes([3; 16])];
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([4; 16]), "Bulk example").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    for id in ids {
        let node = kinds.get("source.text").unwrap().descriptor().instantiate(id, &BTreeMap::new()).unwrap();
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    }
    (Session::new(project).unwrap(), canvas, ids)
}
fn request(canvas: CanvasId, ids: &[NodeId], revision: u64, value: Json) -> Json {
    json!({"canvas":canvas,"ids":ids,"revision":revision,"name":"text","value":value})
}
#[test]
fn one_bulk_transaction_has_one_undo_and_redo_and_null_restores_defaults() {
    let (mut session, canvas, ids) = setup();
    let original = session.document().clone();
    let args = request(canvas, &ids, 0, json!(Value::Text("Shared prompt".into())));
    session.enabled("node.params_set_bulk", &args).unwrap();
    assert_eq!(session.revision(), 0);
    session.dispatch("node.params_set_bulk", &args).unwrap();
    assert_eq!(session.revision(), 1);
    let changed = session.document().clone();
    for id in ids {
        assert_eq!(changed.canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().params.get("text"), Some(&Value::Text("Shared prompt".into())));
    }
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &original);
    assert!(!session.can_undo());
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(session.document(), &changed);
    let args = request(canvas, &ids, session.revision(), Json::Null);
    session.dispatch("node.params_set_bulk", &args).unwrap();
    for id in ids {
        assert!(!session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().params.contains_key("text"));
    }
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &changed);
}
#[test]
fn any_bad_target_or_value_refuses_without_document_history_or_revision_change() {
    let (mut session, canvas, ids) = setup();
    let original = session.document().clone();
    let missing = NodeId::from_bytes([9; 16]);
    let valid = json!(Value::Text("Shared".into()));
    let mut absent_value = request(canvas, &ids, 0, valid.clone());
    absent_value.as_object_mut().unwrap().remove("value");
    for args in [
        request(canvas, &[ids[0], missing], 0, valid.clone()),
        request(canvas, &[ids[0], ids[0]], 0, valid.clone()),
        request(canvas, &ids[..1], 0, valid.clone()),
        request(canvas, &vec![ids[0]; 257], 0, valid.clone()),
        request(canvas, &ids, 1, valid.clone()),
        request(canvas, &ids, 0, json!(Value::Bool(true))),
        absent_value,
    ] {
        assert!(session.enabled("node.params_set_bulk", &args).is_err());
        assert!(session.dispatch("node.params_set_bulk", &args).is_err());
        assert_eq!(session.document(), &original);
        assert_eq!(session.revision(), 0);
        assert!(!session.can_undo());
    }
}
#[test]
fn command_schema_requires_explicit_nullable_value_and_bounded_unique_targets() {
    let (session, _, _) = setup();
    let schema = (session.registry().get("node.params_set_bulk").unwrap().input_schema)();
    assert!(schema["required"].as_array().unwrap().contains(&json!("value")));
    assert_eq!(schema["properties"]["ids"]["uniqueItems"], true);
    assert_eq!(schema["properties"]["ids"]["maxItems"], 256);
    assert_eq!(schema["properties"]["revision"]["maximum"], u64::MAX);
    assert!(schema["properties"].get("id").is_none());
}

#[test]
fn hidden_parameters_and_model_migration_bypass_are_refused_atomically() {
    use nodecraft_nodes::{JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{Error, ParamControl, ParamSpec, PortType, VisibleIf};
    struct Conditional(NodeDescriptor);
    impl NodeKind for Conditional {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }
    let (initial, canvas, ids) = setup();
    let mut descriptor = initial.kinds().get("source.text").unwrap().descriptor().clone();
    descriptor.params.params.insert(
        0,
        ParamSpec {
            name: "show".into(),
            label: "Show".into(),
            description: None,
            control: ParamControl::Toggle,
            required: false,
            default: Some(Value::Bool(true)),
            visible_if: None,
        },
    );
    descriptor.params.params.get_mut(1).unwrap().visible_if = Some(VisibleIf { parameter: "show".into(), equals: Value::Bool(true) });
    descriptor.params.params.push(ParamSpec {
        name: "model".into(),
        label: "Model".into(),
        description: None,
        control: ParamControl::Model,
        required: false,
        default: None,
        visible_if: None,
    });
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Conditional(descriptor))).unwrap();
    let mut document = initial.document().clone();
    Change::SetParam { canvas, id: ids[1], name: "show".into(), value: Some(Value::Bool(false)) }.apply(&mut document).unwrap();
    let mut session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let original = session.document().clone();
    let hidden = request(canvas, &ids, 0, json!(Value::Text("Never written".into())));
    let mut model = request(canvas, &ids, 0, json!(Value::Text("example-model".into())));
    model["name"] = json!("model");
    for args in [hidden, model] {
        assert!(session.enabled("node.params_set_bulk", &args).is_err());
        assert!(session.dispatch("node.params_set_bulk", &args).is_err());
        assert_eq!(session.document(), &original);
        assert!(!session.can_undo());
        assert_eq!(session.revision(), 0);
    }
    let mut show = request(canvas, &ids, 0, json!(Value::Bool(true)));
    show["name"] = json!("show");
    session.dispatch("node.params_set_bulk", &show).unwrap();
    session.dispatch("node.params_set_bulk", &request(canvas, &ids, 1, json!(Value::Text("Visible on both".into())))).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &original);
}

#[test]
fn semantic_contract_can_authorize_equal_meaning_across_unknown_instance_configuration() {
    let (initial, canvas, ids) = setup();
    let mut document = initial.document().clone();
    let mut second = document.canvases().get(&canvas).unwrap().nodes().get(&ids[1]).unwrap().clone();
    second.config.insert("source.text.v1.variant".into(), Value::Bool(true));
    Change::SetNode { canvas, node: second }.apply(&mut document).unwrap();
    let mut session = Session::new(document).unwrap();
    let original = session.document().clone();
    let args = request(canvas, &ids, 0, json!(Value::Text("No guessed migration".into())));
    session.enabled("node.params_set_bulk", &args).unwrap();
    session.dispatch("node.params_set_bulk", &args).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &original, "contract-owned compatibility retains unknown configuration");
    assert!(!session.can_undo());
}

#[test]
fn complete_proposed_nodes_obey_kind_owned_cross_field_validation_before_any_publication() {
    use nodecraft_nodes::{JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{Error, ErrorKind, PortType, ResolvedParams};
    struct Guarded(NodeDescriptor);
    impl NodeKind for Guarded {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
        fn validate_instance(&self, node: &nodecraft_graph::Node, ready: bool) -> Result<ResolvedParams, Error> {
            if node.config.contains_key("source.text.v1.reject") && node.params.get("text") == Some(&Value::Text("Rejected by kind".into())) {
                return Err(Error::new(ErrorKind::InvalidInput, "kind-specific parameter constraint"));
            }
            self.0.params.resolve(&node.params, ready)
        }
    }
    let (initial, canvas, ids) = setup();
    let descriptor = initial.kinds().get("source.text").unwrap().descriptor().clone();
    let mut document = initial.document().clone();
    // Equal configuration keeps this test inside the conservative adapter's supported seam.
    for id in ids {
        let mut node = document.canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().clone();
        node.config.insert("source.text.v1.reject".into(), Value::Bool(true));
        Change::SetNode { canvas, node }.apply(&mut document).unwrap();
    }
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Guarded(descriptor))).unwrap();
    let mut session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let original = session.document().clone();
    let args = request(canvas, &ids, 0, json!(Value::Text("Rejected by kind".into())));
    assert!(session.enabled("node.params_set_bulk", &args).is_err());
    assert!(session.dispatch("node.params_set_bulk", &args).is_err());
    assert_eq!(session.document(), &original);
    assert_eq!(session.revision(), 0);
    assert!(!session.can_undo());
}

#[test]
fn persistent_bulk_edit_replays_as_one_frame_and_append_failure_publishes_nothing() {
    use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation, journal};
    use nodecraft_types::{Error, ErrorKind};
    use std::{cell::RefCell, rc::Rc};
    #[derive(Default)]
    struct State {
        memory: MemoryStorage,
        fail: bool,
        appends: usize,
    }
    #[derive(Clone)]
    struct Storage(Rc<RefCell<State>>);
    impl ProjectStorage for Storage {
        fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
            self.0.borrow_mut().memory.load(location)
        }
        fn save(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<(), Error> {
            self.0.borrow_mut().memory.save(location, bytes)
        }
        fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
            self.0.borrow_mut().memory.checkpoint(bytes)
        }
        fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
            let mut state = self.0.borrow_mut();
            state.appends += 1;
            if state.fail {
                return Err(Error::new(ErrorKind::Conflict, "injected append refusal"));
            }
            state.memory.append(bytes)
        }
    }
    for fail in [false, true] {
        let (initial, canvas, ids) = setup();
        let original = initial.document().clone();
        let storage = Storage(Rc::new(RefCell::new(State::default())));
        let mut session =
            Session::with_storage(Snapshot { project: original.clone(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
        let recovery = storage.0.borrow().memory.recovery.clone();
        let invalid = request(canvas, &[ids[0], NodeId::from_bytes([9; 16])], 0, json!(Value::Text("No partial edit".into())));
        assert!(session.dispatch("node.params_set_bulk", &invalid).is_err());
        assert_eq!(storage.0.borrow().appends, 0);
        assert_eq!(storage.0.borrow().memory.recovery, recovery);
        storage.0.borrow_mut().fail = fail;
        let args = request(canvas, &ids, 0, json!(Value::Text("One persisted bulk edit".into())));
        let result = session.dispatch("node.params_set_bulk", &args);
        let recovered = journal::recover(&storage.0.borrow().memory.recovery).unwrap();
        assert_eq!(storage.0.borrow().appends, 1);
        if fail {
            assert!(result.is_err());
            assert_eq!(session.document(), &original);
            assert_eq!(session.revision(), 0);
            assert!(!session.can_undo());
            assert!(session.events_since(0).unwrap().is_empty());
            assert_eq!(recovered.applied, 0);
            assert_eq!(recovered.snapshot.project, original);
        } else {
            result.unwrap();
            assert_eq!(recovered.applied, 1);
            assert_eq!(recovered.snapshot.project, *session.document());
            assert_eq!(session.events_since(0).unwrap().len(), 1);
            session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(session.document(), &original);
        }
    }
}

#[test]
fn contract_enabled_checks_and_hidden_resets_are_atomic_with_the_controlling_edit() {
    use nodecraft_nodes::{JobSpec, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{Check, Condition, Error, HiddenPolicy, ParamControl, ParamRule, ParamSpec, PortType};
    struct Conditional(NodeDescriptor);
    impl NodeKind for Conditional {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
        fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
            let mut c = NodeContract::derive(descriptor);
            c.params.rules.push(ParamRule {
                enabled_when: Some(Condition::Equals { param: "show".into(), value: Value::Bool(true) }),
                hidden: HiddenPolicy::Reset,
                ..ParamRule::implied("text")
            });
            c.params.checks.push(Check {
                id: "allowed_text".into(),
                when: Some(Condition::Equals { param: "show".into(), value: Value::Bool(true) }),
                require: Condition::Not { condition: Box::new(Condition::Equals { param: "text".into(), value: Value::Text("Forbidden".into()) }) },
                message: "Text is forbidden".into(),
            });
            c
        }
    }
    let (initial, canvas, ids) = setup();
    let mut descriptor = initial.kinds().get("source.text").unwrap().descriptor().clone();
    descriptor.params.params.insert(
        0,
        ParamSpec {
            name: "show".into(),
            label: "Show".into(),
            description: None,
            control: ParamControl::Toggle,
            required: false,
            default: Some(Value::Bool(true)),
            visible_if: None,
        },
    );
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Conditional(descriptor))).unwrap();
    let mut session = Session::with_registries(initial.document().clone(), nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let original = session.document().clone();
    let bad = request(canvas, &ids, 0, json!(Value::Text("Forbidden".into())));
    assert!(session.dispatch("node.params_set_bulk", &bad).is_err(), "Ok Resolution with issues must refuse publication");
    assert_eq!(session.document(), &original);
    assert_eq!(session.revision(), 0);
    let mut hide = request(canvas, &ids, 0, json!(Value::Bool(false)));
    hide["name"] = json!("show");
    session.dispatch("node.params_set_bulk", &hide).unwrap();
    for id in ids {
        assert!(
            !session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().params.contains_key("text"),
            "disabled reset is part of the controlling transaction"
        );
    }
    let hidden = session.document().clone();
    assert!(session.dispatch("node.params_set_bulk", &request(canvas, &ids, 1, json!(Value::Text("Disabled".into())))).is_err());
    assert_eq!(session.document(), &hidden);
    assert_eq!(session.revision(), 1);
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &original);
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(session.document(), &hidden);
}

#[test]
fn semantic_intersection_admits_different_descriptors_but_refuses_unit_aliases_and_one_target_bounds() {
    use nodecraft_nodes::{JobSpec, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{Error, Integer, ParamControl, ParamRule, PortType, Unit, Unsigned};
    use std::borrow::Cow;
    struct Dynamic(NodeDescriptor);
    impl NodeKind for Dynamic {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
        fn instance<'a>(&'a self, node: &nodecraft_graph::Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            let mut descriptor = self.0.clone();
            if node.config.contains_key("source.text.v1.other") {
                descriptor.label = "Same kind, another descriptor".into();
                descriptor.params.params[0].control = ParamControl::Integer { min: Some(Integer(5)), max: Some(Integer(20)), step: Unsigned(1) };
                descriptor.params.params[0].default = Some(Value::Integer(Integer(5)));
            }
            Ok(Cow::Owned(descriptor))
        }
        fn instance_contract(&self, node: &nodecraft_graph::Node) -> Result<NodeContract, Error> {
            let descriptor = self.resolve_instance(node)?;
            let mut contract = NodeContract::derive(&descriptor);
            contract.params.rules.push(ParamRule {
                unit: if node.config.contains_key("source.text.v1.seconds") { Unit::Seconds } else { Unit::Frames },
                ..ParamRule::implied("text")
            });
            contract.validate(&descriptor)?;
            Ok(contract)
        }
    }
    for differing_units in [false, true] {
        let (initial, canvas, ids) = setup();
        let mut descriptor = initial.kinds().get("source.text").unwrap().descriptor().clone();
        descriptor.params.params[0].control = ParamControl::Integer { min: Some(Integer(0)), max: Some(Integer(10)), step: Unsigned(1) };
        descriptor.params.params[0].default = Some(Value::Integer(Integer(0)));
        let mut document = initial.document().clone();
        for (index, id) in ids.into_iter().enumerate() {
            let mut node = document.canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().clone();
            node.params.insert("text".into(), Value::Integer(Integer(5)));
            if index == 1 {
                node.config.insert("source.text.v1.other".into(), Value::Bool(true));
                if differing_units {
                    node.config.insert("source.text.v1.seconds".into(), Value::Bool(true));
                }
            }
            Change::SetNode { canvas, node }.apply(&mut document).unwrap();
        }
        let mut kinds = nodecraft_nodes::Registry::default();
        kinds.register(Box::new(Dynamic(descriptor))).unwrap();
        let mut session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let original = session.document().clone();
        let out_of_one_domain = request(canvas, &ids, 0, json!(Value::Integer(Integer(3))));
        assert!(session.dispatch("node.params_set_bulk", &out_of_one_domain).is_err());
        assert_eq!(session.document(), &original);
        assert_eq!(session.revision(), 0);
        let args = request(canvas, &ids, 0, json!(Value::Integer(Integer(7))));
        if differing_units {
            assert!(session.dispatch("node.params_set_bulk", &args).is_err());
            assert_eq!(session.document(), &original);
            assert!(!session.can_undo());
        } else {
            session.dispatch("node.params_set_bulk", &args).unwrap();
            session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(session.document(), &original);
        }
    }
}

#[test]
fn borrowed_bulk_preflight_refuses_deep_and_oversized_caller_payload_before_publication() {
    let (mut session, canvas, ids) = setup();
    let original = session.document().clone();
    let mut deep = json!({"type":"text","value":"bottom"});
    for _ in 0..24 {
        deep = json!({"type":"list","value":[deep]});
    }
    for value in [deep, json!({"type":"text","value":"x".repeat(1024*1024+1)})] {
        let args = request(canvas, &ids, 0, value);
        assert!(session.enabled("node.params_set_bulk", &args).is_err());
        assert!(session.dispatch("node.params_set_bulk", &args).is_err());
        assert_eq!(session.document(), &original);
        assert_eq!(session.revision(), 0);
        assert!(!session.can_undo());
        assert!(session.events_since(0).unwrap().is_empty());
    }
}

#[test]
fn model_owned_parameter_intersection_requires_real_cross_owner_meaning_and_units() {
    use nodecraft_nodes::{JobSpec, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{Contract, Error, ParamControl, ParamOwner, ParamRule, ParamSchema, ParamSpec, PortType, Unit, Unsigned};
    use std::borrow::Cow;
    struct Owned {
        descriptor: NodeDescriptor,
        tagged: bool,
        mismatched_units: bool,
    }
    impl NodeKind for Owned {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.descriptor
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
        fn instance<'a>(&'a self, node: &nodecraft_graph::Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            let mut descriptor = self.descriptor.clone();
            if node.config.contains_key("source.text.v1.other") {
                descriptor.params.params[1].control = ParamControl::Unsigned { min: Some(Unsigned(5)), max: Some(Unsigned(20)), step: Unsigned(1) };
                descriptor.params.params[1].default = Some(Value::Unsigned(Unsigned(5)));
            }
            Ok(Cow::Owned(descriptor))
        }
        fn instance_contract(&self, node: &nodecraft_graph::Node) -> Result<NodeContract, Error> {
            let descriptor = self.resolve_instance(node)?;
            let other = node.config.contains_key("source.text.v1.other");
            let mut c = NodeContract::derive(&descriptor);
            let model_spec = c.params.schema.params.pop().unwrap();
            let mut group = Contract::from_schema(
                if other { "provider/model-two" } else { "provider/model-one" },
                ParamOwner::Model,
                ParamSchema { params: vec![model_spec] },
            );
            group.rules.push(ParamRule {
                unit: if other && self.mismatched_units { Unit::Seconds } else { Unit::Pixels },
                meaning: self.tagged.then(|| "width".into()),
                ..ParamRule::implied("size")
            });
            c.owned_groups.push(group);
            c.validate(&descriptor)?;
            Ok(c)
        }
    }
    for (tagged, mismatched_units) in [(true, false), (false, false), (true, true)] {
        let (initial, canvas, ids) = setup();
        let mut descriptor = initial.kinds().get("source.text").unwrap().descriptor().clone();
        descriptor.params.params.push(ParamSpec {
            name: "size".into(),
            label: "Size".into(),
            description: None,
            control: ParamControl::Unsigned { min: Some(Unsigned(0)), max: Some(Unsigned(10)), step: Unsigned(1) },
            required: false,
            default: Some(Value::Unsigned(Unsigned(0))),
            visible_if: None,
        });
        let mut document = initial.document().clone();
        let mut second = document.canvases().get(&canvas).unwrap().nodes().get(&ids[1]).unwrap().clone();
        second.config.insert("source.text.v1.other".into(), Value::Bool(true));
        Change::SetNode { canvas, node: second }.apply(&mut document).unwrap();
        let mut kinds = nodecraft_nodes::Registry::default();
        kinds.register(Box::new(Owned { descriptor, tagged, mismatched_units })).unwrap();
        let mut session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let original = session.document().clone();
        let mut args = request(canvas, &ids, 0, json!(Value::Unsigned(Unsigned(8))));
        args["name"] = json!("size");
        let contracts: Vec<_> = ids
            .iter()
            .map(|id| {
                session
                    .kinds()
                    .get("source.text")
                    .unwrap()
                    .instance_contract(session.document().canvases().get(&canvas).unwrap().nodes().get(id).unwrap())
                    .unwrap()
            })
            .collect();
        let specs = nodecraft_engine::bulk_cmds::shared_node_specs(&contracts.iter().collect::<Vec<_>>());
        assert_eq!(specs.iter().any(|spec| spec.name == "size"), tagged && !mismatched_units);
        if tagged && !mismatched_units {
            session.dispatch("node.params_set_bulk", &args).unwrap();
            assert_eq!(session.revision(), 1);
            for id in ids {
                assert_eq!(
                    session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().params.get("size"),
                    Some(&Value::Unsigned(Unsigned(8)))
                );
            }
            session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(session.document(), &original);
        } else {
            assert!(session.dispatch("node.params_set_bulk", &args).is_err());
            assert_eq!(session.document(), &original);
            assert!(!session.can_undo());
        }
    }
}

#[test]
fn real_registered_mixed_models_share_node_prompt_without_guessing_model_parameter_meanings() {
    let canvas = CanvasId::from_bytes([81; 16]);
    let ids = [NodeId::from_bytes([82; 16]), NodeId::from_bytes([83; 16])];
    let mut document = Project::new(ProjectId::from_bytes([84; 16]), "Public mixed image models").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let initial = Session::new(document.clone()).unwrap();
    let models = ["openai-images.sunburst", "openai-images.flare"];
    for (id, model) in ids.into_iter().zip(models) {
        let values = BTreeMap::from([("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text(format!("A public scene for {model}")))]);
        let node = initial.kinds().get("generation.image").unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        Change::AddNode { canvas, node }.apply(&mut document).unwrap();
    }
    let mut session = Session::new(document).unwrap();
    let original = session.document().clone();
    let contracts: Vec<_> = ids
        .iter()
        .map(|id| {
            session
                .kinds()
                .get("generation.image")
                .unwrap()
                .instance_contract(session.document().canvases().get(&canvas).unwrap().nodes().get(id).unwrap())
                .unwrap()
        })
        .collect();
    let shared = nodecraft_engine::bulk_cmds::shared_node_specs(&contracts.iter().collect::<Vec<_>>());
    assert!(shared.iter().any(|spec| spec.name == "prompt"));
    assert!(!shared.iter().any(|spec| spec.name == "quality"), "matching catalog labels do not establish model-owned semantic compatibility");
    let mut args = request(canvas, &ids, 0, json!(Value::Text("A quiet lake at sunset".into())));
    args["name"] = json!("prompt");
    session.dispatch("node.params_set_bulk", &args).unwrap();
    assert_eq!(session.revision(), 1);
    for (id, model) in ids.into_iter().zip(models) {
        let node = session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
        assert_eq!(node.params.get("prompt"), Some(&Value::Text("A quiet lake at sunset".into())));
        assert_eq!(node.params.get("model"), Some(&Value::Text(model.into())));
    }
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &original);
    let revision = session.revision();
    args["revision"] = json!(revision);
    args["name"] = json!("quality");
    args["value"] = json!(Value::Text("high".into()));
    assert!(session.dispatch("node.params_set_bulk", &args).is_err());
    assert_eq!(session.document(), &original);
    assert_eq!(session.revision(), revision);
    assert!(session.can_redo());
}

#[test]
fn production_catalog_owned_groups_share_tagged_domains_conditions_and_atomic_resets() {
    use nodecraft_catalog::Catalog;
    use nodecraft_engine::{generation_catalog::GenerationCatalog, run_cmds::RunCatalog};
    use nodecraft_types::{Check, Condition, Contract, HiddenPolicy, ParamControl, ParamOwner, ParamRule, Unsigned};
    let builtin = Catalog::builtin().unwrap();
    let mut models = Vec::new();
    for (index, id) in ["openai-images.sunburst", "openai-images.flare"].into_iter().enumerate() {
        let mut model = builtin.get(id).unwrap().clone();
        let n = model.params.params.remove(model.params.params.iter().position(|spec| spec.name == "n").unwrap());
        model.params.params.insert(0, n);
        if index == 1 {
            model.params.params.iter_mut().find(|spec| spec.name == "n").unwrap().control =
                ParamControl::Unsigned { min: Some(Unsigned(1)), max: Some(Unsigned(4)), step: Unsigned(1) };
            let quality = model.params.params.iter_mut().find(|spec| spec.name == "quality").unwrap();
            let ParamControl::Choice { options } = &mut quality.control else { panic!("fixture quality is a choice") };
            options.retain(|option| ["medium", "high"].contains(&option.value.as_str()));
            quality.default = Some(Value::Text("medium".into()));
        }
        let mut contract = Contract::from_schema(model.id.clone(), ParamOwner::Model, model.params.clone());
        contract.rules.push(ParamRule { meaning: Some("image_count".into()), unit: nodecraft_types::Unit::Count, ..ParamRule::implied("n") });
        contract.rules.push(ParamRule {
            meaning: Some("render_quality".into()),
            enabled_when: Some(Condition::Equals { param: "n".into(), value: Value::Unsigned(Unsigned(1)) }),
            hidden: HiddenPolicy::Reset,
            ..ParamRule::implied("quality")
        });
        contract.checks.push(Check {
            id: "fixture_count".into(),
            when: None,
            require: Condition::In { param: "n".into(), values: vec![Value::Unsigned(Unsigned(1)), Value::Unsigned(Unsigned(2))] },
            message: "Fixture count accepts one or two".into(),
        });
        model.parameter_contract = Some(contract);
        model.validate().unwrap();
        models.push(model);
    }
    let snapshot = GenerationCatalog::new(RunCatalog::new(Catalog::from_models(models).unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let canvas = CanvasId::from_bytes([91; 16]);
    let ids = [NodeId::from_bytes([92; 16]), NodeId::from_bytes([93; 16])];
    let mut document = Project::new(ProjectId::from_bytes([94; 16]), "Explicitly tagged production catalog").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    for (id, model) in ids.into_iter().zip(["openai-images.sunburst", "openai-images.flare"]) {
        let values = BTreeMap::from([("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("A public test scene".into()))]);
        let node = kinds.get("generation.image").unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        Change::AddNode { canvas, node }.apply(&mut document).unwrap();
    }
    let mut session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    session.swap_resource(Some(snapshot.run_catalog()));
    session.swap_resource(Some(snapshot));
    let original = session.document().clone();
    let contracts: Vec<_> = ids
        .iter()
        .map(|id| {
            session
                .kinds()
                .get("generation.image")
                .unwrap()
                .instance_contract(session.document().canvases().get(&canvas).unwrap().nodes().get(id).unwrap())
                .unwrap()
        })
        .collect();
    let specs = nodecraft_engine::bulk_cmds::shared_node_specs(&contracts.iter().collect::<Vec<_>>());
    assert_eq!(
        specs.iter().find(|spec| spec.name == "n").unwrap().control,
        ParamControl::Unsigned { min: Some(Unsigned(1)), max: Some(Unsigned(4)), step: Unsigned(1) }
    );
    let ParamControl::Choice { options } = &specs.iter().find(|spec| spec.name == "quality").unwrap().control else { panic!("shared quality is a choice") };
    assert_eq!(options.iter().map(|option| option.value.as_str()).collect::<Vec<_>>(), vec!["medium", "high"]);
    let mut args = request(canvas, &ids, 0, json!(Value::Unsigned(Unsigned(3))));
    args["name"] = json!("n");
    assert!(session.dispatch("node.params_set_bulk", &args).is_err());
    assert_eq!(session.document(), &original);
    assert_eq!(session.revision(), 0);
    args["value"] = json!(Value::Unsigned(Unsigned(2)));
    session.dispatch("node.params_set_bulk", &args).unwrap();
    assert_eq!(session.revision(), 1);
    for id in ids {
        assert!(!session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().params.contains_key("quality"));
    }
    let reset = session.document().clone();
    args["revision"] = json!(1);
    args["name"] = json!("quality");
    args["value"] = json!(Value::Text("high".into()));
    assert!(session.dispatch("node.params_set_bulk", &args).is_err());
    assert_eq!(session.document(), &reset);
    assert_eq!(session.revision(), 1);
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &original);
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(session.document(), &reset);
}

#[test]
fn current_catalog_pair_cannot_reauthorize_old_installed_generation_definitions() {
    use nodecraft_engine::generation_catalog::GenerationCatalog;
    let canvas = CanvasId::from_bytes([111; 16]);
    let ids = [NodeId::from_bytes([112; 16]), NodeId::from_bytes([113; 16])];
    let mut document = Project::new(ProjectId::from_bytes([114; 16]), "Catalog freshness").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let initial = Session::new(document.clone()).unwrap();
    for id in ids {
        let values =
            BTreeMap::from([("model".into(), Value::Text("openai-images.sunburst".into())), ("prompt".into(), Value::Text("Original public prompt".into()))]);
        let node = initial.kinds().get("generation.image").unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        Change::AddNode { canvas, node }.apply(&mut document).unwrap();
    }
    let mut session = Session::new(document).unwrap();
    let original = session.document().clone();
    let old = session.generation_catalog().unwrap().clone();
    let replacement = GenerationCatalog::builtin().unwrap();
    assert_ne!(old.stamp(), replacement.stamp());
    session.swap_resource(Some(replacement.run_catalog()));
    session.swap_resource(Some(replacement));
    let mut args = request(canvas, &ids, 0, json!(Value::Text("Must refuse".into())));
    args["name"] = json!("prompt");
    assert!(session.enabled("node.params_set_bulk", &args).is_err());
    assert!(session.dispatch("node.params_set_bulk", &args).is_err());
    assert_eq!(session.document(), &original);
    assert_eq!(session.revision(), 0);
    assert!(!session.can_undo());
    assert!(session.events_since(0).unwrap().is_empty());
    session.swap_resource(Some(old.run_catalog()));
    session.swap_resource(Some(old));
    session.dispatch("node.params_set_bulk", &args).unwrap();
    assert_eq!(session.revision(), 1);
}

#[test]
fn unrelated_generation_authority_does_not_block_source_only_bulk_admission() {
    use nodecraft_engine::generation_catalog::GenerationCatalog;
    let (initial, canvas, ids) = setup();
    let mut document = initial.document().clone();
    let values =
        BTreeMap::from([("model".into(), Value::Text("openai-images.sunburst".into())), ("prompt".into(), Value::Text("Unselected image node".into()))]);
    let node = initial.kinds().get("generation.image").unwrap().instantiate_instance(NodeId::from_bytes([115; 16]), Default::default(), &values).unwrap();
    Change::AddNode { canvas, node }.apply(&mut document).unwrap();
    let mut session = Session::new(document).unwrap();
    let replacement = GenerationCatalog::builtin().unwrap();
    session.swap_resource(Some(replacement.run_catalog()));
    session.swap_resource(Some(replacement));
    session.dispatch("node.params_set_bulk", &request(canvas, &ids, 0, json!(Value::Text("Selected source prompts".into())))).unwrap();
    assert_eq!(session.revision(), 1);
}
