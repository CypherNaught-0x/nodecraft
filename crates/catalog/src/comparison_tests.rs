//! Keep the public capability audit complete as the builtin catalog grows.
use super::Catalog;
use std::collections::BTreeSet;

#[test]
fn every_builtin_description_has_a_dated_primary_source() {
    let catalog = Catalog::builtin().unwrap();
    let audit = include_str!("../../../docs/model-comparison.md");
    let mut sourced = BTreeSet::new();
    for line in audit.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<_> = line.split('|').map(str::trim).collect();
        let id = cells.get(1).unwrap().trim_matches('`');
        assert!(sourced.insert(id), "duplicate audit row: {id}");
        assert!(cells.get(2).unwrap().starts_with("[Documentation](https://"), "missing primary source: {id}");
        let date = cells.get(3).unwrap();
        let parts: Vec<u32> = date.split('-').map(|part| part.parse().unwrap()).collect();
        assert_eq!(parts.len(), 3, "missing checked date: {id}");
        assert!(*parts.first().unwrap() >= 2026);
        assert!((1..=12).contains(parts.get(1).unwrap()));
        assert!((1..=31).contains(parts.get(2).unwrap()));
    }
    let builtins: BTreeSet<_> = catalog.models().map(|model| model.id.as_str()).collect();
    assert_eq!(sourced, builtins);
    for model in catalog.models() {
        assert!(!model.description.trim().is_empty(), "{} has no description", model.id);
        assert!(model.description.len() <= 220, "{} needs a concise picker description", model.id);
    }
}
