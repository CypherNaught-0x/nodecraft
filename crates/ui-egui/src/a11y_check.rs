//! Test-only AccessKit checks shared by the shell's kittests (#1118).
use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable, by};

/// Roles a person activates: each must carry a name a screen reader can say.
const CLICKABLE: [Role; 7] = [Role::Button, Role::CheckBox, Role::Link, Role::MenuItem, Role::Tab, Role::RadioButton, Role::ComboBox];

/// The on-screen controls whose accessible name is empty or only punctuation and symbols
/// ("…", "‹", "›"), as `role: name`.
pub(crate) fn unnamed_controls<S>(h: &egui_kittest::Harness<'_, S>) -> Vec<String> {
    h.query_all(
        by().predicate(|node| {
            CLICKABLE.contains(&node.role()) && !node.is_hidden() && !node.label().is_some_and(|name| name.chars().any(char::is_alphanumeric))
        }),
    )
    .map(|node| format!("{:?}: {:?}", node.accesskit_node().role(), node.accesskit_node().label()))
    .collect()
}
