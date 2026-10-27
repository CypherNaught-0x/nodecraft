//! Pure editing helpers: preserve hidden widget values and collect image previews as files.
use super::*;
use nodecraft_comfy::{InputKind, InputValue, NodeCatalog};

/// Bake the current exposed values before changing visibility. A hidden widget keeps its
/// effective value in the embedded workflow, without retaining a hidden override parameter.
pub fn bake_values(node: &Node, catalog: &NodeCatalog) -> Result<ApiWorkflow, Error> {
    let card = card(node)?;
    let mut workflow = ApiWorkflow::parse(card.workflow.as_bytes(), None).map_err(|_| invalid("the stored workflow is malformed"))?;
    for parameter in &card.mapping.params {
        let Some(value) = node.params.get(&parameter.spec.name).or(parameter.spec.default.as_ref()) else { continue };
        parameter.spec.control.validate_value(value)?;
        let value = to_json(value)?;
        let target = workflow.nodes.get_mut(&parameter.node).ok_or_else(|| invalid("a mapped widget has no workflow node"))?;
        if matches!(target.inputs.get(&parameter.input), Some(InputValue::Link { .. })) {
            return Err(invalid("a mapped widget is an internal connection"));
        }
        let spec =
            catalog.get(&target.class_type).and_then(|n| n.input(&parameter.input)).ok_or_else(|| invalid("the server no longer describes a mapped widget"))?;
        spec.check(&value).map_err(|_| invalid("a current widget value does not match the server; correct it before editing the mapping"))?;
        target.inputs.insert(parameter.input.clone(), InputValue::Value(value));
    }
    // Account for the resulting aggregate once, rather than re-walking it per widget.
    workflow.to_json().map_err(|_| invalid("the effective workflow exceeds its resource limit"))?;
    Ok(workflow)
}

/// Core PreviewImage has no retained result. In an imported card, its image connection can be
/// saved by core SaveImage instead. Only the server-described compatible, images-only shape
/// is converted; unrelated/custom preview nodes retain their existing behavior.
pub fn save_image_previews(workflow: &mut ApiWorkflow, catalog: &NodeCatalog) -> Result<Vec<String>, Error> {
    let compatible = catalog.get("SaveImage").is_some_and(|info| {
        info.output_node
            && !info.api_node
            && info.input("images").is_some_and(|s| matches!(&s.kind, InputKind::Link(kind) if kind == "IMAGE"))
            && info.input("filename_prefix").is_some_and(|s| matches!(s.kind, InputKind::String { .. }))
    });
    if !compatible {
        return Ok(Vec::new());
    }
    let mut changed = Vec::new();
    for id in workflow.ids() {
        changed.push(id.to_owned());
    }
    let mut warnings = Vec::new();
    for id in changed {
        let Some(node) = workflow.nodes.get_mut(&id) else { continue };
        if node.class_type != "PreviewImage" || node.inputs.len() != 1 || !node.inputs.contains_key("images") {
            continue;
        }
        let supported = catalog.get("PreviewImage").is_some_and(|info| {
            info.output_node && !info.api_node && info.input("images").is_some_and(|s| matches!(&s.kind, InputKind::Link(kind) if kind == "IMAGE"))
        });
        if !supported {
            continue;
        }
        node.class_type = "SaveImage".into();
        node.inputs.insert("filename_prefix".into(), InputValue::Value(serde_json::Value::String("NodeCraft".into())));
        if warnings.len() < 32 {
            warnings.push(format!("image preview {} is saved as a retained image result", bounded(&id, 64)));
        }
    }
    workflow.to_json().map_err(|_| invalid("the retained-preview workflow exceeds its resource limit"))?;
    Ok(warnings)
}
