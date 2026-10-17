use nodecraft_graph::{Node, Port};
use nodecraft_nodes::{JobSpec, ModeId, NodeDescriptor, NodeKind, NodeMigration, ParamValues, PlanCtx};
use nodecraft_types::{Error, Modality, NodeId, PortType, Value};
use std::{borrow::Cow, collections::BTreeMap};

struct Dynamic {
    base: NodeDescriptor,
}
impl Dynamic {
    fn new() -> Self {
        Self { base: nodecraft_nodes::text::kind().descriptor().clone() }
    }
}
impl NodeKind for Dynamic {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.base
    }
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        let mut descriptor = self.base.clone();
        if let Some(Value::Text(label)) = node.config.get("source.text.v1.label") {
            descriptor.label = label.clone();
        }
        if node.config.contains_key("source.text.v1.input") {
            descriptor.inputs.push(Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } });
        }
        Ok(Cow::Owned(descriptor))
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn instance_mode(&self, node: &Node, inputs: &[PortType]) -> Result<Option<ModeId>, Error> {
        let descriptor = self.resolve_instance(node)?;
        Ok((descriptor.inputs.iter().map(|port| port.port_type).collect::<Vec<_>>() == inputs).then(|| ModeId::new("configured").unwrap()))
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        self.validate_instance(ctx.node, true)?;
        Ok(vec![])
    }
    fn migrate(&self, node: &Node) -> Result<Option<NodeMigration>, Error> {
        if !node.params.contains_key("old_text") {
            return Ok(None);
        }
        let mut migrated = node.clone();
        if let Some(value) = migrated.params.remove("old_text") {
            migrated.params.insert("text".into(), value);
        }
        Ok(Some(NodeMigration { node: migrated, warnings: vec!["Renamed old_text to text".into()] }))
    }
}

#[test]
fn instances_derive_ports_and_schema_from_bounded_config_and_own_mode() {
    let kind = Dynamic::new();
    let config = BTreeMap::from([("source.text.v1.label".into(), Value::Text("Configured".into())), ("source.text.v1.input".into(), Value::Bool(true))]);
    let node = kind.instantiate_instance(NodeId::from_bytes([1; 16]), config, &ParamValues::new()).unwrap();
    assert!(node.title.is_empty());
    assert_eq!(kind.resolve_instance(&node).unwrap().label, "Configured");
    assert_eq!(node.inputs.len(), 1);
    assert!(kind.descriptor().validate_node(&node, true).is_err());
    kind.plan(&PlanCtx { node: &node, inputs: &[] }).unwrap();
    let inputs = [PortType { modality: Modality::Text, many: false }];
    assert_eq!(kind.instance_mode(&node, &inputs).unwrap().unwrap().as_str(), "configured");
    assert!(kind.instance_mode(&node, &[]).unwrap().is_none());
    let config = BTreeMap::from([("source.text.v1.label".into(), Value::Text("x".repeat(257)))]);
    assert!(kind.instantiate_instance(node.id, config, &ParamValues::new()).is_err());
    let mut hostile = Node::new(node.id, "source.text");
    let mut deep = Value::Bool(true);
    for _ in 0..40 {
        deep = Value::List(vec![deep]);
    }
    hostile.config.insert("opaque".into(), deep);
    assert!(kind.resolve_instance(&hostile).is_err());
}

#[test]
fn stale_parameters_require_explicit_migration_and_keep_unknown_config() {
    let kind = Dynamic::new();
    let mut node = kind.descriptor().instantiate(NodeId::from_bytes([1; 16]), &ParamValues::new()).unwrap();
    node.params.remove("text");
    node.params.insert("old_text".into(), Value::Text("saved prompt".into()));
    node.config.insert("future.plugin.v2.unknown".into(), Value::Unsigned(nodecraft_types::Unsigned(99)));
    assert!(kind.validate_instance(&node, true).unwrap_err().message.contains("old_text"));
    let migrated = kind.prepare_migration(&node).unwrap().unwrap();
    assert_eq!(migrated.warnings, vec!["Renamed old_text to text"]);
    assert!(node.params.contains_key("old_text"));
    assert_eq!(migrated.node.config, node.config);
    assert_eq!(kind.validate_instance(&migrated.node, true).unwrap().values.get("text"), Some(&Value::Text("saved prompt".into())));
    assert!(kind.prepare_migration(&migrated.node).unwrap().is_none());
}
