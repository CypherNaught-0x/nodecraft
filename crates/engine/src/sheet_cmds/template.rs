//! Prompt templates for Batch Generate sheets (#163): `{{name}}` in a prompt stands for a named
//! variable's value (`docs/decisions/2026-10-28-sheet-templates.md`).
//!
//! The syntax is the Template text action's: `{{name}}` with optional spaces inside the braces,
//! `{{{{` for a literal `{{` and `}}}}` for a literal `}}`. A `}}` outside a placeholder is
//! refused, so a typo never silently becomes text. Values are inserted as they are and never read
//! as template text again. `@[Title](id)` card mentions and `#element` mentions hold no double
//! braces, so they pass through unchanged.
use nodecraft_types::{Error, ErrorKind};
use std::collections::BTreeMap;

/// Longest template, in bytes.
pub const MAX_TEMPLATE_BYTES: usize = 16 * 1024;
/// Most placeholders one template holds, repeats included.
pub const MAX_PLACEHOLDERS: usize = 64;
/// Longest variable name, in bytes.
pub const MAX_NAME_BYTES: usize = 32;

fn bad(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// `text` cut to 40 characters, for an error message.
pub fn clip(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(40).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// One part of a parsed template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Variable(String),
}

/// Whether `name` can name a variable: 1 to 32 ASCII letters, digits, `_` or `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_BYTES && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn push_text(pieces: &mut Vec<Piece>, text: &str) {
    if let Some(Piece::Text(last)) = pieces.last_mut() {
        last.push_str(text);
    } else if !text.is_empty() {
        pieces.push(Piece::Text(text.to_owned()));
    }
}

/// `template` split into text and placeholders, with the escapes applied.
pub fn parse(template: &str) -> Result<Vec<Piece>, Error> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(bad(format!("a template holds at most {MAX_TEMPLATE_BYTES} bytes")));
    }
    let mut pieces = Vec::new();
    let mut rest = template;
    let mut placeholders = 0usize;
    while !rest.is_empty() {
        if let Some(next) = rest.strip_prefix("{{{{") {
            push_text(&mut pieces, "{{");
            rest = next;
        } else if let Some(next) = rest.strip_prefix("}}}}") {
            push_text(&mut pieces, "}}");
            rest = next;
        } else if let Some(inner) = rest.strip_prefix("{{") {
            let end = inner.find("}}").ok_or_else(|| bad("a `{{` has no closing `}}`; write `{{{{` for a literal `{{`"))?;
            let name = inner.get(..end).unwrap_or_default().trim();
            if !valid_name(name) {
                return Err(bad(format!(
                    "`{{{{{}}}}}` isn't a variable: names are 1 to {MAX_NAME_BYTES} letters, digits, `_` or `-`; write `{{{{{{{{` for a literal `{{{{`",
                    clip(name)
                )));
            }
            placeholders = placeholders.saturating_add(1);
            if placeholders > MAX_PLACEHOLDERS {
                return Err(bad(format!("a template holds at most {MAX_PLACEHOLDERS} placeholders")));
            }
            pieces.push(Piece::Variable(name.to_owned()));
            rest = inner.get(end.saturating_add(2)..).unwrap_or_default();
        } else if rest.starts_with("}}") {
            return Err(bad("a `}}` closes no placeholder; write `}}}}` for a literal `}}`"));
        } else {
            let end = rest.char_indices().skip(1).find(|(_, c)| *c == '{' || *c == '}').map_or(rest.len(), |(i, _)| i);
            push_text(&mut pieces, rest.get(..end).unwrap_or_default());
            rest = rest.get(end..).unwrap_or_default();
        }
    }
    Ok(pieces)
}

/// The placeholder names `pieces` use, each once, in first-use order.
pub fn names(pieces: &[Piece]) -> Vec<&str> {
    let mut seen = Vec::new();
    for piece in pieces {
        if let Piece::Variable(name) = piece
            && !seen.contains(&name.as_str())
        {
            seen.push(name.as_str());
        }
    }
    seen
}

/// The prompt `pieces` make with `values` (every placeholder must have one; the caller checks).
pub fn fill(pieces: &[Piece], values: &BTreeMap<&str, &str>) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Variable(name) => out.push_str(values.get(name.as_str()).copied().unwrap_or_default()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(template: &str, values: &[(&str, &str)]) -> String {
        fill(&parse(template).unwrap(), &values.iter().copied().collect())
    }

    #[test]
    fn placeholders_are_filled_and_escapes_are_literal() {
        assert_eq!(text("a {{colour}} {{ thing }}", &[("colour", "red"), ("thing", "mug")]), "a red mug");
        assert_eq!(text("{{{{not}}}} {{x}}", &[("x", "1")]), "{{not}} 1");
        assert_eq!(text("{{x}}{{x}}", &[("x", "ab")]), "abab", "a repeat inserts the value again");
        assert_eq!(text("{{x}}", &[("x", "{{y}}")]), "{{y}}", "a value is never read as a template");
        assert_eq!(text("single { and } braces", &[]), "single { and } braces");
        assert_eq!(text("@[Fox](00000000-0000-0000-0000-000000000000) #style {{x}}", &[("x", "")]), "@[Fox](00000000-0000-0000-0000-000000000000) #style ");
        assert_eq!(names(&parse("{{b}} {{a}} {{b}}").unwrap()), ["b", "a"]);
    }

    #[test]
    fn malformed_templates_are_refused_with_the_fix() {
        for (template, says) in [
            ("a {{colour", "no closing"),
            ("a }} b", "closes no placeholder"),
            ("{{}}", "isn't a variable"),
            ("{{two words}}", "isn't a variable"),
            ("{{ünï}}", "isn't a variable"),
        ] {
            let error = parse(template).unwrap_err();
            assert!(error.message.contains(says), "{template}: {}", error.message);
        }
        assert!(parse(&"{{x}}".repeat(MAX_PLACEHOLDERS)).is_ok());
        assert!(parse(&"{{x}}".repeat(MAX_PLACEHOLDERS + 1)).is_err());
        assert!(parse(&"a".repeat(MAX_TEMPLATE_BYTES + 1)).is_err());
        assert!(parse(&format!("{{{{{}}}}}", "n".repeat(MAX_NAME_BYTES + 1))).is_err());
        // Multibyte text next to braces never splits a character.
        assert_eq!(text("é{{x}}ü}}}}", &[("x", "ø")]), "éøü}}");
    }
}
