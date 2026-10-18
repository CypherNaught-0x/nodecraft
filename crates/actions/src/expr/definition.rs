//! Portable, versioned custom text action definitions.
use super::parse::Func;
use super::{ErrorReason, Expr, ExprError, ExprLimits, MAX_SOURCE_BYTES, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The wire format version of [`TextActionDefinition`].
pub const TEXT_ACTION_SCHEMA: u32 = 1;
const MAX_INPUTS: usize = 16;
const MAX_PARAMS: usize = 32;
const MAX_OUTPUTS: usize = 16;
const MAX_LABEL_BYTES: usize = 128;
const MAX_ID_BYTES: usize = 64;
const MAX_NAME_BYTES: usize = 32;
/// All of a definition's expression sources together.
const MAX_DEFINITION_SOURCE_BYTES: usize = 4 * MAX_SOURCE_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Text,
    TextList,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Text,
    TextList,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Text,
    Int,
    Bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortDef {
    pub name: String,
    pub kind: InputKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamDef {
    pub name: String,
    pub kind: ParamKind,
    /// Used when a run doesn't supply the parameter.
    pub default: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextOutputDef {
    pub name: String,
    pub kind: OutputKind,
    /// The expression computing this output from the inputs and parameters.
    pub expr: String,
}

/// A custom text action as saved and shared: a stable id and version, typed inputs, parameters
/// with defaults, and one expression per output.
///
/// Schema 1 is provisional: its parameter shape will follow the shared parameter contract (#162)
/// when custom actions become node kinds, through a schema bump with a migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextActionDefinition {
    /// [`TEXT_ACTION_SCHEMA`]; newer schemas are refused rather than misread.
    pub schema: u32,
    /// Stable dotted id, such as `user.slugify`.
    pub id: String,
    /// Bumped whenever the definition's behaviour changes.
    pub version: u32,
    pub label: String,
    #[serde(default)]
    pub inputs: Vec<PortDef>,
    #[serde(default)]
    pub params: Vec<ParamDef>,
    pub outputs: Vec<TextOutputDef>,
}

/// A checked definition, ready to run many times.
#[derive(Clone, Debug)]
pub struct CompiledTextAction {
    definition: TextActionDefinition,
    params: Vec<(String, Value)>,
    outputs: Vec<(String, OutputKind, Expr)>,
}

fn invalid(part: &str) -> ExprError {
    ExprError::new(ErrorReason::InvalidDefinition, 0).of(part)
}

/// Lowercase identifiers, not keywords or built-in functions (`it` and `index` are reserved for
/// `map`/`filter`).
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_NAME_BYTES
        && chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !matches!(name, "let" | "in" | "if" | "then" | "else" | "true" | "false" | "and" | "or" | "not" | "it" | "index")
        && !Func::is_builtin(name)
}

fn valid_id(id: &str) -> bool {
    id.len() <= MAX_ID_BYTES
        && id.contains('.')
        && id.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
}

fn param_value(kind: ParamKind, value: &serde_json::Value) -> Option<Value> {
    match (kind, value) {
        (ParamKind::Text, serde_json::Value::String(t)) if t.len() <= MAX_SOURCE_BYTES => Some(Value::Text(t.clone())),
        (ParamKind::Int, serde_json::Value::Number(n)) => n.as_i64().map(Value::Int),
        (ParamKind::Bool, serde_json::Value::Bool(b)) => Some(Value::Bool(*b)),
        _ => None,
    }
}

impl TextActionDefinition {
    /// Checks the definition and parses every output expression. Every name an expression reads
    /// must be a declared input or parameter.
    pub fn compile(&self) -> Result<CompiledTextAction, ExprError> {
        if self.schema != TEXT_ACTION_SCHEMA {
            return Err(invalid("schema"));
        }
        if !valid_id(&self.id) {
            return Err(invalid("id"));
        }
        if self.version == 0 {
            return Err(invalid("version"));
        }
        if self.label.trim().is_empty() || self.label.len() > MAX_LABEL_BYTES || self.label.chars().any(char::is_control) {
            return Err(invalid("label"));
        }
        if self.inputs.len() > MAX_INPUTS || self.params.len() > MAX_PARAMS || self.outputs.is_empty() || self.outputs.len() > MAX_OUTPUTS {
            return Err(invalid("ports"));
        }
        // An invalid name is reported by position: it may be long or hold control characters.
        let mut names = BTreeSet::new();
        let declared = (self.inputs.iter().map(|p| &p.name).enumerate().map(|(i, n)| (n, "inputs", i)))
            .chain(self.params.iter().map(|p| &p.name).enumerate().map(|(i, n)| (n, "params", i)))
            .chain(self.outputs.iter().map(|o| &o.name).enumerate().map(|(i, n)| (n, "outputs", i)));
        for (name, list, i) in declared {
            if !valid_name(name) {
                return Err(invalid(&format!("{list}[{i}]")));
            }
            if !names.insert(name.as_str()) {
                return Err(invalid(name));
            }
        }
        let mut params = Vec::with_capacity(self.params.len());
        for p in &self.params {
            params.push((p.name.clone(), param_value(p.kind, &p.default).ok_or_else(|| invalid(&p.name))?));
        }
        let scope: BTreeSet<&str> = self.inputs.iter().map(|p| p.name.as_str()).chain(self.params.iter().map(|p| p.name.as_str())).collect();
        let source: usize = self.outputs.iter().fold(0usize, |n, o| n.saturating_add(o.expr.len()));
        if source > MAX_DEFINITION_SOURCE_BYTES {
            return Err(invalid("outputs"));
        }
        let mut outputs = Vec::with_capacity(self.outputs.len());
        for o in &self.outputs {
            let expr = Expr::parse(&o.expr).map_err(|e| e.of(&o.name))?;
            if let Some((_, at)) = expr.free_names().into_iter().find(|(name, _)| !scope.contains(name.as_str())) {
                return Err(ExprError::new(ErrorReason::UnknownName, at).of(&o.name));
            }
            outputs.push((o.name.clone(), o.kind, expr));
        }
        Ok(CompiledTextAction { definition: self.clone(), params, outputs })
    }
}

impl CompiledTextAction {
    pub fn definition(&self) -> &TextActionDefinition {
        &self.definition
    }

    /// Runs every output. `inputs` must supply each declared input (a text for `text`, a list of
    /// texts for `text_list`) and nothing else; `params` may override declared parameters.
    /// Outputs come back in declaration order. `cancel` is polled while work is done.
    pub fn run(
        &self,
        inputs: &BTreeMap<String, Value>,
        params: &BTreeMap<String, serde_json::Value>,
        limits: ExprLimits,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<(String, Value)>, ExprError> {
        if let Some(unknown) = inputs.keys().find(|k| !self.definition.inputs.iter().any(|p| &p.name == *k)) {
            // The caller's key may be anything: don't echo it.
            let _ = unknown;
            return Err(ExprError::new(ErrorReason::UnknownInput, 0));
        }
        if params.keys().any(|k| !self.definition.params.iter().any(|p| &p.name == k)) {
            return Err(ExprError::new(ErrorReason::UnknownParam, 0));
        }
        if inputs.values().any(|v| v.deeper_than(super::MAX_VALUE_DEPTH)) {
            return Err(ExprError::new(ErrorReason::TooDeep, 0));
        }
        let input: usize = inputs.values().fold(0usize, |n, v| n.saturating_add(v.size()));
        if input > limits.input_bytes {
            return Err(ExprError::new(ErrorReason::InputTooLarge, 0));
        }
        let mut scope: Vec<(&str, Value)> = Vec::with_capacity(self.definition.inputs.len() + self.params.len());
        for port in &self.definition.inputs {
            let value = inputs.get(&port.name).ok_or_else(|| ExprError::new(ErrorReason::MissingInput, 0).of(&port.name))?;
            let ok = match (port.kind, value) {
                (InputKind::Text, Value::Text(_)) => true,
                (InputKind::TextList, Value::List(items)) => items.iter().all(|v| matches!(v, Value::Text(_))),
                _ => false,
            };
            if !ok {
                return Err(ExprError::new(ErrorReason::WrongInputKind, 0).of(&port.name));
            }
            scope.push((&port.name, value.clone()));
        }
        for (def, (name, default)) in self.definition.params.iter().zip(&self.params) {
            let value = match params.get(name) {
                Some(given) => param_value(def.kind, given).ok_or_else(|| ExprError::new(ErrorReason::WrongParamKind, 0).of(name))?,
                None => default.clone(),
            };
            scope.push((name, value));
        }
        let names: Vec<(&str, &Value)> = scope.iter().map(|(n, v)| (*n, v)).collect();
        let mut out = Vec::with_capacity(self.outputs.len());
        let mut fuel = limits.fuel;
        for (name, kind, expr) in &self.outputs {
            // One fuel budget covers the whole run.
            let before = fuel;
            let mut meter = limits;
            meter.fuel = fuel;
            let (value, used) = expr.eval_metered(&names, meter, cancel).map_err(|e| e.of(name))?;
            fuel = before.saturating_sub(used);
            let ok = match (kind, &value) {
                (OutputKind::Text, Value::Text(_)) => true,
                (OutputKind::TextList, Value::List(items)) => items.iter().all(|v| matches!(v, Value::Text(_))),
                _ => false,
            };
            if !ok {
                return Err(ExprError::new(ErrorReason::WrongOutputKind, 0).of(name));
            }
            out.push((name.clone(), value));
        }
        Ok(out)
    }
}
