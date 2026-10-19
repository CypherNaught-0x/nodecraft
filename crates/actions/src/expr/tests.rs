use super::*;
use proptest::prelude::*;
use std::cell::Cell;
use std::collections::BTreeMap;

fn never() -> bool {
    false
}

fn run(source: &str) -> Result<Value, ExprError> {
    Expr::parse(source)?.eval(&[], ExprLimits::default(), &never)
}

fn run_with(source: &str, names: &[(&str, Value)]) -> Result<Value, ExprError> {
    let names: Vec<(&str, &Value)> = names.iter().map(|(n, v)| (*n, v)).collect();
    Expr::parse(source)?.eval(&names, ExprLimits::default(), &never)
}

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

fn list(items: &[&str]) -> Value {
    Value::List(items.iter().map(|s| t(s)).collect())
}

fn reason(source: &str) -> ErrorReason {
    run(source).unwrap_err().reason
}

#[test]
fn arithmetic_comparison_and_logic() {
    assert_eq!(run("1 + 2 * 3 - 4 / 2 % 3").unwrap(), Value::Int(5));
    assert_eq!(run("-7 / 2").unwrap(), Value::Int(-3), "division truncates toward zero");
    assert_eq!(run("-7 % 2").unwrap(), Value::Int(-1));
    assert_eq!(run("(1 + 2) * 3").unwrap(), Value::Int(9));
    assert_eq!(run("1 < 2 and \"a\" < \"b\" and not (2 <= 1) or false").unwrap(), Value::Bool(true));
    assert_eq!(run("[1, \"a\"] == [1, \"a\"]").unwrap(), Value::Bool(true));
    assert_eq!(run("1 == \"1\"").unwrap(), Value::Bool(false), "different kinds are unequal, not an error");
    assert_eq!(reason("9223372036854775807 + 1"), ErrorReason::Overflow);
    assert_eq!(reason("-9223372036854775807 - 2"), ErrorReason::Overflow);
    assert_eq!(reason("1 / 0"), ErrorReason::DivisionByZero);
    assert_eq!(reason("1 % 0"), ErrorReason::DivisionByZero);
    assert_eq!(reason("1 < \"a\""), ErrorReason::TypeMismatch);
    assert_eq!(reason("true < false"), ErrorReason::TypeMismatch);
    assert_eq!(reason("1 and true"), ErrorReason::TypeMismatch);
    assert_eq!(run("false and 1").unwrap(), Value::Bool(false), "short-circuits");
    assert_eq!(reason("9223372036854775808"), ErrorReason::InvalidNumber);
}

#[test]
fn let_if_and_scoping() {
    assert_eq!(run("let x = 2 in let y = x * 3 in if y > 5 then \"big\" else \"small\"").unwrap(), t("big"));
    assert_eq!(run("let x = 1 in (let x = 2 in x) + x").unwrap(), Value::Int(3), "inner bindings shadow and end");
    assert_eq!(reason("let x = 1 in y"), ErrorReason::UnknownName);
    assert_eq!(reason("if 1 then 2 else 3"), ErrorReason::TypeMismatch);
    assert_eq!(reason("let len = 1 in len"), ErrorReason::UnexpectedToken, "built-in names can't be bound");
}

#[test]
fn text_functions_are_unicode_aware() {
    let names = [("name", t("  Ünïcødé Straße  "))];
    assert_eq!(run_with("trim(name)", &names).unwrap(), t("Ünïcødé Straße"));
    assert_eq!(run_with("upper(trim(name))", &names).unwrap(), t("ÜNÏCØDÉ STRASSE"));
    assert_eq!(run_with("len(trim(name))", &names).unwrap(), Value::Int(14));
    assert_eq!(run("slice(\"héllo wörld\", 1, 4)").unwrap(), t("éll"));
    assert_eq!(run("slice(\"héllo\", -2)").unwrap(), t("lo"));
    assert_eq!(run("slice(\"héllo\", 10)").unwrap(), t(""), "bounds clamp");
    assert_eq!(run("\"日本語\"[1] + \"日本語\"[-1]").unwrap(), t("本語"));
    assert_eq!(reason("\"ab\"[2]"), ErrorReason::IndexOutOfRange);
    assert_eq!(run("reverse(\"åbc\")").unwrap(), t("cbå"));
    assert_eq!(run("index_of(\"naïve café\", \"café\")").unwrap(), Value::Int(6), "a character index, not a byte offset");
    assert_eq!(run("index_of(\"abc\", \"z\")").unwrap(), Value::Int(-1));
    assert_eq!(run("count(\"banana\", \"an\")").unwrap(), Value::Int(2));
    assert_eq!(run("replace(\"a-b-c\", \"-\", \"–\")").unwrap(), t("a–b–c"));
    assert_eq!(run("pad_start(\"7\", 3, \"0\") + pad_end(\"é\", 3, \".\")").unwrap(), t("007é.."));
    assert_eq!(reason("pad_start(\"7\", 3, \"00\")"), ErrorReason::TypeMismatch);
    assert_eq!(run("starts_with(\"prefix-x\", \"prefix\") and ends_with(\"x.png\", \".png\")").unwrap(), Value::Bool(true));
    assert_eq!(run("\"tab\\there \\u{1F600} \\\"q\\\"\"").unwrap(), t("tab\there 😀 \"q\""));
    assert_eq!(reason("\"bad \\u{D800}\""), ErrorReason::InvalidString, "surrogates are not scalar values");
    assert_eq!(reason("\"open"), ErrorReason::InvalidString);
    assert_eq!(reason("replace(\"a\", \"\", \"b\")"), ErrorReason::EmptySeparator);
    assert_eq!(reason("split(\"a\", \"\")"), ErrorReason::EmptySeparator);
}

#[test]
fn lists_and_per_item_functions() {
    assert_eq!(run("split(\"b,a,c,a\", \",\")").unwrap(), list(&["b", "a", "c", "a"]));
    assert_eq!(run("sort(unique(split(\"b,a,c,a\", \",\")))").unwrap(), list(&["a", "b", "c"]));
    assert_eq!(run("join(map(split(\"x y\", \" \"), upper(it) + text(index)), \"|\")").unwrap(), t("X0|Y1"));
    assert_eq!(run("filter(lines(\"keep\\n\\nskip me\\nkeep too\"), starts_with(it, \"keep\"))").unwrap(), list(&["keep", "keep too"]));
    assert_eq!(run("range(0, 4)").unwrap(), Value::List((0..4).map(Value::Int).collect()));
    assert_eq!(run("range(3, 1)").unwrap(), Value::List(vec![]));
    assert_eq!(run("first(chars(\"héy\")) + last([\"a\", \"z\"])").unwrap(), t("hz"));
    assert_eq!(reason("first([])"), ErrorReason::EmptyList);
    assert_eq!(run("[1, 2] + [3]").unwrap(), Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
    assert_eq!(run("contains([\"a\", \"b\"], \"b\") and not contains(\"abc\", \"d\")").unwrap(), Value::Bool(true));
    assert_eq!(run("min(3, 2) + max(3, 2)").unwrap(), Value::Int(5));
    assert_eq!(run("int(\" -42 \") + len(repeat([1], 3))").unwrap(), Value::Int(-39));
    assert_eq!(reason("int(\"4.2\")"), ErrorReason::InvalidConversion);
    assert_eq!(reason("sort([1, \"a\"])"), ErrorReason::TypeMismatch);
    assert_eq!(reason("sort([[1]])"), ErrorReason::TypeMismatch);
    assert_eq!(reason("map(\"abc\", it)"), ErrorReason::TypeMismatch);
    assert_eq!(reason("filter([1], it)"), ErrorReason::TypeMismatch);
    assert_eq!(reason("it"), ErrorReason::UnknownName, "`it` exists only inside map/filter");
    assert_eq!(reason("repeat(\"a\", -1)"), ErrorReason::IndexOutOfRange);
    assert_eq!(run("text(12) + text(true)").unwrap(), t("12true"));
    assert_eq!(reason("text([1])"), ErrorReason::TypeMismatch);
    assert_eq!(run("[1, 2, 3][-1]").unwrap(), Value::Int(3));
}

#[test]
fn syntax_errors_name_a_position_and_never_echo_values() {
    let e = run("1 +").unwrap_err();
    assert_eq!((e.reason, e.at), (ErrorReason::UnexpectedEnd, 3));
    let e = run("len(1, 2)").unwrap_err();
    assert_eq!((e.reason, e.at), (ErrorReason::WrongArgumentCount, 0));
    assert_eq!(reason("nope(1)"), ErrorReason::UnknownFunction);
    assert_eq!(reason("len"), ErrorReason::UnknownName);
    assert_eq!(reason("1 2"), ErrorReason::UnexpectedToken);
    assert_eq!(reason("a ! b"), ErrorReason::InvalidCharacter);
    assert_eq!(reason("12abc"), ErrorReason::InvalidNumber);
    assert_eq!(reason("@"), ErrorReason::InvalidCharacter);
    assert_eq!(run("1 # a comment\n + 1").unwrap(), Value::Int(2));
    let secret = [("prompt", t("SECRET-PROMPT-TEXT"))];
    let e = run_with("int(prompt)", &secret).unwrap_err();
    assert!(!e.to_string().contains("SECRET"), "{e}");
}

#[test]
fn parsing_is_bounded() {
    assert_eq!(Expr::parse(&"1".repeat(MAX_SOURCE_BYTES + 1)).unwrap_err().reason, ErrorReason::SourceTooLong);
    let nested = format!("{}1{}", "(".repeat(MAX_DEPTH + 1), ")".repeat(MAX_DEPTH + 1));
    assert_eq!(Expr::parse(&nested).unwrap_err().reason, ErrorReason::TooDeep);
    let chain = vec!["1"; 200].join(" + ");
    assert_eq!(Expr::parse(&chain).unwrap_err().reason, ErrorReason::TooDeep, "operator chains are bounded too");
    let index = format!("[1]{}", "[0]".repeat(200));
    assert_eq!(Expr::parse(&index).unwrap_err().reason, ErrorReason::TooDeep);
    let wide = format!("[{}]", vec!["1"; MAX_NODES].join(","));
    assert!(matches!(Expr::parse(&wide).unwrap_err().reason, ErrorReason::TooComplex | ErrorReason::SourceTooLong));
    assert_eq!(run(&vec!["1"; 90].join(" + ")).unwrap(), Value::Int(90));
}

#[test]
fn running_is_bounded_by_fuel_values_items_and_scope() {
    let small = ExprLimits { fuel: 1000, ..ExprLimits::default() };
    let e = Expr::parse("map(range(0, 10000), it * 2)").unwrap().eval(&[], small, &never).unwrap_err();
    assert_eq!(e.reason, ErrorReason::OutOfFuel);
    assert_eq!(reason("repeat(\"x\", 5000000)"), ErrorReason::ValueTooLarge);
    assert_eq!(reason("range(0, 20000)"), ErrorReason::TooManyItems);
    assert_eq!(reason("let a = repeat(\"x\", 1000000) in let b = a + a in let c = b + b in c + c"), ErrorReason::ValueTooLarge);
    assert_eq!(reason("replace(repeat(\"a\", 100000), \"a\", repeat(\"b\", 100))"), ErrorReason::ValueTooLarge, "refused before building");
    assert_eq!(reason("split(repeat(\",\", 20000), \",\")"), ErrorReason::TooManyItems);
    // Nested repetition multiplies work; fuel stops it.
    let bomb = "map(range(0, 10000), map(range(0, 10000), it))";
    assert!(matches!(reason(bomb), ErrorReason::OutOfFuel | ErrorReason::ValueTooLarge));
    let ceiling = ExprLimits { fuel: MAX_FUEL + 1, ..ExprLimits::default() };
    assert_eq!(Expr::parse("1").unwrap().eval(&[], ceiling, &never).unwrap_err().reason, ErrorReason::InvalidLimits);
    let big = Value::Text("x".repeat(MAX_INPUT_BYTES + 1));
    assert_eq!(run_with("1", &[("big", big)]).unwrap_err().reason, ErrorReason::InputTooLarge);
}

#[test]
fn cancellation_is_polled_while_working() {
    let polls = Cell::new(0);
    let cancel = || {
        polls.set(polls.get() + 1);
        polls.get() > 2
    };
    let e = Expr::parse("map(range(0, 10000), len(text(it)))").unwrap().eval(&[], ExprLimits::default(), &cancel).unwrap_err();
    assert_eq!(e.reason, ErrorReason::Cancelled);
    assert_eq!(polls.get(), 3);
}

#[test]
fn evaluation_is_deterministic() {
    let source = "join(sort(unique(map(split(lower(input), \" \"), trim(it)))), \",\")";
    let names = [("input", t("Zeta alpha  Beta alpha ZETA"))];
    let first = run_with(source, &names).unwrap();
    for _ in 0..10 {
        assert_eq!(run_with(source, &names).unwrap(), first);
    }
    assert_eq!(first, t(",alpha,beta,zeta"));
}

fn slugify() -> TextActionDefinition {
    serde_json::from_value(serde_json::json!({
        "schema": 1,
        "id": "user.slugify",
        "version": 2,
        "label": "Slugify titles",
        "inputs": [{"name": "titles", "kind": "text_list"}, {"name": "prefix", "kind": "text"}],
        "params": [{"name": "sep", "kind": "text", "default": "-"}, {"name": "keep", "kind": "int", "default": 12}],
        "outputs": [
            {"name": "slugs", "kind": "text_list", "expr": "map(titles, prefix + slice(join(split(lower(trim(it)), \" \"), sep), 0, keep))"},
            {"name": "summary", "kind": "text", "expr": "text(len(titles)) + \" titles\""}
        ]
    }))
    .unwrap()
}

#[test]
fn definitions_compile_once_and_run_with_inputs_and_parameters() {
    let action = slugify().compile().unwrap();
    let inputs = BTreeMap::from([("titles".to_owned(), list(&["  Hello Big World ", "Ünï Code"])), ("prefix".to_owned(), t("p/"))]);
    let out = action.run(&inputs, &BTreeMap::new(), ExprLimits::default(), &never).unwrap();
    assert_eq!(out, vec![("slugs".to_owned(), list(&["p/hello-big-wo", "p/ünï-code"])), ("summary".to_owned(), t("2 titles"))]);
    let params = BTreeMap::from([("sep".to_owned(), serde_json::json!("_")), ("keep".to_owned(), serde_json::json!(50))]);
    let out = action.run(&inputs, &params, ExprLimits::default(), &never).unwrap();
    assert_eq!(out.first().unwrap().1, list(&["p/hello_big_world", "p/ünï_code"]));
    // The portable wire form round-trips exactly.
    let wire = serde_json::to_string(action.definition()).unwrap();
    assert_eq!(serde_json::from_str::<TextActionDefinition>(&wire).unwrap(), slugify());
}

#[test]
fn bad_runs_are_refused_with_the_declared_name_only() {
    let action = slugify().compile().unwrap();
    let good = BTreeMap::from([("titles".to_owned(), list(&["a"])), ("prefix".to_owned(), t(""))]);
    let check = |inputs: &BTreeMap<String, Value>, params: &BTreeMap<String, serde_json::Value>, expected: ErrorReason| {
        let e = action.run(inputs, params, ExprLimits::default(), &never).unwrap_err();
        assert_eq!(e.reason, expected);
        e
    };
    let none = BTreeMap::new();
    let mut missing = good.clone();
    missing.remove("prefix");
    assert_eq!(check(&missing, &none, ErrorReason::MissingInput).part.as_deref(), Some("prefix"));
    let mut extra = good.clone();
    extra.insert("PRIVATE_KEY_NAME".into(), t(""));
    let e = check(&extra, &none, ErrorReason::UnknownInput);
    assert!(!e.to_string().contains("PRIVATE"));
    let mut wrong = good.clone();
    wrong.insert("titles".into(), t("not a list"));
    check(&wrong, &none, ErrorReason::WrongInputKind);
    let mut mixed = good.clone();
    mixed.insert("titles".into(), Value::List(vec![Value::Int(1)]));
    check(&mixed, &none, ErrorReason::WrongInputKind);
    check(&good, &BTreeMap::from([("keep".to_owned(), serde_json::json!("12"))]), ErrorReason::WrongParamKind);
    check(&good, &BTreeMap::from([("other".to_owned(), serde_json::json!(1))]), ErrorReason::UnknownParam);
    let mut bad_output = slugify();
    bad_output.outputs.truncate(1);
    bad_output.outputs[0].expr = "len(titles)".into();
    let e = bad_output.compile().unwrap().run(&good, &none, ExprLimits::default(), &never).unwrap_err();
    assert_eq!((e.reason, e.part.as_deref()), (ErrorReason::WrongOutputKind, Some("slugs")));
}

#[test]
fn one_fuel_budget_covers_every_output() {
    let mut def = slugify();
    def.outputs =
        (0..3).map(|n| TextOutputDef { name: format!("out{n}"), kind: OutputKind::Text, expr: "join(map(range(0, 300), text(it)), \"\")".into() }).collect();
    let action = def.compile().unwrap();
    let inputs = BTreeMap::from([("titles".to_owned(), list(&[])), ("prefix".to_owned(), t(""))]);
    let one = Expr::parse("join(map(range(0, 300), text(it)), \"\")").unwrap();
    // The exact fuel one output needs.
    let (mut low, mut high) = (0u64, 1_000_000u64);
    while low + 1 < high {
        let mid = (low + high) / 2;
        // Input inspection now shares fuel; compare the same admitted scope.
        let names: Vec<(&str, &Value)> = inputs.iter().map(|(name, value)| (name.as_str(), value)).collect();
        if one.eval(&names, ExprLimits { fuel: mid, ..ExprLimits::default() }, &never).is_ok() {
            high = mid;
        } else {
            low = mid;
        }
    }
    let run = |fuel| action.run(&inputs, &BTreeMap::new(), ExprLimits { fuel, ..ExprLimits::default() }, &never);
    assert_eq!(run(high * 5 / 2).unwrap_err().reason, ErrorReason::OutOfFuel, "enough for two outputs, not three");
    assert!(run(high * 3).is_ok());
}

type Edit = fn(&mut TextActionDefinition);

#[test]
fn malformed_definitions_are_refused() {
    let edits: Vec<(&str, Edit)> = vec![
        ("schema", |d| d.schema = 2),
        ("id", |d| d.id = "NoDot".into()),
        ("id", |d| d.id = "user..x".into()),
        ("version", |d| d.version = 0),
        ("label", |d| d.label = " ".into()),
        ("ports", |d| d.outputs.clear()),
        ("titles", |d| d.params[0].name = "titles".into()),
        ("inputs[0]", |d| d.inputs[0].name = "len".into()),
        ("params[1]", |d| d.params[1].name = "max".into()),
        ("inputs[0]", |d| d.inputs[0].name = "it".into()),
        ("inputs[0]", |d| d.inputs[0].name = "Upper".into()),
        ("outputs[1]", |d| d.outputs[1].name = format!("{}\u{1b}[31m", "x".repeat(100_000))),
        ("keep", |d| d.params[1].default = serde_json::json!("12")),
        ("keep", |d| d.params[1].default = serde_json::json!(1.5)),
        ("slugs", |d| d.outputs[0].expr = "titles +".into()),
        ("slugs", |d| d.outputs[0].expr = "undeclared".into()),
    ];
    for (part, edit) in edits {
        let mut def = slugify();
        edit(&mut def);
        let e = def.compile().unwrap_err();
        assert_eq!(e.part.as_deref(), Some(part), "{e}");
        assert!(e.to_string().len() < 200, "errors stay short");
    }
    assert!(
        serde_json::from_value::<TextActionDefinition>(serde_json::json!({"schema": 1, "id": "a.b", "version": 1, "label": "x", "outputs": [], "extra": 1}))
            .is_err()
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_source_never_panics(source in "[ -~\\n]{0,200}") {
        if let Ok(expr) = Expr::parse(&source) {
            let names = [("x", &Value::Text("héllo, wörld".into())), ("n", &Value::Int(3))];
            let _ = expr.eval(&names, ExprLimits { fuel: 100_000, ..ExprLimits::default() }, &never);
        }
    }

    #[test]
    fn token_soup_never_panics(parts in proptest::collection::vec(prop::sample::select(vec![
        "(", ")", "[", "]", ",", "+", "-", "*", "/", "%", "==", "<", "let", "x", "=", "in", "if", "then", "else",
        "map", "filter", "it", "index", "slice", "repeat", "range", "split", "join", "\"a,b\"", "\",\"", "0", "-1",
        "9223372036854775807", "true", "not", "and", "or", "len", "chars", "sort", "unique", "pad_start", "3", "\"é\"",
    ]), 0..60)) {
        let source = parts.join(" ");
        if let Ok(expr) = Expr::parse(&source) {
            let x = Value::List(vec![Value::Text("é".into()), Value::Text("b".into())]);
            let _ = expr.eval(&[("x", &x)], ExprLimits { fuel: 100_000, ..ExprLimits::default() }, &never);
        }
    }
}

#[test]
fn repeating_nothing_is_instant_and_large_counts_agree_on_every_target() {
    let cancelled = || true;
    let tiny = ExprLimits { fuel: 10, ..ExprLimits::default() };
    for source in ["repeat([], 9223372036854775807)", "repeat(\"\", 9223372036854775807)", "repeat(\"x\", 0)"] {
        let started = std::time::Instant::now();
        let v = Expr::parse(source).unwrap().eval(&[], tiny, &cancelled).unwrap();
        assert!(v == Value::List(vec![]) || v == t(""), "{source}");
        assert!(started.elapsed() < std::time::Duration::from_secs(1), "{source}");
    }
    // Counts beyond 32 bits are judged in 64 bits, so wasm32 and native give the same answer.
    assert_eq!(reason("repeat(\"x\", 5000000000)"), ErrorReason::ValueTooLarge);
    assert_eq!(reason("repeat([1], 5000000000)"), ErrorReason::TooManyItems);
    assert_eq!(reason("pad_start(\"a\", 5000000000, \"x\")"), ErrorReason::ValueTooLarge);
    assert_eq!(run("pad_start(\"abc\", -5, \"x\")").unwrap(), t("abc"));
}

#[test]
fn held_temporaries_share_one_budget() {
    let big = Value::Text("x".repeat(MAX_INPUT_BYTES));
    // Each nested comparison keeps its left side alive while the right is evaluated.
    let mut source = "big".to_owned();
    for _ in 0..8 {
        source = format!("big == ({source})");
    }
    assert_eq!(run_with(&source, &[("big", big)]).unwrap_err().reason, ErrorReason::ValueTooLarge);
    assert_eq!(run_with("[big, big, big, big]", &[("big", Value::Text("y".repeat(1024 * 1024)))]).unwrap_err().reason, ErrorReason::ValueTooLarge);
    // Results a map is still building count too.
    let mut nested = "1".to_owned();
    for _ in 0..15 {
        nested = format!("map([0, 1], if index == 0 then repeat(\"x\", 4000000) else {nested})");
    }
    assert_eq!(reason(&nested), ErrorReason::ValueTooLarge);
}

#[test]
fn deeply_nested_input_values_are_refused() {
    let mut v = Value::Int(1);
    for _ in 0..=MAX_VALUE_DEPTH {
        v = Value::List(vec![v]);
    }
    assert_eq!(run_with("1", &[("v", v)]).unwrap_err().reason, ErrorReason::TooDeep);
}

/// Worst-case recursion within the parse limits, on a thread with half the default stack, in
/// whatever profile the tests run (debug frames are the large ones).
#[test]
fn the_deepest_allowed_expressions_fit_a_small_stack() {
    let chain = |n: usize, term: &str| vec![term; n].join(" + ");
    let worst = vec![
        format!("{}1{}", "(".repeat(MAX_DEPTH - 1), ")".repeat(MAX_DEPTH - 1)),
        chain(95, "x"),
        format!("{}{}{}", "min(1, ".repeat(MAX_DEPTH - 2), chain(60, "1"), ")".repeat(MAX_DEPTH - 2)),
        format!("{}{}{}", "map([1], ".repeat(30), chain(35, "it"), ")[0]".repeat(30)),
        format!("{}{}", "let a = 1 in ".repeat(MAX_DEPTH - 2), chain(60, "a")),
        format!("[1]{}", "[0]".repeat(94)),
    ];
    for source in &worst {
        assert!(Expr::parse(source).is_ok(), "every worst case must parse to reach full depth: {}", source.get(..40).unwrap_or(source));
    }
    let handle = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || {
            for source in worst {
                let expr = Expr::parse(&source).unwrap();
                let _ = expr.eval(&[("x", &Value::Int(1))], ExprLimits::default(), &never);
                let _ = Expr::parse(&source).map(|e| e.free_names());
            }
        })
        .unwrap();
    handle.join().unwrap();
}
