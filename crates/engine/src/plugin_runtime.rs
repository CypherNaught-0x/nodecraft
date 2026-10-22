//! Running text action packages (#173). The plugin worker publishes a [`PluginSnapshot`] of the
//! enabled, granted packages; the session registers their node kinds from it, and runs execute
//! their actions through [`Routed`]. Every action carries the grant it was published under:
//! once that grant is gone it is flagged revoked, its running jobs stop, and a result that
//! still arrives is dropped rather than recorded.
//!
//! A card records the package it was made with ([`PluginLock`]) in its node's config, so a
//! project keeps the card, says what it needs when the plugin is missing, and names the digest
//! to the store so uninstalling or pruning keeps it.
use crate::plugin_text::{PluginTextAction, compile};
use nodecraft_actions::expr::{self, CompiledTextAction, ExprLimits, InputKind, OutputKind, ParamKind};
use nodecraft_exec::run::ActionExecutor;
use nodecraft_graph::{Node, Port};
use nodecraft_nodes::action::ActionKind;
use nodecraft_nodes::{JobSpec, ModeId, NodeContract, NodeDescriptor, NodeKind, NodeMigration, PlanCtx};
use nodecraft_plugin::PackageDigest;
use nodecraft_plugin::id::{PLUGIN_PREFIX, PackageId};
use nodecraft_plugin_store::{State, Store, StoreBackend};
use nodecraft_types::{Error, ErrorKind, Integer, Modality, NodeId, ParamControl, ParamSpec, ParamValues, PortType, Unsigned, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The grant an action was published under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStamp {
    pub grant_id: String,
    pub package: PackageId,
    pub digest: PackageDigest,
    pub epoch: u64,
}

/// One runnable action of a granted package.
#[derive(Debug)]
pub struct PluginAction {
    pub stamp: PluginStamp,
    pub lock: PluginLock,
    action: CompiledTextAction,
    revoked: AtomicBool,
}

impl PluginAction {
    /// Whether its grant is gone: nothing it computes may be recorded.
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    /// Runs the action on a planned job's inputs and parameters.
    fn run(&self, params: &ParamValues, inputs: &[Value], cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
        if self.is_revoked() {
            return Err(revoked());
        }
        let definition = self.action.definition();
        let mut given = BTreeMap::new();
        let mut values = inputs.iter();
        let last = definition.inputs.len().saturating_sub(1);
        for (n, port) in definition.inputs.iter().enumerate() {
            let value = if port.kind == InputKind::TextList && n == last {
                // A list port's values come flattened: each a text, or a list of texts.
                let mut items = Vec::new();
                for value in values.by_ref() {
                    match value {
                        Value::List(list) => items.extend(list.iter().map(text_of).collect::<Result<Vec<_>, _>>()?),
                        other => items.push(text_of(other)?),
                    }
                }
                expr::Value::List(items)
            } else {
                text_of(values.next().ok_or_else(|| format!("connect the {} input first", port.name))?)?
            };
            given.insert(port.name.clone(), value);
        }
        if values.next().is_some() {
            return Err("the action got more inputs than it takes".into());
        }
        let params = params
            .iter()
            .filter(|(name, _)| definition.params.iter().any(|p| &p.name == *name))
            .map(|(name, value)| Ok((name.clone(), json_of(value)?)))
            .collect::<Result<_, String>>()?;
        let stop = || cancelled() || self.is_revoked();
        let outputs = self
            .action
            .run(&given, &params, ExprLimits::default(), &stop)
            .map_err(|e| if self.is_revoked() { revoked() } else { format!("the plugin action failed: {e}") })?;
        let (_, value) = outputs.into_iter().next().ok_or("the plugin action gave no result")?;
        value_of(value)
    }
}

const LOCK_PACKAGE: &str = "plugin.package";
const LOCK_VERSION: &str = "plugin.version";
const LOCK_DIGEST: &str = "plugin.digest";
const LOCK_LABEL: &str = "plugin.label";
/// Longest version or label a lock keeps.
const MAX_LOCK_TEXT: usize = 256;

/// The package a plugin card was made with, kept in its node's config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginLock {
    pub package: PackageId,
    pub version: String,
    pub digest: PackageDigest,
    /// The action's label, shown when its plugin is missing.
    pub label: String,
}

impl PluginLock {
    /// The lock `node` records; none when it records none, or one this build can't read.
    pub fn of(node: &Node) -> Option<Self> {
        let text = |key: &str| match node.config.get(key) {
            Some(Value::Text(t)) if t.len() <= MAX_LOCK_TEXT => Some(t.as_str()),
            _ => None,
        };
        Some(Self {
            package: PackageId::parse(text(LOCK_PACKAGE)?).ok()?,
            version: text(LOCK_VERSION)?.to_owned(),
            digest: PackageDigest::parse(text(LOCK_DIGEST)?).ok()?,
            label: text(LOCK_LABEL)?.to_owned(),
        })
    }

    fn write(&self, config: &mut BTreeMap<String, Value>) {
        config.insert(LOCK_PACKAGE.into(), Value::Text(self.package.to_string()));
        config.insert(LOCK_VERSION.into(), Value::Text(self.version.clone()));
        config.insert(LOCK_DIGEST.into(), Value::Text(self.digest.to_string()));
        config.insert(LOCK_LABEL.into(), Value::Text(self.label.clone()));
    }
}

/// Every package digest the plugin cards of `document` were made with.
pub fn referenced(document: &nodecraft_graph::Project) -> std::collections::BTreeSet<PackageDigest> {
    document.canvases().values().flat_map(|c| c.nodes().values()).filter_map(|n| PluginLock::of(n).map(|l| l.digest)).collect()
}

/// A plugin action's card: the action kind, recording its package in every card it makes.
struct PluginCard {
    kind: ActionKind,
    lock: PluginLock,
}

impl NodeKind for PluginCard {
    fn descriptor(&self) -> &NodeDescriptor {
        self.kind.descriptor()
    }

    fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
        self.kind.contract(descriptor)
    }

    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        self.kind.mode(inputs)
    }

    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        self.kind.plan(ctx)
    }

    fn instantiate_instance(&self, id: NodeId, mut config: BTreeMap<String, Value>, supplied: &ParamValues) -> Result<Node, Error> {
        self.lock.write(&mut config);
        self.kind.instantiate_instance(id, config, supplied)
    }

    /// Brings a card made with another version of its package to this one: the ports and lock
    /// of this version, the settings it still has, and a warning for everything that goes.
    fn migrate(&self, node: &Node) -> Result<Option<NodeMigration>, Error> {
        let descriptor = self.descriptor();
        let old = PluginLock::of(node);
        let known = |name: &String| descriptor.params.params.iter().any(|p| &p.name == name);
        if old.as_ref() == Some(&self.lock) && node.inputs == descriptor.inputs && node.outputs == descriptor.outputs && node.params.keys().all(known) {
            return Ok(None);
        }
        let mut warnings = Vec::new();
        match &old {
            Some(old) if old.digest != self.lock.digest => {
                warnings.push(format!("Updates the card from version {} to {} of {}.", old.version, self.lock.version, self.lock.package))
            }
            _ => {}
        }
        let gone = |ports: &[Port], now: &[Port]| ports.iter().filter(|p| !now.contains(p)).map(|p| p.name.clone()).collect::<Vec<_>>();
        for name in gone(&node.inputs, &descriptor.inputs) {
            warnings.push(format!("Drops the {name} input as it was, and its connections."));
        }
        for name in gone(&node.outputs, &descriptor.outputs) {
            warnings.push(format!("Drops the {name} output as it was, and its connections."));
        }
        let mut kept = ParamValues::new();
        for (name, value) in &node.params {
            if known(name) {
                kept.insert(name.clone(), value.clone());
            } else {
                warnings.push(format!("Drops the {name} setting, which this version doesn't have."));
            }
        }
        let params = match descriptor.params.resolve(&kept, false) {
            Ok(resolved) => resolved.values,
            Err(_) => {
                warnings.push("Resets the settings, which don't fit this version.".to_owned());
                descriptor.params.resolve(&ParamValues::new(), false)?.values
            }
        };
        let mut next = node.clone();
        self.lock.write(&mut next.config);
        next.params = params;
        next.inputs = descriptor.inputs.clone();
        next.outputs = descriptor.outputs.clone();
        Ok(Some(NodeMigration { node: next, warnings }))
    }
}

/// The card kind of a plugin action registered as `id`: one port per input (a text list takes
/// many), one output, and a parameter per definition parameter. Checked as the registry checks
/// it, so a kind that is made always registers.
pub(crate) fn card(id: &str, action: &CompiledTextAction, lock: PluginLock) -> Result<Box<dyn NodeKind>, Error> {
    let definition = action.definition();
    let text = |name: &str, many: bool| Port { name: name.into(), port_type: PortType { modality: Modality::Text, many } };
    let inputs = definition.inputs.iter().map(|p| text(&p.name, p.kind == InputKind::TextList)).collect();
    let output = definition.outputs.first().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "a plugin action has no output"))?;
    let output = text(&output.name, output.kind == OutputKind::TextList);
    let params = definition.params.iter().map(param).collect::<Result<_, _>>()?;
    let kind = nodecraft_nodes::action::plugin_kind(id.to_owned(), definition.label.clone(), inputs, output, params)?;
    let descriptor = kind.descriptor();
    descriptor.validate()?;
    kind.contract(descriptor).validate(descriptor)?;
    let card = PluginCard { kind, lock };
    // The lock must fit a node's config.
    let mut config = BTreeMap::new();
    card.lock.write(&mut config);
    if config.values().any(|v| matches!(v, Value::Text(t) if t.len() > MAX_LOCK_TEXT)) {
        return Err(Error::new(ErrorKind::InvalidInput, "a plugin action's label is too long"));
    }
    Ok(Box::new(card))
}

fn revoked() -> String {
    "this plugin's grant was revoked; its result was dropped".into()
}

fn param(def: &expr::ParamDef) -> Result<ParamSpec, Error> {
    let wrong = || Error::new(ErrorKind::InvalidInput, "a plugin action's parameter default doesn't fit its kind");
    let (control, default) = match def.kind {
        ParamKind::Text => {
            (ParamControl::Text { multiline: false, max_bytes: expr::MAX_SOURCE_BYTES }, Value::Text(def.default.as_str().ok_or_else(wrong)?.to_owned()))
        }
        ParamKind::Int => (ParamControl::Integer { min: None, max: None, step: Unsigned(1) }, Value::Integer(Integer(def.default.as_i64().ok_or_else(wrong)?))),
        ParamKind::Bool => (ParamControl::Toggle, Value::Bool(def.default.as_bool().ok_or_else(wrong)?)),
    };
    Ok(ParamSpec { name: def.name.clone(), label: def.name.clone(), description: None, control, required: true, default: Some(default), visible_if: None })
}

fn text_of(value: &Value) -> Result<expr::Value, String> {
    match value {
        Value::Text(t) => Ok(expr::Value::Text(t.clone())),
        _ => Err("a plugin action takes only text".into()),
    }
}

fn json_of(value: &Value) -> Result<serde_json::Value, String> {
    match value {
        Value::Text(t) => Ok(serde_json::Value::from(t.as_str())),
        Value::Integer(Integer(n)) => Ok(serde_json::Value::from(*n)),
        Value::Unsigned(Unsigned(n)) => Ok(serde_json::Value::from(*n)),
        Value::Bool(b) => Ok(serde_json::Value::from(*b)),
        _ => Err("a plugin action's parameter has the wrong kind".into()),
    }
}

/// A result as a wire value: texts, numbers, flags, or one level of list of them.
fn value_of(value: expr::Value) -> Result<Value, String> {
    let scalar = |value: expr::Value| match value {
        expr::Value::Text(t) => Ok(Value::Text(t)),
        expr::Value::Int(n) => Ok(Value::Integer(Integer(n))),
        expr::Value::Bool(b) => Ok(Value::Bool(b)),
        expr::Value::List(_) => Err("the plugin action gave a list of lists".to_owned()),
    };
    match value {
        expr::Value::List(items) => items.into_iter().map(scalar).collect::<Result<_, _>>().map(Value::List),
        other => scalar(other),
    }
}

/// The actions of every enabled, granted text action package, by registered ID.
#[derive(Clone, Debug, Default)]
pub struct PluginSnapshot {
    actions: BTreeMap<String, Arc<PluginAction>>,
}

impl PluginSnapshot {
    /// What `store` may run now: each enabled package with a live grant for its active digest.
    /// A package that can't be loaded or compiled is left out; enabling it reported why.
    pub fn of<B: StoreBackend>(store: &Store<B>) -> Self {
        let Ok(grants) = store.grants() else { return Self::default() };
        let mut actions = BTreeMap::new();
        for (id, (digest, state)) in &store.index().active {
            let Some(grant) = (*state == State::Enabled).then(|| grants.live(id, digest)).flatten() else { continue };
            let Ok(compiled) = store.load(id).and_then(|package| compile(&package)) else { continue };
            let stamp = PluginStamp { grant_id: grant.grant_id.clone(), package: id.clone(), digest: *digest, epoch: grant.epoch };
            for PluginTextAction { id: action_id, action, lock } in compiled {
                let action = PluginAction { stamp: stamp.clone(), lock, action, revoked: AtomicBool::new(false) };
                actions.insert(action_id.as_str().to_owned(), Arc::new(action));
            }
        }
        Self { actions }
    }

    pub fn get(&self, id: &str) -> Option<&Arc<PluginAction>> {
        self.actions.get(id)
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Whether both hold the same actions under the same grants.
    pub fn same(&self, other: &Self) -> bool {
        self.actions.len() == other.actions.len() && self.actions.iter().zip(&other.actions).all(|((a, x), (b, y))| a == b && x.stamp == y.stamp)
    }

    /// The node kinds of its actions; a definition that makes no kind is left out.
    pub fn kinds(&self) -> Vec<Box<dyn NodeKind>> {
        self.actions.iter().filter_map(|(id, action)| card(id, &action.action, action.lock.clone()).ok()).collect()
    }

    /// `next`, keeping this snapshot's actions that kept their grants (running jobs hold them);
    /// the rest are flagged revoked.
    pub(crate) fn succeed(&self, next: Self) -> Self {
        let mut actions = BTreeMap::new();
        for (id, action) in next.actions {
            let kept = self.actions.get(&id).filter(|old| old.stamp == action.stamp).cloned();
            actions.insert(id, kept.unwrap_or(action));
        }
        for (id, old) in &self.actions {
            if !actions.get(id).is_some_and(|new| Arc::ptr_eq(new, old)) {
                old.revoke();
            }
        }
        Self { actions }
    }
}

/// Whether `action` names a plugin's action.
pub fn is_plugin(action: &str) -> bool {
    action.starts_with(PLUGIN_PREFIX)
}

/// The host's actions, and the granted plugins' when a run has them.
pub struct Routed {
    pub local: Arc<dyn ActionExecutor>,
    pub plugins: Option<Arc<PluginSnapshot>>,
}

impl Routed {
    fn plugin(&self, action: &str) -> Result<&PluginAction, String> {
        self.plugins.as_deref().and_then(|p| p.get(action)).map(Arc::as_ref).ok_or_else(|| "that plugin action isn't granted to run".to_owned())
    }
}

impl ActionExecutor for Routed {
    fn long_running(&self, action: &str) -> bool {
        // Bounded by fuel, but that can be a second or two: off the runtime's threads.
        is_plugin(action) || self.local.long_running(action)
    }

    fn execute_cancellable(&self, action: &str, params: &ParamValues, inputs: &[Value], cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
        if is_plugin(action) {
            return self.plugin(action)?.run(params, inputs, cancelled);
        }
        self.local.execute_cancellable(action, params, inputs, cancelled)
    }

    fn execute(&self, action: &str, params: &ParamValues, inputs: &[Value]) -> Result<Value, String> {
        if is_plugin(action) {
            return self.plugin(action)?.run(params, inputs, &|| false);
        }
        self.local.execute(action, params, inputs)
    }
}

#[cfg(test)]
mod tests;
