use super::*;

fn report() -> serde_json::Value {
    json!({"nodes":[NodeId::from_bytes([1;16]),NodeId::from_bytes([2;16])],"unknown_total":2,
      "unknown":{"node":NodeId::from_bytes([1;16]),"cause":NodeId::from_bytes([3;16]),"kind":"unsupported","reason":"Unavailable"}})
}
#[test]
fn bounded_summary_preserves_checked_and_cause_and_overlapping_counts() {
    let text = summary(&report(), 2).unwrap();
    assert!(text.contains("2 stale results; 2 checked results"));
    assert!(text.contains(&format!("First invalid checked result: {}", NodeId::from_bytes([1; 16]))));
    assert!(text.contains(&format!("Failing input node: {}", NodeId::from_bytes([3; 16]))));
    assert!(text.contains("unsupported: Unavailable"));
    assert!(text.len() <= MAX_TEXT);
}
#[test]
fn zero_report_does_not_certify_unrecorded_results() {
    let text = summary(&json!({"nodes":[],"unknown_total":0,"unknown":null}), 10).unwrap();
    assert!(text.contains("No stale or locally invalid checked results reported"));
    assert!(text.contains("Unrecorded results are not verified fresh"));
    assert!(!text.contains("all fresh"));
}
#[test]
fn malformed_reports_are_refused_instead_of_zero_success() {
    for bad in [json!(null), json!({}), json!({"nodes":[],"unknown_total":0}), json!({"nodes":[],"unknown_total":1,"unknown":null})] {
        assert!(summary(&bad, 2).is_err());
    }
    for (field, value) in [("node", json!("bad")), ("cause", json!("x".repeat(10000))), ("kind", json!("x".repeat(10000))), ("reason", json!("x".repeat(513)))]
    {
        let mut value_report = report();
        value_report["unknown"][field] = value;
        assert!(summary(&value_report, 2).is_err());
    }
    let mut value = report();
    value["unknown_total"] = json!(3);
    assert!(summary(&value, 2).is_err());
    value = report();
    value["unknown_total"] = json!(0);
    assert!(summary(&value, 2).is_err());
    assert!(summary(&report(), 1).is_err());
}
#[test]
fn utf8_and_controls_are_bounded_and_normalized() {
    let mut value = report();
    value["unknown"]["reason"] = json!(format!("{}\n\u{202e}\t", "é".repeat(254)));
    // 508+1+3+1 exceeds the independently enforced 512-byte reason limit.
    assert!(summary(&value, 2).is_err());
    value["unknown"]["reason"] = json!(format!("{}\n\u{202e}\t", "é".repeat(250)));
    let text = summary(&value, 2).unwrap();
    assert!(!text.contains('\u{202e}'));
    assert!(!text.contains('\t'));
    assert!(text.len() <= MAX_TEXT);
}
