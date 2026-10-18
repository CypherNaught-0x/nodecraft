//! A recursive-descent parser with explicit depth and node budgets.
//!
//! ```text
//! expr    := "let" NAME "=" expr "in" expr | "if" expr "then" expr "else" expr | or
//! or      := and ("or" and)*
//! and     := not ("and" not)*
//! not     := "not" not | compare
//! compare := sum (("=="|"!="|"<"|"<="|">"|">=") sum)?
//! sum     := product (("+"|"-") product)*
//! product := unary (("*"|"/"|"%") unary)*
//! unary   := "-" unary | postfix
//! postfix := primary ("[" expr "]")*
//! primary := INT | STRING | "true" | "false" | NAME | NAME "(" args ")" | "(" expr ")" | "[" items "]"
//! ```
use super::lex::Tok;
use super::{ErrorReason, ExprError, MAX_DEPTH, MAX_NODES};

/// Deepest syntax tree: operator chains (`a + b + c`) and indexing grow it without nesting, so
/// it is bounded separately to keep evaluation's recursion small.
pub(super) const MAX_TREE_DEPTH: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// The built-in functions. `Map` and `Filter` evaluate their second argument once per item,
/// with `it` (the item) and `index` (from 0) in scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Func {
    Len,
    Upper,
    Lower,
    Trim,
    TrimStart,
    TrimEnd,
    Chars,
    Contains,
    StartsWith,
    EndsWith,
    Replace,
    Split,
    Lines,
    Join,
    Slice,
    Repeat,
    Reverse,
    Sort,
    Unique,
    Text,
    Int,
    Count,
    IndexOf,
    First,
    Last,
    Range,
    Min,
    Max,
    PadStart,
    PadEnd,
    Map,
    Filter,
}

impl Func {
    fn lookup(name: &str) -> Option<(Self, usize, usize)> {
        Some(match name {
            "len" => (Self::Len, 1, 1),
            "upper" => (Self::Upper, 1, 1),
            "lower" => (Self::Lower, 1, 1),
            "trim" => (Self::Trim, 1, 1),
            "trim_start" => (Self::TrimStart, 1, 1),
            "trim_end" => (Self::TrimEnd, 1, 1),
            "chars" => (Self::Chars, 1, 1),
            "contains" => (Self::Contains, 2, 2),
            "starts_with" => (Self::StartsWith, 2, 2),
            "ends_with" => (Self::EndsWith, 2, 2),
            "replace" => (Self::Replace, 3, 3),
            "split" => (Self::Split, 2, 2),
            "lines" => (Self::Lines, 1, 1),
            "join" => (Self::Join, 2, 2),
            "slice" => (Self::Slice, 2, 3),
            "repeat" => (Self::Repeat, 2, 2),
            "reverse" => (Self::Reverse, 1, 1),
            "sort" => (Self::Sort, 1, 1),
            "unique" => (Self::Unique, 1, 1),
            "text" => (Self::Text, 1, 1),
            "int" => (Self::Int, 1, 1),
            "count" => (Self::Count, 2, 2),
            "index_of" => (Self::IndexOf, 2, 2),
            "first" => (Self::First, 1, 1),
            "last" => (Self::Last, 1, 1),
            "range" => (Self::Range, 2, 2),
            "min" => (Self::Min, 2, 2),
            "max" => (Self::Max, 2, 2),
            "pad_start" => (Self::PadStart, 3, 3),
            "pad_end" => (Self::PadEnd, 3, 3),
            "map" => (Self::Map, 2, 2),
            "filter" => (Self::Filter, 2, 2),
            _ => return None,
        })
    }

    /// Whether `name` is a built-in function (and so can't name an input or parameter).
    pub(super) fn is_builtin(name: &str) -> bool {
        Self::lookup(name).is_some()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Kind {
    Int(i64),
    Str(String),
    Bool(bool),
    Name(String),
    List(Vec<Node>),
    Neg(Box<Node>),
    Not(Box<Node>),
    Binary(BinOp, Box<Node>, Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    If(Box<Node>, Box<Node>, Box<Node>),
    Let(String, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
    Index(Box<Node>, Box<Node>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Node {
    pub(super) kind: Kind,
    pub(super) at: usize,
    depth: usize,
}

impl Kind {
    fn child_depth(&self) -> usize {
        match self {
            Kind::Int(_) | Kind::Str(_) | Kind::Bool(_) | Kind::Name(_) => 0,
            Kind::List(items) | Kind::Call(_, items) => items.iter().map(|n| n.depth).max().unwrap_or(0),
            Kind::Neg(a) | Kind::Not(a) => a.depth,
            Kind::Binary(_, a, b) | Kind::And(a, b) | Kind::Or(a, b) | Kind::Index(a, b) | Kind::Let(_, a, b) => a.depth.max(b.depth),
            Kind::If(a, b, c) => a.depth.max(b.depth).max(c.depth),
        }
    }
}

struct Parser<'a> {
    tokens: &'a [(Tok, usize)],
    pos: usize,
    end: usize,
    depth: usize,
    nodes: usize,
}

pub(super) fn parse(tokens: &[(Tok, usize)], end: usize) -> Result<Node, ExprError> {
    let mut p = Parser { tokens, pos: 0, end, depth: 0, nodes: 0 };
    let node = p.expr()?;
    match p.tokens.get(p.pos) {
        None => Ok(node),
        Some((_, at)) => Err(ExprError::new(ErrorReason::UnexpectedToken, *at)),
    }
}

impl Parser<'_> {
    fn at(&self) -> usize {
        self.tokens.get(self.pos).map_or(self.end, |(_, at)| *at)
    }
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|(t, _)| t)
    }
    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, tok: &Tok) -> Result<(), ExprError> {
        if self.eat(tok) {
            Ok(())
        } else if self.pos >= self.tokens.len() {
            Err(ExprError::new(ErrorReason::UnexpectedEnd, self.end))
        } else {
            Err(ExprError::new(ErrorReason::UnexpectedToken, self.at()))
        }
    }
    fn node(&mut self, kind: Kind, at: usize) -> Result<Node, ExprError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(ExprError::new(ErrorReason::TooComplex, at));
        }
        let depth = kind.child_depth() + 1;
        if depth > MAX_TREE_DEPTH {
            return Err(ExprError::new(ErrorReason::TooDeep, at));
        }
        Ok(Node { kind, at, depth })
    }
    /// Runs `f` one nesting level deeper.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, ExprError>) -> Result<T, ExprError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ExprError::new(ErrorReason::TooDeep, self.at()));
        }
        let result = f(self);
        self.depth -= 1;
        result
    }

    fn expr(&mut self) -> Result<Node, ExprError> {
        self.nested(|p| {
            let at = p.at();
            if p.eat(&Tok::Let) {
                let name = p.name()?;
                p.expect(&Tok::Assign)?;
                let value = p.expr()?;
                p.expect(&Tok::In)?;
                let body = p.expr()?;
                return p.node(Kind::Let(name, Box::new(value), Box::new(body)), at);
            }
            if p.eat(&Tok::If) {
                let cond = p.expr()?;
                p.expect(&Tok::Then)?;
                let yes = p.expr()?;
                p.expect(&Tok::Else)?;
                let no = p.expr()?;
                return p.node(Kind::If(Box::new(cond), Box::new(yes), Box::new(no)), at);
            }
            p.or()
        })
    }
    fn name(&mut self) -> Result<String, ExprError> {
        match self.tokens.get(self.pos) {
            Some((Tok::Ident(name), at)) => {
                if Func::is_builtin(name) {
                    return Err(ExprError::new(ErrorReason::UnexpectedToken, *at));
                }
                self.pos += 1;
                Ok(name.clone())
            }
            Some((_, at)) => Err(ExprError::new(ErrorReason::UnexpectedToken, *at)),
            None => Err(ExprError::new(ErrorReason::UnexpectedEnd, self.end)),
        }
    }
    fn or(&mut self) -> Result<Node, ExprError> {
        let mut left = self.and()?;
        while self.peek() == Some(&Tok::Or) {
            let at = self.at();
            self.pos += 1;
            let right = self.nested(|p| p.and())?;
            left = self.node(Kind::Or(Box::new(left), Box::new(right)), at)?;
        }
        Ok(left)
    }
    fn and(&mut self) -> Result<Node, ExprError> {
        let mut left = self.not()?;
        while self.peek() == Some(&Tok::And) {
            let at = self.at();
            self.pos += 1;
            let right = self.nested(|p| p.not())?;
            left = self.node(Kind::And(Box::new(left), Box::new(right)), at)?;
        }
        Ok(left)
    }
    fn not(&mut self) -> Result<Node, ExprError> {
        let at = self.at();
        if self.eat(&Tok::Not) {
            let inner = self.nested(|p| p.not())?;
            return self.node(Kind::Not(Box::new(inner)), at);
        }
        self.compare()
    }
    fn compare(&mut self) -> Result<Node, ExprError> {
        let left = self.sum()?;
        let op = match self.peek() {
            Some(Tok::Eq) => BinOp::Eq,
            Some(Tok::Ne) => BinOp::Ne,
            Some(Tok::Lt) => BinOp::Lt,
            Some(Tok::Le) => BinOp::Le,
            Some(Tok::Gt) => BinOp::Gt,
            Some(Tok::Ge) => BinOp::Ge,
            _ => return Ok(left),
        };
        let at = self.at();
        self.pos += 1;
        let right = self.nested(|p| p.sum())?;
        self.node(Kind::Binary(op, Box::new(left), Box::new(right)), at)
    }
    fn sum(&mut self) -> Result<Node, ExprError> {
        let mut left = self.product()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => return Ok(left),
            };
            let at = self.at();
            self.pos += 1;
            let right = self.nested(|p| p.product())?;
            left = self.node(Kind::Binary(op, Box::new(left), Box::new(right)), at)?;
        }
    }
    fn product(&mut self) -> Result<Node, ExprError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Rem,
                _ => return Ok(left),
            };
            let at = self.at();
            self.pos += 1;
            let right = self.nested(|p| p.unary())?;
            left = self.node(Kind::Binary(op, Box::new(left), Box::new(right)), at)?;
        }
    }
    fn unary(&mut self) -> Result<Node, ExprError> {
        let at = self.at();
        if self.eat(&Tok::Minus) {
            let inner = self.nested(|p| p.unary())?;
            return self.node(Kind::Neg(Box::new(inner)), at);
        }
        self.postfix()
    }
    fn postfix(&mut self) -> Result<Node, ExprError> {
        let mut node = self.primary()?;
        while self.peek() == Some(&Tok::LBracket) {
            let at = self.at();
            self.pos += 1;
            let index = self.expr()?;
            self.expect(&Tok::RBracket)?;
            node = self.node(Kind::Index(Box::new(node), Box::new(index)), at)?;
        }
        Ok(node)
    }
    fn primary(&mut self) -> Result<Node, ExprError> {
        let at = self.at();
        let Some((tok, _)) = self.tokens.get(self.pos) else { return Err(ExprError::new(ErrorReason::UnexpectedEnd, self.end)) };
        self.pos += 1;
        match tok {
            Tok::Int(n) => self.node(Kind::Int(*n), at),
            Tok::Str(s) => self.node(Kind::Str(s.clone()), at),
            Tok::True => self.node(Kind::Bool(true), at),
            Tok::False => self.node(Kind::Bool(false), at),
            Tok::LParen => {
                let inner = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(inner)
            }
            Tok::LBracket => {
                let items = self.list(&Tok::RBracket)?;
                self.node(Kind::List(items), at)
            }
            Tok::Ident(name) => {
                if self.eat(&Tok::LParen) {
                    let (func, min, max) = Func::lookup(name).ok_or_else(|| ExprError::new(ErrorReason::UnknownFunction, at))?;
                    let args = self.list(&Tok::RParen)?;
                    if args.len() < min || args.len() > max {
                        return Err(ExprError::new(ErrorReason::WrongArgumentCount, at));
                    }
                    self.node(Kind::Call(func, args), at)
                } else if Func::is_builtin(name) {
                    Err(ExprError::new(ErrorReason::UnknownName, at))
                } else {
                    self.node(Kind::Name(name.clone()), at)
                }
            }
            _ => Err(ExprError::new(ErrorReason::UnexpectedToken, at)),
        }
    }
    /// Comma-separated expressions up to `close` (consumed); a trailing comma is allowed.
    fn list(&mut self, close: &Tok) -> Result<Vec<Node>, ExprError> {
        let mut items = Vec::new();
        loop {
            if self.eat(close) {
                return Ok(items);
            }
            items.push(self.expr()?);
            if !self.eat(&Tok::Comma) {
                self.expect(close)?;
                return Ok(items);
            }
        }
    }
}

/// Collects names read but not bound inside `node`; `bound` holds the enclosing bindings.
pub(super) fn free_names(node: &Node, bound: &mut Vec<String>, out: &mut Vec<(String, usize)>) {
    match &node.kind {
        Kind::Int(_) | Kind::Str(_) | Kind::Bool(_) => {}
        Kind::Name(name) => {
            if !bound.iter().any(|b| b == name) {
                out.push((name.clone(), node.at));
            }
        }
        Kind::List(items) => items.iter().for_each(|n| free_names(n, bound, out)),
        Kind::Neg(a) | Kind::Not(a) => free_names(a, bound, out),
        Kind::Binary(_, a, b) | Kind::And(a, b) | Kind::Or(a, b) | Kind::Index(a, b) => {
            free_names(a, bound, out);
            free_names(b, bound, out);
        }
        Kind::If(a, b, c) => {
            free_names(a, bound, out);
            free_names(b, bound, out);
            free_names(c, bound, out);
        }
        Kind::Let(name, value, body) => {
            free_names(value, bound, out);
            bound.push(name.clone());
            free_names(body, bound, out);
            bound.pop();
        }
        Kind::Call(func, args) => {
            let per_item = matches!(func, Func::Map | Func::Filter);
            for (i, arg) in args.iter().enumerate() {
                if per_item && i == 1 {
                    bound.push("it".into());
                    bound.push("index".into());
                    free_names(arg, bound, out);
                    bound.pop();
                    bound.pop();
                } else {
                    free_names(arg, bound, out);
                }
            }
        }
    }
}
