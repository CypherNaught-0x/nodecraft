use super::*;
use nodecraft_catalog::Catalog;
use nodecraft_nodes::{GenerateInput, ModeId};
use nodecraft_types::{Currency, NodeId};

fn model(provider: &str) -> ModelDescriptor {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "fixture.speech"
provider = "{provider}"
provider_model = "speech"
label = "Speech"
description = "Deterministic character pricing fixture"
latency_s = 0
quality = 1
pricing = {{ type = "per_unit", unit = "thousand_characters", price = "0.08 USD" }}
[[model.modes]]
id = "text-to-speech"
output = "audio"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
    ))
    .unwrap()
    .get("fixture.speech")
    .unwrap()
    .clone()
}

fn step(provider: &str, values: &[Value]) -> Step {
    let node = NodeId::from_bytes([67; 16]);
    Step {
        node,
        kind: "fixture.speech".into(),
        mode: ModeId::new("text-to-speech").unwrap(),
        after: vec![],
        inputs: vec![],
        jobs: Some(
            values
                .iter()
                .map(|value| JobSpec::Generate {
                    instance: None,
                    node,
                    provider: provider.into(),
                    model: "speech".into(),
                    params: Default::default(),
                    inputs: vec![GenerateInput { role: "prompt".into(), value: value.clone() }],
                    outputs: vec![],
                })
                .collect(),
        ),
        fanout: None,
    }
}

fn price(model: &ModelDescriptor, step: &Step) -> Result<Option<Money>, Error> {
    let mut bytes = crate::planner::MAX_PLAN_INPUT_BYTES;
    quote(model, step, &mut bytes)
}

#[test]
fn speech_counts_the_trimmed_unicode_text_the_adapter_sends() {
    let step = step("elevenlabs", &[Value::Text("\n雪é🙂\t".into())]);
    assert_eq!(price(&model("elevenlabs"), &step).unwrap(), Some(Money::new(Currency::USD, 240)));
    let mut low_rate = model("elevenlabs");
    if let Pricing::PerUnit { price, .. } = &mut low_rate.pricing {
        price.0.micros = 1;
    }
    assert_eq!(price(&low_rate, &step).unwrap().unwrap().micros, 1, "fractional micros round upward");
}

#[test]
fn generic_text_keeps_whitespace_and_flattens_text_inputs() {
    let values = [Value::List(vec![Value::Text(" 雪 ".into()), Value::List(vec![Value::Text("é🙂".into())])])];
    assert_eq!(price(&model("mock"), &step("mock", &values)).unwrap().unwrap().micros, 400);
    assert!(price(&model("elevenlabs"), &step("elevenlabs", &values)).unwrap().is_none(), "speech refuses multiple prompt values");
}

#[test]
fn a_fanned_step_uses_the_dearest_known_job() {
    let step = step("mock", &[Value::Text("a".into()), Value::Text("雪é🙂".into())]);
    assert_eq!(price(&model("mock"), &step).unwrap().unwrap().micros, 240);
}

#[test]
fn deferred_unmatched_and_nontext_work_stays_unknown() {
    let model = model("mock");
    let mut deferred = step("mock", &[Value::Text("known only later".into())]);
    deferred.jobs = None;
    assert!(price(&model, &deferred).unwrap().is_none());
    assert!(price(&model, &step("other", &[Value::Text("x".into())])).unwrap().is_none());
    assert!(price(&model, &step("mock", &[Value::Bool(true)])).unwrap().is_none());
    assert!(price(&model, &step("mock", &[])).unwrap().is_none());
}

#[test]
fn other_pricing_rules_and_explicit_counts_are_not_reinterpreted() {
    let mut model = model("mock");
    let step = step("mock", &[Value::Text("hello".into())]);
    for pricing in [
        Pricing::Free,
        Pricing::ByUsage { currency: Currency::USD },
        Pricing::PerUnit { unit: Unit::ThousandTokens, price: "0.08 USD".parse().unwrap(), count: None, default_count: None },
        Pricing::PerUnit { unit: Unit::ThousandCharacters, price: "0.08 USD".parse().unwrap(), count: Some("count".into()), default_count: None },
        Pricing::PerUnit { unit: Unit::ThousandCharacters, price: "0.08 USD".parse().unwrap(), count: None, default_count: Some(1000) },
    ] {
        model.pricing = pricing;
        assert!(price(&model, &step).unwrap().is_none());
    }
}

#[test]
fn quote_work_and_nested_input_depth_are_bounded() {
    let model = model("mock");
    let work = step("mock", &[Value::Text("hello".into())]);
    assert!(quote(&model, &work, &mut 0).is_err());
    let mut nested = Value::Text("x".into());
    for _ in 0..=nodecraft_graph::MAX_VALUE_DEPTH {
        nested = Value::List(vec![nested]);
    }
    assert!(price(&model, &step("mock", &[nested])).is_err());
}
