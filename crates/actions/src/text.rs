//! Pure text actions. See `docs/text-actions.md` for wire and boundary semantics.
use regex::{Captures, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ITEMS: usize = 16_384;
pub const MAX_FIELDS: usize = 1024;
pub const MAX_PATTERN_BYTES: usize = 8192;
pub const MAX_SEARCH_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REPLACEMENT_SCAN_BYTES: usize = 16 * 1024 * 1024;

/// Budgets may be lowered, never raised above the hard ceilings. Bytes are UTF-8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub items: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self { input_bytes: MAX_INPUT_BYTES, output_bytes: MAX_OUTPUT_BYTES, items: MAX_ITEMS }
    }
}
impl Limits {
    fn validate(self) -> Result<Self, TextError> {
        if self.input_bytes > MAX_INPUT_BYTES || self.output_bytes > MAX_OUTPUT_BYTES || self.items > MAX_ITEMS {
            return Err(TextError::InvalidLimits);
        }
        Ok(self)
    }
    fn input<'a>(self, values: impl IntoIterator<Item = &'a str>) -> Result<(), TextError> {
        let mut bytes = 0usize;
        for value in values {
            bytes = bytes.checked_add(value.len()).ok_or(TextError::InputLimit)?;
            if bytes > self.input_bytes {
                return Err(TextError::InputLimit);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextError {
    InvalidLimits,
    InputLimit,
    OutputLimit,
    ItemLimit,
    InvalidCount,
    EmptySeparator,
    PatternLimit,
    WorkLimit,
    InvalidPattern,
    InvalidTemplate,
    MissingField,
    FieldLimit,
}
impl std::fmt::Display for TextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidLimits => "text limits exceed hard ceilings",
            Self::InputLimit => "text input byte budget exceeded",
            Self::OutputLimit => "text output byte budget exceeded",
            Self::ItemLimit => "text item or match budget exceeded",
            Self::InvalidCount => "text split count must be positive",
            Self::EmptySeparator => "text separator must not be empty",
            Self::WorkLimit => "text search or replacement work budget exceeded",
            Self::PatternLimit => "regex pattern byte budget exceeded",
            Self::InvalidPattern => "regex pattern is invalid or exceeds compilation limits",
            Self::InvalidTemplate => "template syntax is invalid",
            Self::MissingField => "template field is missing",
            Self::FieldLimit => "template field budget exceeded",
        })
    }
}
impl std::error::Error for TextError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum SplitMode {
    Separator(String),
    Regex(String),
    Paragraphs,
    Lines(usize),
    Chars(usize),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum MatchMode {
    Literal(String),
    Regex(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceOptions {
    pub pattern: MatchMode,
    pub replacement: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcatOptions {
    pub separator: String,
    pub prefix: String,
    pub suffix: String,
    pub trim: bool,
    pub skip_empty: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TextRequest {
    #[serde(rename = "text.split")]
    Split { input: String, mode: SplitMode },
    #[serde(rename = "text.replace")]
    Replace { input: String, options: ReplaceOptions },
    #[serde(rename = "text.concat")]
    Concat { inputs: Vec<String>, options: ConcatOptions },
    #[serde(rename = "text.template")]
    Template { template: String, fields: BTreeMap<String, String> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TextResult {
    Text(String),
    Parts(Vec<String>),
}
impl TextRequest {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Split { .. } => "text.split",
            Self::Replace { .. } => "text.replace",
            Self::Concat { .. } => "text.concat",
            Self::Template { .. } => "text.template",
        }
    }
    pub fn execute(&self, limits: Limits) -> Result<TextResult, TextError> {
        match self {
            Self::Split { input, mode } => split(input, mode, limits).map(TextResult::Parts),
            Self::Replace { input, options } => replace(input, options, limits).map(TextResult::Text),
            Self::Concat { inputs, options } => concat(inputs, options, limits).map(TextResult::Text),
            Self::Template { template, fields } => fill_template(template, fields, limits).map(TextResult::Text),
        }
    }
}
fn append(out: &mut String, value: &str, limits: Limits) -> Result<(), TextError> {
    if out.len().checked_add(value.len()).is_none_or(|n| n > limits.output_bytes) {
        return Err(TextError::OutputLimit);
    }
    out.push_str(value);
    Ok(())
}
struct Parts {
    values: Vec<String>,
    bytes: usize,
    limits: Limits,
}
impl Parts {
    fn push(&mut self, value: &str) -> Result<(), TextError> {
        if self.values.len() >= self.limits.items {
            return Err(TextError::ItemLimit);
        }
        self.bytes = self.bytes.checked_add(value.len()).ok_or(TextError::OutputLimit)?;
        if self.bytes > self.limits.output_bytes {
            return Err(TextError::OutputLimit);
        }
        self.values.push(value.to_owned());
        Ok(())
    }
}
fn charge(used: &mut usize, bytes: usize, maximum: usize) -> Result<(), TextError> {
    *used = used.checked_add(bytes).ok_or(TextError::WorkLimit)?;
    if *used > maximum {
        return Err(TextError::WorkLimit);
    }
    Ok(())
}
// An iterator next may search twice to suppress adjacent empty matches. Charge both
// remaining suffixes before calling it; keep the full haystack for anchor semantics.
fn search_charge(used: &mut usize, input_len: usize, previous: usize) -> Result<(), TextError> {
    let bytes = input_len.saturating_sub(previous).saturating_add(1).saturating_mul(2);
    charge(used, bytes, MAX_SEARCH_BYTES)
}
fn compile(pattern: &str, case_sensitive: bool, whole_word: bool) -> Result<Regex, TextError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(TextError::PatternLimit);
    }
    let wrapped = if whole_word { format!(r"\b(?:{pattern})\b") } else { pattern.to_owned() };
    RegexBuilder::new(&wrapped)
        .case_insensitive(!case_sensitive)
        .size_limit(1024 * 1024)
        .dfa_size_limit(256 * 1024)
        .nest_limit(64)
        .build()
        .map_err(|_| TextError::InvalidPattern)
}
/// Separator/regex modes retain empty fields; line/character modes preserve all source bytes.
pub fn split(input: &str, mode: &SplitMode, limits: Limits) -> Result<Vec<String>, TextError> {
    let limits = limits.validate()?;
    let pattern = match mode {
        SplitMode::Separator(s) | SplitMode::Regex(s) => s.as_str(),
        _ => "",
    };
    limits.input([input, pattern])?;
    let mut out = Parts { values: Vec::new(), bytes: 0, limits };
    match mode {
        SplitMode::Separator(s) => {
            if s.is_empty() {
                return Err(TextError::EmptySeparator);
            }
            for part in input.split(s) {
                out.push(part)?;
            }
        }
        SplitMode::Regex(s) => {
            let re = compile(s, true, false)?;
            let mut matches = re.find_iter(input);
            let mut previous = 0usize;
            let mut work = 0usize;
            loop {
                search_charge(&mut work, input.len(), previous)?;
                let Some(found) = matches.next() else {
                    break;
                };
                out.push(input.get(previous..found.start()).ok_or(TextError::InvalidPattern)?)?;
                previous = found.end();
            }
            out.push(input.get(previous..).ok_or(TextError::InvalidPattern)?)?;
        }
        SplitMode::Paragraphs => {
            // Blank lines contain only Unicode whitespace; paragraph separators are discarded.
            let mut start = None;
            let mut end = 0usize;
            let mut offset = 0usize;
            for line in input.split_inclusive('\n') {
                if line.trim().is_empty() {
                    if let Some(begin) = start.take() {
                        out.push(input.get(begin..end).ok_or(TextError::InvalidCount)?)?;
                    }
                } else {
                    if start.is_none() {
                        start = Some(offset);
                    }
                    end = offset.saturating_add(line.trim_end_matches(['\r', '\n']).len());
                }
                offset = offset.saturating_add(line.len());
            }
            if let Some(begin) = start {
                out.push(input.get(begin..end).ok_or(TextError::InvalidCount)?)?;
            }
        }
        SplitMode::Lines(n) | SplitMode::Chars(n) => {
            if *n == 0 {
                return Err(TextError::InvalidCount);
            }
            let mut start = 0;
            let mut count = 0usize;
            if matches!(mode, SplitMode::Lines(_)) {
                let mut end = 0usize;
                count = 0;
                for line in input.split_inclusive('\n') {
                    end = end.saturating_add(line.len());
                    count = count.saturating_add(1);
                    if count == *n {
                        out.push(input.get(start..end).ok_or(TextError::InvalidCount)?)?;
                        start = end;
                        count = 0;
                    }
                }
            } else {
                for (index, ch) in input.char_indices() {
                    count = count.saturating_add(1);
                    if count == *n {
                        let end = index.saturating_add(ch.len_utf8());
                        out.push(input.get(start..end).ok_or(TextError::InvalidCount)?)?;
                        start = end;
                        count = 0;
                    }
                }
            }
            if start < input.len() {
                out.push(input.get(start..).ok_or(TextError::InvalidCount)?)?;
            }
        }
    }
    Ok(out.values)
}
/// Literal replacement never interprets `$`; regex replacement accepts `$1`, `${name}`, `$name`, `$$`.
pub fn replace(input: &str, options: &ReplaceOptions, limits: Limits) -> Result<String, TextError> {
    let limits = limits.validate()?;
    let (pattern, captures) = match &options.pattern {
        MatchMode::Literal(s) => (s.as_str(), false),
        MatchMode::Regex(s) => (s.as_str(), true),
    };
    limits.input([input, pattern, &options.replacement])?;
    if !captures && pattern.is_empty() {
        return Err(TextError::EmptySeparator);
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(TextError::PatternLimit);
    }
    let escaped = if captures { pattern.to_owned() } else { regex::escape(pattern) };
    let re = compile(&escaped, options.case_sensitive, options.whole_word)?;
    let mut out = String::new();
    let mut previous = 0usize;
    let mut matches = re.captures_iter(input);
    let mut index = 0usize;
    let mut search_work = 0usize;
    let mut replacement_work = 0usize;
    loop {
        // A fixed escaped literal has no greedy suffix alternatives; ordinary literal
        // replacement remains governed by input, output and match budgets.
        if captures {
            search_charge(&mut search_work, input.len(), previous)?;
        }
        let Some(caps) = matches.next() else {
            break;
        };
        if index >= limits.items {
            return Err(TextError::ItemLimit);
        }
        index = index.saturating_add(1);
        if captures {
            charge(&mut replacement_work, options.replacement.len(), MAX_REPLACEMENT_SCAN_BYTES)?;
        }
        let found = caps.get(0).ok_or(TextError::InvalidPattern)?;
        append(&mut out, input.get(previous..found.start()).ok_or(TextError::InvalidPattern)?, limits)?;
        if captures {
            expand(&mut out, &options.replacement, &caps, limits)?;
        } else {
            append(&mut out, &options.replacement, limits)?;
        }
        previous = found.end();
    }
    append(&mut out, input.get(previous..).ok_or(TextError::InvalidPattern)?, limits)?;
    Ok(out)
}
fn expand(out: &mut String, replacement: &str, caps: &Captures<'_>, limits: Limits) -> Result<(), TextError> {
    let mut rest = replacement;
    while let Some(pos) = rest.find('$') {
        append(out, rest.get(..pos).ok_or(TextError::InvalidPattern)?, limits)?;
        rest = rest.get(pos.saturating_add(1)..).ok_or(TextError::InvalidPattern)?;
        if let Some(next) = rest.strip_prefix('$') {
            append(out, "$", limits)?;
            rest = next;
            continue;
        }
        let (name, next) = if let Some(inner) = rest.strip_prefix('{') {
            let end = inner.find('}').ok_or(TextError::InvalidPattern)?;
            (inner.get(..end).ok_or(TextError::InvalidPattern)?, inner.get(end.saturating_add(1)..).ok_or(TextError::InvalidPattern)?)
        } else {
            let end = rest.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(rest.len());
            (rest.get(..end).ok_or(TextError::InvalidPattern)?, rest.get(end..).ok_or(TextError::InvalidPattern)?)
        };
        if name.is_empty() {
            append(out, "$", limits)?;
        } else {
            let value = if name.bytes().all(|c| c.is_ascii_digit()) { name.parse::<usize>().ok().and_then(|i| caps.get(i)) } else { caps.name(name) };
            if let Some(value) = value {
                append(out, value.as_str(), limits)?;
            }
        }
        rest = next;
    }
    append(out, rest, limits)
}
pub fn concat(inputs: &[String], options: &ConcatOptions, limits: Limits) -> Result<String, TextError> {
    let limits = limits.validate()?;
    if inputs.len() > limits.items {
        return Err(TextError::ItemLimit);
    }
    limits.input(inputs.iter().map(String::as_str).chain([options.separator.as_str(), &options.prefix, &options.suffix]))?;
    let mut out = String::new();
    append(&mut out, &options.prefix, limits)?;
    let mut first = true;
    for value in inputs {
        let value = if options.trim { value.trim() } else { value.as_str() };
        if options.skip_empty && value.is_empty() {
            continue;
        }
        if !first {
            append(&mut out, &options.separator, limits)?;
        }
        append(&mut out, value, limits)?;
        first = false;
    }
    append(&mut out, &options.suffix, limits)?;
    Ok(out)
}
/// `{{name}}` inserts a field once; `{{{{` / `}}}}` escape literal double braces.
pub fn fill_template(template: &str, fields: &BTreeMap<String, String>, limits: Limits) -> Result<String, TextError> {
    let limits = limits.validate()?;
    if fields.len() > MAX_FIELDS {
        return Err(TextError::FieldLimit);
    }
    limits.input(std::iter::once(template).chain(fields.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()])))?;
    let mut out = String::new();
    let mut rest = template;
    let mut count = 0usize;
    while !rest.is_empty() {
        if let Some(next) = rest.strip_prefix("{{{{") {
            append(&mut out, "{{", limits)?;
            rest = next;
        } else if let Some(next) = rest.strip_prefix("}}}}") {
            append(&mut out, "}}", limits)?;
            rest = next;
        } else if let Some(inner) = rest.strip_prefix("{{") {
            count = count.saturating_add(1);
            if count > limits.items.min(MAX_FIELDS) {
                return Err(TextError::FieldLimit);
            }
            let end = inner.find("}}").ok_or(TextError::InvalidTemplate)?;
            let key = inner.get(..end).ok_or(TextError::InvalidTemplate)?;
            if key.is_empty() || key.chars().any(|c| c == '{' || c == '}') {
                return Err(TextError::InvalidTemplate);
            }
            append(&mut out, fields.get(key).ok_or(TextError::MissingField)?, limits)?;
            rest = inner.get(end.saturating_add(2)..).ok_or(TextError::InvalidTemplate)?;
        } else if rest.starts_with("}}") {
            return Err(TextError::InvalidTemplate);
        } else {
            let end = rest.char_indices().skip(1).find(|(_, c)| *c == '{' || *c == '}').map(|(i, _)| i).unwrap_or(rest.len());
            append(&mut out, rest.get(..end).ok_or(TextError::InvalidTemplate)?, limits)?;
            rest = rest.get(end..).ok_or(TextError::InvalidTemplate)?;
        }
    }
    Ok(out)
}
