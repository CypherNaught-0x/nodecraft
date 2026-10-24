//! Run-time option lists on a catalog generation node (#162): a model's value is checked
//! against the list the host fetched, through the same edit, bulk and describe paths.
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources},
    generation_catalog::GenerationCatalog,
    run_cmds::RunCatalog,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_types::{CanvasId, Choice, Contract, Error, NodeId, OptionKey, OptionLists, OptionSource, ParamOwner, ParamRule, ProjectId, Value};
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([31; 16]);
const N: NodeId = NodeId::from_bytes([32; 16]);
const M: NodeId = NodeId::from_bytes([34; 16]);
const MODEL: &str = "fixture/listed";

fn lists(values: &[&str]) -> OptionLists {
    let mut lists = OptionLists::default();
    let choices = values.iter().map(|value| Choice { value: (*value).into(), label: (*value).into() }).collect();
    lists.set(OptionKey { owner: MODEL, source: "comfyui.checkpoints", scope: None }, choices).unwrap();
    // An unscoped list under the scoped source's id, which a node without a checkpoint must not
    // be offered.
    lists.set(OptionKey { owner: MODEL, source: "comfyui.loras", scope: None }, vec![Choice { value: "unscoped".into(), label: "Unscoped".into() }]).unwrap();
    lists
}

fn session(fetched: &[&str]) -> Session {
    let mut model = Catalog::builtin().unwrap().get("comfyui.sdxl").unwrap().clone();
    model.id = MODEL.into();
    // The built-in SDXL row takes a checkpoint (#529); add a LoRA narrowed by it.
    assert_eq!(model.params.params[0].name, "checkpoint");
    let mut lora = model.params.params[0].clone();
    lora.name = "lora".into();
    lora.label = "LoRA".into();
    model.params.params.insert(1, lora);
    let mut contract = Contract::from_schema(MODEL, ParamOwner::Model, model.params.clone());
    contract.rules.push(ParamRule { options: Some(OptionSource { id: "comfyui.checkpoints".into(), scope: None }), ..ParamRule::implied("checkpoint") });
    contract
        .rules
        .push(ParamRule { options: Some(OptionSource { id: "comfyui.loras".into(), scope: Some("checkpoint".into()) }), ..ParamRule::implied("lora") });
    model.parameter_contract = Some(contract);
    let snapshot = GenerationCatalog::new(RunCatalog::new(Catalog::from_models([model]).unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([33; 16]), "Option sources").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for id in [N, M] {
        let node = kinds
            .get(KIND)
            .unwrap()
            .instantiate_instance(
                id,
                Default::default(),
                &[("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("A lighthouse".into()))].into(),
            )
            .unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.run_catalog());
    resources.insert(snapshot);
    resources.insert(lists(fetched));
    Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources)
}

fn checkpoint(session: &mut Session, id: NodeId, value: &str) -> Result<serde_json::Value, Error> {
    session.dispatch("node.param_set", &json!({"canvas": C, "id": id, "name": "checkpoint", "value": {"type": "text", "value": value}}))
}

#[test]
fn a_generation_value_must_come_from_the_fetched_list() {
    let mut session = session(&["sdxl.safetensors", "juggernaut.safetensors"]);
    let refused = checkpoint(&mut session, N, "missing.safetensors").unwrap_err();
    assert!(refused.message.contains("not one of the available options"), "{}", refused.message);
    checkpoint(&mut session, N, "juggernaut.safetensors").unwrap();
    let listed = session.dispatch("node.options", &json!({"canvas": C, "id": N, "param": "checkpoint"})).unwrap();
    assert_eq!(listed["source"], "comfyui.checkpoints");
    assert_eq!(listed["options"][1]["value"], "juggernaut.safetensors");
}

#[test]
fn a_checkpoint_removed_from_the_server_blocks_only_setting_it() {
    let mut session = session(&["old.safetensors"]);
    checkpoint(&mut session, N, "old.safetensors").unwrap();
    checkpoint(&mut session, M, "old.safetensors").unwrap();
    session.swap_resource(Some(lists(&["new.safetensors"])));
    session.dispatch("node.param_set", &json!({"canvas": C, "id": N, "name": "steps", "value": {"type": "unsigned", "value": "12"}})).unwrap();
    let revision = session.revision();
    session
        .dispatch(
            "node.params_set_bulk",
            &json!({"canvas": C, "ids": [N, M], "revision": revision, "name": "steps", "value": {"type": "unsigned", "value": "14"}}),
        )
        .unwrap();
    let described = session.dispatch("node.describe", &json!({"canvas": C, "id": N})).unwrap();
    let issues = described["resolution"]["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| issue["code"] == "not_an_option" && issue["param"] == "checkpoint"), "{issues:?}");
    assert!(checkpoint(&mut session, N, "old.safetensors").is_err());
    checkpoint(&mut session, N, "new.safetensors").unwrap();
}

#[test]
fn a_stale_value_can_be_cleared_and_bulk_refuses_setting_one() {
    let mut session = session(&["old.safetensors"]);
    checkpoint(&mut session, N, "old.safetensors").unwrap();
    checkpoint(&mut session, M, "old.safetensors").unwrap();
    session.swap_resource(Some(lists(&["new.safetensors"])));
    session.dispatch("node.param_set", &json!({"canvas": C, "id": N, "name": "checkpoint", "value": null})).unwrap();
    let set = |value: &str| json!({"canvas": C, "ids": [N, M], "revision": 0, "name": "checkpoint", "value": {"type": "text", "value": value}});
    let mut stale = set("old.safetensors");
    stale["revision"] = json!(session.revision());
    assert!(session.dispatch("node.params_set_bulk", &stale).is_err());
    let mut fresh = set("new.safetensors");
    fresh["revision"] = json!(session.revision());
    session.dispatch("node.params_set_bulk", &fresh).unwrap();
}

#[test]
fn a_scoped_source_without_its_scope_value_offers_no_list() {
    let mut session = session(&["sdxl.safetensors"]);
    let listed = session.dispatch("node.options", &json!({"canvas": C, "id": N, "param": "lora"})).unwrap();
    assert_eq!((listed["source"].as_str(), listed["known"].as_bool()), (Some("comfyui.loras"), Some(false)), "{listed}");
    assert!(listed["options"].is_null(), "the unscoped list of the same id is not this node's list");
}
