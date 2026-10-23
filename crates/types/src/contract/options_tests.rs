use super::*;
use crate::{Choice, ParamValues, Value};

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
fn choices(values: &[&str]) -> Vec<Choice> {
    values.iter().map(|value| Choice { value: (*value).into(), label: value.to_uppercase() }).collect()
}
fn text(value: &str) -> Value {
    Value::Text(value.into())
}
fn values(pairs: &[(&str, &str)]) -> ParamValues {
    pairs.iter().map(|(name, value)| ((*name).to_owned(), text(value))).collect()
}

/// A ComfyUI model: `checkpoint` comes from the server's checkpoint list, `lora` from the list
/// for the chosen checkpoint, and `lora` is only enabled while `style` is on.
fn comfy() -> Contract {
    let mut style = line("style");
    style.control = ParamControl::Toggle;
    let mut contract = Contract::from_schema("comfyui.sdxl", ParamOwner::Model, ParamSchema { params: vec![line("checkpoint"), style, line("lora")] });
    contract.rules = vec![
        ParamRule { options: Some(OptionSource { id: "comfyui.checkpoints".into(), scope: None }), ..ParamRule::implied("checkpoint") },
        ParamRule {
            options: Some(OptionSource { id: "comfyui.loras".into(), scope: Some("checkpoint".into()) }),
            enabled_when: Some(Condition::Equals { param: "style".into(), value: Value::Bool(true) }),
            ..ParamRule::implied("lora")
        },
    ];
    contract.validate().unwrap();
    contract
}
type Mutate = fn(&mut Contract);
fn key<'a>(source: &'a str, scope: Option<&'a str>) -> OptionKey<'a> {
    OptionKey { owner: "comfyui.sdxl", source, scope }
}
fn codes(resolution: &Resolution) -> Vec<(IssueCode, Option<&str>)> {
    resolution.issues.iter().map(|issue| (issue.code, issue.param.as_deref())).collect()
}

#[test]
fn an_unknown_list_refuses_nothing_and_says_so() {
    let contract = comfy();
    let resolution = contract.resolve(&values(&[("checkpoint", "anything.safetensors")]), true).unwrap();
    assert!(resolution.issues.is_empty(), "the provider checks a value whose list was never fetched");
    let state = resolution.get("checkpoint").unwrap();
    assert_eq!(state.options, Some(ParamOptions { source: "comfyui.checkpoints".into(), scope: None, known: false }));
    assert_eq!(resolution.get("lora").unwrap().options, None, "a disabled parameter draws from no list");
}

#[test]
fn a_value_off_a_known_list_blocks_a_run_but_not_editing() {
    let contract = comfy();
    let mut lists = OptionLists::default();
    lists.set(key("comfyui.checkpoints", None), choices(&["sdxl.safetensors", "flux.safetensors"])).unwrap();
    let ok = contract.resolve_with(&values(&[("checkpoint", "flux.safetensors")]), true, &lists).unwrap();
    assert!(ok.issues.is_empty());
    assert!(ok.get("checkpoint").unwrap().options.as_ref().unwrap().known);

    let gone = values(&[("checkpoint", "removed.safetensors")]);
    let blocked = contract.resolve_with(&gone, true, &lists).unwrap();
    assert_eq!(codes(&blocked), vec![(IssueCode::NotAnOption, Some("checkpoint"))]);
    assert!(blocked.submission().is_err(), "a value off the list never reaches a provider");
    assert!(contract.resolve_with(&gone, true, &lists).unwrap().into_resolved(true).is_err());
    assert!(contract.resolve_with(&gone, false, &lists).unwrap().into_resolved(false).is_ok(), "the node stays editable");
}

#[test]
fn a_scoped_list_follows_its_scope_value() {
    let contract = comfy();
    let mut lists = OptionLists::default();
    lists.set(key("comfyui.loras", Some("sdxl.safetensors")), choices(&["ink"])).unwrap();
    let on = |checkpoint: &str, lora: &str| {
        let mut v = values(&[("checkpoint", checkpoint), ("lora", lora)]);
        v.insert("style".into(), Value::Bool(true));
        contract.resolve_with(&v, true, &lists).unwrap()
    };
    assert!(on("sdxl.safetensors", "ink").issues.is_empty());
    assert_eq!(codes(&on("sdxl.safetensors", "oil")), vec![(IssueCode::NotAnOption, Some("lora"))]);
    let other = on("flux.safetensors", "oil");
    assert!(other.issues.is_empty(), "another checkpoint's list is unknown, so nothing is refused");
    assert_eq!(other.get("lora").unwrap().options, Some(ParamOptions { source: "comfyui.loras".into(), scope: Some("flux.safetensors".into()), known: false }));
    let unscoped = contract.resolve_with(&[("style".into(), Value::Bool(true)), ("lora".into(), text("oil"))].into(), true, &lists).unwrap();
    assert!(unscoped.issues.is_empty(), "with no checkpoint the scoped list is unknown");
    assert_eq!(unscoped.get("lora").unwrap().options.as_ref().map(|o| (o.scope.clone(), o.known)), Some((None, false)));
}

#[test]
fn option_sources_are_validated() {
    let bad: [(&str, Mutate); 6] = [
        ("multiline text", (|c| c.schema.params[0].control = ParamControl::Text { multiline: true, max_bytes: 8 })),
        ("toggle", (|c| c.rules[0].param = "style".into())),
        ("bad id", (|c| c.rules[0].options = Some(OptionSource { id: "Comfy Checkpoints".into(), scope: None }))),
        ("scope ahead", (|c| c.rules[0].options = Some(OptionSource { id: "a.b".into(), scope: Some("lora".into()) }))),
        ("scope itself", (|c| c.rules[0].options = Some(OptionSource { id: "a.b".into(), scope: Some("checkpoint".into()) }))),
        ("toggle scope", (|c| c.rules[1].options = Some(OptionSource { id: "a.b".into(), scope: Some("style".into()) }))),
    ];
    for (name, mutate) in bad {
        let mut contract = comfy();
        mutate(&mut contract);
        assert!(contract.validate().is_err(), "{name}");
    }
    let mut model = comfy();
    model.schema.params[0].control = ParamControl::Model;
    model.validate().unwrap();
    let json = serde_json::to_string(&comfy()).unwrap();
    assert_eq!(serde_json::from_str::<Contract>(&json).unwrap(), comfy());
    assert!(json.contains(r#""options":{"id":"comfyui.loras","scope":"checkpoint"}"#));
    assert_eq!(ContractVersion::CURRENT, ContractVersion { major: 1, minor: 2 });
}

#[test]
fn option_lists_are_bounded_and_keep_the_old_list_on_refusal() {
    let mut lists = OptionLists::default();
    let k = key("comfyui.checkpoints", None);
    lists.set(k, choices(&["a"])).unwrap();
    for bad in [choices(&["a", "a"]), vec![Choice { value: String::new(), label: "x".into() }], vec![Choice { value: "x".into(), label: " ".into() }]] {
        assert!(lists.set(k, bad).is_err());
    }
    assert!(lists.set(k, (0..=MAX_OPTIONS).map(|n| Choice { value: n.to_string(), label: n.to_string() }).collect()).is_err());
    assert_eq!(lists.get(k).unwrap(), choices(&["a"]).as_slice(), "a refused list keeps the previous one");
    assert!(lists.set(OptionKey { source: "Not Dotted", ..k }, choices(&["a"])).is_err());

    let scopes: Vec<String> = (0..MAX_OPTION_LISTS).map(|n| n.to_string()).collect();
    for scope in &scopes[1..] {
        lists.set(key("comfyui.loras", Some(scope)), choices(&["a"])).unwrap();
    }
    assert_eq!(lists.len(), MAX_OPTION_LISTS);
    assert!(lists.set(key("comfyui.loras", Some("one more")), choices(&["a"])).is_err());
    lists.set(k, choices(&["b"])).unwrap();
    assert_eq!(lists.contains(k, "b"), Some(true), "replacing a list never counts against the list limit");
    lists.clear(k);
    assert!(!lists.known(k));
    assert_eq!(lists.contains(k, "b"), None);
    lists.set(key("comfyui.loras", Some("one more")), choices(&["a"])).unwrap();

    let big = |prefix: &str| (0..MAX_OPTIONS).map(|n| Choice { value: format!("{prefix}{n:0>200}"), label: "x".repeat(300) }).collect::<Vec<_>>();
    let mut bytes = OptionLists::default();
    bytes.set(key("a.one", None), big("a")).unwrap();
    assert!(bytes.set(key("a.two", None), big("b")).is_err(), "aggregate text is capped");
    bytes.set(key("a.one", None), big("c")).unwrap();
    bytes.clear_sources("a.");
    assert!(bytes.is_empty());
    bytes.set(key("a.two", None), big("b")).unwrap();
    assert!(bytes.get(key("a.two", None)).is_some(), "clearing by source frees its bytes");

    let mut mixed = OptionLists::default();
    mixed.set(key("comfyui.checkpoints", None), choices(&["x"])).unwrap();
    mixed.set(key("comfyui.loras", Some("x")), choices(&["y"])).unwrap();
    mixed.set(key("fal.voices", None), choices(&["z"])).unwrap();
    mixed.clear_sources("comfyui.");
    assert_eq!(mixed.len(), 1);
    assert!(mixed.get(key("fal.voices", None)).is_some(), "other providers' lists stay");
}
