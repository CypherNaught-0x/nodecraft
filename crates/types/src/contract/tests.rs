use super::*;
use crate::{Choice, Integer, Number, ParamValues, Unsigned, Value, VisibleIf};
use proptest::prelude::*;

fn spec(name: &str, control: ParamControl, default: Option<Value>) -> ParamSpec {
    ParamSpec { name: name.into(), label: name.into(), description: None, control, required: false, default, visible_if: None }
}
fn choice(values: &[&str]) -> ParamControl {
    ParamControl::Choice { options: values.iter().map(|value| Choice { value: (*value).into(), label: (*value).into() }).collect() }
}
fn text(value: &str) -> Value {
    Value::Text(value.into())
}
fn int(value: i64) -> Value {
    Value::Integer(Integer(value))
}
fn int_control(min: i64, max: i64) -> ParamControl {
    ParamControl::Integer { min: Some(Integer(min)), max: Some(Integer(max)), step: Unsigned(1) }
}
fn values(pairs: &[(&str, Value)]) -> ParamValues {
    pairs.iter().map(|(name, value)| ((*name).to_owned(), value.clone())).collect()
}
fn rule(param: &str) -> ParamRule {
    ParamRule::implied(param)
}
fn equals(param: &str, value: Value) -> Condition {
    Condition::Equals { param: param.into(), value }
}

/// A video model: `mode` picks text or first/last frame; `end_strength` only applies to
/// first/last and resets when hidden; `duration` is in seconds; `seed` is random unless fixed;
/// `quality` is provider-chosen unless set; `steps` is disabled in the fast mode.
fn video() -> Contract {
    let mut duration = spec("duration", int_control(1, 20), Some(int(5)));
    duration.required = true;
    let mut contract = Contract::from_schema(
        "test.video",
        ParamOwner::Model,
        ParamSchema {
            params: vec![
                spec("mode", choice(&["text", "first_last"]), Some(text("text"))),
                spec("speed", choice(&["fast", "slow"]), Some(text("slow"))),
                duration,
                spec("end_strength", int_control(0, 100), None),
                spec("steps", int_control(1, 50), Some(int(20))),
                spec("seed", ParamControl::Seed { max: None }, None),
                spec("quality", choice(&["low", "high"]), None),
            ],
        },
    );
    contract.rules = vec![
        ParamRule { unit: Unit::Seconds, affects: Affects { identity: true, cost: true, shape: true, jobs: false }, ..rule("duration") },
        ParamRule { visible_when: Some(equals("mode", text("first_last"))), hidden: HiddenPolicy::Reset, unit: Unit::Percent, ..rule("end_strength") },
        ParamRule { enabled_when: Some(equals("speed", text("slow"))), ..rule("steps") },
        ParamRule { default: Some(DefaultPolicy::RandomSeed), meaning: Some("seed".into()), ..rule("seed") },
        ParamRule { default: Some(DefaultPolicy::Auto), ..rule("quality") },
    ];
    contract.checks = vec![Check {
        id: "long_needs_slow".into(),
        when: Some(Condition::Compare { param: "duration".into(), cmp: Comparison::Gt, value: Number::new(10.0).unwrap() }),
        require: equals("speed", text("slow")),
        message: "Clips over 10 seconds need the slow speed".into(),
    }];
    contract
}

#[test]
fn unset_auto_random_literal_and_zero_stay_distinct() {
    let contract = video();
    contract.validate().unwrap();
    let resolved = contract.resolve(&values(&[("end_strength", int(0))]), true).unwrap();
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
    assert_eq!(resolved.get("duration").unwrap().value, Effective::Default(int(5)));
    assert_eq!(resolved.get("seed").unwrap().value, Effective::RandomSeed);
    assert_eq!(resolved.get("quality").unwrap().value, Effective::Auto);
    // Zero is an explicit value, even while hidden.
    assert_eq!(resolved.get("end_strength").unwrap().value, Effective::Explicit(int(0)));
    let submitted = resolved.submission().unwrap();
    assert_eq!(submitted, values(&[("duration", int(5)), ("mode", text("text")), ("speed", text("slow")), ("steps", int(20))]));
    assert_eq!(resolved.get("duration").unwrap().id, "test.video/duration");
}

#[test]
fn hidden_and_disabled_values_are_never_submitted() {
    let contract = video();
    let stored = values(&[("end_strength", int(40)), ("speed", text("fast")), ("steps", int(7))]);
    let resolved = contract.resolve(&stored, true).unwrap();
    let strength = resolved.get("end_strength").unwrap();
    assert!(!strength.visible && !strength.enabled && strength.reset, "hidden under the reset policy");
    let steps = resolved.get("steps").unwrap();
    assert!(steps.visible && !steps.enabled && !steps.reset, "disabled but kept");
    let submitted = resolved.submission().unwrap();
    assert!(!submitted.contains_key("end_strength") && !submitted.contains_key("steps"));

    let resolved = contract.resolve(&values(&[("mode", text("first_last")), ("end_strength", int(40))]), true).unwrap();
    assert_eq!(resolved.submission().unwrap().get("end_strength"), Some(&int(40)));
}

#[test]
fn the_run_path_submits_exactly_what_submission_does() {
    let contract = video();
    let stored = values(&[("end_strength", int(40)), ("speed", text("fast")), ("steps", int(7)), ("seed", Value::Unsigned(Unsigned(3)))]);
    let resolution = contract.resolve(&stored, true).unwrap();
    let submitted = resolution.submission().unwrap();
    let resolved = resolution.into_resolved(true).unwrap();
    assert_eq!(resolved.values.get("steps"), Some(&int(7)), "disabled values stay for the editor");
    assert_eq!(resolved.values.get("end_strength"), Some(&int(40)), "hidden ones too");
    assert!(!resolved.values.contains_key("quality"), "auto has no value");
    assert_eq!(resolved.into_visible_values(), submitted);
}

#[test]
fn edits_may_pass_through_states_a_run_refuses() {
    let contract = video();
    // Too long for the fast speed, and the required duration cleared in another edit.
    let failing = values(&[("duration", int(15)), ("speed", text("fast")), ("seed", Value::Unsigned(Unsigned(3)))]);
    assert!(contract.resolve(&failing, false).unwrap().into_resolved(false).is_ok());
    let error = contract.resolve(&failing, true).unwrap().into_resolved(true).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (ErrorKind::InvalidInput, "Clips over 10 seconds need the slow speed"));
    // A wrong value is refused even while editing.
    assert!(contract.resolve(&values(&[("steps", int(99))]), false).unwrap().into_resolved(false).is_err());
}

#[test]
fn a_run_never_silently_drops_a_random_seed() {
    let contract = video();
    assert!(contract.resolve(&values(&[]), false).unwrap().into_resolved(false).is_ok(), "placing the node is fine");
    let error = contract.resolve(&values(&[]), true).unwrap().into_resolved(true).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let fixed = contract.resolve(&values(&[("seed", Value::Unsigned(Unsigned(3)))]), true).unwrap().into_resolved(true).unwrap();
    assert_eq!(fixed.into_visible_values().get("seed"), Some(&Value::Unsigned(Unsigned(3))));
}

#[test]
fn a_stored_value_replaces_its_default_in_the_text_budget() {
    let big = "x".repeat(600_000);
    let contract = Contract::from_schema(
        "test.big",
        ParamOwner::Node,
        ParamSchema { params: vec![spec("prompt", ParamControl::Text { multiline: true, max_bytes: 1_000_000 }, Some(text(&big)))] },
    );
    let resolved = contract.resolve(&values(&[("prompt", text(&big))]), true).unwrap();
    assert!(resolved.issues.is_empty());
    let schema = contract.schema.resolve(&values(&[("prompt", text(&big))]), true).unwrap();
    assert_eq!(resolved.into_resolved(true).unwrap(), schema, "the same admission as ParamSchema::resolve");
}

#[test]
fn a_hidden_invalid_value_still_blocks_submission() {
    let resolved = video().resolve(&values(&[("end_strength", int(400))]), true).unwrap();
    assert_eq!(resolved.issues.len(), 1);
    assert_eq!(resolved.issues[0].code, IssueCode::InvalidValue);
    assert_eq!(resolved.issues[0].param.as_deref(), Some("end_strength"));
    assert!(resolved.submission().is_err());
}

#[test]
fn issues_are_reported_together_and_are_structured() {
    let mut contract = video();
    contract.schema.params[2].default = None;
    contract.rules[0].default = None;
    let stored = values(&[("speed", text("turbo")), ("mystery", int(1)), ("Bad Key", int(1))]);
    let resolved = contract.resolve(&stored, true).unwrap();
    let codes: Vec<_> = resolved.issues.iter().map(|issue| (issue.code, issue.param.clone())).collect();
    assert_eq!(
        codes,
        vec![
            (IssueCode::UnknownParam, None),
            (IssueCode::UnknownParam, Some("mystery".into())),
            (IssueCode::InvalidValue, Some("speed".into())),
            (IssueCode::Required, Some("duration".into())),
        ]
    );
    assert!(contract.resolve(&ParamValues::new(), false).unwrap().issues.is_empty(), "not ready: required values may wait");
}

#[test]
fn cross_field_checks_use_active_values() {
    let contract = video();
    let resolved = contract.resolve(&values(&[("duration", int(15)), ("speed", text("fast"))]), true).unwrap();
    assert_eq!(resolved.issues.len(), 1);
    assert_eq!(resolved.issues[0].check.as_deref(), Some("long_needs_slow"));
    assert_eq!(resolved.submission().unwrap_err().message, "Clips over 10 seconds need the slow speed");
    assert!(contract.resolve(&values(&[("duration", int(15))]), true).unwrap().issues.is_empty());
}

#[test]
fn identity_includes_auto_and_random_markers_but_not_inactive_values() {
    let contract = video();
    let resolved = contract.resolve(&values(&[("speed", text("fast")), ("steps", int(3))]), true).unwrap();
    let identity: Vec<_> = resolved.identity().map(|(name, _)| name).collect();
    assert_eq!(identity, ["mode", "speed", "duration", "seed", "quality"]);
    assert!(resolved.get("duration").unwrap().affects.cost);

    let untouched: Vec<_> = contract.resolve(&values(&[]), true).unwrap().identity().map(|(name, value)| (name.to_owned(), value)).collect();
    let typed_in: Vec<_> = contract.resolve(&values(&[("duration", int(5))]), true).unwrap().identity().map(|(name, value)| (name.to_owned(), value)).collect();
    assert_eq!(untouched, typed_in, "typing in the default is not a new identity");
}

#[test]
fn an_invalid_hidden_value_is_marked_for_reset() {
    let resolved = video().resolve(&values(&[("end_strength", int(500))]), true).unwrap();
    let state = resolved.get("end_strength").unwrap();
    assert!(!state.visible && state.reset, "the editor clears the value that blocks submission");
    assert!(resolved.submission().is_err());
}

#[test]
fn schema_visible_if_still_applies() {
    let mut contract = video();
    contract.schema.params[3].visible_if = Some(VisibleIf { parameter: "speed".into(), equals: text("slow") });
    let resolved = contract.resolve(&values(&[("mode", text("first_last")), ("speed", text("fast")), ("end_strength", int(1))]), true).unwrap();
    assert!(!resolved.get("end_strength").unwrap().visible);
}

#[test]
fn inconsistent_contracts_are_rejected() {
    type Mutation = Box<dyn Fn(&mut Contract)>;
    let cases: Vec<(&str, Mutation)> = vec![
        ("newer major", Box::new(|c| c.version.major = 2)),
        ("bad namespace", Box::new(|c| c.namespace = "Test Video".into())),
        ("duplicate rule", Box::new(|c| c.rules.push(rule("duration")))),
        ("dangling rule", Box::new(|c| c.rules.push(rule("missing")))),
        ("unit on a choice", Box::new(|c| c.rules.push(ParamRule { unit: Unit::Pixels, ..rule("mode") }))),
        ("literal without default", Box::new(|c| c.rules[1].default = Some(DefaultPolicy::Literal))),
        ("auto with a default", Box::new(|c| c.rules[0].default = Some(DefaultPolicy::Auto))),
        ("random non-seed", Box::new(|c| c.rules[4].default = Some(DefaultPolicy::RandomSeed))),
        ("looks ahead", Box::new(|c| c.rules[1].visible_when = Some(equals("seed", Value::Unsigned(Unsigned(1)))))),
        ("names itself", Box::new(|c| c.rules[1].visible_when = Some(Condition::Present { param: "end_strength".into() }))),
        ("unknown param", Box::new(|c| c.checks[0].require = Condition::Present { param: "nope".into() })),
        ("value outside options", Box::new(|c| c.rules[1].visible_when = Some(equals("mode", text("image"))))),
        (
            "compare on text",
            Box::new(|c| c.checks[0].require = Condition::Compare { param: "mode".into(), cmp: Comparison::Lt, value: Number::new(1.0).unwrap() }),
        ),
        ("empty any", Box::new(|c| c.checks[0].require = Condition::Any { of: vec![] })),
        ("empty in", Box::new(|c| c.checks[0].require = Condition::In { param: "mode".into(), values: vec![] })),
        ("duplicate check", Box::new(|c| c.checks.push(c.checks[0].clone()))),
        ("blank message", Box::new(|c| c.checks[0].message = " ".into())),
        ("bad meaning", Box::new(|c| c.rules[3].meaning = Some("Seed!".into()))),
    ];
    for (case, mutate) in cases {
        let mut contract = video();
        mutate(&mut contract);
        assert!(contract.validate().is_err(), "{case} should be rejected");
        assert!(contract.resolve(&ParamValues::new(), false).is_err(), "{case}: resolve validates first");
    }
}

#[test]
fn conditions_are_bounded_in_depth_size_and_total() {
    let mut deep = Condition::Present { param: "mode".into() };
    for _ in 0..MAX_CONDITION_DEPTH {
        deep = Condition::Not { condition: Box::new(deep) };
    }
    let mut contract = video();
    contract.checks[0].require = deep;
    assert!(contract.validate().is_err());

    let wide = Condition::All { of: vec![Condition::Present { param: "mode".into() }; MAX_CONDITION_NODES] };
    contract.checks[0].require = wide;
    assert!(contract.validate().is_err());

    let mut contract = video();
    let medium = Condition::All { of: vec![Condition::Present { param: "mode".into() }; MAX_CONDITION_NODES - 1] };
    contract.checks =
        (0..MAX_CHECKS).map(|i| Check { id: format!("c{i}"), when: Some(medium.clone()), require: medium.clone(), message: "m".into() }).collect();
    assert!(contract.validate().is_err(), "the per-contract budget caps many medium conditions");
    contract.checks.truncate(1);
    contract.validate().unwrap();
}

#[test]
fn model_swaps_keep_only_compatible_valid_values() {
    let tag = |contract: &mut Contract| {
        contract.rules.push(ParamRule { meaning: Some("video_mode".into()), ..rule("mode") });
        if let Some(rule) = contract.rules.iter_mut().find(|rule| rule.param == "end_strength") {
            rule.meaning = Some("end_frame_strength".into());
        }
    };
    let mut from = video();
    tag(&mut from);
    let mut to = video();
    tag(&mut to);
    to.namespace = "test.video_two".into();
    to.schema.params[2].control = int_control(1, 8);
    to.schema.params[4].control = ParamControl::Unsigned { min: None, max: None, step: Unsigned(1) };
    to.schema.params[4].default = None;
    to.rules[0].unit = Unit::Frames;
    to.rules[3].meaning = Some("variation".into());
    to.schema.params.retain(|spec| spec.name != "quality");
    to.rules.retain(|rule| rule.param != "quality");
    to.validate().unwrap();
    let stored = values(&[
        ("mode", text("first_last")),
        ("duration", int(6)),
        ("steps", int(30)),
        ("seed", Value::Unsigned(Unsigned(9))),
        ("quality", text("high")),
        ("end_strength", int(70)),
        ("speed", text("slow")),
        ("stray", text("x")),
    ]);
    let migration = from.migrate(&to, &stored).unwrap();
    assert_eq!(migration.kept, values(&[("end_strength", int(70)), ("mode", text("first_last"))]));
    let dropped: Vec<_> = migration.dropped.iter().map(|drop| (drop.param.as_str(), drop.reason)).collect();
    assert_eq!(
        dropped,
        [
            ("duration", DropReason::UnitChanged),
            ("quality", DropReason::NotInTarget),
            ("seed", DropReason::MeaningChanged),
            ("speed", DropReason::UnverifiedMeaning),
            ("steps", DropReason::ControlChanged),
            ("stray", DropReason::NotInSource),
        ]
    );

    // Within one namespace (a descriptor upgrade) the untagged name is the owner's promise.
    let mut upgraded = to.clone();
    upgraded.namespace = from.namespace.clone();
    assert!(from.migrate(&upgraded, &stored).unwrap().kept.contains_key("speed"));

    let mut to = video();
    tag(&mut to);
    to.schema.params[3].control = int_control(0, 50);
    let migration = from.migrate(&to, &stored).unwrap();
    assert!(migration.dropped.iter().any(|drop| drop.param == "end_strength" && drop.reason == DropReason::InvalidInTarget && drop.value == int(70)));
}

#[test]
fn bulk_edits_offer_shared_fields_and_mark_mixed_values() {
    let first = video();
    let mut second = video();
    second.rules[0].unit = Unit::Frames;
    assert_eq!(Contract::shared_params(&[&first, &second]), ["mode", "speed", "end_strength", "steps", "seed", "quality"]);
    assert!(Contract::shared_params(&[]).is_empty());
    assert_eq!(shared_value([Some(&int(1)), Some(&int(1))]), Shared::Same(int(1)));
    assert_eq!(shared_value([Some(&int(1)), Some(&int(2))]), Shared::Mixed);
    assert_eq!(shared_value([Some(&int(1)), None]), Shared::Mixed);
    assert_eq!(shared_value([None, None]), Shared::Unset);
    assert_eq!(shared_value(std::iter::empty()), Shared::Unset);
}

#[test]
fn later_override_layers_win() {
    let technique = values(&[("steps", int(10)), ("mode", text("text"))]);
    let model = values(&[("steps", int(20))]);
    let node = values(&[("mode", text("first_last"))]);
    assert_eq!(overlay(&[&technique, &model, &node]).unwrap(), values(&[("mode", text("first_last")), ("steps", int(20))]));
    assert!(overlay(&[&technique; 9]).is_err());
}

#[test]
fn connected_text_precedence_is_deterministic() {
    let a = ConnectedText { order: 1, tie: [2; 16], text: "a" };
    let b = ConnectedText { order: 0, tie: [9; 16], text: "b" };
    let c = ConnectedText { order: 1, tie: [1; 16], text: "c" };
    let empty = ConnectedText { order: 2, tie: [0; 16], text: "" };
    let wires = [a, b, c, empty];
    assert_eq!(compose_text(TextPrecedence::ConnectedFirst, Some("local"), &wires, "\n").unwrap(), "b\nc\na\nlocal");
    assert_eq!(compose_text(TextPrecedence::LocalFirst, Some("local"), &wires, " ").unwrap(), "local b c a");
    assert_eq!(compose_text(TextPrecedence::ConnectedReplaces, Some("local"), &wires, " ").unwrap(), "b c a");
    assert_eq!(compose_text(TextPrecedence::ConnectedReplaces, Some("local"), &[empty], " ").unwrap(), "local");
    assert_eq!(compose_text(TextPrecedence::LocalOnly, Some("local"), &wires, " ").unwrap(), "local");
    assert_eq!(compose_text(TextPrecedence::ConnectedFirst, Some(""), &[], " ").unwrap(), "");
    assert!(compose_text(TextPrecedence::ConnectedFirst, None, &[a, a], " ").is_err(), "one position, two texts");
    assert_eq!(compose_text(TextPrecedence::LocalOnly, Some("local"), &[a, a], " ").unwrap(), "local", "ignored wires cannot fail");
    assert!(compose_text(TextPrecedence::ConnectedFirst, None, &[], &"-".repeat(65)).is_err());
}

#[test]
fn contracts_round_trip_and_hostile_json_is_rejected() {
    let contract = video();
    let json = serde_json::to_string(&contract).unwrap();
    assert_eq!(serde_json::from_str::<Contract>(&json).unwrap(), contract);
    let resolution = contract.resolve(&ParamValues::new(), true).unwrap();
    assert_eq!(serde_json::from_str::<Resolution>(&serde_json::to_string(&resolution).unwrap()).unwrap(), resolution);
    let minimal: Contract = serde_json::from_str(r#"{"namespace":"a.b","owner":"node","schema":{"params":[]}}"#).unwrap();
    assert_eq!(minimal.version, ContractVersion::CURRENT);
    minimal.validate().unwrap();
    for hostile in [
        r#"{"namespace":"a","owner":"node","schema":{"params":[]},"extra":1}"#,
        r#"{"namespace":"a","owner":"plugin","schema":{"params":[]}}"#,
        r#"{"namespace":"a","owner":"node","schema":{"params":[]},"checks":[{"id":"x","require":{"op":"xor"},"message":"m"}]}"#,
    ] {
        assert!(serde_json::from_str::<Contract>(hostile).is_err(), "{hostile}");
    }
    let deep = format!("{}{{\"op\":\"present\",\"param\":\"mode\"}}{}", r#"{"op":"not","condition":"#.repeat(200), "}".repeat(200));
    let json = format!(r#"{{"namespace":"a","owner":"node","schema":{{"params":[]}},"checks":[{{"id":"x","require":{deep},"message":"m"}}]}}"#);
    assert!(serde_json::from_str::<Contract>(&json).is_err(), "serde's recursion limit holds");
}

proptest! {
    #[test]
    fn composed_text_ignores_input_order(mut order in proptest::collection::vec((0u32..4, 0u8..255), 0..12), seed in any::<u64>()) {
        order.sort();
        order.dedup();
        let texts: Vec<String> = order.iter().map(|(position, tie)| format!("{position}-{tie}")).collect();
        let wires: Vec<_> = order.iter().zip(&texts).map(|((position, tie), text)| ConnectedText { order: *position, tie: [*tie; 16], text }).collect();
        let mut shuffled = wires.clone();
        let len = shuffled.len().max(1);
        shuffled.rotate_left(usize::try_from(seed).unwrap_or(0) % len);
        shuffled.reverse();
        prop_assert_eq!(compose_text(TextPrecedence::ConnectedFirst, Some("x"), &wires, ",").unwrap(), compose_text(TextPrecedence::ConnectedFirst, Some("x"), &shuffled, ",").unwrap());
    }

    #[test]
    fn arbitrary_stored_values_resolve_without_panicking(entries in proptest::collection::btree_map("[a-z_]{1,12}", prop_oneof![
        any::<i64>().prop_map(|n| Value::Integer(Integer(n))),
        any::<u64>().prop_map(|n| Value::Unsigned(Unsigned(n))),
        any::<bool>().prop_map(Value::Bool),
        "[a-z_]{0,12}".prop_map(Value::Text),
    ], 0..16)) {
        let contract = video();
        let resolved = contract.resolve(&entries, true).unwrap();
        prop_assert_eq!(resolved.params.len(), contract.schema.params.len());
        if let Ok(submitted) = resolved.submission() {
            prop_assert!(contract.schema.resolve(&submitted, false).is_ok(), "a submission always satisfies the schema");
        }
    }
}

#[test]
fn identity_leaves_out_parameters_that_do_not_affect_it() {
    let mut contract = video();
    for rule in contract.rules.iter_mut().filter(|rule| rule.param == "duration") {
        rule.affects.identity = false;
    }
    let pinned = values(&[("seed", Value::Unsigned(Unsigned(3)))]);
    let identity = contract.resolve(&pinned, true).unwrap().into_identity(true).unwrap();
    assert!(!identity.visible.contains("duration"), "editing it keeps results fresh");
    assert!(identity.visible.contains("seed"));
    let submitted = contract.resolve(&pinned, true).unwrap().into_resolved(true).unwrap();
    assert!(submitted.visible.contains("duration"), "it is still sent");
}
