//! Public-route regression witnesses for expression budgets (#401).
use super::*;
use std::collections::BTreeMap;

#[test]
fn identity_refuses_an_input_list_over_the_item_limit() {
    let input = Value::List((0..20_000).map(Value::Int).collect());
    let limits = ExprLimits::default();
    let result = Expr::parse("input").unwrap().eval(&[("input", &input)], limits, &|| false);
    match result {
        Err(error) => assert_eq!(error.reason, ErrorReason::TooManyItems),
        Ok(Value::List(items)) => assert!(items.len() <= limits.list_items, "identity returned {} items with limit {}", items.len(), limits.list_items),
        Ok(_) => panic!("identity changed the input kind"),
    }
}

#[test]
fn a_scalar_result_must_fit_the_value_limit() {
    let limits = ExprLimits { value_bytes: 0, ..ExprLimits::default() };
    match Expr::parse("1").unwrap().eval(&[], limits, &|| false) {
        Err(error) => assert_eq!(error.reason, ErrorReason::ValueTooLarge),
        Ok(value) => assert!(value.size() <= limits.value_bytes, "scalar result charge {} exceeds value limit {}", value.size(), limits.value_bytes),
    }
}

fn repeated_text_action(count: usize) -> CompiledTextAction {
    TextActionDefinition {
        schema: TEXT_ACTION_SCHEMA,
        id: "user.output_budget".into(),
        version: 1,
        label: "Output budget".into(),
        inputs: vec![],
        params: vec![],
        outputs: (0..count)
            .map(|i| TextOutputDef { name: format!("output{i}"), kind: OutputKind::Text, expr: format!("repeat(\"x\", {MAX_VALUE_BYTES})") })
            .collect(),
    }
    .compile()
    .unwrap()
}

#[test]
fn compiled_outputs_share_one_aggregate_return_limit() {
    let limits = ExprLimits::default();
    let cap = limits.value_bytes.checked_mul(4).unwrap();
    let exact = repeated_text_action(4).run(&BTreeMap::new(), &BTreeMap::new(), limits, &|| false).unwrap();
    assert_eq!(exact.iter().map(|(_, v)| v.size()).sum::<usize>(), cap, "the exact four-value boundary remains supported");
    drop(exact);
    match repeated_text_action(5).run(&BTreeMap::new(), &BTreeMap::new(), limits, &|| false) {
        Err(error) => assert_eq!(error.reason, ErrorReason::ValueTooLarge),
        Ok(outputs) => {
            let charge = outputs.iter().map(|(_, v)| v.size()).sum::<usize>();
            assert!(charge <= cap, "compiled action retained {} outputs charged {charge} bytes with aggregate limit {cap}", outputs.len());
        }
    }
}

fn limited(source: &str, names: &[(&str, &Value)], value_bytes: usize) -> Result<Value, ExprError> {
    Expr::parse(source).unwrap().eval(names, ExprLimits { value_bytes, ..ExprLimits::default() }, &|| false)
}

#[test]
fn exact_and_lowered_scalar_text_list_charges() {
    for source in [
        "1",
        "true",
        "-1",
        "not false",
        "1 + 1",
        "1 == 1",
        "false and true",
        "true or false",
        "if true then 1 else 2",
        "let x = 1 in x",
        "len(\"\")",
        "int(\"1\")",
    ] {
        assert_eq!(limited(source, &[], 7).unwrap_err().reason, ErrorReason::ValueTooLarge, "{source}");
        assert!(limited(source, &[], 8).is_ok(), "{source}");
    }
    for source in ["\"\"", "[]"] {
        assert!(limited(source, &[], 0).is_ok());
    }
    assert!(limited("\"12345678\"", &[], 8).is_ok());
    assert_eq!(limited("\"123456789\"", &[], 8).unwrap_err().reason, ErrorReason::ValueTooLarge);
    assert!(limited("[1]", &[], 24).is_ok());
    assert_eq!(limited("[1]", &[], 23).unwrap_err().reason, ErrorReason::ValueTooLarge);
    assert!(limited("[[1]]", &[], 40).is_ok());
    assert_eq!(limited("[[1]]", &[], 39).unwrap_err().reason, ErrorReason::ValueTooLarge);
}

#[test]
fn all_input_values_and_nested_lists_are_admitted_before_use() {
    let nested = Value::List(vec![Value::List(vec![Value::Text(String::new()), Value::Text(String::new())])]);
    let expr = Expr::parse("\"unused\"").unwrap();
    let limits = ExprLimits { list_items: 1, ..ExprLimits::default() };
    assert_eq!(expr.eval(&[("private", &nested)], limits, &|| false).unwrap_err().reason, ErrorReason::TooManyItems);
    let text = Value::Text("123456789".into());
    assert_eq!(limited("\"\"", &[("unused", &text)], 8).unwrap_err().reason, ErrorReason::ValueTooLarge);
    let a = Value::Text("12345678".into());
    let b = Value::Text("x".into());
    let limits = ExprLimits { value_bytes: 8, input_bytes: 8, ..ExprLimits::default() };
    assert_eq!(expr.eval(&[("a", &a), ("b", &b)], limits, &|| false).unwrap_err().reason, ErrorReason::InputTooLarge);
    let mut deep = Value::Text(String::new());
    for _ in 0..MAX_VALUE_DEPTH {
        deep = Value::List(vec![deep]);
    }
    assert!(expr.eval(&[("deep", &deep)], ExprLimits::default(), &|| false).is_ok());
    deep = Value::List(vec![deep]);
    assert_eq!(expr.eval(&[("deep", &deep)], ExprLimits::default(), &|| false).unwrap_err().reason, ErrorReason::TooDeep);
}

#[test]
fn list_item_boundaries_apply_to_inputs_and_constructed_nested_values() {
    let value = Value::List(vec![Value::Text(String::new()); MAX_LIST_ITEMS]);
    assert!(Expr::parse("input").unwrap().eval(&[("input", &value)], ExprLimits::default(), &|| false).is_ok());
    for source in ["range(0, 2)", "chars(\"ab\")", "split(\"a,b\", \",\")", "lines(\"a\\nb\")", "[\"\", \"\"]", "[[\"\", \"\"]]"] {
        let limits = ExprLimits { list_items: 1, ..ExprLimits::default() };
        assert_eq!(Expr::parse(source).unwrap().eval(&[], limits, &|| false).unwrap_err().reason, ErrorReason::TooManyItems, "{source}");
    }
}

#[test]
fn known_size_growth_and_unicode_case_keep_exact_boundaries() {
    for (source, cap) in [
        ("range(0, 2)", 48),
        ("chars(\"ab\")", 34),
        ("split(\"a,b\", \",\")", 34),
        ("lines(\"a\\nb\")", 34),
        ("[1] + [2]", 48),
        ("repeat(\"x\", 9)", 9),
        ("join([\"a\", \"b\"], \"--\")", 34),
    ] {
        assert!(limited(source, &[], cap).is_ok(), "{source}");
        assert_eq!(limited(source, &[], cap - 1).unwrap_err().reason, ErrorReason::ValueTooLarge, "{source}");
    }
    let dotted = Value::Text("İ".into());
    assert_eq!(limited("lower(x)", &[("x", &dotted)], 2).unwrap_err().reason, ErrorReason::ValueTooLarge);
    assert_eq!(limited("lower(x)", &[("x", &dotted)], 3).unwrap(), Value::Text("i\u{307}".into()));
    let sigma = Value::Text("ΟΣ".into());
    assert_eq!(limited("lower(x)", &[("x", &sigma)], 4).unwrap(), Value::Text("ος".into()));
    let sharp = Value::Text("ß".into());
    assert_eq!(limited("upper(x)", &[("x", &sharp)], 2).unwrap(), Value::Text("SS".into()));
    let greek = Value::Text("ΐ".into());
    assert_eq!(limited("upper(x)", &[("x", &greek)], 5).unwrap_err().reason, ErrorReason::ValueTooLarge);
    assert_eq!(limited("upper(x)", &[("x", &greek)], 6).unwrap(), Value::Text("Ϊ́".into()));
    let number = Value::Int(i64::MIN);
    assert_eq!(limited("text(n)", &[("n", &number)], 19).unwrap_err().reason, ErrorReason::ValueTooLarge);
    assert_eq!(limited("text(n)", &[("n", &number)], 20).unwrap(), Value::Text(i64::MIN.to_string()));
}

#[test]
fn names_are_borrowed_shadowed_and_metered_on_hits_and_misses() {
    let empty = Value::Text(String::new());
    let mut names = vec![("target", &empty)];
    names.extend(std::iter::repeat_n(("other", &empty), 200));
    let source = std::iter::repeat_n("target", 8).collect::<Vec<_>>().join(" + ");
    let limits = ExprLimits { fuel: 700, ..ExprLimits::default() };
    assert_eq!(Expr::parse(&source).unwrap().eval(&names, limits, &|| false).unwrap_err().reason, ErrorReason::OutOfFuel);
    let limits = ExprLimits { fuel: 250, ..ExprLimits::default() };
    assert_eq!(Expr::parse("let local = \"\" in absent").unwrap().eval(&names, limits, &|| false).unwrap_err().reason, ErrorReason::OutOfFuel);
    let first = Value::Text("first".into());
    let last = Value::Text("last".into());
    assert_eq!(Expr::parse("target").unwrap().eval(&[("target", &first), ("target", &last)], ExprLimits::default(), &|| false).unwrap(), last);
    assert_eq!(
        Expr::parse("let target = \"local\" in target").unwrap().eval(&[("target", &last)], ExprLimits::default(), &|| false).unwrap(),
        Value::Text("local".into())
    );
}

#[test]
fn admission_and_lookup_both_poll_cancellation() {
    use std::cell::Cell;
    let empty = Value::Text(String::new());
    let names = vec![("other", &empty); 5000];
    let polls = Cell::new(0);
    let cancel = || {
        polls.set(polls.get() + 1);
        polls.get() >= 2
    };
    assert_eq!(Expr::parse("absent").unwrap().eval(&names, ExprLimits::default(), &cancel).unwrap_err().reason, ErrorReason::Cancelled);
    assert_eq!(polls.get(), 2, "one poll in admission, the second during the unsuccessful lookup");
}

fn parameter_action(default: serde_json::Value) -> CompiledTextAction {
    TextActionDefinition {
        schema: 1,
        id: "user.params".into(),
        version: 1,
        label: "Parameters".into(),
        inputs: vec![PortDef { name: "input".into(), kind: InputKind::Text }],
        params: vec![ParamDef { name: "parameter".into(), kind: ParamKind::Text, default }],
        outputs: vec![TextOutputDef { name: "out".into(), kind: OutputKind::Text, expr: "parameter".into() }],
    }
    .compile()
    .unwrap()
}

#[test]
fn effective_defaults_and_overrides_share_input_and_value_admission() {
    let action = parameter_action(serde_json::json!("1234"));
    let input = BTreeMap::from([("input".into(), Value::Text(String::new()))]);
    let limits = ExprLimits { value_bytes: 3, ..ExprLimits::default() };
    assert_eq!(action.run(&input, &BTreeMap::new(), limits, &|| false).unwrap_err().reason, ErrorReason::ValueTooLarge);
    let overrides = BTreeMap::from([("parameter".into(), serde_json::json!("xyz"))]);
    assert_eq!(action.run(&input, &overrides, limits, &|| false).unwrap().first().unwrap().1, Value::Text("xyz".into()));
    let input = BTreeMap::from([("input".into(), Value::Text("12345".into()))]);
    let limits = ExprLimits { input_bytes: 8, ..ExprLimits::default() };
    assert_eq!(action.run(&input, &BTreeMap::new(), limits, &|| false).unwrap_err().reason, ErrorReason::InputTooLarge);
    let overrides = BTreeMap::from([("parameter".into(), serde_json::json!("xyz"))]);
    assert!(action.run(&input, &overrides, limits, &|| false).is_ok());
    let bad = BTreeMap::from([("parameter".into(), serde_json::json!("PRIVATE-OVERSIZED"))]);
    let error = action
        .run(&BTreeMap::from([("input".into(), Value::Text(String::new()))]), &bad, ExprLimits { value_bytes: 3, ..ExprLimits::default() }, &|| false)
        .unwrap_err();
    assert_eq!(error.reason, ErrorReason::ValueTooLarge);
    assert!(!error.to_string().contains("PRIVATE"));
}

#[test]
fn lowered_aggregate_outputs_refuse_one_extra_byte_and_preserve_order() {
    let mut definition = repeated_text_action(4).definition().clone();
    for output in &mut definition.outputs {
        output.expr = "\"12345678\"".into();
    }
    let limits = ExprLimits { value_bytes: 8, ..ExprLimits::default() };
    let action = definition.compile().unwrap();
    let outputs = action.run(&BTreeMap::new(), &BTreeMap::new(), limits, &|| false).unwrap();
    assert_eq!(outputs.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), vec!["output0", "output1", "output2", "output3"]);
    definition.outputs.push(TextOutputDef { name: "extra".into(), kind: OutputKind::Text, expr: "\"x\"".into() });
    let error = definition.compile().unwrap().run(&BTreeMap::new(), &BTreeMap::new(), limits, &|| false).unwrap_err();
    assert_eq!(error.reason, ErrorReason::ValueTooLarge);
    assert_eq!(error.part.as_deref(), Some("extra"));
}

#[test]
fn text_and_empty_list_scalar_producers_refuse_before_a_raw_oversize_candidate() {
    for source in [
        "len(\"\")",
        "len([])",
        "contains(\"\", \"\")",
        "count([], [])",
        "index_of([], [])",
        "starts_with(\"\", \"\")",
        "ends_with(\"\", \"\")",
        "\"\" == \"\"",
        "[] != []",
        "\"\" <= \"\"",
    ] {
        assert_eq!(limited(source, &[], 0).unwrap_err().reason, ErrorReason::ValueTooLarge, "{source}");
        assert!(limited(source, &[], 8).is_ok(), "{source}");
    }
    assert_eq!(limited("int(\"1\")", &[], 1).unwrap_err().reason, ErrorReason::ValueTooLarge);
    assert_eq!(limited("int(\"x\")", &[], 1).unwrap_err().reason, ErrorReason::InvalidConversion, "a failed primitive parse creates no scalar result");
    assert_eq!(limited("int(\"\")", &[], 0).unwrap_err().reason, ErrorReason::InvalidConversion);
}
