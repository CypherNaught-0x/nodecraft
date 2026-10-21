//! `@node` mentions in generation prompts (#505). A mention is the token `@[Title](node id)`
//! typed into a generation card's prompt: the card's title for people, the node id so renaming
//! or retitling the mentioned card never breaks it. At plan time each mention is replaced by the
//! mentioned card's text output, fed in through [`PORT`] like a wire (crates/exec/src/node_mentions.rs).
//!
//! The UI shows a token as `@Title`; agents and files write the token itself.
use nodecraft_types::{Error, ErrorKind, NodeId};
use std::borrow::Cow;
use std::collections::BTreeMap;

/// The input the planner feeds mentioned cards' text into, each value's origin the card it came
/// from. It is never a port of the node: nothing can be wired to it.
pub const PORT: &str = "@mentions";
/// Distinct cards one prompt can mention.
pub const MAX_MENTIONS: usize = 16;
/// Longest title a token keeps, in characters.
pub const MAX_TITLE_CHARS: usize = 80;
/// Tokens one prompt is scanned for, repeats included.
pub const MAX_TOKENS: usize = 256;
/// Longest prompt substitution makes: a generation prompt's own limit.
pub const MAX_PROMPT_BYTES: usize = 1024 * 1024;
/// A node id's text form: a hyphenated UUID.
const ID_LEN: usize = 36;

/// One `@[Title](id)` in a prompt: its byte range, the title as written and the node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub start: usize,
    pub end: usize,
    pub title: &'a str,
    pub id: NodeId,
}

/// The token for `id`, titled `title` with the characters a token can't hold left out.
pub fn token(title: &str, id: NodeId) -> String {
    let title: String = title.chars().filter(|c| !matches!(c, '[' | ']' | '(' | ')') && !c.is_control()).take(MAX_TITLE_CHARS).collect();
    let title = title.trim();
    format!("@[{}]({id})", if title.is_empty() { "card" } else { title })
}

/// The mention tokens in `text`, in order, repeats included (at most [`MAX_TOKENS`]).
pub fn tokens(text: &str) -> Vec<Token<'_>> {
    scan(text, MAX_TOKENS)
}

/// Up to `limit` tokens of `text`.
fn scan(text: &str, limit: usize) -> Vec<Token<'_>> {
    let mut found = Vec::new();
    let mut from = 0;
    while found.len() < limit {
        let Some(at) = text.get(from..).and_then(|rest| rest.find("@[")).map(|at| at.saturating_add(from)) else { break };
        from = at.saturating_add(2);
        let Some(token) = parse(text, at) else { continue };
        from = token.end;
        found.push(token);
    }
    found
}

/// Whether `text` mentions more than [`MAX_MENTIONS`] cards or holds more than [`MAX_TOKENS`]
/// tokens: those past the limit would feed nothing.
pub fn too_many(text: &str) -> bool {
    let found = scan(text, MAX_TOKENS.saturating_add(1));
    found.len() > MAX_TOKENS || {
        let mut ids: Vec<NodeId> = found.iter().map(|token| token.id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len() > MAX_MENTIONS
    }
}

/// How many `@[` in `text` start no token: a mention edited by hand into plain text.
pub fn broken(text: &str) -> usize {
    let tokens = tokens(text);
    text.match_indices("@[").take(MAX_TOKENS.saturating_mul(2)).filter(|(at, _)| !tokens.iter().any(|token| token.start == *at)).count()
}

/// The token starting at byte `at` (on `@[`), if one does.
fn parse(text: &str, at: usize) -> Option<Token<'_>> {
    let title_start = at.checked_add(2)?;
    let rest = text.get(title_start..)?;
    // A title is short and on one line; stop looking well before the prompt ends.
    let close = rest.char_indices().take(MAX_TITLE_CHARS.saturating_add(1)).find(|(_, c)| matches!(c, ']' | '[' | '\n')).filter(|(_, c)| *c == ']')?.0;
    let title = rest.get(..close)?;
    let id_start = title_start.checked_add(close)?.checked_add(2)?;
    if text.get(title_start.checked_add(close)?..id_start)? != "](" {
        return None;
    }
    let id_end = id_start.checked_add(ID_LEN)?;
    let id = text.get(id_start..id_end)?.parse().ok()?;
    let end = id_end.checked_add(1)?;
    (text.get(id_end..end)? == ")").then_some(Token { start: at, end, title, id })
}

/// The distinct cards `text` mentions, in order of first mention.
pub fn ids(text: &str) -> Vec<NodeId> {
    let mut ids = Vec::new();
    for token in tokens(text) {
        if !ids.contains(&token.id) {
            if ids.len() >= MAX_MENTIONS {
                break;
            }
            ids.push(token.id);
        }
    }
    ids
}

/// `text` with each token shown as `@Title`.
pub fn display(text: &str) -> Cow<'_, str> {
    let tokens = tokens(text);
    if tokens.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for token in tokens {
        out.push_str(text.get(at..token.start).unwrap_or_default());
        out.push('@');
        out.push_str(token.title);
        at = token.end;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    Cow::Owned(out)
}

/// The first `chars` characters of [`display`]`(text)`, without showing the rest: what a card
/// title or preview draws every frame, however long the prompt.
pub fn display_start(text: &str, chars: usize) -> String {
    let mut out = String::new();
    let mut left = chars;
    let take = |out: &mut String, piece: &str, left: &mut usize| {
        let end = piece.char_indices().nth(*left).map_or(piece.len(), |(at, _)| at);
        let piece = piece.get(..end).unwrap_or_default();
        *left = left.saturating_sub(piece.chars().count());
        out.push_str(piece);
    };
    let (mut at, mut from) = (0, 0);
    while left > 0 {
        let Some(start) = text.get(from..).and_then(|rest| rest.find("@[")).map(|start| start.saturating_add(from)) else { break };
        from = start.saturating_add(2);
        let Some(token) = parse(text, start) else { continue };
        take(&mut out, text.get(at..token.start).unwrap_or_default(), &mut left);
        take(&mut out, "@", &mut left);
        take(&mut out, token.title, &mut left);
        at = token.end;
        from = token.end;
    }
    take(&mut out, text.get(at..).unwrap_or_default(), &mut left);
    out
}

/// `text` with each token replaced by the text of the card it mentions, from `values`. A card
/// past [`MAX_MENTIONS`] isn't fed: its token stays as written. The result is at most
/// [`MAX_PROMPT_BYTES`].
pub fn substitute(text: &str, values: &BTreeMap<NodeId, &str>) -> Result<String, Error> {
    let fed = ids(text);
    let mut out = String::with_capacity(text.len().min(MAX_PROMPT_BYTES));
    let push = |out: &mut String, piece: &str| {
        if out.len().saturating_add(piece.len()) > MAX_PROMPT_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("the prompt with its mentioned cards' text is longer than {} MiB; mention shorter texts", MAX_PROMPT_BYTES / (1024 * 1024)),
            ));
        }
        out.push_str(piece);
        Ok(())
    };
    let mut at = 0;
    for token in tokens(text) {
        push(&mut out, text.get(at..token.start).unwrap_or_default())?;
        match values.get(&token.id) {
            Some(value) => push(&mut out, value)?,
            None if fed.contains(&token.id) => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("the prompt mentions @{}, but no text from it arrived; plan the card again", token.title),
                ));
            }
            None => push(&mut out, text.get(token.start..token.end).unwrap_or_default())?,
        }
        at = token.end;
    }
    push(&mut out, text.get(at..).unwrap_or_default())?;
    Ok(out)
}

#[cfg(test)]
mod tests;
