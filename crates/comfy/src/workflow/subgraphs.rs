//! Expand saved UI subgraphs from their public JSON, without consulting ComfyUI implementation.
//! Boundaries become aliases; every real node still passes through the existing server-catalog
//! converter. Expansion never runs a node or substitutes model/prompt values of its own.
use super::*;
use crate::json_bounds::Budget;
const MAX_DEPTH: usize = 16;
const MAX_ID_BYTES: usize = 256;
#[derive(Clone, Debug)]
pub(super) enum Signal {
    Link { node: String, output: u32, wired: bool },
    Literal { value: Value, mode: Option<String>, inherit_mode: bool },
}
pub(super) struct Flat {
    pub conversion: Conversion,
    pub bindings: BTreeMap<String, Vec<Option<Signal>>>,
    pub outputs: Vec<Option<Signal>>,
    pub fallbacks: BTreeMap<(String, String), Signal>,
}
fn invalid(message: &'static str) -> ComfyError {
    ComfyError::InvalidWorkflow(message.into())
}
pub(super) fn copy_source(source: Source<'_>, budget: &mut Budget, boundary: Option<&str>) -> Result<Signal, ComfyError> {
    match source {
        Source::Link { node, output } => {
            budget.text(node, 0)?;
            Ok(Signal::Link { node: node.into(), output, wired: boundary != Some(node) })
        }
        Source::Value(_, value, mode) => {
            budget.value(value, 0)?;
            Ok(Signal::Literal { value: value.clone(), mode, inherit_mode: false })
        }
    }
}
fn qualified(prefix: &str, id: &str) -> Result<String, ComfyError> {
    if id.is_empty() || prefix.len().saturating_add(id.len()) > MAX_ID_BYTES {
        return Err(invalid("expanded subgraph node ids exceed 256 bytes"));
    }
    Ok(format!("{prefix}{id}"))
}
fn qualify(signal: Signal, prefix: &str) -> Result<Signal, ComfyError> {
    match signal {
        Signal::Link { node, output, wired } => Ok(Signal::Link { node: qualified(prefix, &node)?, output, wired }),
        literal => Ok(literal),
    }
}
fn charge(signal: &Signal, budget: &mut Budget) -> Result<(), ComfyError> {
    match signal {
        Signal::Link { node, .. } => budget.text(node, 0),
        Signal::Literal { value, mode, .. } => {
            budget.value(value, 0)?;
            if let Some(mode) = mode {
                budget.text(mode, 0)?;
            }
            Ok(())
        }
    }
}
struct Interface {
    instance: String,
    name: String,
    label: String,
    kind: String,
}
struct Expansion<'a> {
    definitions: BTreeMap<String, &'a Value>,
    catalog: &'a NodeCatalog,
    budget: Budget,
    declared: usize,
    conversion: Conversion,
    aliases: BTreeMap<(String, u32), Option<Signal>>,
    interfaces: BTreeMap<(String, u32), Interface>,
    fallbacks: BTreeMap<(String, String), Signal>,
    overrides: BTreeMap<(String, String), Signal>,
    modes: BTreeMap<(String, String), String>,
}
impl Expansion<'_> {
    fn alias(&mut self, id: String, slot: usize, signal: Option<Signal>) -> Result<(), ComfyError> {
        self.budget.text(&id, 0)?;
        if let Some(signal) = &signal {
            charge(signal, &mut self.budget)?;
        }
        let slot = u32::try_from(slot).map_err(|_| invalid("subgraph has too many interface ports"))?;
        if self.aliases.insert((id, slot), signal).is_some() {
            return Err(invalid("expanded subgraph aliases reuse a node id"));
        }
        Ok(())
    }
    fn scope(&mut self, graph: &Value, prefix: &str, boundary: Option<&str>, stack: &mut Vec<String>) -> Result<Vec<Option<Signal>>, ComfyError> {
        if stack.len() > MAX_DEPTH {
            return Err(invalid("subgraphs exceed the 16-level expansion limit"));
        }
        let nodes = graph.get("nodes").and_then(Value::as_array).ok_or_else(|| invalid("a subgraph definition has no node list"))?;
        self.declared = self.declared.saturating_add(nodes.len());
        if self.declared > MAX_NODES {
            return Err(invalid("expanded subgraphs exceed the 10000-node limit"));
        }
        // Charge reused definitions and the converted result before retaining either. A small
        // shared definition may otherwise amplify primitive strings at every instance.
        self.budget.value(graph, 0)?;
        let mut flat = flat_ui(graph, self.catalog, &self.definitions, boundary)?;
        flat.conversion.workflow.validate_budget(&mut self.budget, 0)?;
        for (id, mut node) in flat.conversion.workflow.nodes {
            let id = qualified(prefix, &id)?;
            self.budget.text(&id, 0)?;
            for value in node.inputs.values_mut() {
                if let InputValue::Link { node, .. } = value {
                    *node = qualified(prefix, node)?;
                    self.budget.text(node, 0)?;
                }
            }
            if self.conversion.workflow.nodes.insert(id, node).is_some() {
                return Err(invalid("expanded subgraph nodes reuse an id"));
            }
        }
        for ((id, input), mode) in flat.conversion.controls {
            self.conversion.controls.insert((qualified(prefix, &id)?, input), mode);
        }
        for ((id, input), fallback) in flat.fallbacks {
            charge(&fallback, &mut self.budget)?;
            self.budget.text(&input, 0)?;
            self.fallbacks.insert((qualified(prefix, &id)?, input), qualify(fallback, prefix)?);
        }
        for warning in flat.conversion.warnings {
            if self.conversion.warnings.len() < 256 {
                self.budget.text(&warning, 0)?;
                self.conversion.warnings.push(warning);
            }
        }
        for instance in nodes {
            let Some(class) = instance.get("type").and_then(Value::as_str) else { continue };
            let Some(definition) = self.definitions.get(class).copied() else { continue };
            if matches!(instance.get("mode").and_then(Value::as_i64), Some(MODE_MUTED | MODE_BYPASS)) {
                continue;
            }
            if stack.iter().any(|s| s == class) {
                return Err(invalid("subgraph definitions contain a cycle"));
            }
            let id = instance.get("id").and_then(id_text).ok_or_else(|| invalid("a subgraph instance has no id"))?;
            let instance_id = qualified(prefix, &id)?;
            let nested_prefix = format!("{instance_id}:");
            let input_boundary =
                definition.get("inputNode").and_then(|n| n.get("id")).and_then(id_text).ok_or_else(|| invalid("a subgraph has no input boundary"))?;
            stack.push(class.into());
            let outputs = self.scope(definition, &nested_prefix, Some(&input_boundary), stack)?;
            stack.pop();
            for (slot, signal) in outputs.into_iter().enumerate() {
                self.alias(instance_id.clone(), slot, signal)?;
            }
            let mut bindings = flat.bindings.remove(&id).unwrap_or_default();
            let inputs = definition.get("inputs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
            if bindings.len() != inputs.len() {
                return Err(invalid("a subgraph input interface is malformed"));
            }
            self.saved_widgets(instance, definition, &nested_prefix, &mut bindings)?;
            for (slot, signal) in bindings.into_iter().enumerate() {
                let boundary = qualified(&nested_prefix, &input_boundary)?;
                self.alias(boundary.clone(), slot, signal.map(|s| qualify(s, prefix)).transpose()?)?;
                let field = inputs.get(slot).ok_or_else(|| invalid("a subgraph input slot is unavailable"))?;
                let name = field.get("name").and_then(Value::as_str).ok_or_else(|| invalid("a subgraph input has no name"))?;
                let kind = field.get("type").and_then(Value::as_str).unwrap_or_default();
                if name.trim().is_empty() || name.chars().any(char::is_control) || name.len() > 256 || kind.len() > 256 {
                    return Err(invalid("a subgraph input description exceeds its limits"));
                }
                let label = field
                    .get("label")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(name)
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(128)
                    .collect::<String>();
                for text in [&instance_id, name, &label, kind] {
                    self.budget.text(text, 0)?;
                }
                let slot = u32::try_from(slot).map_err(|_| invalid("a subgraph has too many inputs"))?;
                self.interfaces.insert((boundary, slot), Interface { instance: instance_id.clone(), name: name.into(), label, kind: kind.into() });
            }
        }
        flat.outputs.into_iter().map(|s| s.map(|s| qualify(s, prefix)).transpose()).collect()
    }
    fn saved_widgets(&mut self, instance: &Value, definition: &Value, prefix: &str, bindings: &mut [Option<Signal>]) -> Result<(), ComfyError> {
        let Some(saved) = instance.get("widgets_values") else { return Ok(()) };
        if saved.is_null() {
            return Ok(());
        }
        let values = saved.as_array().ok_or_else(|| invalid("saved subgraph widgets are not a positional list; unpack this subgraph before importing"))?;
        if values.is_empty() {
            return Ok(());
        }
        if let Some(proxies) = instance.get("properties").and_then(|p| p.get("proxyWidgets")).and_then(Value::as_array) {
            if proxies.len() != values.len() {
                return Err(invalid("saved subgraph proxy widgets are ambiguous; unpack this subgraph before importing"));
            }
            for (proxy, value) in proxies.iter().zip(values) {
                let proxy = proxy.as_array().ok_or_else(|| invalid("a subgraph proxy widget is malformed"))?;
                let id = proxy.first().and_then(id_text).ok_or_else(|| invalid("a subgraph proxy widget has no target"))?;
                let name = proxy.get(1).and_then(Value::as_str).ok_or_else(|| invalid("a subgraph proxy widget has no name"))?;
                let id = qualified(prefix, &id)?;
                let node = self.conversion.workflow.nodes.get(&id).ok_or_else(|| invalid("a subgraph proxy refers to a missing node"))?;
                let info = self.catalog.get(&node.class_type).ok_or_else(|| invalid("a subgraph proxy has no server description"))?;
                if name == "control_after_generate" {
                    let mut candidates = info.inputs.iter().filter(|s| s.control_after_generate || matches!(s.name.as_str(), "seed" | "noise_seed"));
                    let spec =
                        candidates.next().filter(|_| candidates.next().is_none()).ok_or_else(|| invalid("a subgraph seed-control target is ambiguous"))?;
                    let mode = value.as_str().filter(|s| SEED_CONTROLS.contains(s)).ok_or_else(|| invalid("a saved subgraph seed control is invalid"))?;
                    self.modes.insert((id, spec.name.clone()), mode.into());
                } else {
                    if !info.input(name).is_some_and(|s| s.is_widget()) {
                        return Err(invalid("a subgraph proxy is not a server widget"));
                    }
                    self.budget.value(value, 0)?;
                    self.budget.text(name, 0)?;
                    self.overrides.insert((id, name.into()), Signal::Literal { value: value.clone(), mode: None, inherit_mode: true });
                }
            }
            return Ok(());
        }
        let scalar = definition
            .get("inputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, i)| matches!(i.get("type").and_then(Value::as_str), Some("STRING" | "INT" | "FLOAT" | "BOOLEAN" | "COMBO")))
            .collect::<Vec<_>>();
        let mut at = 0usize;
        for (place, (slot, input)) in scalar.iter().enumerate() {
            let value = values.get(at).ok_or_else(|| invalid("saved subgraph widget values do not match its interface"))?;
            at = at.saturating_add(1);
            let mut mode = None;
            let extra = values.len().saturating_sub(at) > scalar.len().saturating_sub(place.saturating_add(1));
            if extra
                && self.control_input(definition, *slot)
                && let Some(control) = values.get(at).and_then(Value::as_str).filter(|s| SEED_CONTROLS.contains(s))
            {
                mode = Some(control.to_owned());
                at = at.saturating_add(1);
            }
            let binding = bindings.get_mut(*slot).ok_or_else(|| invalid("a subgraph input slot is unavailable"))?;
            if binding.is_none() {
                self.budget.value(value, 0)?;
                *binding = Some(Signal::Literal { value: value.clone(), mode, inherit_mode: true });
            }
            let _ = input;
        }
        if at != values.len() {
            return Err(invalid("saved subgraph widget values do not match its interface; unpack it before importing"));
        }
        Ok(())
    }
    fn control_input(&self, definition: &Value, slot: usize) -> bool {
        let mut pending = vec![(definition, slot, 0usize)];
        let mut seen = HashSet::new();
        while let Some((definition, slot, depth)) = pending.pop() {
            let identity = definition.get("id").and_then(Value::as_str).unwrap_or_default();
            if depth > MAX_DEPTH || seen.len() >= MAX_NODES || !seen.insert((identity.to_owned(), slot)) {
                continue;
            }
            let boundary = definition.get("inputNode").and_then(|v| v.get("id")).and_then(id_text);
            for link in definition.get("links").and_then(Value::as_array).into_iter().flatten() {
                let (origin, origin_slot, target, target_slot) = match link {
                    Value::Array(a) => (a.get(1), a.get(2), a.get(3), a.get(4)),
                    _ => (link.get("origin_id"), link.get("origin_slot"), link.get("target_id"), link.get("target_slot")),
                };
                if origin.and_then(id_text) != boundary || origin_slot.and_then(Value::as_u64).and_then(|s| usize::try_from(s).ok()) != Some(slot) {
                    continue;
                }
                let Some(id) = target.and_then(id_text) else { continue };
                let Some(port) = target_slot.and_then(Value::as_u64).and_then(|s| usize::try_from(s).ok()) else { continue };
                let Some(node) = definition
                    .get("nodes")
                    .and_then(Value::as_array)
                    .and_then(|nodes| nodes.iter().find(|n| n.get("id").and_then(id_text).as_deref() == Some(id.as_str())))
                else {
                    continue;
                };
                let Some(name) =
                    node.get("inputs").and_then(Value::as_array).and_then(|inputs| inputs.get(port)).and_then(|i| i.get("name")).and_then(Value::as_str)
                else {
                    continue;
                };
                let Some(class) = node.get("type").and_then(Value::as_str) else { continue };
                if self
                    .catalog
                    .get(class)
                    .and_then(|info| info.input(name))
                    .is_some_and(|spec| spec.control_after_generate || matches!(spec.name.as_str(), "seed" | "noise_seed"))
                {
                    return true;
                }
                if let Some(nested) = self.definitions.get(class)
                    && let Some(slot) = nested
                        .get("inputs")
                        .and_then(Value::as_array)
                        .and_then(|inputs| inputs.iter().position(|i| i.get("name").and_then(Value::as_str) == Some(name)))
                {
                    pending.push((nested, slot, depth.saturating_add(1)));
                }
            }
        }
        false
    }
    fn resolve(&mut self, mut signal: Signal) -> Result<Option<Signal>, ComfyError> {
        let mut seen = HashSet::new();
        let mut supplied_by_wire = false;
        for _ in 0..MAX_LINK_HOPS {
            let Signal::Link { node, output, wired } = &signal else {
                if supplied_by_wire && let Signal::Literal { inherit_mode, .. } = &mut signal {
                    *inherit_mode = false;
                }
                return Ok(Some(signal));
            };
            supplied_by_wire |= *wired;
            let key = (node.clone(), *output);
            let Some(next) = self.aliases.get(&key) else { return Ok(Some(signal)) };
            if !seen.insert(key) {
                return Err(invalid("subgraph links contain a cycle"));
            }
            let Some(next) = next else { return Ok(None) };
            charge(next, &mut self.budget)?;
            signal = next.clone();
        }
        Err(invalid("subgraph links exceed the 256-hop limit"))
    }
    fn input_interface(&self, node: &str, output: u32) -> Result<Option<(String, u32)>, ComfyError> {
        let mut key = (node.to_owned(), output);
        let mut seen = HashSet::new();
        let mut interface = None;
        for _ in 0..MAX_LINK_HOPS {
            let Some(signal) = self.aliases.get(&key) else { return Ok(None) };
            if !seen.insert(key.clone()) {
                return Err(invalid("subgraph links contain a cycle"));
            }
            if self.interfaces.contains_key(&key) {
                interface = Some(key.clone());
            }
            match signal {
                Some(Signal::Link { node, output, .. }) => key = (node.clone(), *output),
                _ => return Ok(interface),
            }
        }
        Err(invalid("subgraph links exceed the 256-hop limit"))
    }
    fn shared_inputs(&mut self, nodes: &mut BTreeMap<String, ApiNode>, groups: BTreeMap<(String, u32), Vec<(String, String)>>) -> Result<(), ComfyError> {
        for (key, mut targets) in groups {
            targets.sort();
            targets.dedup();
            let Some(interface) = self.interfaces.get(&key) else { continue };
            self.budget.container(targets.len(), 0)?;
            for (node, input) in &targets {
                self.budget.text(node, 0)?;
                self.budget.text(input, 0)?;
            }
            for text in [&interface.instance, &interface.name, &interface.label, &interface.kind] {
                self.budget.text(text, 0)?;
            }
            let mut promoted = PromotedInput {
                instance: interface.instance.clone(),
                name: interface.name.clone(),
                label: interface.label.clone(),
                kind: interface.kind.clone(),
                targets: targets.clone(),
            };
            if targets.len() > 1 && matches!(interface.kind.as_str(), "INT" | "FLOAT" | "BOOLEAN" | "STRING") {
                let values = targets
                    .iter()
                    .map(|(id, name)| match nodes.get(id).and_then(|n| n.inputs.get(name)) {
                        Some(InputValue::Value(v)) => Some(v),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>();
                let common = values.as_ref().and_then(|values| values.first().filter(|first| values.iter().all(|v| v == *first))).copied();
                if let Some(value) = common {
                    let primitive = super::primitive::compatible(self.catalog, &interface.kind, value).ok_or_else(|| {
                        invalid("shared subgraph widgets need a compatible free Primitive node on the server; unpack the subgraph before importing")
                    })?;
                    let suffix = format!(":shared:{}", key.1);
                    let id = qualified(&key.0, &suffix)?;
                    if nodes.contains_key(&id) || self.aliases.keys().any(|(existing, _)| existing == &id) {
                        return Err(invalid("a shared subgraph widget reuses a node id"));
                    }
                    if nodes.len() >= MAX_NODES {
                        return Err(invalid("expanded subgraphs exceed the 10000-node limit"));
                    }
                    self.budget.value(value, 0)?;
                    self.budget.text(&id, 0)?;
                    let value = value.clone();
                    let first_mode = targets.first().and_then(|target| self.conversion.controls.get(target));
                    let mode = first_mode.filter(|first| targets.iter().all(|target| self.conversion.controls.get(target) == Some(*first))).cloned();
                    for (target, name) in &targets {
                        let node = nodes.get_mut(target).ok_or_else(|| invalid("a shared subgraph widget target is unavailable"))?;
                        self.budget.text(&id, 0)?;
                        node.inputs.insert(name.clone(), InputValue::Link { node: id.clone(), output: 0 });
                        self.conversion.controls.remove(&(target.clone(), name.clone()));
                    }
                    nodes.insert(
                        id.clone(),
                        ApiNode {
                            class_type: primitive.class_type.clone(),
                            inputs: BTreeMap::from([("value".into(), InputValue::Value(value))]),
                            title: Some(interface.label.clone()),
                        },
                    );
                    if let Some(mode) = mode {
                        self.conversion.controls.insert((id.clone(), "value".into()), mode);
                    }
                    promoted.targets = vec![(id, "value".into())];
                }
            }
            self.conversion.promoted.push(promoted);
        }
        Ok(())
    }
    fn finish(mut self) -> Result<Conversion, ComfyError> {
        // String ids in a source file can already look like our qualified boundary ids.
        // Keep alias and executable namespaces disjoint before resolving any connection.
        if self.aliases.keys().any(|(id, _)| self.conversion.workflow.nodes.contains_key(id)) {
            return Err(invalid("expanded subgraph aliases reuse an executable node id"));
        }
        let mut groups: BTreeMap<(String, u32), Vec<(String, String)>> = BTreeMap::new();
        let mut nodes = std::mem::take(&mut self.conversion.workflow.nodes);
        for (id, node) in &mut nodes {
            for ((target, name), _) in self.overrides.range((id.clone(), String::new())..).take_while(|((target, _), _)| target == id) {
                let _ = target;
                node.inputs.entry(name.clone()).or_insert(InputValue::Value(Value::Null));
            }
            let inputs = std::mem::take(&mut node.inputs);
            for (name, input) in inputs {
                if let InputValue::Link { node, output } = &input
                    && let Some(interface) = self.input_interface(node, *output)?
                {
                    self.budget.text(id, 0)?;
                    self.budget.text(&name, 0)?;
                    groups.entry(interface).or_default().push((id.clone(), name.clone()));
                }
                let key = (id.clone(), name.clone());
                let fallback = self.fallbacks.get(&key);
                let base_mode = self.conversion.controls.remove(&key).or_else(|| match fallback {
                    Some(Signal::Literal { mode, .. }) => mode.clone(),
                    _ => None,
                });
                let resolved = match input {
                    InputValue::Link { node, output } => self.resolve(Signal::Link { node, output, wired: false })?,
                    InputValue::Value(value) => Some(Signal::Literal { value, mode: None, inherit_mode: true }),
                };
                let resolved = match resolved {
                    wire @ Some(Signal::Link { .. }) => wire,
                    literal @ Some(Signal::Literal { inherit_mode: false, .. }) => literal,
                    other => {
                        if let Some(override_value) = self.overrides.get(&key) {
                            charge(override_value, &mut self.budget)?;
                            Some(override_value.clone())
                        } else if other.is_some() {
                            other
                        } else if let Some(fallback) = self.fallbacks.get(&key) {
                            charge(fallback, &mut self.budget)?;
                            Some(fallback.clone())
                        } else {
                            None
                        }
                    }
                };
                match resolved {
                    Some(Signal::Link { node: source, output, .. }) => {
                        node.inputs.insert(name, InputValue::Link { node: source, output });
                    }
                    Some(Signal::Literal { value, mode, inherit_mode }) => {
                        let mode = if inherit_mode { self.modes.get(&key).cloned().or(mode).or(base_mode) } else { mode };
                        if let Some(mode) = mode {
                            self.conversion.controls.insert(key, mode);
                        }
                        node.inputs.insert(name, InputValue::Value(value));
                    }
                    None => {}
                }
            }
        }
        if nodes.is_empty() {
            return Err(invalid("the workflow has no nodes the server can run"));
        }
        self.shared_inputs(&mut nodes, groups)?;
        self.conversion.workflow.nodes = nodes;
        self.conversion.workflow.validate_budget(&mut Budget::default(), 0)?;
        Ok(self.conversion)
    }
}
pub(super) fn expand(root: &Value, catalog: &NodeCatalog) -> Result<Conversion, ComfyError> {
    let mut definitions = BTreeMap::new();
    for definition in root.get("definitions").and_then(|d| d.get("subgraphs")).and_then(Value::as_array).into_iter().flatten() {
        let id = definition
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= MAX_ID_BYTES)
            .ok_or_else(|| invalid("a subgraph definition has no bounded id"))?;
        if definitions.insert(id.to_owned(), definition).is_some() {
            return Err(invalid("subgraph definitions reuse an id"));
        }
    }
    if definitions.is_empty() {
        return flat_ui(root, catalog, &definitions, None).map(|f| f.conversion);
    }
    let mut expansion = Expansion {
        definitions,
        catalog,
        budget: Budget::default(),
        declared: 0,
        conversion: Conversion::default(),
        aliases: BTreeMap::new(),
        interfaces: BTreeMap::new(),
        fallbacks: BTreeMap::new(),
        overrides: BTreeMap::new(),
        modes: BTreeMap::new(),
    };
    expansion.scope(root, "", None, &mut Vec::new())?;
    expansion.finish()
}
#[cfg(test)]
mod tests;
