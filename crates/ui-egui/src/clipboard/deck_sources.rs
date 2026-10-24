//! Copy-time Deck references follow only nodes included in the same fresh-ID map (#695).
use nodecraft_actions::{deck::Item, layers::Content};
use nodecraft_graph::Node;
use nodecraft_types::{NodeId, Value};
use std::collections::BTreeMap;

pub(super) fn remap(nodes: &mut [Node], ids: &BTreeMap<NodeId, NodeId>) -> Option<()> {
    for node in nodes.iter_mut().filter(|node| node.kind == nodecraft_nodes::deck::ID) {
        // Unknown versions and unreadable stored data travel untouched with their card.
        let Ok(mut deck) = nodecraft_engine::deck_cmds::stored(node) else { continue };
        if !deck.writable() {
            continue;
        }
        let mut changed = false;
        for item in deck.slides.iter_mut().flat_map(|slide| &mut slide.layers) {
            if let Item::Layer(layer) = item
                && let Content::Source { origin, .. } = &mut layer.content
                && let Some(fresh) = origin.parse::<NodeId>().ok().and_then(|old| ids.get(&old))
            {
                *origin = fresh.to_string();
                changed = true;
            }
        }
        if changed {
            // Refuse a whole paste if encoding would exceed the existing stored-deck limit.
            node.config.insert(nodecraft_nodes::deck::CONFIG.into(), Value::Text(deck.encode().ok()?));
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deck695_copy_remaps_known_sources_and_preserves_unknown_data_and_uncopied_sources() {
        let (old, fresh, external) = (NodeId::from_bytes([1; 16]), NodeId::from_bytes([2; 16]), NodeId::from_bytes([3; 16]));
        let source = |id: &str, origin: NodeId| nodecraft_actions::deck::source_layer(&origin.to_string(), id, (50.0, 50.0), (100.0, 100.0));
        let raw = json!({"version":1,"future_deck":{"keep":true},"slides":[{"id":"intro","future_slide":7,"layers":[source("copied",old),source("external",external),{"id":"unknown","content":{"kind":"future","origin":old.to_string()},"keep":[1,2,3]}]}]});
        let mut node = Node::new(NodeId::from_bytes([4; 16]), nodecraft_nodes::deck::ID);
        node.config.insert(nodecraft_nodes::deck::CONFIG.into(), Value::Text(raw.to_string()));
        let mut nodes = vec![node];
        remap(&mut nodes, &BTreeMap::from([(old, fresh)])).unwrap();
        let changed: serde_json::Value = serde_json::from_str(nodecraft_nodes::deck::stored(&nodes[0]).unwrap()).unwrap();
        assert_eq!(changed["slides"][0]["layers"][0]["content"]["origin"], fresh.to_string());
        assert_eq!(changed["slides"][0]["layers"][1]["content"]["origin"], external.to_string());
        assert_eq!(changed["slides"][0]["layers"][2], raw["slides"][0]["layers"][2]);
        assert_eq!(changed["future_deck"], raw["future_deck"]);
        assert_eq!(changed["slides"][0]["future_slide"], 7);
    }

    #[test]
    fn deck695_copy_keeps_newer_and_malformed_decks_byte_for_byte() {
        for raw in ["{\"version\": 99, \"slides\": [{\"future\": true}]}", "not json"] {
            let mut node = Node::new(NodeId::from_bytes([4; 16]), nodecraft_nodes::deck::ID);
            node.config.insert(nodecraft_nodes::deck::CONFIG.into(), Value::Text(raw.into()));
            let mut nodes = vec![node.clone()];
            remap(&mut nodes, &BTreeMap::from([(node.id, NodeId::from_bytes([5; 16]))])).unwrap();
            assert_eq!(nodes[0], node);
        }
    }
}
