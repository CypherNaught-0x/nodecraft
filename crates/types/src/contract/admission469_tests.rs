use super::*;
use crate::{Integer, Number, ParamControl, ParamSchema, ParamSpec, ParamValues, Unsigned, Value};
use std::cmp::Ordering;
fn empty() -> Contract {
    Contract::from_schema("test.owner", ParamOwner::Node, ParamSchema::default())
}
fn map(name: &str, value: Value) -> ParamValues {
    [(name.into(), value)].into()
}
fn check(value: Value, threshold: f64, expected: Ordering) {
    let control = match value {
        Value::Integer(_) => ParamControl::Integer { min: None, max: None, step: Unsigned(1) },
        Value::Unsigned(_) => ParamControl::Unsigned { min: None, max: None, step: Unsigned(1) },
        _ => ParamControl::Slider { min: None, max: None, step: Number::new(1.0).unwrap() },
    };
    let mut contract = Contract::from_schema(
        "test.owner",
        ParamOwner::Node,
        ParamSchema {
            params: vec![ParamSpec {
                name: "number".into(),
                label: "Number".into(),
                description: None,
                control,
                required: false,
                default: None,
                visible_if: None,
            }],
        },
    );
    for (cmp, truth) in
        [(Comparison::Lt, expected.is_lt()), (Comparison::Lte, !expected.is_gt()), (Comparison::Gt, expected.is_gt()), (Comparison::Gte, !expected.is_lt())]
    {
        contract.checks = vec![Check {
            id: "comparison".into(),
            when: None,
            require: Condition::Compare { param: "number".into(), cmp, value: Number::new(threshold).unwrap() },
            message: "comparison failed".into(),
        }];
        assert_eq!(contract.resolve(&map("number", value.clone()), false).unwrap().issues.is_empty(), truth, "{value:?} {cmp:?} {threshold}");
    }
}
#[test]
fn exact_integer_conditions_cover_full_width_and_fractional_thresholds() {
    for (n, t, o) in [
        (9_007_199_254_740_993, 9_007_199_254_740_992.0, Ordering::Greater),
        (-9_007_199_254_740_993, -9_007_199_254_740_992.0, Ordering::Less),
        (i64::MAX, 9_223_372_036_854_775_808.0, Ordering::Less),
        (i64::MIN, -9_223_372_036_854_775_808.0, Ordering::Equal),
        (0, -0.5, Ordering::Greater),
        (0, 0.5, Ordering::Less),
        (-1, -0.5, Ordering::Less),
        (1, 1.5, Ordering::Less),
        (0, -0.0, Ordering::Equal),
        (i64::MIN, -1e100, Ordering::Greater),
        (i64::MAX, 1e100, Ordering::Less),
    ] {
        check(Value::Integer(Integer(n)), t, o);
    }
    for (n, t, o) in [
        (9_007_199_254_740_993, 9_007_199_254_740_992.0, Ordering::Greater),
        (u64::MAX, 18_446_744_073_709_551_616.0, Ordering::Less),
        (0, -0.5, Ordering::Greater),
        (0, 0.5, Ordering::Less),
        (u64::MAX, 1e100, Ordering::Less),
        (0, -1e100, Ordering::Greater),
    ] {
        check(Value::Unsigned(Unsigned(n)), t, o);
    }
    check(Value::Number(Number::new(0.25).unwrap()), 0.5, Ordering::Less);
}
fn refused(values: &ParamValues) {
    let c = empty();
    for result in [c.migrate(&c, values).map(|_| ()), overlay(&[values]).map(|_| ())] {
        let err = result.unwrap_err();
        assert_eq!(err.kind, crate::ErrorKind::InvalidInput);
        assert!(err.message.len() < 160);
    }
}
#[test]
fn unknown_oversized_and_deep_values_are_refused_before_copying() {
    refused(&map("unknown", Value::Text("x".repeat(1024 * 1024 + 1))));
    let mut deep = Value::Bool(false);
    for _ in 0..34 {
        deep = Value::List(vec![deep]);
    }
    refused(&map("unknown", deep));
    refused(&map("unknown", Value::List(vec![Value::Bool(false); 4097])));
    refused(&map(&"n".repeat(129), Value::Bool(false)));
}
#[test]
fn aggregate_layers_admit_every_input_even_when_later_overwritten() {
    let a = map("unknown", Value::Text("a".repeat(600_000)));
    let b = map("unknown", Value::Text("b".repeat(600_000)));
    assert!(overlay(&[&a, &b]).is_err());
    let c = empty();
    let values: ParamValues = [("first".into(), Value::Text("a".repeat(600_000))), ("second".into(), Value::Text("b".repeat(600_000)))].into();
    assert!(c.migrate(&c, &values).is_err());
}
#[test]
fn bounded_unknown_names_and_values_keep_existing_drop_and_override_semantics() {
    let c = empty();
    let a = map("unknown.name/kept", Value::List(vec![Value::Text("keep".into())]));
    let migration = c.migrate(&c, &a).unwrap();
    assert_eq!(migration.dropped[0].param, "unknown.name/kept");
    assert_eq!(migration.dropped[0].reason, DropReason::NotInSource);
    assert_eq!(migration.dropped[0].value, a["unknown.name/kept"]);
    let b = map("unknown.name/kept", Value::Bool(true));
    assert_eq!(overlay(&[&a, &b]).unwrap(), b);
}

#[test]
fn exact_comparisons_cover_representable_neighbors_and_tiny_fractions() {
    for (n, t, o) in [
        (i64::MIN + 1, -9_223_372_036_854_775_808.0, Ordering::Greater),
        (9_007_199_254_740_991, 9_007_199_254_740_991.0, Ordering::Equal),
        (-9_007_199_254_740_991, -9_007_199_254_740_991.0, Ordering::Equal),
        (0, f64::from_bits(1), Ordering::Less),
        (0, -f64::from_bits(1), Ordering::Greater),
    ] {
        check(Value::Integer(Integer(n)), t, o);
    }
    check(Value::Unsigned(Unsigned(u64::MAX)), 18_446_744_073_709_549_568.0, Ordering::Greater);
    check(Value::Unsigned(Unsigned(9_007_199_254_740_992)), 9_007_199_254_740_992.0, Ordering::Equal);
}
#[test]
fn admission_boundaries_and_unknown_name_grammar_remain_compatible() {
    let c = empty();
    let exact = map("unknown", Value::Text("x".repeat(1024 * 1024)));
    assert!(c.migrate(&c, &exact).is_ok());
    assert!(overlay(&[&exact]).is_ok());
    for name in ["", "unknown.name/kept", "未知", &"n".repeat(128)] {
        let values = map(name, Value::Bool(true));
        assert_eq!(c.migrate(&c, &values).unwrap().dropped[0].param, name);
        assert_eq!(overlay(&[&values]).unwrap(), values);
    }
    let mut at_depth = Value::Bool(false);
    for _ in 0..32 {
        at_depth = Value::List(vec![at_depth]);
    }
    let depth = map("depth", at_depth);
    assert!(c.migrate(&c, &depth).is_ok());
    assert!(overlay(&[&depth]).is_ok());
    let exact_items = map("items", Value::List(vec![Value::Bool(false); 4095]));
    assert!(overlay(&[&exact_items]).is_ok());
    let extra = map("extra", Value::Bool(false));
    assert!(overlay(&[&exact_items, &extra]).is_err());
    let a: ParamValues = (0..256).map(|n| (format!("key{n}"), Value::Bool(false))).collect();
    let b = map("other", Value::Bool(false));
    assert!(overlay(&[&a, &b]).is_err());
    // Each individual value fits the item budget, but their combined ownership does not.
    let many: ParamValues = [("one".into(), Value::List(vec![Value::Bool(false); 2048])), ("two".into(), Value::List(vec![Value::Bool(false); 2048]))].into();
    refused(&many);
}
