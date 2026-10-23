//! Popup-only, incremental filtering over bounded, installed choices.
use nodecraft_types::OptionList;

pub(super) const MAX_QUERY_CHARS: usize = 256;
const FRAME_BYTES: usize = 128 * 1024;
const FRAME_CHOICES: usize = 1024;

#[derive(Clone, Default)]
pub(super) struct Filter {
    pub query: String,
    folded: String,
    cursor: usize,
    matches: Vec<usize>,
}

fn folded(text: &str) -> String {
    // Match the canvas search's lowercase and Greek final-sigma treatment.
    text.chars().flat_map(char::to_lowercase).map(|ch| if ch == 'ς' { 'σ' } else { ch }).collect()
}

impl Filter {
    pub fn query_changed(&mut self) {
        self.query = self.query.chars().take(MAX_QUERY_CHARS).collect();
        self.folded = folded(self.query.trim());
        self.restart();
    }

    pub fn restart(&mut self) {
        self.cursor = 0;
        self.matches.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.folded.is_empty()
    }

    pub fn complete(&self, total: usize) -> bool {
        self.is_empty() || self.cursor >= total
    }

    pub fn rows(&self, total: usize) -> usize {
        if self.is_empty() { total } else { self.matches.len() }
    }

    pub fn choice_index(&self, row: usize) -> Option<usize> {
        if self.is_empty() { Some(row) } else { self.matches.get(row).copied() }
    }

    /// Return the raw text bytes and choices visited, for work-budget verification.
    pub fn advance(&mut self, list: OptionList<'_>) -> (usize, usize) {
        if self.is_empty() {
            return (0, 0);
        }
        let mut bytes = 0usize;
        let mut visits = 0usize;
        // OptionLists validates each label/value at <=4 KiB, so one choice always fits.
        while let Some(choice) = list.choices().get(self.cursor) {
            let cost = choice.label.len().saturating_add(choice.value.len());
            if visits >= FRAME_CHOICES || bytes.saturating_add(cost) > FRAME_BYTES {
                break;
            }
            if folded(&choice.label).contains(&self.folded) || (choice.value != choice.label && folded(&choice.value).contains(&self.folded)) {
                self.matches.push(self.cursor);
            }
            self.cursor = self.cursor.saturating_add(1);
            bytes = bytes.saturating_add(cost);
            visits = visits.saturating_add(1);
        }
        (bytes, visits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::{Choice, MAX_OPTIONS, OptionKey, OptionLists};

    const KEY: OptionKey<'static> = OptionKey { owner: "test", source: "test.options", scope: None };

    fn lists(choices: Vec<Choice>) -> OptionLists {
        let mut lists = OptionLists::default();
        lists.set(KEY, choices).unwrap();
        lists
    }

    fn drain(filter: &mut Filter, list: OptionList<'_>) -> usize {
        let mut frames = 0usize;
        while !filter.complete(list.choices().len()) {
            let (bytes, visits) = filter.advance(list);
            assert!(bytes <= FRAME_BYTES);
            assert!(visits > 0 && visits <= FRAME_CHOICES);
            frames += 1;
        }
        frames
    }

    #[test]
    fn labels_and_distinct_values_match_case_insensitively_in_host_order() {
        let lists = lists(vec![
            Choice { value: "hidden-file".into(), label: "Portrait".into() },
            Choice { value: "greek".into(), label: "ΟΣ".into() },
            Choice { value: "Portrait-value".into(), label: "Different label".into() },
        ]);
        let view = lists.view(KEY).unwrap();
        let mut filter = Filter { query: "PORTRAIT".into(), ..Filter::default() };
        filter.query_changed();
        drain(&mut filter, view);
        assert_eq!(filter.matches, vec![0, 2]);
        filter.query = "ος".into();
        filter.query_changed();
        drain(&mut filter, view);
        assert_eq!(filter.matches, vec![1]);
    }

    #[test]
    fn long_labels_finish_across_bounded_frames_and_queries_are_bounded() {
        let lists = lists((0..2000).map(|n| Choice { value: n.to_string(), label: "A".repeat(4000) }).collect());
        let view = lists.view(KEY).unwrap();
        let mut filter = Filter { query: "missing".into(), ..Filter::default() };
        filter.query_changed();
        assert!(drain(&mut filter, view) > 1);
        assert_eq!(filter.cursor, 2000);
        assert!(filter.matches.is_empty());
        filter.query = "🦀".repeat(4096);
        filter.query_changed();
        assert_eq!(filter.query.chars().count(), MAX_QUERY_CHARS);
        assert!(filter.query.len() <= MAX_QUERY_CHARS * 4);
    }

    #[test]
    fn an_empty_query_uses_the_original_range_without_scanning_or_copying_indices() {
        let lists = lists((0..MAX_OPTIONS).map(|n| Choice { value: n.to_string(), label: n.to_string() }).collect());
        let view = lists.view(KEY).unwrap();
        let mut filter = Filter::default();
        assert_eq!(filter.advance(view), (0, 0));
        assert!(filter.complete(MAX_OPTIONS));
        assert_eq!(filter.rows(MAX_OPTIONS), MAX_OPTIONS);
        assert_eq!(filter.choice_index(123), Some(123));
        assert!(filter.matches.is_empty());
    }
}
