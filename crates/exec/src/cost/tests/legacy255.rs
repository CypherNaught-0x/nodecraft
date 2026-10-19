use super::*;
const FOLDED: &str = include_str!("../fixtures/legacy-folded-open.json");
#[test]
fn legacy_folded_release_preserves_new_same_session_reservation() {
    let mut log: UsageLog = serde_json::from_str(FOLDED).unwrap();
    log.record(estimate("a", "new", 5)).unwrap();
    log.release("t", "old").unwrap();
    assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(5)));
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(5)));
}

const AMBIGUOUS: &str = include_str!("../fixtures/legacy-folded-open-reused-actual.json");
fn reload(log: UsageLog) -> UsageLog {
    let json = serde_json::to_string(&log).unwrap();
    assert_eq!(serde_json::from_str::<UsageLogFile>(&json).unwrap().open_session_ownership, if log.quarantine_entries().next().is_some() { 2 } else { 1 });
    let loaded = serde_json::from_str(&json).unwrap();
    assert_eq!(log, loaded);
    loaded
}
#[test]
fn legacy_folded_settlement_normalization_is_durable_for_smaller_equal_and_larger_costs() {
    for actual in [2, 5, 8] {
        for status in [Charge::Actual, Charge::Unresolved] {
            let mut log = reload(serde_json::from_str(FOLDED).unwrap());
            assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(5)));
            log.record(estimate("a", "new", 7)).unwrap();
            let mut log = reload(log);
            log.settle("t", "old", usd(actual), status).unwrap();
            let mut log = reload(log);
            assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(actual + 7)));
            assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(actual + 7)));
            assert!(!log.record(estimate("a", "old", 5)).unwrap(), "same identity is still charged once");
            if status == Charge::Unresolved {
                let before = log.clone();
                assert!(log.release("t", "old").is_err());
                assert_eq!(log, before);
            }
        }
    }
}
#[test]
fn ambiguous_legacy_reused_closed_spend_is_retained_but_refused() {
    let original = AMBIGUOUS.as_bytes().to_vec();
    let mut uncertain: UsageLog = serde_json::from_str(AMBIGUOUS).unwrap();
    let before = uncertain.clone();
    assert!(uncertain.release("t", "old").is_err());
    assert_eq!(uncertain, before);
    assert_eq!(uncertain.quarantine_entries().count(), 1);
    assert_eq!(reload(uncertain), before);
    assert_eq!(AMBIGUOUS.as_bytes(), original);
    // Identical totals can also describe an owned open charge and unrelated folded closed spend.
    // Legacy wire cannot distinguish them; this is an intentional conservative false positive.
    let mut file: UsageLogFile = serde_json::from_str(AMBIGUOUS).unwrap();
    file.open_session_ownership = 1;
    let log = UsageLog::try_from(file).unwrap();
    let mut log = reload(log);
    log.release("t", "old").unwrap();
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(5)));
    assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(0)));
}
#[test]
fn legacy_multiple_listed_pending_keys_and_currencies_keep_closed_earlier_totals() {
    let mut file: UsageLogFile = serde_json::from_str(FOLDED).unwrap();
    let eur = |micros| Money::new(Currency::EUR, micros);
    file.entries.push(UsageEntry { cost: eur(3), ..estimate("a", "eur", 0) });
    file.pending.push(estimate("b", "pending", 4));
    file.entries.push(estimate("a", "second", 2));
    file.project = BTreeMap::from([(Currency::USD, usd(20)), (Currency::EUR, eur(7))]);
    file.sessions = BTreeMap::from([(EARLIER_SESSIONS.into(), file.project.clone())]);
    let mut log = reload(UsageLog::try_from(file).unwrap());
    assert_eq!(log.spent("a").session, BTreeMap::from([(Currency::USD, usd(7)), (Currency::EUR, eur(3))]));
    assert_eq!(log.spent("b").session.get(&Currency::USD), Some(&usd(4)));
    assert_eq!(log.spent(EARLIER_SESSIONS).session, BTreeMap::from([(Currency::USD, usd(9)), (Currency::EUR, eur(4))]));
    log.record(estimate("a", "new", 6)).unwrap();
    log.release("t", "old").unwrap();
    assert!(log.record(estimate("a", "old", 1)).unwrap(), "released key can be reused");
    assert!(!log.record(estimate("a", "old", 100)).unwrap(), "duplicate cannot change its amount");
    log.release("t", "pending").unwrap();
    log.settle("t", "eur", eur(4), Charge::Actual).unwrap();
    log.settle("t", "second", usd(1), Charge::Actual).unwrap();
    let log = reload(log);
    assert_eq!(log.spent("a").session, BTreeMap::from([(Currency::USD, usd(8)), (Currency::EUR, eur(4))]));
    assert_eq!(log.spent("a").project, BTreeMap::from([(Currency::USD, usd(17)), (Currency::EUR, eur(8))]));
    assert_eq!(log.spent(EARLIER_SESSIONS).session, BTreeMap::from([(Currency::USD, usd(9)), (Currency::EUR, eur(4))]));
}
#[test]
fn ownership_marker_and_aggregate_coverage_are_checked_before_admission() {
    let file: UsageLogFile = serde_json::from_str(FOLDED).unwrap();
    assert_eq!(file.open_session_ownership, 0);
    let mut invalid = file.clone();
    invalid.open_session_ownership = 1;
    assert!(UsageLog::try_from(invalid).unwrap_err().message.contains("named session"));
    let mut invalid = file.clone();
    invalid.open_session_ownership = 4;
    assert!(UsageLog::try_from(invalid).unwrap_err().message.contains("newer NodeCraft"));
    let mut invalid = file.clone();
    invalid.open_session_ownership = 1;
    invalid.entries.push(estimate("a", "second", 5));
    invalid.project.insert(Currency::USD, usd(10));
    invalid.sessions.insert("a".into(), BTreeMap::from([(Currency::USD, usd(5))]));
    assert!(UsageLog::try_from(invalid).is_err(), "aggregate opens exceed named own total, despite earlier coverage");
    let mut invalid = file.clone();
    invalid.sessions.insert(EARLIER_SESSIONS.into(), BTreeMap::from([(Currency::USD, usd(4))]));
    assert!(UsageLog::try_from(invalid).is_err());
    let mut invalid = file.clone();
    invalid.project.insert(Currency::USD, usd(MAX_TOTAL_MICROS + 1));
    invalid.sessions.insert(EARLIER_SESSIONS.into(), invalid.project.clone());
    assert!(UsageLog::try_from(invalid).is_err());
    let mut zero = file;
    zero.entries.first_mut().unwrap().cost = usd(0);
    zero.project.clear();
    zero.sessions.clear();
    let mut log = reload(UsageLog::try_from(zero).unwrap());
    log.settle("t", "old", usd(3), Charge::Unresolved).unwrap();
    assert_eq!(reload(log).spent("a").session.get(&Currency::USD), Some(&usd(3)));
}

#[test]
fn legacy_normalization_cannot_expand_beyond_the_session_bound() {
    let mut file: UsageLogFile = serde_json::from_str(FOLDED).unwrap();
    let limit = MAX_SESSIONS + 1 + MAX_USAGE_ENTRIES + MAX_PENDING;
    for n in 1..limit {
        file.sessions.insert(format!("closed-{n}"), BTreeMap::new());
    }
    assert_eq!(file.sessions.len(), limit);
    let error = UsageLog::try_from(file).unwrap_err();
    assert!(error.message.contains("too many sessions after legacy ownership normalization"));
}
