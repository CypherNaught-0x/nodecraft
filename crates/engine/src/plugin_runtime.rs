//! Running text action packages (#173). The plugin worker publishes a [`PluginSnapshot`] of the
//! enabled, granted packages; the session registers their node kinds from it, and runs execute
//! their actions through [`Routed`]. Every action carries the grant it was published under:
//! once that grant is gone it is flagged revoked, its running jobs stop, and a result that
//! still arrives is dropped rather than recorded.
use crate::plugin_text::{PluginTextAction, compile};
use nodecraft_actions::expr::{self, CompiledTextAction, ExprLimits, InputKind, OutputKind, ParamKind};
use nodecraft_exec::run::ActionExecutor;
use nodecraft_graph::Port;
use nodecraft_nodes::NodeKind;
use nodecraft_plugin::PackageDigest;
use nodecraft_plugin::id::{PLUGIN_PREFIX, PackageId};
use nodecraft_plugin_store::{State, Store, StoreBackend};
use nodecraft_types::{Error, ErrorKind, Integer, Modality, ParamControl, ParamSpec, ParamValues, PortType, Unsigned, Value};
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

    /// Its node kind: one port per input (a text list takes many), one output, and a parameter
    /// per definition parameter.
    fn kind(&self, id: &str) -> Result<Box<dyn NodeKind>, Error> {
        let definition = self.action.definition();
        let text = |name: &str, many: bool| Port { name: name.into(), port_type: PortType { modality: Modality::Text, many } };
        let inputs = definition.inputs.iter().map(|p| text(&p.name, p.kind == InputKind::TextList)).collect();
        let output = definition.outputs.first().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "a plugin action has no output"))?;
        let output = text(&output.name, output.kind == OutputKind::TextList);
        let params = definition.params.iter().map(param).collect::<Result<_, _>>()?;
        Ok(Box::new(nodecraft_nodes::action::plugin_kind(id.to_owned(), definition.label.clone(), inputs, output, params)?))
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

fn revoked() -> String {
    "this plugin's grant was revoked; its result was dropped".into()
}

fn param(def: &expr::ParamDef) -> Result<ParamSpec, Error> {
    let wrong = || Error::new(ErrorKind::InvalidInput, "a plugin action's parameter default doesn't fit its kind");
    let (control, default) = match def.kind {
        ParamKind::Text => (ParamControl::Text { multiline: false, max_bytes: 8192 }, Value::Text(def.default.as_str().ok_or_else(wrong)?.to_owned())),
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
            for PluginTextAction { id, action } in compiled {
                actions.insert(id.as_str().to_owned(), Arc::new(PluginAction { stamp: stamp.clone(), action, revoked: AtomicBool::new(false) }));
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
        self.actions.iter().filter_map(|(id, action)| action.kind(id).ok()).collect()
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
