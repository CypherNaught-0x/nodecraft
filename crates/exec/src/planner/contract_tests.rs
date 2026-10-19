//! Planning and staleness resolve through the node's contract (#162): a disabled value is never
//! submitted nor part of a result's identity, and a failed check blocks the run but not edits.

use super::plan as plan_run;
use super::*;
use crate::staleness::input_hash;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::{NodeContract, NodeDescriptor, NodeKind};
use nodecraft_types::{CanvasId, Check, Choice, Condition, HiddenPolicy, ParamControl, ParamRule, ParamSchema, ParamSpec, ProjectId};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const KIND: &str = "test.ruled";

/// `detail` applies only when `style` is fine, and may never be "bad".
struct Ruled(NodeDescriptor);
impl NodeKind for Ruled {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
        let mut contract = NodeContract::derive(descriptor);
        let fine = Condition::Equals { param: "style".into(), value: Value::Text("fine".into()) };
        contract.params.rules = vec![ParamRule { enabled_when: Some(fine), hidden: HiddenPolicy::Reset, ..ParamRule::implied("detail") }];
        contract.params.checks = vec![Check {
            id: "not_bad".into(),
            when: None,
            require: Condition::Not { condition: Box::new(Condition::Equals { param: "detail".into(), value: Value::Text("bad".into()) }) },
            message: "detail must not be bad".into(),
        }];
        contract
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("ruled").ok()
    }
    /// Echoes what it would submit, so tests see exactly that.
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let sent = self.validate_instance(ctx.node, true)?.into_visible_values();
        let echo = sent.iter().map(|(name, value)| format!("{name}={value:?}")).collect::<Vec<_>>().join(";");
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text(echo) }])
    }
}

fn spec(name: &str, control: ParamControl, default: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: name.into(),
        description: None,
        control,
        required: false,
        default: Some(Value::Text(default.into())),
        visible_if: None,
    }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let style = ParamControl::Choice { options: ["plain", "fine"].iter().map(|v| Choice { value: (*v).into(), label: (*v).into() }).collect() };
    let descriptor = NodeDescriptor {
        id: KIND.into(),
        label: "Ruled".into(),
        category: Category::Generation,
        inputs: Vec::new(),
        outputs: vec![nodecraft_graph::Port { name: "out".into(), port_type: PortType { modality: nodecraft_types::Modality::Text, many: false } }],
        params: ParamSchema { params: vec![spec("style", style, "plain"), spec("detail", ParamControl::Text { multiline: false, max_bytes: 64 }, "auto")] },
        quick_add: None,
    };
    kinds.register(Box::new(Ruled(descriptor))).unwrap();
    kinds
}

fn node(style: &str, detail: &str) -> Node {
    let values = [("style", style), ("detail", detail)].into_iter().map(|(k, v)| (k.to_owned(), Value::Text(v.into()))).collect();
    kinds().get(KIND).unwrap().instantiate_instance(NodeId::from_bytes([1; 16]), BTreeMap::new(), &values).unwrap()
}

fn canvas(node: Node) -> Canvas {
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Rules").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    project.canvases().get(&C).unwrap().clone()
}

fn planned(node: Node) -> Planned {
    let id = node.id;
    plan_run(&canvas(node), &kinds(), &RunMode::Selected, &[id].into_iter().collect(), &BTreeSet::new()).unwrap()
}

fn sent(node: Node) -> String {
    let Planned::Ready(plan) = planned(node) else { panic!("blocked") };
    match plan.steps.first().and_then(|step| step.jobs.as_ref()).and_then(|jobs| jobs.first()) {
        Some(JobSpec::Source { value: Value::Text(echo), .. }) => echo.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_disabled_value_is_kept_on_the_node_but_never_submitted() {
    let plain = node("plain", "secret");
    assert_eq!(plain.params.get("detail"), Some(&Value::Text("secret".into())), "editors keep it");
    assert_eq!(sent(plain), r#"style=Text("plain")"#);
    assert_eq!(sent(node("fine", "secret")), r#"detail=Text("secret");style=Text("fine")"#);
}

#[test]
fn a_failed_check_blocks_the_run_but_not_editing() {
    let bad = node("fine", "bad");
    let Planned::Blocked(blockers) = planned(bad.clone()) else { panic!("a failed check must block") };
    assert!(blockers.iter().any(|b| matches!(&b.reason, Reason::Invalid(message) if message == "detail must not be bad")), "{blockers:?}");
    assert!(kinds().get(KIND).unwrap().validate_instance(&bad, false).is_ok(), "an intermediate edit is allowed");
    // A disabled parameter takes no part in checks either.
    assert_eq!(sent(node("plain", "bad")), r#"style=Text("plain")"#);
}

#[test]
fn a_disabled_value_is_not_part_of_the_results_identity() {
    let hash = |style: &str, detail: &str| {
        let n = node(style, detail);
        let id = n.id;
        input_hash(&canvas(n), &kinds(), id).unwrap()
    };
    assert_eq!(hash("plain", "a"), hash("plain", "b"), "editing a disabled value keeps results fresh");
    assert_ne!(hash("fine", "a"), hash("fine", "b"));
    assert_ne!(hash("plain", "a"), hash("fine", "a"));
}
