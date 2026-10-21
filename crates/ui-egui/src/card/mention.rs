//! The `@` picker in a generation card's prompt (#505): typing `@` lists the canvas's text cards;
//! arrows choose, Enter or Tab (or a click) puts the card's token (`@[Title](id)`,
//! `nodecraft_nodes::mention`) where `@` was typed, Escape closes the list. The preview shows a
//! token as `@Title`, and the hint under the prompt names what each mention reads or why it can't.
use super::*;
use nodecraft_nodes::mention;
use nodecraft_types::NodeId;

/// Most cards the list shows.
const MAX_SHOWN: usize = 8;
/// Longest text after `@` still read as a search.
const MAX_QUERY_CHARS: usize = 32;

/// A card a prompt can mention: every card on the canvas, so a mention of an image card can say
/// so. Built once per frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: NodeId,
    pub title: String,
    /// It has a single text output.
    pub text: bool,
}

/// The `@search` the cursor (a char index) is in: the byte where `@` is, and what follows it.
fn search(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let end = text.char_indices().nth(cursor).map_or(text.len(), |(at, _)| at);
    let before = text.get(..end)?;
    let (at, _) =
        before.char_indices().rev().take(MAX_QUERY_CHARS.saturating_add(1)).find(|(_, c)| *c == '@' || c.is_whitespace()).filter(|(_, c)| *c == '@')?;
    let query = before.get(at.saturating_add(1)..)?;
    // `@` starts a word, and isn't a token already.
    let word_start = before.get(..at)?.chars().next_back().is_none_or(char::is_whitespace);
    (word_start && !query.starts_with('[')).then_some((at, query))
}

/// The text cards matching `query`, `own` (the card being edited) left out.
fn matches<'c>(candidates: &'c [Candidate], own: NodeId, query: &str) -> Vec<&'c Candidate> {
    let query = query.to_lowercase();
    candidates.iter().filter(|c| c.text && c.id != own && c.title.to_lowercase().contains(&query)).take(MAX_SHOWN).collect()
}

#[derive(Clone, Copy, Default)]
struct Picker {
    /// The highlighted row.
    chosen: usize,
    /// The `@` Escape closed the list at, so it stays closed until another is typed.
    closed: Option<usize>,
}

fn cursor(ctx: &egui::Context, id: egui::Id) -> Option<usize> {
    egui::TextEdit::load_state(ctx, id)?.cursor.char_range().map(|range| range.primary.index.0)
}

/// `text` with `candidate`'s token in place of the search at `at..end`, followed by a space
/// unless one is there already, and the char index after that space.
fn insert(text: &str, at: usize, end: usize, candidate: &Candidate) -> Option<(String, usize)> {
    let token = mention::token(&candidate.title, candidate.id);
    let (before, after) = (text.get(..at)?, text.get(end..)?);
    let mut out = String::with_capacity(text.len().saturating_add(token.len()).saturating_add(1));
    out.push_str(before);
    out.push_str(&token);
    let rest = after.strip_prefix(' ').unwrap_or(after);
    out.push(' ');
    let caret = out.chars().count();
    out.push_str(rest);
    Some((out, caret))
}

fn place_cursor(ctx: &egui::Context, id: egui::Id, caret: usize) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(caret))));
    state.store(ctx, id);
}

/// The open list's search over `draft`: the byte range it covers and the cards it matches.
fn open<'c>(ctx: &egui::Context, id: egui::Id, draft: &str, candidates: &'c [Candidate], own: NodeId) -> Option<(usize, usize, Vec<&'c Candidate>)> {
    let caret = cursor(ctx, id)?;
    let (at, query) = search(draft, caret)?;
    let picker: Picker = ctx.data(|data| data.get_temp(id.with("mention"))).unwrap_or_default();
    if picker.closed == Some(at) {
        return None;
    }
    let found = matches(candidates, own, query);
    let end = at.saturating_add(1).saturating_add(query.len());
    (!found.is_empty()).then_some((at, end, found))
}

/// Before the prompt's text field takes this frame's keys: while the list is open, arrows,
/// Enter, Tab and Escape are the list's. `id` is the text field's.
pub(super) fn keys(ui: &egui::Ui, id: egui::Id, draft: Option<&mut Draft>, candidates: &[Candidate], own: NodeId) {
    let Some(draft) = draft.filter(|draft| draft.was_focused) else { return };
    let ctx = ui.ctx();
    let Some((at, end, found)) = open(ctx, id, &draft.text, candidates, own) else { return };
    let key = |key| ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
    let state = id.with("mention");
    let mut picker: Picker = ctx.data(|data| data.get_temp(state)).unwrap_or_default();
    picker.chosen = picker.chosen.min(found.len().saturating_sub(1));
    if key(egui::Key::ArrowDown) {
        picker.chosen = picker.chosen.saturating_add(1) % found.len();
    }
    if key(egui::Key::ArrowUp) {
        picker.chosen = picker.chosen.checked_sub(1).unwrap_or(found.len().saturating_sub(1));
    }
    // egui drops focus on Escape and moves it on Tab before any widget runs; the field keeps it.
    let mut keep_focus = false;
    if key(egui::Key::Escape) {
        picker.closed = Some(at);
        keep_focus = true;
    }
    if (key(egui::Key::Enter) | key(egui::Key::Tab))
        && let Some((text, caret)) = found.get(picker.chosen).and_then(|candidate| insert(&draft.text, at, end, candidate))
    {
        draft.text = text;
        place_cursor(ctx, id, caret);
        picker = Picker::default();
        keep_focus = true;
    }
    if keep_focus {
        ctx.memory_mut(|memory| {
            memory.request_focus(id);
            memory.move_focus(egui::FocusDirection::None);
        });
    }
    ctx.data_mut(|data| data.insert_temp(state, picker));
}

/// After the text field: the list under it while there is a search, and a click on a row.
/// `was_focused` is the field's focus before it ran: a click on a row ends that focus first.
/// Whether a row was picked, so the field's commit of the search itself is dropped.
#[allow(clippy::too_many_arguments)]
pub(super) fn list(
    ui: &egui::Ui,
    id: egui::Id,
    was_focused: bool,
    draft: Option<&mut Draft>,
    candidates: &[Candidate],
    own: NodeId,
    below: f32,
    t: &Tokens,
) -> bool {
    let ctx = ui.ctx().clone();
    if !was_focused && !ctx.memory(|memory| memory.has_focus(id)) {
        return false;
    }
    let Some(draft) = draft else { return false };
    let Some((at, end, found)) = open(&ctx, id, &draft.text, candidates, own) else { return false };
    let chosen = ctx.data(|data| data.get_temp::<Picker>(id.with("mention"))).unwrap_or_default().chosen.min(found.len().saturating_sub(1));
    let mut picked = None;
    egui::Area::new(id.with("mention list")).order(egui::Order::Foreground).fixed_pos(Pos2::new(ui.min_rect().left(), below)).show(&ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_max_width(ui.max_rect().width().min(t.card_width));
            for (row, candidate) in found.iter().enumerate() {
                let label = format!("@{}", candidate.title);
                let r = ui.selectable_label(row == chosen, label);
                if r.clicked() {
                    picked = Some(*candidate);
                }
            }
        });
    });
    if let Some((text, caret)) = picked.and_then(|candidate| insert(&draft.text, at, end, candidate)) {
        draft.text = text;
        place_cursor(&ctx, id, caret);
        ctx.memory_mut(|memory| memory.request_focus(id));
        ctx.data_mut(|data| data.insert_temp(id.with("mention"), Picker::default()));
        return true;
    }
    false
}

/// What the prompt's mentions read, under it: plain for text cards, in the danger colour for a
/// mention that blocks the run.
pub(super) fn hint(ui: &mut egui::Ui, prompt: &str, candidates: &[Candidate], own: NodeId, t: &Tokens) {
    if !prompt.contains("@[") {
        return;
    }
    let tokens = mention::tokens(prompt);
    let mut used = Vec::new();
    for id in mention::ids(prompt) {
        let title = tokens.iter().find(|token| token.id == id).map_or("", |token| token.title);
        let problem = match candidates.iter().find(|c| c.id == id) {
            _ if id == own => Some("is this card"),
            None => Some("is no longer on this canvas"),
            Some(candidate) if !candidate.text => Some("makes no text"),
            Some(_) => None,
        };
        match problem {
            Some(problem) => {
                ui.label(egui::RichText::new(format!("@{title} {problem}")).small().color(t.danger));
            }
            None => used.push(format!("@{title}")),
        }
    }
    if !used.is_empty() {
        ui.label(egui::RichText::new(format!("Reads {}", used.join(", "))).small().color(t.text_muted))
            .on_hover_text("These cards' text goes into the prompt where they are mentioned");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_starts_at_an_at_sign_beginning_a_word() {
        assert_eq!(search("hello @wor", 10), Some((6, "wor")));
        assert_eq!(search("@", 1), Some((0, "")));
        assert_eq!(search("hello @wor", 8), Some((6, "w")));
        assert_eq!(search("mail me@home", 12), None, "inside a word");
        assert_eq!(search("hello @wor ld", 13), None, "past a space");
        assert_eq!(search("an @[Card](x", 12), None, "a token, not a search");
        assert_eq!(search("é @ü", 4), Some((3, "ü")), "char indices, not bytes");
        assert_eq!(search("x", 99), None);
    }

    #[test]
    fn inserting_replaces_the_search_with_the_token() {
        let id = NodeId::from_bytes([7; 16]);
        let card = Candidate { id, title: "Story".into(), text: true };
        let (text, caret) = insert("about @st now", 6, 9, &card).unwrap();
        assert_eq!(text, format!("about {} now", mention::token("Story", id)));
        assert_eq!(text.chars().nth(caret.saturating_sub(1)), Some(' '));
        assert_eq!(text.chars().nth(caret), Some('n'), "the space already there is reused");
        assert_eq!(insert("@st", 0, 3, &card).unwrap().0, format!("{} ", mention::token("Story", id)));
        assert!(insert("ab", 1, 9, &card).is_none());
    }
}
