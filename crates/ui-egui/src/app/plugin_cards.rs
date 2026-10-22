//! Plugin cards made with another version of their plugin (#173).
use super::NodeCraftApp;
use nodecraft_types::NodeId;
use serde_json::{Value, json};

impl NodeCraftApp {
    /// Brings card `id` to its plugin's current version, one undo step, and says what it dropped.
    pub(super) fn migrate_card(&mut self, id: NodeId) {
        self.notice = match self.session.dispatch("node.migrate", &json!({"canvas": self.canvas, "node": id})) {
            Ok(result) => {
                let warnings: Vec<&str> = result.get("warnings").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                (!warnings.is_empty()).then(|| format!("Updated the card. {} Undo to put it back.", warnings.join(" ")))
            }
            Err(error) => Some(error.message),
        };
    }
}
