//! Tokens, with the byte offset each starts at.
use super::{ErrorReason, ExprError, MAX_NODES};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tok {
    Int(i64),
    Str(String),
    Ident(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Let,
    In,
    If,
    Then,
    Else,
    True,
    False,
    And,
    Or,
    Not,
}

pub(super) fn lex(source: &str) -> Result<Vec<(Tok, usize)>, ExprError> {
    let mut out = Vec::new();
    let mut chars = source.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if out.len() >= MAX_NODES {
            return Err(ExprError::new(ErrorReason::TooComplex, at));
        }
        let tok = match c {
            c if c.is_whitespace() => continue,
            '#' => {
                // A comment runs to the end of the line.
                while chars.next_if(|(_, c)| *c != '\n').is_some() {}
                continue;
            }
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            ',' => Tok::Comma,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '%' => Tok::Percent,
            '=' => {
                if chars.next_if(|(_, c)| *c == '=').is_some() {
                    Tok::Eq
                } else {
                    Tok::Assign
                }
            }
            '!' => {
                if chars.next_if(|(_, c)| *c == '=').is_some() {
                    Tok::Ne
                } else {
                    return Err(ExprError::new(ErrorReason::InvalidCharacter, at));
                }
            }
            '<' => {
                if chars.next_if(|(_, c)| *c == '=').is_some() {
                    Tok::Le
                } else {
                    Tok::Lt
                }
            }
            '>' => {
                if chars.next_if(|(_, c)| *c == '=').is_some() {
                    Tok::Ge
                } else {
                    Tok::Gt
                }
            }
            '"' => Tok::Str(string(&mut chars, at)?),
            '0'..='9' => {
                let mut value: i64 = 0;
                let mut digit = Some(c);
                while let Some(d) = digit {
                    let n = i64::from(d.to_digit(10).unwrap_or(0));
                    value = value.checked_mul(10).and_then(|v| v.checked_add(n)).ok_or_else(|| ExprError::new(ErrorReason::InvalidNumber, at))?;
                    digit = chars.next_if(|(_, c)| c.is_ascii_digit()).map(|(_, c)| c);
                }
                if chars.peek().is_some_and(|(_, c)| c.is_alphabetic() || *c == '_') {
                    return Err(ExprError::new(ErrorReason::InvalidNumber, at));
                }
                Tok::Int(value)
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut word = String::from(c);
                while let Some((_, c)) = chars.next_if(|(_, c)| c.is_ascii_alphanumeric() || *c == '_') {
                    if word.len() >= 64 {
                        return Err(ExprError::new(ErrorReason::UnknownName, at));
                    }
                    word.push(c);
                }
                match word.as_str() {
                    "let" => Tok::Let,
                    "in" => Tok::In,
                    "if" => Tok::If,
                    "then" => Tok::Then,
                    "else" => Tok::Else,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "and" => Tok::And,
                    "or" => Tok::Or,
                    "not" => Tok::Not,
                    _ => Tok::Ident(word),
                }
            }
            _ => return Err(ExprError::new(ErrorReason::InvalidCharacter, at)),
        };
        out.push((tok, at));
    }
    Ok(out)
}

/// The rest of a string literal after its opening quote. Escapes: `\"`, `\\`, `\n`, `\t`, `\r`
/// and `\u{…}` with one to six hex digits naming a Unicode scalar value.
fn string(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>, start: usize) -> Result<String, ExprError> {
    let mut text = String::new();
    loop {
        let Some((at, c)) = chars.next() else { return Err(ExprError::new(ErrorReason::InvalidString, start)) };
        match c {
            '"' => return Ok(text),
            '\\' => {
                let Some((_, e)) = chars.next() else { return Err(ExprError::new(ErrorReason::InvalidString, at)) };
                match e {
                    '"' => text.push('"'),
                    '\\' => text.push('\\'),
                    'n' => text.push('\n'),
                    't' => text.push('\t'),
                    'r' => text.push('\r'),
                    'u' => {
                        if chars.next().map(|(_, c)| c) != Some('{') {
                            return Err(ExprError::new(ErrorReason::InvalidString, at));
                        }
                        let mut code: u32 = 0;
                        let mut digits = 0;
                        loop {
                            let Some((_, h)) = chars.next() else { return Err(ExprError::new(ErrorReason::InvalidString, at)) };
                            if h == '}' {
                                break;
                            }
                            let digit = h.to_digit(16).ok_or_else(|| ExprError::new(ErrorReason::InvalidString, at))?;
                            digits += 1;
                            if digits > 6 {
                                return Err(ExprError::new(ErrorReason::InvalidString, at));
                            }
                            code = code * 16 + digit;
                        }
                        let c = (digits > 0).then(|| char::from_u32(code)).flatten().ok_or_else(|| ExprError::new(ErrorReason::InvalidString, at))?;
                        text.push(c);
                    }
                    _ => return Err(ExprError::new(ErrorReason::InvalidString, at)),
                }
            }
            c => text.push(c),
        }
    }
}
