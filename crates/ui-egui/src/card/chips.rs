//! `@` mentions as chips while a prompt is edited (#579). The field keeps the stored token
//! (`@[Title](id)`, `nodecraft_nodes::mention`) as its text, so undo and the document see the
//! token, but draws only `@Title` on a chip: the token's other characters are laid out with no
//! width and no colour. The caret never rests inside a token, a selection reaching into one
//! takes all of it, and an edit that touches part of a token removes the whole token.
use super::*;
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use nodecraft_nodes::mention;
use std::sync::Arc;

/// Font size the hidden part of a token is laid out at: no visible width.
const HIDDEN_SIZE: f32 = 0.001;

/// The char ranges of `text`'s tokens, with the char index where each title ends.
struct Span {
    start: usize,
    end: usize,
    title_end: usize,
}

fn spans(text: &str) -> Vec<Span> {
    // Counted on from the previous token, so a long prompt is walked once per frame.
    let (mut byte, mut chars) = (0, 0usize);
    let mut chars_to = |to: usize| {
        chars = chars.saturating_add(text.get(byte..to).map_or(0, |between| between.chars().count()));
        byte = to;
        chars
    };
    mention::tokens(text)
        .iter()
        .map(|token| {
            let start = chars_to(token.start);
            Span { start, end: chars_to(token.end), title_end: start.saturating_add(2).saturating_add(token.title.chars().count()) }
        })
        .collect()
}

/// Lays `text` out as the field would, its tokens as chips.
pub(super) fn layout(ui: &egui::Ui, text: &str, wrap_width: f32, t: &Tokens) -> Arc<egui::Galley> {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().override_text_color.unwrap_or_else(|| ui.visuals().widgets.inactive.text_color());
    let line_height = ui.fonts_mut(|f| f.row_height(&font)) + ui.spacing().extra_text_line_spacing;
    let plain = TextFormat { font_id: font.clone(), color, line_height: Some(line_height), ..Default::default() };
    let chip = TextFormat { background: t.mention_fill, color: t.text, ..plain.clone() };
    let hidden = TextFormat { font_id: egui::FontId::new(HIDDEN_SIZE, font.family.clone()), color: egui::Color32::TRANSPARENT, ..plain.clone() };
    let mut job = LayoutJob { wrap: egui::text::TextWrapping { max_width: wrap_width, ..Default::default() }, ..Default::default() };
    job.keep_trailing_whitespace = true;
    let mut at = 0;
    let piece = |job: &mut LayoutJob, from: usize, to: usize, format: &TextFormat| {
        if let Some(piece) = text.get(from..to).filter(|piece| !piece.is_empty()) {
            job.append(piece, 0.0, format.clone());
        }
    };
    for token in mention::tokens(text) {
        let title_start = token.start.saturating_add(2);
        let title_end = title_start.saturating_add(token.title.len());
        piece(&mut job, at, token.start, &plain);
        piece(&mut job, token.start, token.start.saturating_add(1), &chip);
        piece(&mut job, token.start.saturating_add(1), title_start, &hidden);
        piece(&mut job, title_start, title_end, &chip);
        piece(&mut job, title_end, token.end, &hidden);
        at = token.end;
    }
    piece(&mut job, at, text.len(), &plain);
    ui.fonts_mut(|f| f.layout_job(job))
}

/// `cursor` with any end inside a token moved out of it. `before` is the caret last frame and
/// `moved` whether a key moved it: a key moves the caret over a token in its own direction, a
/// click lands on the token's nearer end. A selection grows to take whole tokens.
fn snap(text: &str, (primary, secondary): (usize, usize), before: Option<usize>, moved: bool) -> (usize, usize) {
    let spans = spans(text);
    let inside = |at: usize| spans.iter().find(|span| span.start < at && at < span.end);
    let caret = primary == secondary;
    let primary = match inside(primary) {
        None => primary,
        Some(span) => match before {
            Some(before) if moved && before < primary => span.end,
            Some(before) if moved && before > primary => span.start,
            _ if primary.saturating_sub(span.start) <= span.title_end.saturating_sub(primary) => span.start,
            _ => span.end,
        },
    };
    if caret {
        return (primary, primary);
    }
    let secondary = match inside(secondary) {
        Some(span) if secondary < primary => span.start,
        Some(span) => span.end,
        None => secondary,
    };
    (primary, secondary)
}

/// Common leading bytes of `a` and `b`, at a char boundary.
fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices().zip(b.chars()).find(|((_, x), y)| x != y).map_or(a.len().min(b.len()), |((at, _), _)| at)
}

/// Common trailing bytes of `a` and `b`, at a char boundary.
fn common_suffix(a: &str, b: &str) -> usize {
    a.chars().rev().zip(b.chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).fold(0, usize::saturating_add)
}

/// `new` (an edit of `old`, the caret at char `caret` after it) with every token the edit
/// touched only part of removed whole, and text typed inside a token moved after it. `start` is
/// the char where the selection before the edit began. The fixed text and its caret, or `None`
/// when the edit left every token whole or gone.
fn repair(old: &str, new: &str, caret: usize, start: usize) -> Option<(String, usize)> {
    let byte = |text: &str, at: usize| text.char_indices().nth(at).map_or(text.len(), |(at, _)| at);
    // The edit ends at the caret and begins no later than the selection did, which settles where
    // a repeated character was typed, or text sharing a token's start replaced it.
    let suffix = common_suffix(old, new).min(new.len().saturating_sub(byte(new, caret))).min(old.len());
    let prefix = common_prefix(old, new).min(byte(old, start)).min(old.len().saturating_sub(suffix)).min(new.len().saturating_sub(suffix));
    let (mut from, mut to) = (prefix, old.len().saturating_sub(suffix));
    let typed = new.get(prefix..new.len().saturating_sub(suffix))?;
    let mut touched = false;
    for token in mention::tokens(old) {
        let partial =
            if from == to { token.start < from && from < token.end } else { token.start < to && from < token.end && (token.start < from || to < token.end) };
        if !partial {
            continue;
        }
        touched = true;
        if from == to {
            // Typed inside a token: it goes after the token, which stays.
            (from, to) = (token.end, token.end);
        } else {
            (from, to) = (from.min(token.start), to.max(token.end));
        }
    }
    if !touched {
        return None;
    }
    let (before, after) = (old.get(..from)?, old.get(to..)?);
    let text = [before, typed, after].concat();
    let caret = before.chars().count().saturating_add(typed.chars().count());
    Some((text, caret))
}

/// After the field ran with `old` as its text before: repairs a partial edit of a token and moves
/// the caret out of tokens. `before` is the field's state before it ran.
///
/// Undo and redo restore a settled text whole, so they are not repaired. egui records an undo
/// point before this runs, so one taken mid-edit (after 30 s of steady typing) can hold a broken
/// token; undoing to it then removes that token whole, which is accepted.
pub(super) fn settle(ui: &egui::Ui, id: egui::Id, old: &str, text: &mut String, before: Option<CCursorRange>) {
    let ctx = ui.ctx();
    let Some(mut state) = egui::TextEdit::load_state(ctx, id) else { return };
    let Some(range) = state.cursor.char_range() else { return };
    let (mut primary, mut secondary) = (range.primary.index.0, range.secondary.index.0);
    let history = ui.input(|i| {
        i.events.iter().any(|event| matches!(event, egui::Event::Key { key: egui::Key::Z | egui::Key::Y, pressed: true, modifiers, .. } if modifiers.command))
    });
    let start = before.map_or(primary, |range| range.primary.index.0.min(range.secondary.index.0));
    if old != text.as_str()
        && !history
        && let Some((fixed, caret)) = repair(old, text, primary, start)
    {
        *text = fixed;
        (primary, secondary) = (caret, caret);
        // The field already drew the broken token; draw the repaired text instead.
        ctx.request_discard("mention chip repair");
    }
    let moved = ui.input(|i| {
        i.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown | egui::Key::Home | egui::Key::End,
                    pressed: true,
                    ..
                }
            )
        })
    });
    let (p, s) = snap(text, (primary, secondary), before.map(|range| range.primary.index.0), moved);
    if (p, s) != (range.primary.index.0, range.secondary.index.0) {
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(s), CCursor::new(p))));
        state.store(ctx, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::NodeId;

    fn chip(title: &str) -> String {
        mention::token(title, NodeId::from_bytes([3; 16]))
    }

    #[test]
    fn a_partial_deletion_takes_the_whole_token() {
        let token = chip("Story");
        let old = format!("about {token} now");
        let end = 6 + token.chars().count();
        // Backspace at the chip's end deletes its last hidden character.
        let new = format!("about {} now", &token[..token.len() - 1]);
        assert_eq!(repair(&old, &new, end - 1, end), Some(("about  now".into(), 6)));
        // Delete at the chip's start deletes its `@`.
        let new = format!("about {} now", &token[1..]);
        assert_eq!(repair(&old, &new, 6, 6), Some(("about  now".into(), 6)));
        // A selection from inside the plain text to inside the token, replaced by typing.
        let new = format!("abx{} now", &token[4..]);
        assert_eq!(repair(&old, &new, 3, 2), Some(("abx now".into(), 3)));
    }

    #[test]
    fn text_typed_inside_a_token_goes_after_it() {
        let token = chip("Story");
        let old = format!("a{token}b");
        let new = format!("a{}x{}b", &token[..4], &token[4..]);
        assert_eq!(repair(&old, &new, 6, 5), Some((format!("a{token}xb"), 1 + token.chars().count() + 1)));
    }

    #[test]
    fn edits_beside_a_token_are_left_alone() {
        let token = chip("Story");
        let old = format!("{token} b");
        let n = token.chars().count();
        assert_eq!(repair(&old, &format!("{token}x b"), n + 1, n), None, "typed right after it");
        assert_eq!(repair(&old, &format!("@{token} b"), 1, 0), None, "an `@` typed right before it");
        assert_eq!(repair(&old, &format!("){token} b"), 1, 0), None);
        assert_eq!(repair(&old, " b", 0, 0), None, "the whole token selected and deleted");
        assert_eq!(repair(&old, &format!("{token}b"), n, n), None);
        // A selected chip replaced by text that starts like it: an `@` for another mention, or
        // another chip pasted.
        assert_eq!(repair(&old, "@ b", 1, 0), None, "replaced by an `@`");
        let other = mention::token("Storyboard", NodeId::from_bytes([4; 16]));
        let m = other.chars().count();
        assert_eq!(repair(&old, &format!("{other} b"), m, 0), None, "replaced by another chip");
    }

    #[test]
    fn the_caret_steps_over_a_chip() {
        let token = chip("Story");
        let text = format!("a {token} b");
        let (start, end) = (2, 2 + token.chars().count());
        assert_eq!(snap(&text, (start + 1, start + 1), Some(start), true), (end, end), "right arrow");
        assert_eq!(snap(&text, (end - 1, end - 1), Some(end), true), (start, start), "left arrow");
        assert_eq!(snap(&text, (start + 2, start + 2), Some(0), false), (start, start), "a click near the front");
        assert_eq!(snap(&text, (end - 3, end - 3), Some(0), false), (end, end), "a click in the hidden tail");
        assert_eq!(snap(&text, (1, 1), Some(0), true), (1, 1), "outside a token");
    }

    #[test]
    fn a_selection_takes_whole_chips() {
        let token = chip("Story");
        let text = format!("a {token} b");
        let (start, end) = (2, 2 + token.chars().count());
        assert_eq!(snap(&text, (start + 1, 0), Some(start), true), (end, 0), "shift+right into it");
        assert_eq!(snap(&text, (end - 1, text.chars().count()), Some(end), true), (start, text.chars().count()), "shift+left into it");
        assert_eq!(snap(&text, (text.chars().count(), start + 3), None, false), (text.chars().count(), start), "anchored inside");
    }
}
