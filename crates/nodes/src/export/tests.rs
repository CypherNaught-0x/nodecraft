use super::*;
use crate::{InputValue, Registry};
use nodecraft_types::{NodeId, ParamValues};

fn node() -> (Registry, Node) {
    let kinds = Registry::builtins().unwrap();
    let node = kinds.get(ID).unwrap().instantiate_instance(NodeId::from_bytes([7; 16]), Default::default(), &ParamValues::default()).unwrap();
    (kinds, node)
}

#[test]
fn an_export_card_declares_media_ports_and_its_schema() {
    let (kinds, node) = node();
    let kind = kinds.get(ID).unwrap();
    let descriptor = kind.descriptor();
    descriptor.validate().unwrap();
    assert!(descriptor.outputs.is_empty(), "an export card produces no results");
    let names: Vec<_> = descriptor.inputs.iter().map(|p| (p.name.as_str(), p.port_type.modality, p.port_type.many)).collect();
    assert_eq!(names, INPUTS.iter().map(|(n, m)| (*n, *m, true)).collect::<Vec<_>>());
    assert!(!descriptor.inputs.iter().any(|p| p.port_type.modality == Modality::Text), "text is copied, never exported");
    let params: Vec<_> = descriptor.params.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(params, [TEMPLATE, ON_COLLISION, FORMAT, SUBFOLDER]);
    let defaults = settings(kind, &node).unwrap();
    assert_eq!(defaults, Settings { template: DEFAULT_TEMPLATE.into(), on_collision: "skip".into(), format: "original".into(), subfolder: vec![] });
    // The schema carries no absolute destination: the folder is a grant, not data.
    assert!(!params.contains(&"destination") && !params.contains(&"folder"));
}

#[test]
fn an_export_card_round_trips_through_serde() {
    let (kinds, mut node) = node();
    node.params.insert(SUBFOLDER.into(), Value::Text("renders/final".into()));
    node.params.insert(ON_COLLISION.into(), Value::Text("numbered".into()));
    let json = serde_json::to_string(&node).unwrap();
    let back: Node = serde_json::from_str(&json).unwrap();
    assert_eq!(back, node);
    let read = settings(kinds.get(ID).unwrap(), &back).unwrap();
    assert_eq!(read.subfolder, ["renders", "final"]);
    assert_eq!(read.on_collision, "numbered");
}

#[test]
fn an_export_card_never_plans_a_job() {
    let (kinds, node) = node();
    let kind = kinds.get(ID).unwrap();
    assert_eq!(kind.mode(&[]).map(|m| m.as_str().to_owned()).as_deref(), Some("export"));
    assert!(kind.mode(&[PortType { modality: Modality::Video, many: false }]).is_some());
    assert!(kind.mode(&[PortType { modality: Modality::Text, many: false }]).is_none(), "text is never written to a file");
    let input = InputValue { port: "image".into(), port_type: PortType { modality: Modality::Image, many: true }, value: Value::List(vec![]), origins: vec![] };
    let jobs = kind.plan(&PlanCtx { node: &node, inputs: &[input], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).unwrap();
    assert!(jobs.is_empty());
}

#[test]
fn an_unknown_policy_or_format_is_refused() {
    let (kinds, mut node) = node();
    let kind = kinds.get(ID).unwrap();
    node.params.insert(ON_COLLISION.into(), Value::Text("clobber".into()));
    assert!(settings(kind, &node).is_err());
    node.params.insert(ON_COLLISION.into(), Value::Text("skip".into()));
    node.params.insert(FORMAT.into(), Value::Text("convert".into()));
    assert!(settings(kind, &node).is_err());
    node.params.insert(FORMAT.into(), Value::Text("original".into()));
    node.params.insert(TEMPLATE.into(), Value::Text("x".repeat(MAX_TEMPLATE_BYTES + 1)));
    assert!(settings(kind, &node).is_err());
}

#[test]
fn a_subfolder_stays_inside_the_grant() {
    assert_eq!(subfolder_components("").unwrap(), Vec::<String>::new());
    assert_eq!(subfolder_components("./a//b\\c/").unwrap(), ["a", "b", "c"]);
    for hostile in ["..", "a/../b", "/etc", "\\\\server\\share", "C:\\Users", "a\u{0}b", "a\nb"] {
        assert!(subfolder_components(hostile).is_err(), "{hostile:?} must be refused");
    }
    assert!(subfolder_components(&"a".repeat(MAX_SUBFOLDER_BYTES + 1)).is_err());
    // A space at either end would name a different folder than the person sees.
    for padded in [" renders", "renders ", "a/ b", "a /b"] {
        assert!(subfolder_components(padded).is_err(), "{padded:?} must be refused");
    }
    assert_eq!(subfolder_components("final renders/v 2").unwrap(), ["final renders", "v 2"]);
}
