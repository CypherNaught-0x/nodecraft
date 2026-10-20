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
use nodecraft_types::{
    CanvasId, Choice, Contract, Error, NodeId, OptionKey, OptionLists, OptionSource, ParamControl, ParamOwner, ParamRule, ParamSpec, ProjectId, Value,
};
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([31; 16]);
const N: NodeId = NodeId::from_bytes([32; 16]);
const M: NodeId = NodeId::from_bytes([34; 16]);
const MODEL: &str = "fixture/listed";

fn lists(values: &[&str]) -> OptionLists {
    let mut lists = OptionLists::default();
    let choices = values.iter().map(|value| Choice { value: (*value).into(), label: (*value).into() }).collect();
    lists.set(OptionKey { owner: MODEL, source: "comfyui.checkpoints", scope: None }, choices).unwrap();
    lists
}

fn session(fetched: &[&str]) -> Session {
    let mut model = Catalog::builtin().unwrap().get("comfyui.sdxl").unwrap().clone();
    model.id = MODEL.into();
    model.params.params.insert(
        0,
        ParamSpec {
            name: "checkpoint".into(),
            label: "Checkpoint".into(),
            description: None,
            control: ParamControl::Text { multiline: false, max_bytes: 512 },
            required: false,
            default: None,
            visible_if: None,
        },
    );
    let mut contract = Contract::from_schema(MODEL, ParamOwner::Model, model.params.clone());
    contract.rules.push(ParamRule { options: Some(OptionSource { id: "comfyui.checkpoints".into(), scope: None }), ..ParamRule::implied("checkpoint") });
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
fn a_checkpoint_removed_from_the_server_blocks_only_its_own_edit_and_the_run() {
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
