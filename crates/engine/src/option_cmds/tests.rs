use crate::{Session, commands::Registry};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{
    CanvasId, Choice, Error, ErrorKind, NodeId, OptionKey, OptionLists, OptionSource, ParamControl, ParamRule, ParamSchema, ParamSpec, PortType, ProjectId,
};
use serde_json::{Value, json};

/// A ComfyUI-like kind: `checkpoint` comes from the server's list, `note` is free text.
struct Listed(NodeDescriptor);
impl NodeKind for Listed {
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
        let mut contract = NodeContract::derive(descriptor);
        contract
            .params
            .rules
            .push(ParamRule { options: Some(OptionSource { id: "comfyui.checkpoints".into(), scope: None }), ..ParamRule::implied("checkpoint") });
        contract
    }
}

fn line(name: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: name.into(),
        description: None,
        control: ParamControl::Text { multiline: false, max_bytes: 256 },
        required: false,
        default: None,
        visible_if: None,
    }
}

fn session() -> (Session, CanvasId, NodeId) {
    let canvas = CanvasId::from_bytes([1; 16]);
    let node = NodeId::from_bytes([2; 16]);
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds
        .register(Box::new(Listed(NodeDescriptor {
            id: "test.listed".into(),
            label: "Listed".into(),
            category: Category::Generation,
            inputs: vec![],
            outputs: vec![],
            quick_add: None,
            params: ParamSchema { params: vec![line("checkpoint"), line("note")] },
        })))
        .unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Options").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas, node: Node::new(node, "test.listed") }.apply(&mut project).unwrap();
    (Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap(), canvas, node)
}

fn lists(values: &[&str]) -> OptionLists {
    let mut lists = OptionLists::default();
    let choices = values.iter().map(|value| Choice { value: (*value).into(), label: value.to_uppercase() }).collect();
    lists.set(OptionKey { owner: "test.listed", source: "comfyui.checkpoints", scope: None }, choices).unwrap();
    lists
}

fn set(session: &mut Session, canvas: CanvasId, node: NodeId, name: &str, value: &str) -> Result<Value, Error> {
    session.dispatch("node.param_set", &json!({"canvas": canvas, "id": node, "name": name, "value": {"type": "text", "value": value}}))
}

#[test]
fn an_unknown_list_accepts_any_value_and_says_it_is_unknown() {
    let (mut session, canvas, node) = session();
    set(&mut session, canvas, node, "checkpoint", "anything.safetensors").unwrap();
    let listed = session.dispatch("node.options", &json!({"canvas": canvas, "id": node, "param": "checkpoint"})).unwrap();
    assert_eq!(listed, json!({"source": "comfyui.checkpoints", "scope": null, "known": false, "options": null}));
    let free = session.dispatch("node.options", &json!({"canvas": canvas, "id": node, "param": "note"})).unwrap();
    assert_eq!(free, json!({"source": null, "scope": null, "known": false, "options": null}));
}

#[test]
fn a_known_list_refuses_other_values_and_is_listed() {
    let (mut session, canvas, node) = session();
    session.swap_resource(Some(lists(&["sdxl.safetensors", "flux.safetensors"])));
    let refused = set(&mut session, canvas, node, "checkpoint", "missing.safetensors").unwrap_err();
    assert_eq!(refused.kind, ErrorKind::InvalidInput);
    assert!(refused.message.contains("not one of the available options"), "{}", refused.message);
    set(&mut session, canvas, node, "checkpoint", "flux.safetensors").unwrap();
    let listed = session.dispatch("node.options", &json!({"canvas": canvas, "id": node, "param": "checkpoint"})).unwrap();
    assert_eq!(listed["known"], true);
    assert_eq!(
        listed["options"],
        json!([{"value": "sdxl.safetensors", "label": "SDXL.SAFETENSORS"}, {"value": "flux.safetensors", "label": "FLUX.SAFETENSORS"}])
    );
}

#[test]
fn a_value_dropped_from_the_list_blocks_a_run_but_not_other_edits() {
    let (mut session, canvas, node) = session();
    set(&mut session, canvas, node, "checkpoint", "old.safetensors").unwrap();
    session.swap_resource(Some(lists(&["new.safetensors"])));
    set(&mut session, canvas, node, "note", "still editable").unwrap();
    let described = session.dispatch("node.describe", &json!({"canvas": canvas, "id": node})).unwrap();
    assert_eq!(described["resolution"]["issues"][0]["code"], "not_an_option");
    assert_eq!(described["resolution"]["issues"][0]["param"], "checkpoint");
    assert_eq!(described["resolution"]["params"][0]["options"], json!({"source": "comfyui.checkpoints", "known": true}));
    assert!(set(&mut session, canvas, node, "checkpoint", "old.safetensors").is_err(), "the stale value cannot be set again");
    session.dispatch("node.param_set", &json!({"canvas": canvas, "id": node, "name": "checkpoint", "value": null})).unwrap();
}

#[test]
fn listing_is_read_only_and_refuses_bad_targets() {
    let (mut session, canvas, node) = session();
    session.swap_resource(Some(lists(&["a"])));
    let revision = session.revision();
    for (params, kind) in [
        (json!({"canvas": canvas, "id": node, "param": "nope"}), ErrorKind::NotFound),
        (json!({"canvas": canvas, "id": NodeId::from_bytes([9; 16]), "param": "checkpoint"}), ErrorKind::NotFound),
        (json!({"canvas": canvas, "id": node}), ErrorKind::InvalidInput),
        (json!({"canvas": canvas, "id": node, "param": "checkpoint", "extra": 1}), ErrorKind::InvalidInput),
    ] {
        assert_eq!(session.dispatch("node.options", &params).unwrap_err().kind, kind, "{params}");
    }
    session.dispatch("node.options", &json!({"canvas": canvas, "id": node, "param": "checkpoint"})).unwrap();
    assert_eq!(session.revision(), revision);
}
