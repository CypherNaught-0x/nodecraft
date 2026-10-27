//! Retained cache ownership, including parsed schemas and their duplicated descriptor values.
//! Counts capacities where public; excludes allocator overhead and caller-owned Arc clones.
use super::*;

fn slots<T>(items: &Vec<T>) -> usize {
    items.capacity().saturating_mul(size_of::<T>())
}

fn value(value: &Value, depth: usize) -> usize {
    if depth > 32 {
        return usize::MAX;
    }
    match value {
        Value::Text(text) => text.capacity(),
        // MIME owns a private string and validates a 1 KiB limit. Reserve its bounded storage.
        Value::Media(_) => 4 * 1024,
        Value::List(items) => items.iter().fold(slots(items), |bytes, item| bytes.saturating_add(self::value(item, depth.saturating_add(1)))),
        _ => 0,
    }
}

fn spec(param: &ParamSpec) -> usize {
    let mut bytes = param.name.capacity().saturating_add(param.label.capacity()).saturating_add(param.description.as_ref().map_or(0, String::capacity));
    if let ParamControl::Choice { options } = &param.control {
        bytes = bytes.saturating_add(
            options.iter().fold(slots(options), |bytes, choice| bytes.saturating_add(choice.value.capacity()).saturating_add(choice.label.capacity())),
        );
    }
    if let Some(default) = &param.default {
        bytes = bytes.saturating_add(value(default, 0));
    }
    if let Some(condition) = &param.visible_if {
        bytes = bytes.saturating_add(condition.parameter.capacity()).saturating_add(value(&condition.equals, 0));
    }
    bytes
}

pub(super) fn retained(text: &str, mapping: &Mapping, descriptor: &NodeDescriptor) -> usize {
    // Three Arc headers: text, mapping and descriptor. Cached owns each root once.
    let mut bytes = size_of::<Cached>()
        .saturating_add(6 * size_of::<usize>())
        .saturating_add(text.len())
        .saturating_add(size_of::<Mapping>())
        .saturating_add(size_of::<NodeDescriptor>());
    bytes = mapping.inputs.iter().fold(bytes.saturating_add(slots(&mapping.inputs)), |bytes, input| {
        bytes.saturating_add(input.node.capacity()).saturating_add(input.input.capacity()).saturating_add(input.port.capacity())
    });
    bytes = mapping.params.iter().fold(bytes.saturating_add(slots(&mapping.params)), |bytes, param| {
        bytes.saturating_add(param.node.capacity()).saturating_add(param.input.capacity()).saturating_add(spec(&param.spec))
    });
    bytes = mapping.outputs.iter().fold(bytes.saturating_add(slots(&mapping.outputs)), |bytes, output| {
        bytes.saturating_add(output.node.capacity()).saturating_add(output.label.capacity())
    });
    bytes = mapping.partners.iter().fold(bytes.saturating_add(slots(&mapping.partners)), |bytes, class| bytes.saturating_add(class.capacity()));
    bytes = mapping.output_names.iter().fold(bytes.saturating_add(slots(&mapping.output_names)), |bytes, port| bytes.saturating_add(port.name.capacity()));
    bytes = bytes
        .saturating_add(descriptor.id.capacity())
        .saturating_add(descriptor.label.capacity())
        .saturating_add(descriptor.quick_add.as_ref().map_or(0, String::capacity));
    for ports in [&descriptor.inputs, &descriptor.outputs] {
        bytes = ports.iter().fold(bytes.saturating_add(slots(ports)), |bytes, port| bytes.saturating_add(port.name.capacity()));
    }
    descriptor.params.params.iter().fold(bytes.saturating_add(slots(&descriptor.params.params)), |bytes, param| bytes.saturating_add(spec(param)))
}
