//! Disjoint semantic ownership for dynamic model-backed instances (#465).
use super::NodeContract;
use crate::{NodeDescriptor, descriptor::invalid};
use nodecraft_types::{Contract, Error, IssueCode, ParamControl, ParamOwner, ParamValues, Resolution};
use std::collections::BTreeSet;

impl NodeContract {
    /// Authoritative contracts, in control-layout order. A model parameter keeps its own
    /// namespace even while several different models use one node kind.
    pub fn parameter_groups(&self) -> impl Iterator<Item = &Contract> {
        std::iter::once(&self.params).chain(self.owned_groups.iter())
    }
    pub(super) fn validate_ownership(&self, descriptor: &NodeDescriptor) -> Result<(), Error> {
        descriptor.params.validate()?;
        self.validate_groups()?;
        if self.owned_groups.len() > 256 {
            return Err(invalid("too many parameter owners"));
        }
        let mut names = BTreeSet::new();
        let mut owners = BTreeSet::new();
        let mut index = 0usize;
        let mut checks = 0usize;
        for group in self.parameter_groups() {
            checks = checks.checked_add(group.checks.len()).ok_or_else(|| invalid("too many parameter checks"))?;
            if checks > 64 || !owners.insert(group.namespace.as_str()) {
                return Err(invalid("duplicate parameter owner or too many checks"));
            }
            group.validate()?;
            for spec in &group.schema.params {
                if index >= 256 || !names.insert(spec.name.as_str()) || descriptor.params.params.get(index) != Some(spec) {
                    return Err(invalid("parameter owners must form the exact disjoint instance schema"));
                }
                index = index.saturating_add(1);
            }
        }
        if index != descriptor.params.params.len() {
            return Err(invalid("parameter owners omit instance parameters"));
        }
        Ok(())
    }
    fn validate_groups(&self) -> Result<(), Error> {
        if self.params.owner == ParamOwner::Model || self.params.namespace != self.kind {
            return Err(invalid("node parameters must retain their own kind namespace and owner"));
        }
        if self.owned_groups.len() > 255 {
            return Err(invalid("too many parameter owners"));
        }
        let mut namespaces = BTreeSet::new();
        let mut checks = 0usize;
        for group in &self.owned_groups {
            if group.owner != ParamOwner::Model {
                return Err(invalid("additional parameter owners must be models"));
            }
        }
        for group in self.parameter_groups() {
            if !namespaces.insert(group.namespace.as_str()) {
                return Err(invalid("duplicate parameter owner"));
            }
            checks = checks.checked_add(group.checks.len()).ok_or_else(|| invalid("too many parameter checks"))?;
            if checks > 64 {
                return Err(invalid("too many parameter checks"));
            }
            group.validate()?;
        }
        let schemas: Vec<_> = self.parameter_groups().map(|g| &g.schema).collect();
        nodecraft_types::ParamSchema::validate_parts(&schemas)
    }
    /// Resolve all owners without constructing or cloning a caller-controlled partition.
    /// Each shared resolver validates its own values before copying; foreign-owner unknown
    /// reports are removed, genuinely unknown supplied keys are reported exactly once.
    pub fn resolve(&self, values: &ParamValues, ready: bool) -> Result<Resolution, Error> {
        if self.owned_groups.is_empty() {
            return self.params.resolve(values, ready);
        }
        self.validate_groups()?;
        if values.len() > 256 {
            return Err(invalid("too many supplied parameters"));
        }
        let bytes = |value: &nodecraft_types::Value| match value {
            nodecraft_types::Value::Text(text) => text.len(),
            nodecraft_types::Value::Media(asset) => asset.mime.as_str().len(),
            _ => 0,
        };
        let mut text = values.values().fold(0usize, |n, v| n.saturating_add(bytes(v)));
        for group in self.parameter_groups() {
            for spec in &group.schema.params {
                text = text.saturating_add(spec.default.as_ref().map_or(0, bytes));
            }
        }
        if text > 1024 * 1024 {
            return Err(invalid("resolved parameters exceed text limit"));
        }
        let mut result = self.params.resolve(values, ready)?;
        result.issues.retain(|issue| {
            issue.code != IssueCode::UnknownParam
                || !self.owned_groups.iter().any(|g| g.schema.params.iter().any(|p| issue.param.as_deref() == Some(p.name.as_str())))
        });
        for group in &self.owned_groups {
            let mut resolution = group.resolve(values, ready)?;
            resolution.issues.retain(|issue| issue.code != IssueCode::UnknownParam);
            result.params.extend(resolution.params);
            result.issues.extend(resolution.issues);
        }
        Ok(result)
    }
    /// The node-owned selector of a model-backed contract. Static/unavailable instances
    /// also retain their selector; production-kind ownership is checked by the engine.
    pub fn model_selector(&self) -> Option<&str> {
        let mut models = self.params.schema.params.iter().filter(|p| p.control == ParamControl::Model);
        let first = models.next()?;
        if self.params.owner == ParamOwner::Node && models.next().is_none() { Some(first.name.as_str()) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Category, NodeContract};
    use nodecraft_types::{ParamSchema, ParamSpec, Value};
    fn spec(name: &str) -> ParamSpec {
        ParamSpec {
            name: name.into(),
            label: name.into(),
            description: None,
            control: ParamControl::Text { multiline: false, max_bytes: 1024 * 1024 },
            required: false,
            default: None,
            visible_if: None,
        }
    }
    fn owned() -> (NodeDescriptor, NodeContract) {
        let descriptor = NodeDescriptor {
            id: "generation.test".into(),
            label: "Generation".into(),
            category: Category::Generation,
            inputs: vec![],
            outputs: vec![],
            params: ParamSchema { params: vec![spec("prompt"), spec("size")] },
            quick_add: None,
        };
        let mut contract = NodeContract::derive(&descriptor);
        contract.params.schema.params.truncate(1);
        contract.owned_groups.push(Contract::from_schema("provider/model-one", ParamOwner::Model, ParamSchema { params: vec![spec("size")] }));
        (descriptor, contract)
    }
    #[test]
    fn ownership_preserves_lossless_model_ids_and_reports_unknown_once() {
        let (descriptor, contract) = owned();
        contract.validate(&descriptor).unwrap();
        let values = [
            ("prompt".into(), Value::Text("A circle".into())),
            ("size".into(), Value::Text("large".into())),
            ("unknown".into(), Value::List(vec![Value::Text("retained".into())])),
        ]
        .into();
        let r = contract.resolve(&values, true).unwrap();
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.params[1].id, "provider/model-one/size");
        assert!(r.submission().is_err());
        let r = contract.resolve(&[("size".into(), Value::Text("large".into()))].into(), true).unwrap();
        assert_eq!(r.submission().unwrap().get("size"), Some(&Value::Text("large".into())));
    }
    #[test]
    fn ownership_refuses_collisions_and_aggregate_before_resolution_copy() {
        let (descriptor, mut contract) = owned();
        contract.owned_groups[0].namespace = descriptor.id.clone();
        assert!(contract.validate(&descriptor).is_err());
        let (_, mut contract) = owned();
        contract.owned_groups[0].schema.params[0].name = "prompt".into();
        assert!(contract.validate(&descriptor).is_err());
        let (_, mut contract) = owned();
        contract.params.schema.params[0].default = Some(Value::Text("x".repeat(700_000)));
        contract.owned_groups[0].schema.params[0].default = Some(Value::Text("y".repeat(700_000)));
        assert!(contract.resolve(&ParamValues::new(), false).is_err());
    }
}
