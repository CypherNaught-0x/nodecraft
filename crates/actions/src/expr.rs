//! Custom text actions (#165): a small, original expression language, evaluated with explicit
//! budgets. See `docs/text-expressions.md` for the language and `docs/decisions/` for why this is
//! an interpreter and not Wasm.
//!
//! Nothing here touches the filesystem, network, clock, randomness or the host: an expression
//! sees only the values it is given, so the same definition, inputs and parameters always give
//! the same result on every platform, native or wasm. Every run is bounded by:
//!
//! - source size, token and syntax-tree node counts, and nesting depth (checked when parsing);
//! - fuel: every evaluation step costs at least one unit, and text/list work costs in proportion
//!   to the bytes or items it touches;
//! - the size of every value produced, the items of every list, and the bytes held by names;
//! - a cancellation check polled while fuel is spent.
//!
//! Diagnostics name a position in the expression and a fixed reason. They never repeat input or
//! parameter values, which may be private prompts.

mod definition;
mod eval;
mod lex;
mod parse;

pub use definition::{CompiledTextAction, InputKind, OutputKind, ParamDef, ParamKind, PortDef, TEXT_ACTION_SCHEMA, TextActionDefinition, TextOutputDef};
pub use eval::Value;

use serde::{Deserialize, Serialize};

/// Longest expression source, in bytes.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024;
/// Most tokens and syntax-tree nodes one expression may have.
pub const MAX_NODES: usize = 8192;
/// Deepest nesting of one expression (parentheses, operators, calls, `let`/`if`).
pub const MAX_DEPTH: usize = 32;
/// Deepest list nesting of a value passed in.
pub const MAX_VALUE_DEPTH: usize = 32;
/// Most fuel one run may spend.
pub const MAX_FUEL: u64 = 50_000_000;
/// Largest value (text bytes, or list items and their bytes) any step may produce.
pub const MAX_VALUE_BYTES: usize = 4 * 1024 * 1024;
/// Most items in any list.
pub const MAX_LIST_ITEMS: usize = 16_384;
/// Most bytes all inputs of one run may hold together.
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Budgets for one run. They may be lowered, never raised above the hard ceilings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExprLimits {
    pub fuel: u64,
    pub value_bytes: usize,
    pub list_items: usize,
    pub input_bytes: usize,
}

impl Default for ExprLimits {
    fn default() -> Self {
        Self { fuel: 10_000_000, value_bytes: MAX_VALUE_BYTES, list_items: MAX_LIST_ITEMS, input_bytes: MAX_INPUT_BYTES }
    }
}

impl ExprLimits {
    fn validate(self) -> Result<Self, ExprError> {
        if self.fuel > MAX_FUEL || self.value_bytes > MAX_VALUE_BYTES || self.list_items > MAX_LIST_ITEMS || self.input_bytes > MAX_INPUT_BYTES {
            return Err(ExprError::new(ErrorReason::InvalidLimits, 0));
        }
        Ok(self)
    }
}

/// Why an expression or definition was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorReason {
    InvalidLimits,
    SourceTooLong,
    TooComplex,
    TooDeep,
    InvalidCharacter,
    InvalidNumber,
    InvalidString,
    UnexpectedToken,
    UnexpectedEnd,
    UnknownName,
    UnknownFunction,
    WrongArgumentCount,
    TypeMismatch,
    Overflow,
    DivisionByZero,
    IndexOutOfRange,
    EmptySeparator,
    EmptyList,
    InvalidConversion,
    OutOfFuel,
    ValueTooLarge,
    TooManyItems,
    InputTooLarge,
    Cancelled,
    InvalidDefinition,
    MissingInput,
    UnknownInput,
    WrongInputKind,
    UnknownParam,
    WrongParamKind,
    WrongOutputKind,
}

impl ErrorReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidLimits => "expression limits exceed hard ceilings",
            Self::SourceTooLong => "the expression is too long",
            Self::TooComplex => "the expression has too many parts",
            Self::TooDeep => "the expression is nested too deeply",
            Self::InvalidCharacter => "unexpected character",
            Self::InvalidNumber => "invalid or too large number",
            Self::InvalidString => "invalid string literal or escape",
            Self::UnexpectedToken => "unexpected token",
            Self::UnexpectedEnd => "the expression ends too early",
            Self::UnknownName => "unknown name",
            Self::UnknownFunction => "unknown function",
            Self::WrongArgumentCount => "wrong number of arguments",
            Self::TypeMismatch => "a value has the wrong type here",
            Self::Overflow => "integer overflow",
            Self::DivisionByZero => "division by zero",
            Self::IndexOutOfRange => "index out of range",
            Self::EmptySeparator => "the separator must not be empty",
            Self::EmptyList => "the list is empty",
            Self::InvalidConversion => "the text is not a whole number",
            Self::OutOfFuel => "the expression did too much work",
            Self::ValueTooLarge => "a value grew too large",
            Self::TooManyItems => "a list grew too long",
            Self::InputTooLarge => "the inputs are too large",
            Self::Cancelled => "the run was cancelled",
            Self::InvalidDefinition => "the action definition is invalid",
            Self::MissingInput => "a required input is missing",
            Self::UnknownInput => "an input is not declared by the action",
            Self::WrongInputKind => "an input has the wrong kind",
            Self::UnknownParam => "a parameter is not declared by the action",
            Self::WrongParamKind => "a parameter has the wrong kind",
            Self::WrongOutputKind => "an output expression produced the wrong kind",
        }
    }
}

/// A refused expression: a fixed reason and the byte offset in the expression source (0 when
/// it isn't about one place). For definitions, `part` names the declared input, parameter or
/// output it concerns; it never holds a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExprError {
    pub reason: ErrorReason,
    pub at: usize,
    pub part: Option<String>,
}

impl ExprError {
    fn new(reason: ErrorReason, at: usize) -> Self {
        Self { reason, at, part: None }
    }
    fn of(mut self, part: &str) -> Self {
        self.part.get_or_insert_with(|| part.to_owned());
        self
    }
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.part {
            Some(part) => write!(f, "{} ({part}, at byte {})", self.reason.message(), self.at),
            None => write!(f, "{} (at byte {})", self.reason.message(), self.at),
        }
    }
}

impl std::error::Error for ExprError {}

/// One parsed expression, ready to evaluate any number of times.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    root: parse::Node,
}

impl Expr {
    /// Parses `source`, checking its size, complexity and depth. Names are resolved when it runs
    /// (or up front through [`TextActionDefinition::compile`]).
    pub fn parse(source: &str) -> Result<Self, ExprError> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(ExprError::new(ErrorReason::SourceTooLong, 0));
        }
        let tokens = lex::lex(source)?;
        Ok(Self { root: parse::parse(&tokens, source.len())? })
    }

    /// Evaluates with the given names in scope.
    pub fn eval(&self, names: &[(&str, &Value)], limits: ExprLimits, cancel: &dyn Fn() -> bool) -> Result<Value, ExprError> {
        self.eval_metered(names, limits, cancel).map(|(value, _)| value)
    }

    /// [`Self::eval`], also returning the fuel spent.
    fn eval_metered(&self, names: &[(&str, &Value)], limits: ExprLimits, cancel: &dyn Fn() -> bool) -> Result<(Value, u64), ExprError> {
        let limits = limits.validate()?;
        // Values are walked recursively (size, comparison, drop), so their nesting is checked
        // first, iteratively.
        if names.iter().any(|(_, v)| v.deeper_than(MAX_VALUE_DEPTH)) {
            return Err(ExprError::new(ErrorReason::TooDeep, 0));
        }
        let input: usize = names.iter().fold(0usize, |n, (_, v)| n.saturating_add(v.size()));
        if input > limits.input_bytes {
            return Err(ExprError::new(ErrorReason::InputTooLarge, 0));
        }
        let mut machine = eval::Machine::new(limits, cancel);
        let value = machine.run(&self.root, names)?;
        Ok((value, machine.spent()))
    }

    /// Names the expression reads that it doesn't bind itself, with their positions.
    fn free_names(&self) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        parse::free_names(&self.root, &mut Vec::new(), &mut out);
        out
    }
}

#[cfg(test)]
mod tests;
