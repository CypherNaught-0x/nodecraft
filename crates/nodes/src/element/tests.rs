use super::*;
use crate::InputValue;
use nodecraft_types::{AssetRef, Blake3, MediaMeta, NodeId, ParamValues};

fn image(n: u8) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() })
}

fn element(name: &str) -> Node {
    let mut supplied = ParamValues::new();
    supplied.insert(NAME.into(), Value::Text(name.into()));
    kind().instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &supplied).unwrap()
}

fn input(port: &str, values: Vec<Value>) -> InputValue {
    InputValue { port: port.into(), port_type: images(true), value: Value::List(values), origins: Vec::new() }
}

fn plan(node: &Node, inputs: &[InputValue]) -> Result<Vec<Value>, Error> {
    let jobs = kind().plan(&PlanCtx { node, inputs, targets: &nodecraft_types::EMPTY_TARGET_SELECTION })?;
    Ok(jobs
        .into_iter()
        .map(|job| match job {
            JobSpec::Source { output, value, .. } if output == REFERENCE => value,
            other => panic!("unexpected job {other:?}"),
        })
        .collect())
}

#[test]
fn the_kind_registers_and_defaults_to_general() {
    let registry = crate::Registry::builtins().unwrap();
    assert_eq!(registry.get(ID).unwrap().descriptor().label, "Element");
    assert_eq!(element("hero").params.get(CATEGORY), Some(&Value::Text("general".into())));
    assert_eq!(registry.contract(ID).unwrap().inputs.iter().find(|i| i.port == IMAGES).unwrap().max, Some(8));
}

#[test]
fn the_images_pass_through_in_order_until_a_sheet_replaces_them() {
    let node = element("hero");
    assert_eq!(plan(&node, &[input(IMAGES, vec![image(1), image(2)])]).unwrap(), vec![image(1), image(2)]);
    let both = [input(IMAGES, vec![image(1), image(2)]), input(SHEET, vec![image(9)])];
    assert_eq!(plan(&node, &both).unwrap(), vec![image(9)]);
    // An empty sheet (not run yet) falls back to the images.
    assert_eq!(plan(&node, &[input(IMAGES, vec![image(1)]), input(SHEET, vec![])]).unwrap(), vec![image(1)]);
}

#[test]
fn an_element_needs_images_a_name_and_at_most_eight() {
    let node = element("hero");
    assert!(plan(&node, &[]).unwrap_err().message.contains("no reference images"));
    let nine: Vec<Value> = (0..9).map(image).collect();
    assert!(plan(&node, &[input(IMAGES, nine)]).unwrap_err().message.contains("at most 8"));
    let eight: Vec<Value> = (0..8).map(image).collect();
    assert_eq!(plan(&node, &[input(IMAGES, eight)]).unwrap().len(), 8);
    let mut unnamed = element("hero");
    unnamed.params.insert(NAME.into(), Value::Text("two words".into()));
    assert!(plan(&unnamed, &[input(IMAGES, vec![image(1)])]).is_err());
    assert_eq!(name(&unnamed), None);
    assert_eq!(name(&node), Some("hero"));
}

#[test]
fn mentions_are_hash_names_each_once() {
    assert_eq!(mentions("#Hero walks into #city, then #hero again"), vec!["Hero", "city"]);
    assert_eq!(mentions("colour#ff0000 and a#b are not mentions, (#ok) is"), vec!["ok"]);
    assert_eq!(mentions("# alone, ## double, #-dash"), vec!["-dash"]);
    assert_eq!(mentions(&format!("#{}", "x".repeat(41))), Vec::<&str>::new());
    assert_eq!(mentions("é#naïve"), vec!["na"]);
    assert_eq!(mentions(""), Vec::<&str>::new());
    let many: String = (0..1000).map(|n| format!("#n{n} ")).collect();
    assert_eq!(mentions(&many).len(), MAX_MENTIONS);
}
