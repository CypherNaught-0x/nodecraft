//! Keep frontend primitive values that feed connection-only inputs as free executable server
//! primitives. The original source id is shared by all consumers; never substitute a paid node.
use super::*;
use crate::json_bounds::Budget;

pub(super) fn compatible<'a>(catalog: &'a NodeCatalog, kind: &str, value: &Value) -> Option<&'a NodeInfo> {
    let candidates: &[&str] = match kind {
        "INT" => &["PrimitiveInt"],
        "FLOAT" => &["PrimitiveFloat"],
        "BOOLEAN" => &["PrimitiveBoolean"],
        "STRING" => &["PrimitiveStringMultiline", "PrimitiveString"],
        _ => return None,
    };
    candidates.iter().filter_map(|class| catalog.get(class)).find(|info| {
        !info.api_node
            && !info.output_node
            && info.inputs.len() == 1
            && info.input("value").is_some_and(|spec| {
                matches!(
                    (kind, &spec.kind),
                    ("INT", InputKind::Int { .. })
                        | ("FLOAT", InputKind::Float { .. })
                        | ("BOOLEAN", InputKind::Boolean { .. })
                        | ("STRING", InputKind::String { .. })
                ) && spec.check(value).is_ok()
            })
            && info.outputs.first().is_some_and(|output| output.kind == kind && !output.is_list)
    })
}

pub(super) fn materialize(
    catalog: &NodeCatalog,
    conversion: &mut Conversion,
    budget: &mut Budget,
    source: &UiNode,
    wanted: &str,
    mode: Option<&str>,
) -> Result<String, ComfyError> {
    let invalid = || ComfyError::InvalidWorkflow("a frontend primitive connection needs a compatible free typed Primitive node on the server".into());
    if source.id.len() > 256 || wanted.len() > 256 {
        return Err(invalid());
    }
    budget.text(&source.id, 0)?;
    if let Some(existing) = conversion.workflow.nodes.get(&source.id) {
        // Source ids are unique and the UI map stays immutable during conversion. Reading
        // the value from that source below makes this cache independent of caller values;
        // never rescan a shared megabyte string once per consumer.
        let info = catalog.get(&existing.class_type).ok_or_else(invalid)?;
        if info.api_node || info.output_node || !info.outputs.first().is_some_and(|o| !o.is_list && types_match(&o.kind, wanted)) {
            return Err(invalid());
        }
        return Ok(source.id.clone());
    }
    let value = source.widgets.as_array().and_then(|values| values.first()).ok_or_else(invalid)?;
    let inferred = match value {
        Value::String(_) => "STRING",
        Value::Bool(_) => "BOOLEAN",
        Value::Number(_) if wanted == "FLOAT" => "FLOAT",
        Value::Number(_) if crate::object_info::whole_number(value).is_some() => "INT",
        Value::Number(_) => "FLOAT",
        _ => return Err(invalid()),
    };
    let kind = source.output_types.first().map(String::as_str).filter(|kind| matches!(*kind, "STRING" | "BOOLEAN" | "INT" | "FLOAT")).unwrap_or(inferred);
    if !types_match(kind, wanted) {
        return Err(invalid());
    }
    let info = compatible(catalog, kind, value).ok_or_else(invalid)?;
    if conversion.workflow.nodes.len() >= MAX_NODES {
        return Err(invalid());
    }
    budget.text(&source.id, 0)?;
    budget.text(&info.class_type, 0)?;
    budget.value(value, 0)?;
    if let Some(title) = &source.title {
        budget.text(title, 0)?;
    }
    conversion.workflow.nodes.insert(
        source.id.clone(),
        ApiNode {
            class_type: info.class_type.clone(),
            inputs: BTreeMap::from([("value".into(), InputValue::Value(value.clone()))]),
            title: source.title.clone(),
        },
    );
    if let Some(mode) = mode {
        budget.text(&source.id, 0)?;
        budget.text(mode, 0)?;
        conversion.controls.insert((source.id.clone(), "value".into()), mode.into());
    }
    Ok(source.id.clone())
}
