//! Evaluation with fuel, value-size, item and scope budgets.
use super::parse::{BinOp, Func, Kind, Node};
use super::{ErrorReason, ExprError, ExprLimits};
use std::collections::BTreeSet;

/// A value: whole numbers are 64-bit and overflow is an error, never a wrap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Text(String),
    List(Vec<Value>),
}

impl Value {
    /// Bytes a value is charged as: text bytes, 8 per number or flag, and 16 per list item plus
    /// the item itself, recursively. A list item thus costs at least one fuel unit to touch.
    pub fn size(&self) -> usize {
        match self {
            Value::Int(_) | Value::Bool(_) => 8,
            Value::Text(t) => t.len(),
            Value::List(items) => items.iter().fold(items.len().saturating_mul(16), |n, v| n.saturating_add(v.size())),
        }
    }

    /// Whether lists nest deeper than `limit`, found without recursion (stops at the limit).
    pub(super) fn deeper_than(&self, limit: usize) -> bool {
        let mut stack = vec![(self, 0usize)];
        while let Some((value, depth)) = stack.pop() {
            if depth > limit {
                return true;
            }
            if let Value::List(items) = value {
                stack.extend(items.iter().map(|v| (v, depth + 1)));
            }
        }
        false
    }
}

/// Fuel is checked against cancellation every this many units.
const CANCEL_EVERY: u64 = 4096;
/// Text work costs one fuel unit per this many bytes touched (plus one per step).
const BYTES_PER_FUEL: usize = 16;

pub(super) struct Machine<'c> {
    limits: ExprLimits,
    fuel: u64,
    since_check: u64,
    cancel: &'c dyn Fn() -> bool,
    scope: Vec<(String, Value)>,
    held: usize,
}

fn err(reason: ErrorReason, at: usize) -> ExprError {
    ExprError::new(reason, at)
}

fn int(v: &Value, at: usize) -> Result<i64, ExprError> {
    match v {
        Value::Int(n) => Ok(*n),
        _ => Err(err(ErrorReason::TypeMismatch, at)),
    }
}

fn text(v: &Value, at: usize) -> Result<&str, ExprError> {
    match v {
        Value::Text(t) => Ok(t),
        _ => Err(err(ErrorReason::TypeMismatch, at)),
    }
}

fn boolean(v: &Value, at: usize) -> Result<bool, ExprError> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => Err(err(ErrorReason::TypeMismatch, at)),
    }
}

/// Python-style bounds: negative indices count from the end; out-of-range bounds clamp.
fn bounds(len: usize, start: i64, end: Option<i64>) -> (usize, usize) {
    let len_i = i64::try_from(len).unwrap_or(i64::MAX);
    let clamp = |i: i64| -> usize {
        let i = if i < 0 { len_i.saturating_add(i).max(0) } else { i.min(len_i) };
        usize::try_from(i).unwrap_or(0)
    };
    let s = clamp(start);
    let e = end.map_or(len, clamp);
    (s, e.max(s))
}

/// An index into `len` items, negative from the end.
fn position(len: usize, i: i64, at: usize) -> Result<usize, ExprError> {
    let len_i = i64::try_from(len).unwrap_or(i64::MAX);
    let i = if i < 0 { len_i.checked_add(i) } else { Some(i) };
    i.filter(|i| (0..len_i).contains(i)).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| err(ErrorReason::IndexOutOfRange, at))
}

/// Byte range of chars `[start, end)` of `t`.
fn char_range(t: &str, start: usize, end: usize) -> &str {
    let mut indices = t.char_indices().map(|(i, _)| i).chain(std::iter::once(t.len()));
    let from = indices.nth(start).unwrap_or(t.len());
    let to = if end > start { indices.nth(end - start - 1).unwrap_or(t.len()) } else { from };
    t.get(from..to).unwrap_or("")
}

/// Ordering key for sort/unique/min/max: numbers, texts or flags of one kind.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key<'a> {
    Int(i64),
    Bool(bool),
    Text(&'a str),
}

fn key(v: &Value, at: usize) -> Result<Key<'_>, ExprError> {
    match v {
        Value::Int(n) => Ok(Key::Int(*n)),
        Value::Bool(b) => Ok(Key::Bool(*b)),
        Value::Text(t) => Ok(Key::Text(t)),
        Value::List(_) => Err(err(ErrorReason::TypeMismatch, at)),
    }
}

fn same_kind(a: &Key<'_>, b: &Key<'_>) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

impl<'c> Machine<'c> {
    pub(super) fn new(limits: ExprLimits, cancel: &'c dyn Fn() -> bool) -> Self {
        Self { limits, fuel: limits.fuel, since_check: 0, cancel, scope: Vec::new(), held: 0 }
    }

    pub(super) fn run(&mut self, root: &Node, names: &[(&str, &Value)]) -> Result<Value, ExprError> {
        for (name, value) in names {
            self.bind(name, (*value).clone(), 0)?;
        }
        self.eval(root)
    }

    pub(super) fn spent(&self) -> u64 {
        self.limits.fuel.saturating_sub(self.fuel)
    }

    fn spend(&mut self, units: u64, at: usize) -> Result<(), ExprError> {
        self.fuel = self.fuel.checked_sub(units).ok_or_else(|| err(ErrorReason::OutOfFuel, at))?;
        self.since_check = self.since_check.saturating_add(units);
        if self.since_check >= CANCEL_EVERY {
            self.since_check = 0;
            if (self.cancel)() {
                return Err(err(ErrorReason::Cancelled, at));
            }
        }
        Ok(())
    }
    fn spend_bytes(&mut self, bytes: usize, at: usize) -> Result<(), ExprError> {
        self.spend(1 + u64::try_from(bytes / BYTES_PER_FUEL).unwrap_or(u64::MAX), at)
    }
    fn fits(&self, bytes: usize, at: usize) -> Result<(), ExprError> {
        if bytes > self.limits.value_bytes { Err(err(ErrorReason::ValueTooLarge, at)) } else { Ok(()) }
    }
    fn items(&self, n: usize, at: usize) -> Result<(), ExprError> {
        if n > self.limits.list_items { Err(err(ErrorReason::TooManyItems, at)) } else { Ok(()) }
    }
    /// Checks a produced value against the budgets, charging for its size.
    fn admit(&mut self, v: Value, at: usize) -> Result<Value, ExprError> {
        if let Value::List(items) = &v {
            self.items(items.len(), at)?;
        }
        let size = v.size();
        self.fits(size, at)?;
        self.spend_bytes(size, at)?;
        Ok(v)
    }
    /// Counts `bytes` as held while other parts are evaluated (names in scope, a left operand,
    /// evaluated arguments, list items so far): at most four maximal values at once.
    fn hold(&mut self, bytes: usize, at: usize) -> Result<(), ExprError> {
        let total = self.held.saturating_add(bytes);
        if total > self.limits.value_bytes.saturating_mul(4) {
            return Err(err(ErrorReason::ValueTooLarge, at));
        }
        self.held = total;
        Ok(())
    }
    fn release(&mut self, bytes: usize) {
        self.held = self.held.saturating_sub(bytes);
    }
    fn bind(&mut self, name: &str, value: Value, at: usize) -> Result<(), ExprError> {
        self.hold(value.size(), at)?;
        self.scope.push((name.to_owned(), value));
        Ok(())
    }
    fn unbind(&mut self) {
        if let Some((_, v)) = self.scope.pop() {
            self.held = self.held.saturating_sub(v.size());
        }
    }

    fn eval(&mut self, node: &Node) -> Result<Value, ExprError> {
        self.spend(1, node.at)?;
        let at = node.at;
        match &node.kind {
            Kind::Int(n) => Ok(Value::Int(*n)),
            Kind::Bool(b) => Ok(Value::Bool(*b)),
            Kind::Str(s) => self.admit(Value::Text(s.clone()), at),
            Kind::Name(name) => {
                let value = self.scope.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone()).ok_or_else(|| err(ErrorReason::UnknownName, at))?;
                self.spend_bytes(value.size(), at)?;
                Ok(value)
            }
            Kind::List(items) => {
                self.items(items.len(), at)?;
                let mut out = Vec::with_capacity(items.len());
                let mut bytes = 0usize;
                let result = (|| {
                    for item in items {
                        let v = self.eval(item)?;
                        let size = v.size().saturating_add(16);
                        bytes = bytes.saturating_add(size);
                        self.fits(bytes, at)?;
                        self.hold(size, at)?;
                        out.push(v);
                    }
                    Ok(())
                })();
                self.release(bytes);
                result.map(|()| Value::List(out))
            }
            Kind::Neg(a) => {
                let n = int(&self.eval(a)?, a.at)?;
                n.checked_neg().map(Value::Int).ok_or_else(|| err(ErrorReason::Overflow, at))
            }
            Kind::Not(a) => Ok(Value::Bool(!boolean(&self.eval(a)?, a.at)?)),
            Kind::And(a, b) => {
                if !boolean(&self.eval(a)?, a.at)? {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(boolean(&self.eval(b)?, b.at)?))
            }
            Kind::Or(a, b) => {
                if boolean(&self.eval(a)?, a.at)? {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(boolean(&self.eval(b)?, b.at)?))
            }
            Kind::If(c, yes, no) => {
                if boolean(&self.eval(c)?, c.at)? {
                    self.eval(yes)
                } else {
                    self.eval(no)
                }
            }
            Kind::Let(name, value, body) => {
                let v = self.eval(value)?;
                self.bind(name, v, at)?;
                let result = self.eval(body);
                self.unbind();
                result
            }
            Kind::Binary(op, a, b) => {
                let left = self.eval(a)?;
                let size = left.size();
                self.hold(size, at)?;
                let right = self.eval(b);
                self.release(size);
                self.binary(*op, left, right?, at)
            }
            Kind::Index(a, i) => {
                let target = self.eval(a)?;
                let size = target.size();
                self.hold(size, at)?;
                let index = self.eval(i);
                self.release(size);
                let i = int(&index?, i.at)?;
                self.index(target, i, at)
            }
            Kind::Call(func, args) => self.call(*func, args, at),
        }
    }

    fn binary(&mut self, op: BinOp, left: Value, right: Value, at: usize) -> Result<Value, ExprError> {
        use BinOp::*;
        match op {
            Add => match (left, right) {
                (Value::Int(a), Value::Int(b)) => a.checked_add(b).map(Value::Int).ok_or_else(|| err(ErrorReason::Overflow, at)),
                (Value::Text(mut a), Value::Text(b)) => {
                    self.fits(a.len().saturating_add(b.len()), at)?;
                    a.push_str(&b);
                    self.admit(Value::Text(a), at)
                }
                (Value::List(mut a), Value::List(b)) => {
                    self.items(a.len().saturating_add(b.len()), at)?;
                    a.extend(b);
                    self.admit(Value::List(a), at)
                }
                _ => Err(err(ErrorReason::TypeMismatch, at)),
            },
            Sub | Mul | Div | Rem => {
                let (a, b) = (int(&left, at)?, int(&right, at)?);
                if matches!(op, Div | Rem) && b == 0 {
                    return Err(err(ErrorReason::DivisionByZero, at));
                }
                let result = match op {
                    Sub => a.checked_sub(b),
                    Mul => a.checked_mul(b),
                    Div => a.checked_div(b),
                    _ => a.checked_rem(b),
                };
                result.map(Value::Int).ok_or_else(|| err(ErrorReason::Overflow, at))
            }
            Eq | Ne => {
                self.spend_bytes(left.size().min(right.size()), at)?;
                Ok(Value::Bool((left == right) == (op == Eq)))
            }
            Lt | Le | Gt | Ge => {
                let (a, b) = (key(&left, at)?, key(&right, at)?);
                if !same_kind(&a, &b) || matches!(a, Key::Bool(_)) {
                    return Err(err(ErrorReason::TypeMismatch, at));
                }
                self.spend_bytes(left.size().min(right.size()), at)?;
                Ok(Value::Bool(match op {
                    Lt => a < b,
                    Le => a <= b,
                    Gt => a > b,
                    _ => a >= b,
                }))
            }
        }
    }

    fn index(&mut self, target: Value, i: i64, at: usize) -> Result<Value, ExprError> {
        match target {
            Value::List(mut items) => {
                let p = position(items.len(), i, at)?;
                Ok(items.swap_remove(p))
            }
            Value::Text(t) => {
                self.spend_bytes(t.len(), at)?;
                let p = position(t.chars().count(), i, at)?;
                Ok(Value::Text(t.chars().nth(p).map(String::from).unwrap_or_default()))
            }
            _ => Err(err(ErrorReason::TypeMismatch, at)),
        }
    }

    /// Evaluates the arguments (the recursive part, kept to a small stack frame), then applies
    /// the function in [`Self::apply`], which doesn't recurse.
    fn call(&mut self, func: Func, args: &[Node], at: usize) -> Result<Value, ExprError> {
        if matches!(func, Func::Map | Func::Filter) {
            return self.per_item(func, args, at);
        }
        let values = self.arguments(args, at)?;
        let v = self.apply(func, values, args, at)?;
        self.admit(v, at)
    }

    fn arguments(&mut self, args: &[Node], at: usize) -> Result<Vec<Value>, ExprError> {
        let mut values = Vec::with_capacity(args.len());
        let mut held = 0usize;
        let evaluated = (|| {
            for arg in args {
                let v = self.eval(arg)?;
                held = held.saturating_add(v.size());
                self.hold(v.size(), at)?;
                values.push(v);
            }
            Ok(())
        })();
        self.release(held);
        evaluated?;
        Ok(values)
    }

    #[inline(never)]
    fn apply(&mut self, func: Func, values: Vec<Value>, args: &[Node], at: usize) -> Result<Value, ExprError> {
        let arg_at = |i: usize| args.get(i).map_or(at, |n| n.at);
        let mut values = values.into_iter();
        let mut next = || values.next().ok_or_else(|| err(ErrorReason::WrongArgumentCount, at));
        let a = next()?;
        self.spend_bytes(a.size(), at)?;
        let v = match func {
            Func::Len => match &a {
                Value::Text(t) => Value::Int(i64::try_from(t.chars().count()).unwrap_or(i64::MAX)),
                Value::List(items) => Value::Int(i64::try_from(items.len()).unwrap_or(i64::MAX)),
                _ => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
            },
            Func::Upper => Value::Text(text(&a, arg_at(0))?.to_uppercase()),
            Func::Lower => Value::Text(text(&a, arg_at(0))?.to_lowercase()),
            Func::Trim => Value::Text(text(&a, arg_at(0))?.trim().to_owned()),
            Func::TrimStart => Value::Text(text(&a, arg_at(0))?.trim_start().to_owned()),
            Func::TrimEnd => Value::Text(text(&a, arg_at(0))?.trim_end().to_owned()),
            Func::Chars => {
                let t = text(&a, arg_at(0))?;
                self.items(t.chars().count(), at)?;
                Value::List(t.chars().map(|c| Value::Text(c.into())).collect())
            }
            Func::Contains | Func::Count | Func::IndexOf => {
                let b = next()?;
                self.spend_bytes(b.size(), at)?;
                match (&a, &b) {
                    (Value::Text(t), Value::Text(s)) => {
                        if s.is_empty() && func != Func::Contains {
                            return Err(err(ErrorReason::EmptySeparator, arg_at(1)));
                        }
                        match func {
                            Func::Contains => Value::Bool(t.contains(s.as_str())),
                            Func::Count => Value::Int(i64::try_from(t.matches(s.as_str()).count()).unwrap_or(i64::MAX)),
                            _ => Value::Int(t.find(s.as_str()).map_or(-1, |byte| i64::try_from(t.get(..byte).map_or(0, |p| p.chars().count())).unwrap_or(-1))),
                        }
                    }
                    (Value::List(items), item) => match func {
                        Func::Contains => Value::Bool(items.contains(item)),
                        Func::Count => Value::Int(i64::try_from(items.iter().filter(|v| *v == item).count()).unwrap_or(i64::MAX)),
                        _ => Value::Int(items.iter().position(|v| v == item).map_or(-1, |p| i64::try_from(p).unwrap_or(-1))),
                    },
                    _ => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
                }
            }
            Func::StartsWith | Func::EndsWith => {
                let b = next()?;
                let (t, s) = (text(&a, arg_at(0))?, text(&b, arg_at(1))?);
                Value::Bool(if func == Func::StartsWith { t.starts_with(s) } else { t.ends_with(s) })
            }
            Func::Replace => {
                let (b, c) = (next()?, next()?);
                let (t, from, to) = (text(&a, arg_at(0))?, text(&b, arg_at(1))?, text(&c, arg_at(2))?);
                if from.is_empty() {
                    return Err(err(ErrorReason::EmptySeparator, arg_at(1)));
                }
                let n = t.matches(from).count();
                let grown = n.checked_mul(to.len()).and_then(|g| t.len().checked_add(g)).map(|g| g.saturating_sub(n.saturating_mul(from.len())));
                self.fits(grown.unwrap_or(usize::MAX), at)?;
                Value::Text(t.replace(from, to))
            }
            Func::Split => {
                let b = next()?;
                let (t, sep) = (text(&a, arg_at(0))?, text(&b, arg_at(1))?);
                if sep.is_empty() {
                    return Err(err(ErrorReason::EmptySeparator, arg_at(1)));
                }
                self.items(t.matches(sep).count().saturating_add(1), at)?;
                Value::List(t.split(sep).map(|p| Value::Text(p.to_owned())).collect())
            }
            Func::Lines => {
                let t = text(&a, arg_at(0))?;
                self.items(t.lines().count(), at)?;
                Value::List(t.lines().map(|p| Value::Text(p.to_owned())).collect())
            }
            Func::Join => {
                let b = next()?;
                let sep = text(&b, arg_at(1))?;
                let Value::List(items) = &a else { return Err(err(ErrorReason::TypeMismatch, arg_at(0))) };
                let mut bytes = sep.len().saturating_mul(items.len().saturating_sub(1));
                let mut parts = Vec::with_capacity(items.len());
                for item in items {
                    let t = text(item, arg_at(0))?;
                    bytes = bytes.saturating_add(t.len());
                    parts.push(t);
                }
                self.fits(bytes, at)?;
                Value::Text(parts.join(sep))
            }
            Func::Slice => {
                let start = int(&next()?, arg_at(1))?;
                let end = if args.len() > 2 { Some(int(&next()?, arg_at(2))?) } else { None };
                match a {
                    Value::Text(t) => {
                        let (s, e) = bounds(t.chars().count(), start, end);
                        Value::Text(char_range(&t, s, e).to_owned())
                    }
                    Value::List(items) => {
                        let (s, e) = bounds(items.len(), start, end);
                        Value::List(items.into_iter().skip(s).take(e - s).collect())
                    }
                    _ => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
                }
            }
            Func::Repeat => {
                let n = u64::try_from(int(&next()?, arg_at(1))?).map_err(|_| err(ErrorReason::IndexOutOfRange, arg_at(1)))?;
                // Decide in u64 against the caps before converting, so every target agrees, and
                // nothing empty is repeated (that would take time without producing bytes).
                let (len, unit) = match &a {
                    Value::Text(t) => (t.len(), t.len()),
                    Value::List(items) => (items.len(), a.size()),
                    _ => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
                };
                let n = if len == 0 { 0 } else { n };
                if let Value::List(items) = &a {
                    self.items(usize::try_from(u64::try_from(items.len()).unwrap_or(u64::MAX).saturating_mul(n)).unwrap_or(usize::MAX), at)?;
                }
                self.fits(usize::try_from(u64::try_from(unit).unwrap_or(u64::MAX).saturating_mul(n)).unwrap_or(usize::MAX), at)?;
                let n = usize::try_from(n).map_err(|_| err(ErrorReason::ValueTooLarge, at))?;
                match &a {
                    Value::Text(t) => Value::Text(t.repeat(n)),
                    Value::List(items) => Value::List(std::iter::repeat_n(items, n).flatten().cloned().collect()),
                    _ => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
                }
            }
            Func::Reverse => match a {
                Value::Text(t) => Value::Text(t.chars().rev().collect()),
                Value::List(mut items) => {
                    items.reverse();
                    Value::List(items)
                }
                _ => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
            },
            Func::Sort | Func::Unique => {
                let Value::List(items) = a else { return Err(err(ErrorReason::TypeMismatch, arg_at(0))) };
                let mut keyed = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    let k = key(item, arg_at(0))?;
                    if keyed.first().is_some_and(|(first, _)| !same_kind(first, &k)) {
                        return Err(err(ErrorReason::TypeMismatch, arg_at(0)));
                    }
                    keyed.push((k, i));
                }
                let n = u64::try_from(items.len()).unwrap_or(u64::MAX);
                self.spend(n.saturating_mul(16), at)?;
                let order: Vec<usize> = if func == Func::Sort {
                    keyed.sort();
                    keyed.into_iter().map(|(_, i)| i).collect()
                } else {
                    let mut seen = BTreeSet::new();
                    keyed.into_iter().filter(|(k, _)| seen.insert(k.clone())).map(|(_, i)| i).collect()
                };
                let mut slots: Vec<Option<Value>> = items.into_iter().map(Some).collect();
                Value::List(order.into_iter().filter_map(|i| slots.get_mut(i).and_then(Option::take)).collect())
            }
            Func::Text => match a {
                Value::Int(n) => Value::Text(n.to_string()),
                Value::Bool(b) => Value::Text(b.to_string()),
                Value::Text(t) => Value::Text(t),
                Value::List(_) => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
            },
            Func::Int => match &a {
                Value::Int(n) => Value::Int(*n),
                Value::Text(t) => Value::Int(t.trim().parse::<i64>().map_err(|_| err(ErrorReason::InvalidConversion, arg_at(0)))?),
                _ => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
            },
            Func::First | Func::Last => match a {
                Value::List(mut items) => {
                    if items.is_empty() {
                        return Err(err(ErrorReason::EmptyList, arg_at(0)));
                    }
                    if func == Func::First { items.swap_remove(0) } else { items.pop().ok_or_else(|| err(ErrorReason::EmptyList, at))? }
                }
                Value::Text(t) => {
                    let c = if func == Func::First { t.chars().next() } else { t.chars().next_back() };
                    Value::Text(c.map(String::from).ok_or_else(|| err(ErrorReason::EmptyList, arg_at(0)))?)
                }
                _ => return Err(err(ErrorReason::TypeMismatch, arg_at(0))),
            },
            Func::Range => {
                let (from, to) = (int(&a, arg_at(0))?, int(&next()?, arg_at(1))?);
                let n = (i128::from(to) - i128::from(from)).max(0);
                self.items(usize::try_from(n).unwrap_or(usize::MAX), at)?;
                Value::List((from..to).map(Value::Int).collect())
            }
            Func::Min | Func::Max => {
                let b = next()?;
                let (ka, kb) = (key(&a, arg_at(0))?, key(&b, arg_at(1))?);
                if !same_kind(&ka, &kb) || matches!(ka, Key::Bool(_)) {
                    return Err(err(ErrorReason::TypeMismatch, at));
                }
                let first = if func == Func::Min { ka <= kb } else { ka >= kb };
                if first { a } else { b }
            }
            Func::PadStart | Func::PadEnd => {
                let width = int(&next()?, arg_at(1))?;
                let fill = next()?;
                let (t, fill) = (text(&a, arg_at(0))?, text(&fill, arg_at(2))?);
                let mut fill_chars = fill.chars();
                let (Some(f), None) = (fill_chars.next(), fill_chars.next()) else { return Err(err(ErrorReason::TypeMismatch, arg_at(2))) };
                let have = u64::try_from(t.chars().count()).unwrap_or(u64::MAX);
                let pad = u64::try_from(width).unwrap_or(0).saturating_sub(have);
                let bytes = pad.saturating_mul(f.len_utf8() as u64).saturating_add(t.len() as u64);
                self.fits(usize::try_from(bytes).unwrap_or(usize::MAX), at)?;
                let pad = usize::try_from(pad).map_err(|_| err(ErrorReason::ValueTooLarge, at))?;
                let padding: String = std::iter::repeat_n(f, pad).collect();
                Value::Text(if func == Func::PadStart { padding + t } else { t.to_owned() + &padding })
            }
            Func::Map | Func::Filter => return Err(err(ErrorReason::UnknownFunction, at)),
        };
        Ok(v)
    }

    /// `map(list, expr)` and `filter(list, condition)`, with `it` and `index` bound per item.
    fn per_item(&mut self, func: Func, args: &[Node], at: usize) -> Result<Value, ExprError> {
        let (Some(list), Some(body)) = (args.first(), args.get(1)) else { return Err(err(ErrorReason::WrongArgumentCount, at)) };
        let Value::List(items) = self.eval(list)? else { return Err(err(ErrorReason::TypeMismatch, list.at)) };
        let source = items.iter().fold(0usize, |n, v| n.saturating_add(v.size()));
        self.hold(source, at)?;
        let result = self.each(func, items, body, at);
        self.release(source);
        self.admit(Value::List(result?), at)
    }

    fn each(&mut self, func: Func, items: Vec<Value>, body: &Node, at: usize) -> Result<Vec<Value>, ExprError> {
        let mut bytes = 0usize;
        // The results so far are held while later items are evaluated.
        let result = self.each_held(func, items, body, at, &mut bytes);
        self.release(bytes);
        result
    }

    fn each_held(&mut self, func: Func, items: Vec<Value>, body: &Node, at: usize, bytes: &mut usize) -> Result<Vec<Value>, ExprError> {
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.into_iter().enumerate() {
            self.bind("it", item.clone(), at)?;
            if let Err(e) = self.bind("index", Value::Int(i64::try_from(i).unwrap_or(i64::MAX)), at) {
                self.unbind();
                return Err(e);
            }
            let result = self.eval(body);
            self.unbind();
            self.unbind();
            let result = result?;
            let kept = if func == Func::Map {
                result
            } else if boolean(&result, body.at)? {
                item
            } else {
                continue;
            };
            let size = kept.size().saturating_add(16);
            self.fits(bytes.saturating_add(size), at)?;
            self.hold(size, at)?;
            *bytes = bytes.saturating_add(size);
            out.push(kept);
        }
        Ok(out)
    }
}
