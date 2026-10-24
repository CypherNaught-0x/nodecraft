use super::*;
use proptest::prelude::*;

fn batch(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn tags(b: u8, n: usize) -> Vec<ItemTag> {
    let items: Vec<Value> = (0..n).map(|i| Value::Text(format!("item {i}"))).collect();
    batch_tags(batch(b), &items)
}

fn rows(cells: &[Cell]) -> Vec<Vec<usize>> {
    cells.iter().map(|c| c.rows.clone()).collect()
}

#[test]
fn ids_are_stable_across_reorders_and_distinct_for_duplicates() {
    let a = Value::Text("a".into());
    let b = Value::Text("b".into());
    let forward = batch_tags(batch(1), &[a.clone(), b.clone(), a.clone()]);
    let backward = batch_tags(batch(1), &[b.clone(), a.clone(), a.clone()]);
    assert_eq!(forward.first(), backward.get(1));
    assert_eq!(forward.get(1), backward.first());
    assert_ne!(forward.first(), forward.get(2), "a repeated value is a separate item");
    assert_ne!(batch_tags(batch(2), std::slice::from_ref(&a)).first(), forward.first(), "ids belong to their batch");
}

#[test]
fn one_relation_fans_in_order() {
    let a = tags(1, 3);
    let cells = combine(&[&a], &BTreeMap::new()).unwrap();
    assert_eq!(rows(&cells), [[0], [1], [2]]);
    assert_eq!(cells.iter().map(|c| c.tag.clone()).collect::<Vec<_>>(), a);
}

#[test]
fn two_batches_cross_by_default_and_zip_when_both_ask() {
    let (a, b) = (tags(1, 2), tags(2, 3));
    let cross = combine(&[&a, &b], &BTreeMap::new()).unwrap();
    assert_eq!(rows(&cross), [[0, 0], [0, 1], [0, 2], [1, 0], [1, 1], [1, 2]]);
    assert!(cross.iter().all(|c| c.tag.len() == 2 && c.tag.windows(2).all(|w| w[0].batch < w[1].batch)));
    let zip: BTreeMap<_, _> = [(batch(1), Pairing::Zip), (batch(2), Pairing::Zip)].into();
    let c = tags(2, 2);
    assert_eq!(rows(&combine(&[&a, &c], &zip).unwrap()), [[0, 0], [1, 1]]);
    let unequal = combine(&[&a, &b], &zip).unwrap_err();
    assert!(unequal.contains("2 and 3 items"), "{unequal}");
    let mixed: BTreeMap<_, _> = [(batch(1), Pairing::Zip)].into();
    assert!(combine(&[&a, &b], &mixed).unwrap_err().contains("disagree"));
}

#[test]
fn shared_batches_join_on_their_items() {
    // A result fanned by batch 1, and batch 1 itself on another input: item k meets item k.
    let a = tags(1, 3);
    let cells = combine(&[&a, &a], &BTreeMap::new()).unwrap();
    assert_eq!(rows(&cells), [[0, 0], [1, 1], [2, 2]]);
    // A result of 1×2 joined with batch 2 alone keeps the 1×2 grid.
    let grid = combine(&[&a, &tags(2, 2)], &BTreeMap::new()).unwrap();
    let grid_tags: Vec<ItemTag> = grid.iter().map(|c| c.tag.clone()).collect();
    let joined = combine(&[&grid_tags, &tags(2, 2)], &BTreeMap::new()).unwrap();
    assert_eq!(rows(&joined), [[0, 0], [1, 1], [2, 0], [3, 1], [4, 0], [5, 1]]);
}

#[test]
fn empty_and_oversized_fan_outs() {
    assert!(combine(&[&tags(1, 2), &[]], &BTreeMap::new()).unwrap().is_empty());
    let big = tags(1, 40);
    assert!(combine(&[&big, &tags(2, 30)], &BTreeMap::new()).unwrap_err().contains("more than"));
    assert!(combine(&[&big, &tags(2, 25)], &BTreeMap::new()).is_ok());
    let mut bad = tags(1, 2);
    bad.push(tags(2, 1).remove(0));
    assert!(!consistent(&bad));
    assert!(combine(&[&bad], &BTreeMap::new()).is_err());
}

proptest! {
    #[test]
    fn cross_is_the_product_in_row_major_order(n in 0usize..20, m in 0usize..20) {
        let cells = combine(&[&tags(1, n), &tags(2, m)], &BTreeMap::new()).unwrap();
        prop_assert_eq!(cells.len(), n * m);
        for (index, cell) in cells.iter().enumerate() {
            prop_assert_eq!(&cell.rows, &vec![index / m.max(1), index % m.max(1)]);
        }
    }

    #[test]
    fn zip_pairs_equal_sizes_and_refuses_others(n in 0usize..20, m in 0usize..20) {
        let zip: BTreeMap<_, _> = [(batch(1), Pairing::Zip), (batch(2), Pairing::Zip)].into();
        let result = combine(&[&tags(1, n), &tags(2, m)], &zip);
        if n == m || n == 0 || m == 0 {
            let cells = result.unwrap();
            prop_assert_eq!(cells.len(), if n == m { n } else { 0 });
        } else {
            prop_assert!(result.is_err());
        }
    }
}
