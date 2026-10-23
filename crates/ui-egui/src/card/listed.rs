//! Parameters whose values come from a run-time option list (#676): the checkpoints a ComfyUI
//! server has, Higgsfield's restyle presets. Once the host has fetched the list the field is a
//! picker showing each choice's label and storing its value, through the same parameter edit as
//! every other control; until then it stays a text field with a hint to fetch the lists.
use crate::theme::Tokens;
use nodecraft_types::{OptionKey, OptionList, OptionListId, OptionLists, Value};

mod filter;
use filter::Filter;

// There is one active option popup per context. Keeping just its state bounds the cached
// indices independently of how many cards have been opened, removed or refreshed.
#[derive(Clone)]
struct PickerState {
    owner: egui::Id,
    list: OptionListId,
    filter: Filter,
}

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
    pub fn choices<'a>(&self, lists: Option<&'a OptionLists>) -> Option<OptionList<'a>> {
        let key = OptionKey { owner: &self.owner, source: &self.source, scope: self.scope.as_deref() };
        // A scoped source whose scope has no value must not fall back to the unscoped list of
        // the same name.
        lists.filter(|_| !self.scoped || self.scope.is_some()).and_then(|lists| lists.view(key))
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
pub fn picker(ui: &mut egui::Ui, choices: OptionList<'_>, value: Option<&Value>, overridden: bool, label: egui::Id, t: &Tokens) -> Option<Option<Value>> {
    let current = match value {
        Some(Value::Text(text)) => text.as_str(),
        _ => "",
    };
    let selected = choices.index(current);
    let listed = selected.and_then(|index| choices.choices().get(index));
    let stale = !current.is_empty() && listed.is_none();
    let shown: String = match listed {
        Some(choice) => choice.label.chars().take(t.preview_chars).collect(),
        None if current.is_empty() => "Choose…".to_owned(),
        None => format!("{} (not an option)", current.chars().take(t.preview_chars).collect::<String>()),
    };
    let text = if stale { egui::RichText::new(shown).color(t.danger) } else { egui::RichText::new(shown) };
    let mut edited = None;
    // egui 0.36.2 ComboBox stores an IdSalt before deriving its button id; use the same salt here.
    let id = ui.make_persistent_id(egui::IdSalt::new("option_list"));
    let was_open = egui::ComboBox::is_open(ui.ctx(), id);
    let width = ui.available_width();
    let row = ui.spacing().interact_size.y;
    let rows = (row + ui.spacing().item_spacing.y) * 12.0;
    // Leave room for the query, Default row and status above the rows' own scroll area.
    let popup = rows + row * 4.0 + ui.spacing().menu_margin.sum().y + ui.spacing().item_spacing.y * 8.0;
    let response = egui::ComboBox::from_id_salt("option_list")
        .selected_text(text)
        .width(width)
        .truncate()
        .height(popup)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            let slot = egui::Id::new("nodecraft.option_picker.filter");
            // Move the state out while drawing: neither clone a large index vector nor hold
            // the context data lock while widgets access the context themselves.
            let saved = ui.ctx().data_mut(|data| data.get_temp_mut_or_default::<Option<PickerState>>(slot).take());
            let identity = choices.identity();
            let mut state = saved.unwrap_or_else(|| PickerState { owner: id, list: identity.clone(), filter: Filter::default() });
            let opening = !was_open || state.owner != id;
            let replaced = state.list != identity;
            if opening {
                state.owner = id;
                state.list = identity;
                state.filter = Filter::default();
            } else if replaced {
                state.list = identity;
                state.filter.restart();
            }

            // Truncated rows cannot widen the menu when a new, longer label scrolls into view.
            ui.set_width((width - ui.spacing().menu_margin.sum().x).max(row));
            let filter_label = ui.label(egui::RichText::new("Filter options").color(t.text_muted).small());
            let query = ui
                .add(egui::TextEdit::singleline(&mut state.filter.query).id_salt("query").desired_width(f32::INFINITY).char_limit(filter::MAX_QUERY_CHARS))
                .labelled_by(filter_label.id);
            if opening {
                query.request_focus();
            }
            if query.changed() {
                state.filter.query_changed();
            }
            state.filter.advance(choices);
            if ui
                .add_enabled(overridden, egui::Button::new("Use default").min_size(egui::vec2(ui.available_width(), row)))
                .on_hover_text("Use the parameter's default")
                .clicked()
            {
                edited = Some(None);
                ui.close();
            }
            ui.separator();
            let total = choices.choices().len();
            let count = state.filter.rows(total);
            let complete = state.filter.complete(total);
            let status = if !complete {
                "Filtering…".to_owned()
            } else if state.filter.is_empty() {
                format!("{total} options")
            } else if count == 1 {
                "1 match".to_owned()
            } else {
                format!("{count} matches")
            };
            ui.label(egui::RichText::new(status).color(t.text_muted).small());
            if !complete {
                ui.ctx().request_repaint();
            }
            if count == 0 && complete {
                ui.label("No matches");
            }
            // Keep the virtual viewport when an incremental scan briefly has no matches;
            // otherwise the surrounding combo's scroll area can collapse it to zero height.
            let mut scroll = egui::ScrollArea::vertical().id_salt("option_rows").max_height(rows).min_scrolled_height(rows).auto_shrink([false, false]);
            if opening || replaced || query.changed() {
                // Installed indices are bounded by MAX_OPTIONS, so this offset stays finite.
                let offset = if state.filter.is_empty() {
                    selected.map_or(0.0, |index| (index as f32 * (row + ui.spacing().item_spacing.y) - rows * 0.5).max(0.0))
                } else {
                    0.0
                };
                scroll = scroll.vertical_scroll_offset(offset);
            }
            scroll.show_rows(ui, row, count, |ui, range| {
                for position in range {
                    let Some(index) = state.filter.choice_index(position) else { continue };
                    let Some(choice) = choices.choices().get(index) else { continue };
                    let response = ui.add_sized([ui.available_width(), row], egui::Button::selectable(Some(index) == selected, &choice.label).truncate());
                    let response = if choice.label != choice.value { response.on_hover_text(&choice.value) } else { response };
                    if response.clicked() {
                        if choice.value != current {
                            edited = Some(Some(Value::Text(choice.value.clone())));
                        }
                        ui.close();
                    }
                }
            });
            ui.ctx().data_mut(|data| data.insert_temp(slot, Some(state)));
        })
        .response
        .labelled_by(label);
    if stale {
        response.on_hover_text("This value isn't on the fetched list. Pick one, or run Refresh option lists.");
    }
    edited
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::Choice;

    fn listed(scope: Option<&str>, scoped: bool) -> Listed {
        Listed { owner: "owner".into(), source: "src".into(), scope: scope.map(str::to_owned), scoped }
    }

    #[test]
    fn choices_come_only_from_a_fetched_list_under_its_own_key() {
        let mut lists = OptionLists::default();
        let choice = Choice { value: "v".into(), label: "Label".into() };
        assert!(lists.set(OptionKey { owner: "owner", source: "src", scope: None }, vec![choice.clone()]).is_ok());
        assert_eq!(listed(None, false).choices(Some(&lists)).map(|list| list.choices()), Some(&[choice][..]));
        assert_eq!(listed(None, false).hint(), Some(UNFETCHED_HINT));
        // A scoped source without its scope's value never falls back to the unscoped list,
        // and refreshing can't fill it, so it gets no hint.
        assert!(listed(None, true).choices(Some(&lists)).is_none());
        assert_eq!(listed(None, true).hint(), None);
        assert!(listed(Some("other"), true).choices(Some(&lists)).is_none());
        assert!(listed(None, false).choices(None).is_none());
    }
}
