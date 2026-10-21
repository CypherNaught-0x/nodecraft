//! A model whose seed defaults to `random_seed` (#162): the generation kind accepts it, a ready
//! node validates, and its jobs leave the seed for the run to draw after admission.
use nodecraft_nodes::{
    JobSpec, ModeId, NodeKind, PlanCtx,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole},
};
use nodecraft_types::{
    Contract, DefaultPolicy, Modality, NodeId, ParamControl, ParamOwner, ParamRule, ParamSchema, ParamSpec, ParamValues, ProviderTarget, TargetSelection,
    Unsigned, Value,
};

#[test]
fn a_random_seed_model_plans_without_a_seed_for_the_run_to_draw() {
    let id = NodeId::from_bytes([47; 16]);
    let seed = ParamSpec {
        name: "seed".into(),
        label: "Seed".into(),
        description: None,
        control: ParamControl::Seed { max: Some(Unsigned(999)) },
        required: false,
        default: None,
        visible_if: None,
    };
    let mut parameters = Contract::from_schema("fixture/model", ParamOwner::Model, ParamSchema { params: vec![seed] });
    parameters.rules.push(ParamRule { default: Some(DefaultPolicy::RandomSeed), ..ParamRule::implied("seed") });
    let kind = GenerationKind::new(
        vec![GenerationModel {
            style_family: None,
            id: "fixture/model".into(),
            provider: "fal".into(),
            remote: "remote".into(),
            parameters,
            modes: vec![GenerationMode {
                id: ModeId::new("text-to-image").unwrap(),
                inputs: vec![GenerationRole { role: "prompt".into(), modality: Modality::Text, min: 1, max: 1, inline: true }],
                params: None,
                audio: None,
            }],
        }],
        [1; 32],
    )
    .expect("a random-seed model is accepted");
    let supplied: ParamValues = [("model".into(), Value::Text("fixture/model".into())), ("prompt".into(), Value::Text("a cat".into()))].into();
    let node = kind.instantiate_instance(id, Default::default(), &supplied).unwrap();
    let identity = kind.validate_instance(&node, true).expect("a ready node with a random seed validates");
    assert!(identity.visible.contains("seed") && !identity.values.contains_key("seed"), "the marker is identity, the value is drawn later");
    let mut targets = TargetSelection::empty();
    targets.insert(id, &ProviderTarget::default_instance("fal").unwrap()).unwrap();
    let jobs = kind.plan(&PlanCtx { node: &node, inputs: &[], targets: &targets }).unwrap();
    let JobSpec::Generate { params, .. } = &jobs[0] else { panic!() };
    assert!(!params.contains_key("seed"), "the run draws it after admission");
    // A fixed seed is submitted as set.
    let mut fixed = node.clone();
    fixed.params.insert("seed".into(), Value::Unsigned(Unsigned(5)));
    let jobs = kind.plan(&PlanCtx { node: &fixed, inputs: &[], targets: &targets }).unwrap();
    let JobSpec::Generate { params, .. } = &jobs[0] else { panic!() };
    assert_eq!(params.get("seed"), Some(&Value::Unsigned(Unsigned(5))));
}
