use super::*;

#[test]
fn results966_export_indices_count_every_leaf_and_keep_top_fan_provenance() {
    let value = Value::List(vec![
        Value::List(vec![Value::Text("prompt".into()), Value::List(vec![Value::Bool(true), Value::Text("first".into())])]),
        Value::List(vec![]),
        Value::List(vec![Value::Text("second".into())]),
    ]);
    let leaves = iter(&value, true, &mut Budget::default()).collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        leaves.iter().map(|leaf| (leaf.item, leaf.fan)).collect::<Vec<_>>(),
        vec![(Some(0), Some(0)), (Some(1), Some(0)), (Some(2), Some(0)), (Some(3), Some(2))]
    );
    for (i, expected) in leaves.iter().enumerate() {
        let actual = lookup(&value, true, Some(i), &mut Budget::default()).unwrap();
        assert!(std::ptr::eq(expected.value, actual.value));
        assert_eq!(actual.fan, expected.fan);
    }
    assert!(lookup(&value, false, None, &mut Budget::default()).is_err());
    assert!(lookup(&value, false, Some(4), &mut Budget::default()).is_err());
    assert!(iter(&value, false, &mut Budget::default()).all(|leaf| leaf.unwrap().fan.is_none()));
    let single = Value::Bool(false);
    assert_eq!(lookup(&single, false, None, &mut Budget::default()).unwrap().item, None);
    assert!(lookup(&single, false, Some(0), &mut Budget::default()).is_err());
}

#[test]
fn results966_export_limits_count_empty_containers_share_work_and_bound_depth() {
    let mut value = Value::Bool(true);
    for _ in 0..MAX_DEPTH {
        value = Value::List(vec![value]);
    }
    assert_eq!(iter(&value, false, &mut Budget::default()).collect::<Result<Vec<_>, _>>().unwrap().len(), 1);
    let too_deep = Value::List(vec![value]);
    assert!(iter(&too_deep, false, &mut Budget::default()).collect::<Result<Vec<_>, _>>().is_err());
    let wide = Value::List(vec![Value::List(vec![]); MAX_VISITED - 1]);
    let mut budget = Budget::default();
    assert!(iter(&wide, false, &mut budget).next().is_none());
    assert!(iter(&Value::Bool(true), false, &mut budget).next().unwrap().is_err());
    let excess = Value::List(vec![Value::List(vec![]); MAX_VISITED]);
    let mut budget = Budget::default();
    let mut leaves = iter(&excess, false, &mut budget);
    assert!(leaves.next().unwrap().is_err());
    assert!(leaves.next().is_none());
    assert!(lookup(&Value::List(vec![]), false, Some(usize::MAX), &mut Budget::default()).is_err());
}
