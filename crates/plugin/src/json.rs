//! A bounded JSON reader and the canonical writer.
//!
//! Reading enforces the contract's JSON profile before anything is interpreted: UTF-8 only, no
//! byte-order mark, nesting at most [`Limits::depth`], every string, array and object within its
//! limits, object keys unique, numbers integers within ±(2^53−1) (any number, kept as written, only where a
//! caller asks for it, as in opaque plugin data). Recursion is bounded by the depth limit.
//!
//! Canonical form ([`canonical`]): object keys sorted by their UTF-8 bytes, no insignificant
//! whitespace, integers in shortest decimal, strings with only `"` `\` and U+0000–U+001F escaped
//! (`\b \f \n \r \t` short forms, otherwise `\u00xx` in lowercase hex) and every other code point
//! written as UTF-8, unnormalised. Other numbers have no canonical form in protocol 1.
use crate::error::{Code, Result, WireError, err};
use std::collections::BTreeMap;

/// Largest integer magnitude a JSON number may carry (2^53−1, exact in every JSON reader). Wider
/// values travel as canonical decimal strings.
pub const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;
/// Longest number literal kept as written.
pub const MAX_RAW_NUMBER: usize = 256;

/// A parsed JSON value. Objects keep keys in canonical (byte) order.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    /// A number outside the integer profile (a fraction, an exponent, `-0`, or beyond
    /// ±(2^53−1)), as written; only when [`Limits::raw_numbers`] allows it.
    Number(String),
    Str(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn kind(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "boolean",
            Json::Int(_) | Json::Number(_) => "number",
            Json::Str(_) => "string",
            Json::Array(_) => "array",
            Json::Object(_) => "object",
        }
    }
}

/// What a reader accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Whole input.
    pub bytes: usize,
    /// Nesting of arrays and objects; the outermost container is depth 1.
    pub depth: usize,
    /// Each string (keys included), in bytes after unescaping.
    pub string_bytes: usize,
    /// Entries in each array or object.
    pub entries: usize,
    /// Whether any valid JSON number is accepted, kept as written (opaque plugin data);
    /// otherwise only integers within ±(2^53−1).
    pub raw_numbers: bool,
}

impl Limits {
    /// The protocol's defaults: 1 MiB, depth 32, 64 KiB strings, 4,096 entries, no floats.
    pub const WIRE: Limits = Limits { bytes: crate::MAX_FRAME_BYTES, depth: 32, string_bytes: 64 * 1024, entries: 4096, raw_numbers: false };
}

/// Parses `bytes` as one JSON value under `limits`.
pub fn parse(bytes: &[u8], limits: Limits) -> Result<Json> {
    if bytes.len() > limits.bytes {
        return err(Code::TooLarge, "input is longer than its limit");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| WireError::new(Code::Syntax, "input isn't UTF-8"))?;
    let mut reader = Reader { text, pos: 0, limits };
    reader.space();
    let value = reader.value(0)?;
    reader.space();
    if reader.pos != text.len() {
        return Err(reader.error(Code::Syntax, "unexpected text after the value"));
    }
    Ok(value)
}

struct Reader<'a> {
    text: &'a str,
    pos: usize,
    limits: Limits,
}

impl Reader<'_> {
    fn error(&self, code: Code, detail: &'static str) -> WireError {
        WireError::new(code, detail).at(&format!("@{}", self.pos))
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, literal: &str, value: Json) -> Result<Json> {
        if self.text.get(self.pos..).is_some_and(|rest| rest.starts_with(literal)) {
            self.pos += literal.len();
            Ok(value)
        } else {
            Err(self.error(Code::Syntax, "unexpected character"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json> {
        match self.peek() {
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.expect("true", Json::Bool(true)),
            Some(b'f') => self.expect("false", Json::Bool(false)),
            Some(b'n') => self.expect("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error(Code::Syntax, "unexpected character")),
            None => Err(self.error(Code::Syntax, "unexpected end of input")),
        }
    }

    fn enter(&self, depth: usize) -> Result<()> {
        if depth > self.limits.depth { Err(self.error(Code::TooDeep, "nesting is deeper than its limit")) } else { Ok(()) }
    }

    fn array(&mut self, depth: usize) -> Result<Json> {
        self.enter(depth)?;
        self.pos += 1;
        let mut items = Vec::new();
        self.space();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            if items.len() >= self.limits.entries {
                return Err(self.error(Code::TooLarge, "an array has more entries than its limit"));
            }
            self.space();
            items.push(self.value(depth)?);
            self.space();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.error(Code::Syntax, "expected , or ]")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json> {
        self.enter(depth)?;
        self.pos += 1;
        let mut fields = BTreeMap::new();
        self.space();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            if fields.len() >= self.limits.entries {
                return Err(self.error(Code::TooLarge, "an object has more entries than its limit"));
            }
            self.space();
            if self.peek() != Some(b'"') {
                return Err(self.error(Code::Syntax, "expected a key"));
            }
            let at = self.pos;
            let key = self.string()?;
            self.space();
            if self.peek() != Some(b':') {
                return Err(self.error(Code::Syntax, "expected :"));
            }
            self.pos += 1;
            self.space();
            let value = self.value(depth)?;
            if fields.contains_key(&key) {
                return Err(WireError::new(Code::DuplicateKey, "an object names the same key twice").at(&format!("@{at}")));
            }
            fields.insert(key, value);
            self.space();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(fields));
                }
                _ => return Err(self.error(Code::Syntax, "expected , or }")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        let digits = self.text.get(self.pos..self.pos.saturating_add(4)).ok_or_else(|| self.error(Code::Syntax, "short \\u escape"))?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.error(Code::Syntax, "bad \\u escape"));
        }
        let value = u32::from_str_radix(digits, 16).map_err(|_| self.error(Code::Syntax, "bad \\u escape"))?;
        self.pos += 4;
        Ok(value)
    }

    fn string(&mut self) -> Result<String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let rest = self.text.get(self.pos..).unwrap_or("");
            // Copy the run up to the next quote, backslash or control character in one go.
            let run = rest.bytes().position(|b| b == b'"' || b == b'\\' || b < 0x20).unwrap_or(rest.len());
            out.push_str(rest.get(..run).unwrap_or(""));
            self.pos += run;
            if out.len() > self.limits.string_bytes {
                return Err(self.error(Code::TooLarge, "a string is longer than its limit"));
            }
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let escape = self.peek().ok_or_else(|| self.error(Code::Syntax, "unexpected end of input"))?;
                    self.pos += 1;
                    let c = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let first = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&first) {
                                if self.text.get(self.pos..).is_some_and(|r| r.starts_with("\\u")) {
                                    self.pos += 2;
                                } else {
                                    return Err(self.error(Code::BadString, "a high surrogate without its low half"));
                                }
                                let second = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&second) {
                                    return Err(self.error(Code::BadString, "a high surrogate without its low half"));
                                }
                                0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                            } else if (0xdc00..0xe000).contains(&first) {
                                return Err(self.error(Code::BadString, "a lone low surrogate"));
                            } else {
                                first
                            };
                            char::from_u32(code).ok_or_else(|| self.error(Code::BadString, "not a Unicode scalar value"))?
                        }
                        _ => return Err(self.error(Code::Syntax, "unknown escape")),
                    };
                    out.push(c);
                }
                Some(_) => return Err(self.error(Code::BadString, "a control character must be escaped")),
                None => return Err(self.error(Code::Syntax, "unterminated string")),
            }
        }
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.pos;
        let bytes = self.text.as_bytes();
        let digits = |from: usize| bytes.get(from..).map_or(0, |rest| rest.iter().take_while(|b| b.is_ascii_digit()).count());
        let mut end = start;
        if bytes.get(end) == Some(&b'-') {
            end += 1;
        }
        let int = digits(end);
        if int == 0 {
            return Err(self.error(Code::Syntax, "a number needs digits"));
        }
        if int > 1 && bytes.get(end) == Some(&b'0') {
            return Err(self.error(Code::Syntax, "a number has a leading zero"));
        }
        end += int;
        let mut float = false;
        if bytes.get(end) == Some(&b'.') {
            let frac = digits(end + 1);
            if frac == 0 {
                return Err(self.error(Code::Syntax, "a fraction needs digits"));
            }
            end += 1 + frac;
            float = true;
        }
        if matches!(bytes.get(end), Some(b'e' | b'E')) {
            end += 1;
            if matches!(bytes.get(end), Some(b'+' | b'-')) {
                end += 1;
            }
            let exp = digits(end);
            if exp == 0 {
                return Err(self.error(Code::Syntax, "an exponent needs digits"));
            }
            end += exp;
            float = true;
        }
        let text = self.text.get(start..end).unwrap_or("");
        self.pos = end;
        let integer = if float || text == "-0" { None } else { text.parse::<i64>().ok().filter(|n| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(n)) };
        match integer {
            Some(n) => Ok(Json::Int(n)),
            None if self.limits.raw_numbers => {
                if text.len() > MAX_RAW_NUMBER {
                    return Err(WireError::new(Code::TooLarge, "a number is longer than 256 characters").at(&format!("@{start}")));
                }
                Ok(Json::Number(text.to_owned()))
            }
            None if float => Err(WireError::new(Code::BadNumber, "numbers here are integers; floats aren't allowed").at(&format!("@{start}"))),
            None if text == "-0" => Err(WireError::new(Code::BadNumber, "-0 isn't a canonical integer").at(&format!("@{start}"))),
            None => Err(WireError::new(Code::BadNumber, "an integer is beyond ±(2^53−1); use a decimal string").at(&format!("@{start}"))),
        }
    }
}

/// `value` in canonical form. Floats have none in protocol 1.
pub fn canonical(value: &Json) -> Result<String> {
    let mut out = String::new();
    write(value, &mut out)?;
    Ok(out)
}

fn write(value: &Json, out: &mut String) -> Result<()> {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(n) => out.push_str(&n.to_string()),
        Json::Number(_) => return err(Code::NotCanonical, "only integers within ±(2^53−1) have a canonical form in protocol 1"),
        Json::Str(s) => write_str(s, out),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Json::Object(fields) => {
            out.push('{');
            for (i, (key, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(key, out);
                out.push(':');
                write(item, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Reads an object's fields one by one and, at [`Fields::finish`], refuses any it didn't
/// define. Errors carry the field's location.
pub(crate) struct Fields {
    map: BTreeMap<String, Json>,
    at: String,
}

impl Fields {
    pub fn new(value: Json, at: &str) -> Result<Self> {
        match value {
            Json::Object(map) => Ok(Self { map, at: at.to_owned() }),
            _ => Err(WireError::new(Code::WrongType, "expected an object").at(at)),
        }
    }

    pub fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.at)
    }

    pub fn optional(&mut self, name: &str) -> Option<Json> {
        self.map.remove(name)
    }

    pub fn required(&mut self, name: &str) -> Result<Json> {
        self.map.remove(name).ok_or_else(|| WireError::new(Code::MissingField, "a required field is missing").at(&self.path(name)))
    }

    pub fn string(&mut self, name: &str) -> Result<String> {
        let at = self.path(name);
        string(self.required(name)?, &at)
    }

    pub fn int(&mut self, name: &str) -> Result<i64> {
        match self.required(name)? {
            Json::Int(n) => Ok(n),
            _ => Err(WireError::new(Code::WrongType, "expected an integer").at(&self.path(name))),
        }
    }

    /// A small non-negative integer field (versions, counts).
    pub fn u32(&mut self, name: &str) -> Result<u32> {
        let n = self.int(name)?;
        u32::try_from(n).map_err(|_| WireError::new(Code::BadNumber, "expected a non-negative 32-bit integer").at(&self.path(name)))
    }

    pub fn array(&mut self, name: &str, max: usize) -> Result<Vec<Json>> {
        array(self.required(name)?, max, &self.path(name))
    }

    pub fn finish(self) -> Result<()> {
        match self.map.keys().next() {
            Some(name) => Err(WireError::new(Code::UnknownField, "a field this version doesn't define").at(&format!("{}/{}", self.at, name))),
            None => Ok(()),
        }
    }
}

pub(crate) fn string(value: Json, at: &str) -> Result<String> {
    match value {
        Json::Str(s) => Ok(s),
        _ => Err(WireError::new(Code::WrongType, "expected a string").at(at)),
    }
}

pub(crate) fn array(value: Json, max: usize, at: &str) -> Result<Vec<Json>> {
    match value {
        Json::Array(items) if items.len() <= max => Ok(items),
        Json::Array(_) => Err(WireError::new(Code::TooLarge, "a list has more entries than its limit").at(at)),
        _ => Err(WireError::new(Code::WrongType, "expected an array").at(at)),
    }
}

/// Parses a canonical decimal string (`"0"`, or digits without a leading zero) as a `u64`.
pub fn decimal(text: &str) -> Result<u64> {
    let canonical = !text.is_empty() && text.len() <= 20 && text.bytes().all(|b| b.is_ascii_digit()) && (text == "0" || !text.starts_with('0'));
    if !canonical {
        return err(Code::BadNumber, "expected a canonical decimal string");
    }
    text.parse::<u64>().map_err(|_| WireError::new(Code::BadNumber, "a decimal string is beyond 2^64−1"))
}

/// Parses `len` bytes of lowercase hex.
pub(crate) fn hex<const N: usize>(text: &str) -> Result<[u8; N]> {
    let mut out = [0u8; N];
    if text.len() != N * 2 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return err(Code::BadDigest, "expected lowercase hex of the right length");
    }
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = text.get(i * 2..i * 2 + 2).unwrap_or("");
        *byte = u8::from_str_radix(pair, 16).map_err(|_| WireError::new(Code::BadDigest, "expected lowercase hex"))?;
    }
    Ok(out)
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
