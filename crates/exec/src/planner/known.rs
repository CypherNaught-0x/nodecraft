//! Borrow known inputs without copying text or media while a step is deferred (#795).
use super::{InputSource, PlannedInput};
use nodecraft_nodes::InputFacts;
use nodecraft_types::Value;

pub(super) fn facts(inputs: &[PlannedInput]) -> Vec<InputFacts<'_>> {
    inputs
        .iter()
        .map(|input| {
            let mut fact = InputFacts { port: &input.port, port_type: input.port_type, known: Vec::new(), min_count: 0, max_count: Some(0) };
            for source in &input.from {
                match source {
                    InputSource::Value(value) => {
                        let values = match value {
                            Value::List(items) if !input.port_type.many => items.as_slice(),
                            value => std::slice::from_ref(value),
                        };
                        fact.known.extend(values);
                        fact.min_count = fact.min_count.saturating_add(values.len());
                        fact.max_count = fact.max_count.map(|n| n.saturating_add(values.len()));
                    }
                    InputSource::Step { list, .. } | InputSource::Item { list, .. } if *list && input.port_type.many => fact.max_count = None,
                    InputSource::Step { .. } | InputSource::Item { .. } => {
                        fact.min_count = fact.min_count.saturating_add(1);
                        fact.max_count = fact.max_count.map(|n| n.saturating_add(1));
                    }
                }
            }
            fact
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::{Modality, NodeId, PortType};

    #[test]
    fn pending_lists_keep_their_unknown_cardinality_and_known_text_stays_borrowed() {
        let inputs = [
            PlannedInput {
                port: "items".into(),
                port_type: PortType { modality: Modality::Text, many: true },
                from: vec![
                    InputSource::Value(Value::Text("x".repeat(1024 * 1024))),
                    InputSource::Step { node: NodeId::from_bytes([1; 16]), port: "items".into(), list: true },
                ],
                origins: vec![],
            },
            PlannedInput {
                port: "single".into(),
                port_type: PortType { modality: Modality::Text, many: false },
                from: vec![InputSource::Step { node: NodeId::from_bytes([2; 16]), port: "out".into(), list: false }],
                origins: vec![],
            },
        ];
        let facts = facts(&inputs);
        let list = facts.first().unwrap();
        assert_eq!((list.min_count, list.max_count), (1, None), "the pending list may be empty or add several values");
        let InputSource::Value(original) = inputs.first().unwrap().from.first().unwrap() else { panic!("known input") };
        assert!(std::ptr::eq(*list.known.first().unwrap(), original), "preflight must not clone an already-budgeted large Text");
        let scalar = facts.get(1).unwrap();
        assert_eq!((scalar.min_count, scalar.max_count), (1, Some(1)));
    }
}
