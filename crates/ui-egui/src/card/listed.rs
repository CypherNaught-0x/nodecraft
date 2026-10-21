//! Parameters whose values come from a run-time option list (#676): the checkpoints a ComfyUI
//! server has, Higgsfield's restyle presets. Once the host has fetched the list the field is a
//! picker showing each choice's label and storing its value, through the same parameter edit as
//! every other control; until then it stays a text field with a hint to fetch the lists.
use crate::theme::Tokens;
use nodecraft_types::{Choice, OptionKey, OptionLists, Value};

/// Which fetched list an enabled parameter draws from: the key [`OptionLists`] files it under.
/// Whether the host has the list is read live, never cached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The namespace of the contract group that declares the parameter.
    pub owner: String,
    pub source: String,
    /// The scope parameter's value, for a scoped source.
    pub scope: Option<String>,
    /// The source is narrowed by another parameter; fetches can't narrow lists yet.
    pub scoped: bool,
}

impl Listed {
    /// The fetched choices, while the host has them.
    pub fn choices<'a>(&self, lists: Option<&'a OptionLists>) -> Option<&'a [Choice]> {
        let key = OptionKey { owner: &self.owner, source: &self.source, scope: self.scope.as_deref() };
        // A scoped source whose scope has no value must not fall back to the unscoped list of
        // the same name.
        lists.filter(|_| !self.scoped || self.scope.is_some()).and_then(|lists| lists.get(key))
    }
    /// What an entry retains, for the view cache's budget.
    pub fn bytes(&self) -> usize {
        72usize.saturating_add(self.owner.len()).saturating_add(self.source.len()).saturating_add(self.scope.as_ref().map_or(0, String::len))
    }

    /// The hint under the text field while the host has no list, if fetching one could help.
    pub fn hint(&self) -> Option<&'static str> {
        (!self.scoped).then_some(UNFETCHED_HINT)
    }
}

/// The hint under an unscoped listed parameter's text field while its list isn't fetched. It
/// stays true when the provider isn't set up: the refresh then says why.
pub const UNFETCHED_HINT: &str = "Type a value, or run Refresh option lists to pick one.";

/// A combo box over `choices`. A stored value off the list is shown as it is and marked, never
/// replaced; only picking a choice edits the parameter.
pub fn picker(ui: &mut egui::Ui, choices: &[Choice], value: Option<&Value>, label: egui::Id, t: &Tokens) -> Option<Option<Value>> {
    let current = match value {
        Some(Value::Text(text)) => text.as_str(),
        _ => "",
    };
    let listed = choices.iter().find(|choice| choice.value == current);
    let stale = !current.is_empty() && listed.is_none();
    let shown: String = match listed {
        Some(choice) => choice.label.chars().take(t.preview_chars).collect(),
        None if current.is_empty() => "Choose…".to_owned(),
        None => format!("{} (not an option)", current.chars().take(t.preview_chars).collect::<String>()),
    };
    let text = if stale { egui::RichText::new(shown).color(t.danger) } else { egui::RichText::new(shown) };
    let mut picked = None;
    let row = ui.spacing().interact_size.y;
    let rows = (row + ui.spacing().item_spacing.y) * 12.0;
    // The popup is tall enough for the rows' own scroll area, so it never scrolls as well.
    let popup = rows + ui.spacing().menu_margin.sum().y + ui.spacing().item_spacing.y * 2.0;
    let response = egui::ComboBox::from_id_salt("option_list")
        .selected_text(text)
        .width(ui.available_width())
        .height(popup)
        .show_ui(ui, |ui| {
            // A list may hold thousands of values: lay out only the rows on screen.
            egui::ScrollArea::vertical().id_salt("option_rows").max_height(rows).show_rows(ui, row, choices.len(), |ui, range| {
                for choice in choices.get(range).unwrap_or_default() {
                    let response = ui.selectable_label(choice.value == current, &choice.label);
                    let response = if choice.label != choice.value { response.on_hover_text(&choice.value) } else { response };
                    if response.clicked() {
                        picked = Some(choice.value.clone());
                    }
                }
            });
        })
        .response
        .labelled_by(label);
    if stale {
        response.on_hover_text("This value isn't on the fetched list. Pick one, or run Refresh option lists.");
    }
    picked.filter(|value| value != current).map(|value| Some(Value::Text(value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(scope: Option<&str>, scoped: bool) -> Listed {
        Listed { owner: "owner".into(), source: "src".into(), scope: scope.map(str::to_owned), scoped }
    }

    #[test]
    fn choices_come_only_from_a_fetched_list_under_its_own_key() {
        let mut lists = OptionLists::default();
        let choice = Choice { value: "v".into(), label: "Label".into() };
        assert!(lists.set(OptionKey { owner: "owner", source: "src", scope: None }, vec![choice.clone()]).is_ok());
        assert_eq!(listed(None, false).choices(Some(&lists)), Some(&[choice][..]));
        assert_eq!(listed(None, false).hint(), Some(UNFETCHED_HINT));
        // A scoped source without its scope's value never falls back to the unscoped list,
        // and refreshing can't fill it, so it gets no hint.
        assert_eq!(listed(None, true).choices(Some(&lists)), None);
        assert_eq!(listed(None, true).hint(), None);
        assert_eq!(listed(Some("other"), true).choices(Some(&lists)), None);
        assert_eq!(listed(None, false).choices(None), None);
    }
}
