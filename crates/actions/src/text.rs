//! Pure text actions. See `docs/text-actions.md` for wire and boundary semantics.
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ITEMS: usize = 16_384;
pub const MAX_FIELDS: usize = 1024;
pub const MAX_PATTERN_BYTES: usize = 8192;
pub const MAX_SEARCH_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REPLACEMENT_STEPS: usize = 1024 * 1024;

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
    InvalidMaxParts,
    EmptySeparator,
    PatternLimit,
    WorkLimit,
    InvalidPattern,
    InvalidReplacement,
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
            Self::InvalidMaxParts => "text split maximum parts must be between 1 and 16384",
            Self::EmptySeparator => "text separator must not be empty",
            Self::WorkLimit => "text search or replacement work budget exceeded",
            Self::PatternLimit => "regex pattern byte budget exceeded",
            Self::InvalidReplacement => "regex replacement syntax is invalid",
            Self::InvalidPattern => "regex pattern is invalid or exceeds compilation limits",
            Self::InvalidTemplate => "template syntax is invalid",
            Self::MissingField => "template field is missing",
            Self::FieldLimit => "template field budget exceeded",
        })
    }
}
impl std::error::Error for TextError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum SplitMode {
    Separator(String),
    Regex(String),
    Paragraphs,
    Lines(usize),
    Chars(usize),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case", deny_unknown_fields)]
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
    Split {
        input: String,
        mode: SplitMode,
        /// Persisted output cardinality ceiling; old requests default to the hard ceiling.
        #[serde(default = "default_max_parts")]
        max_parts: usize,
    },
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
fn default_max_parts() -> usize {
    MAX_ITEMS
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
            Self::Split { input, mode, max_parts } => {
                let limits = limits.validate()?;
                if !(1..=MAX_ITEMS).contains(max_parts) {
                    return Err(TextError::InvalidMaxParts);
                }
                split(input, mode, Limits { items: limits.items.min(*max_parts), ..limits }).map(TextResult::Parts)
            }
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
// Keep full-haystack iterators so anchors and adjacent-empty suppression retain regex semantics.
// Finite-width searches can inspect at most B bytes past a candidate start, plus UTF-8
// assertion context. Total forward advancement is charged once, not once per suffix.
struct SearchBudget {
    used: usize,
    lookahead: Option<usize>,
}
impl SearchBudget {
    fn new(pattern: &str, case_sensitive: bool, input_len: usize) -> Result<Self, TextError> {
        let hir = regex_syntax::ParserBuilder::new()
            .nest_limit(64)
            .case_insensitive(!case_sensitive)
            .build()
            .parse(pattern)
            .map_err(|_| TextError::InvalidPattern)?;
        let bound = hir.properties().maximum_len().map(|width| (width, 1)).or_else(|| sequence_bound(&hir));
        let lookahead = bound.map(|(width, _)| width);
        let mut budget = Self { used: 0, lookahead };
        if lookahead.is_some() {
            charge(&mut budget.used, input_len.saturating_add(1).saturating_mul(2).saturating_mul(bound.map_or(1, |(_, scans)| scans)), MAX_SEARCH_BYTES)?;
        }
        Ok(budget)
    }
    fn next(&mut self, input_len: usize, previous: usize) -> Result<(), TextError> {
        let bytes = match self.lookahead {
            Some(width) => width.saturating_add(8).saturating_mul(2),
            None => input_len.saturating_sub(previous).saturating_add(1).saturating_mul(2),
        };
        charge(&mut self.used, bytes, MAX_SEARCH_BYTES)
    }
}
fn uncapture(mut hir: &regex_syntax::hir::Hir) -> &regex_syntax::hir::Hir {
    while let regex_syntax::hir::HirKind::Capture(cap) = hir.kind() {
        hir = &cap.sub;
    }
    hir
}
// Flat greedy runs only: each pair needs a mandatory delimiter outside both alphabets.
// A run cannot cross that delimiter, so a later run cannot make the preceding run rescan
// its input on another iterator call. Fixed prefixes/suffixes add only their bounded width.
fn sequence_bound(hir: &regex_syntax::hir::Hir) -> Option<(usize, usize)> {
    // Preserve the existing lazy single-atom shortcut: a lazy run returns its
    // minimum immediately, or advances monotonically while searching for that minimum.
    if let regex_syntax::hir::HirKind::Repetition(rep) = uncapture(hir).kind()
        && !rep.greedy
    {
        match uncapture(&rep.sub).kind() {
            regex_syntax::hir::HirKind::Class(_) => return Some((4, 1)),
            regex_syntax::hir::HirKind::Literal(lit) if std::str::from_utf8(&lit.0).ok().is_some_and(|s| s.chars().count() == 1) => return Some((4, 1)),
            _ => {}
        }
    }
    struct Atom {
        ranges: Vec<(u32, u32)>,
        repeated: bool,
        assertion: bool,
    }
    fn flatten(hir: &regex_syntax::hir::Hir, out: &mut Vec<Atom>) -> Option<()> {
        use regex_syntax::hir::{Class, HirKind};
        match uncapture(hir).kind() {
            HirKind::Empty => {}
            HirKind::Look(_) => out.push(Atom { ranges: Vec::new(), repeated: false, assertion: true }),
            HirKind::Concat(parts) => {
                for part in parts {
                    flatten(part, out)?;
                }
            }
            HirKind::Literal(lit) => {
                for ch in std::str::from_utf8(&lit.0).ok()?.chars() {
                    out.push(Atom { ranges: vec![(u32::from(ch), u32::from(ch))], repeated: false, assertion: false });
                }
            }
            HirKind::Class(class) => {
                let ranges = match class {
                    Class::Unicode(c) => c.ranges().iter().map(|r| (u32::from(r.start()), u32::from(r.end()))).collect(),
                    Class::Bytes(c) => c.ranges().iter().map(|r| (u32::from(r.start()), u32::from(r.end()))).collect(),
                };
                out.push(Atom { ranges, repeated: false, assertion: false });
            }
            HirKind::Repetition(rep) if rep.max.is_none() && rep.greedy => {
                let before = out.len();
                flatten(&rep.sub, out)?;
                if out.len() != before.checked_add(1)? {
                    return None;
                }
                let atom = out.get_mut(before)?;
                if atom.repeated || atom.assertion {
                    return None;
                }
                atom.repeated = true;
            }
            _ => return None,
        }
        Some(())
    }
    fn disjoint(a: &Atom, b: &Atom) -> bool {
        // HIR ranges are sorted. Merge-scan instead of a cross product for large
        // Unicode classes; classification itself must remain bounded by syntax size.
        let mut left = a.ranges.iter();
        let mut right = b.ranges.iter();
        let mut x = left.next();
        let mut y = right.next();
        while let (Some((a0, a1)), Some((b0, b1))) = (x, y) {
            if a1 < b0 {
                x = left.next();
            } else if b1 < a0 {
                y = right.next();
            } else {
                return false;
            }
        }
        true
    }
    let mut atoms = Vec::new();
    flatten(hir, &mut atoms)?;
    let mut previous: Option<usize> = None;
    for (index, atom) in atoms.iter().enumerate().filter(|(_, atom)| atom.repeated) {
        if let Some(prior) = previous {
            let prior_atom = atoms.get(prior)?;
            if !atoms
                .get(prior.saturating_add(1)..index)?
                .iter()
                .any(|delimiter| !delimiter.assertion && disjoint(prior_atom, delimiter) && disjoint(atom, delimiter))
            {
                return None;
            }
        }
        previous = Some(index);
    }
    // Assertions are predicates of the original haystack position, independent of a
    // search's start. A rejected suffix position stays rejected on the next call;
    // greedy runs choose the last admissible suffix, so a long rejected tail cannot
    // yield a succession of earlier short successes. The final failed search and
    // adjacent-empty suppression are covered by the factor two in SearchBudget.
    // Each assertion examines at most the neighboring Unicode scalars (8 bytes),
    // including CRLF/multiline context. It contributes work but never consumes a
    // delimiter or makes a repeated assertion/overlapping pair admissible.
    let assertions = atoms.iter().filter(|atom| atom.assertion).count();
    let consuming = atoms.len().checked_sub(assertions)?;
    let context = consuming.checked_mul(4)?.checked_add(assertions.checked_mul(8)?)?;
    Some((context, atoms.len().checked_add(1)?))
}
fn compile(pattern: &str, case_sensitive: bool) -> Result<Regex, TextError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(TextError::PatternLimit);
    }
    RegexBuilder::new(pattern)
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
            let re = compile(s, true)?;
            let mut matches = re.find_iter(input);
            let mut previous = 0usize;
            let mut work = SearchBudget::new(s, true, input.len())?;
            loop {
                work.next(input.len(), previous)?;
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
    // Half-boundaries constrain neighbors without requiring a word-valued edge.
    // Keep assertions inside the regex so valid alternative matches are still tried.
    let effective = if options.whole_word { format!(r"\b{{start-half}}(?:{escaped})\b{{end-half}}") } else { escaped };
    let re = compile(&effective, options.case_sensitive)?;
    let replacement = if captures { Some(parse_replacement(&options.replacement, &re)?) } else { None };
    let mut search = if captures { Some(SearchBudget::new(&effective, options.case_sensitive, input.len())?) } else { None };
    let mut out = String::new();
    let mut previous = 0usize;
    let mut searched = 0usize;
    let mut matches = re.captures_iter(input);
    let mut replacement_work = 0usize;
    loop {
        if let Some(search) = &mut search {
            search.next(input.len(), searched)?;
        }
        let Some(caps) = matches.next() else {
            break;
        };
        let found = caps.get(0).ok_or(TextError::InvalidPattern)?;
        searched = found.end();
        append(&mut out, input.get(previous..found.start()).ok_or(TextError::InvalidPattern)?, limits)?;
        if let Some(tokens) = &replacement {
            for token in tokens {
                charge(&mut replacement_work, 1, MAX_REPLACEMENT_STEPS)?;
                match token {
                    Replacement::Text(value) => append(&mut out, value, limits)?,
                    Replacement::Capture(index) => {
                        if let Some(value) = caps.get(*index) {
                            append(&mut out, value.as_str(), limits)?;
                        }
                    }
                }
            }
        } else {
            append(&mut out, &options.replacement, limits)?;
        }
        previous = found.end();
    }
    append(&mut out, input.get(previous..).ok_or(TextError::InvalidPattern)?, limits)?;
    Ok(out)
}
enum Replacement<'a> {
    Text(&'a str),
    Capture(usize),
}
fn replacement_push<'a>(tokens: &mut Vec<Replacement<'a>>, token: Replacement<'a>) -> Result<(), TextError> {
    if tokens.len() >= MAX_ITEMS {
        return Err(TextError::WorkLimit);
    }
    tokens.push(token);
    Ok(())
}
fn parse_replacement<'a>(replacement: &'a str, re: &Regex) -> Result<Vec<Replacement<'a>>, TextError> {
    let names: BTreeMap<_, _> = re.capture_names().enumerate().filter_map(|(i, name)| name.map(|name| (name, i))).collect();
    let mut tokens = Vec::new();
    let mut rest = replacement;
    while let Some(pos) = rest.find('$') {
        if pos > 0 {
            replacement_push(&mut tokens, Replacement::Text(rest.get(..pos).ok_or(TextError::InvalidReplacement)?))?;
        }
        rest = rest.get(pos.saturating_add(1)..).ok_or(TextError::InvalidReplacement)?;
        if let Some(next) = rest.strip_prefix('$') {
            replacement_push(&mut tokens, Replacement::Text("$"))?;
            rest = next;
            continue;
        }
        let (name, next) = if let Some(inner) = rest.strip_prefix('{') {
            let end = inner.find('}').ok_or(TextError::InvalidReplacement)?;
            let name = inner.get(..end).ok_or(TextError::InvalidReplacement)?;
            if name.is_empty() || name.contains('{') {
                return Err(TextError::InvalidReplacement);
            }
            (name, inner.get(end.saturating_add(1)..).ok_or(TextError::InvalidReplacement)?)
        } else {
            let end = rest.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(rest.len());
            (rest.get(..end).ok_or(TextError::InvalidReplacement)?, rest.get(end..).ok_or(TextError::InvalidReplacement)?)
        };
        if name.is_empty() {
            replacement_push(&mut tokens, Replacement::Text("$"))?;
        } else {
            let index =
                if name.bytes().all(|c| c.is_ascii_digit()) { name.parse::<usize>().ok().filter(|i| *i < re.captures_len()) } else { names.get(name).copied() };
            // Unknown references resolve to empty once, not repeatedly for every match.
            if let Some(index) = index {
                replacement_push(&mut tokens, Replacement::Capture(index))?;
            }
        }
        rest = next;
    }
    if !rest.is_empty() {
        replacement_push(&mut tokens, Replacement::Text(rest))?;
    }
    Ok(tokens)
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

#[cfg(test)]
mod search_budget_tests {
    use super::*;

    #[test]
    fn sequence_policy_uses_folded_unicode_alphabets_and_rejects_unproved_shapes() {
        for (pattern, sensitive, admitted) in [
            (r"\p{L}*,\p{L}*", true, true),
            (r"\p{L}*é\p{L}*", true, false),
            ("A*aA*", true, true),
            ("A*aA*", false, false),
            ("K*kK*", true, true),
            ("K*kK*", false, false),
            ("a*a*", true, false),
            ("a*b*", true, false),
            ("(a+)+", true, false),
            ("(?:ab)+", true, false),
            ("a{1,2}b+", true, false),
            (r"a+\b", true, true),
            (r"a+\ba+", true, false),
            (r"a+\Bb+", true, false),
            (r"(?:a+\b)+", true, false),
            (r"(?:a+)+\b", true, false),
            (r"A*\baA*", false, false),
            (r"K*\bkK*", false, false),
            (r"\p{L}*\bé\p{L}*", true, false),
            (r"\p{L}*\b,\b\p{L}*", true, true),
            (r"\b{start-half}a+\b{end-half}", true, true),
            (r"\b{start-half}a+?\b{end-half}", true, false),
            ("a+?b", true, false),
            ("(a+?)", true, true),
        ] {
            let budget = SearchBudget::new(pattern, sensitive, 100).unwrap();
            assert_eq!(budget.lookahead.is_some(), admitted, "{pattern}, case_sensitive={sensitive}");
        }
    }

    #[test]
    fn assertion_search_work_accepts_exact_ceiling_then_rejects_next_step() {
        let mut budget = SearchBudget::new(r"\ba+\b", true, 100).unwrap();
        let step = 56;
        budget.used = MAX_SEARCH_BYTES - step;
        assert_eq!(budget.next(100, 0), Ok(()));
        assert_eq!(budget.used, MAX_SEARCH_BYTES);
        assert_eq!(budget.next(100, 0), Err(TextError::WorkLimit));
    }

    #[test]
    fn documented_precharge_and_per_search_formulas_match_exactly() {
        for (pattern, initial, step) in [("a", 22, 18), ("a+?", 22, 24), ("a+", 44, 24), (r"\s*,\s*", 88, 40), ("a*a*", 0, 22), (r"\ba+\b", 88, 56)] {
            let mut budget = SearchBudget::new(pattern, true, 10).unwrap();
            assert_eq!(budget.used, initial, "{pattern}");
            budget.next(10, 0).unwrap();
            assert_eq!(budget.used, initial + step, "{pattern}");
        }
    }
}
