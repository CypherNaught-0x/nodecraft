//! Plugin cards made with another version of their plugin (#173).
use super::NodeCraftApp;
use nodecraft_engine::Session;
use nodecraft_graph::Node;
use nodecraft_types::NodeId;
use serde_json::{Value, json};

/// Whether `node` is a plugin card its plugin's current version would change.
pub(super) fn outdated(session: &Session, node: &Node) -> bool {
    nodecraft_engine::plugin_runtime::is_plugin(&node.kind)
        && session.kinds().get(&node.kind).ok().is_some_and(|kind| matches!(kind.prepare_migration(node), Ok(Some(_))))
}

impl NodeCraftApp {
    /// Brings card `id` to its plugin's current version, one undo step, and says what it dropped.
    pub(super) fn migrate_card(&mut self, id: NodeId) {
        self.notice = match self.session.dispatch("node.migrate", &json!({"canvas": self.canvas, "node": id})) {
            Ok(result) => {
                let warnings: Vec<&str> = result.get("warnings").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                let said = if warnings.is_empty() { String::new() } else { format!("{} ", warnings.join(" ")) };
                Some(format!("Updated the card. {said}Undo to put it back."))
            }
            Err(error) => Some(error.message),
        };
    }
}
