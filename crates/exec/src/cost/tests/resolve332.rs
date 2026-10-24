//! Person-confirmed settlement of reconciled legacy charges (#332).
use super::*;

fn reconciled() -> UsageLog {
    let mut log: UsageLog = serde_json::from_str(include_str!("../fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    let decision = OwnershipDecision {
        session: "a".into(),
        currency: Currency::USD,
        token: log.ownership_group("a", Currency::USD).unwrap().token,
        holder: Holder::Earlier,
        except: Vec::new(),
        only: None,
    };
    log.reconcile_ownership(&decision, 1).unwrap();
    log
}

fn reload(log: &UsageLog) -> UsageLog {
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(log).unwrap()).unwrap();
    assert_eq!(&back, log);
    back
}

fn decide(log: &UsageLog, resolution: Resolution) -> ResolutionDecision {
    let charge = log.resolvable_charges()[0];
    ResolutionDecision { provider: "t".into(), idempotency_key: "old".into(), token: charge.token, resolution }
}

fn file(log: &UsageLog) -> UsageLogFile {
    serde_json::from_str(&serde_json::to_string(log).unwrap()).unwrap()
}

#[test]
fn a_reconciled_charge_settles_at_the_billed_amount_once_and_is_kept_on_record() {
    let mut log = reconciled();
    let charges = log.resolvable_charges();
    assert_eq!(charges.len(), 1);
    assert_eq!((charges[0].holder, charges[0].reconciled_at_ms, charges[0].entry.cost), (Holder::Earlier, 1, usd(5)));
    assert!(log.resolvable_charges().iter().all(|c| c.location == QuarantineLocation::Listed));

    let decision = decide(&log, Resolution::Settled { cost: usd(3) });
    let before = log.clone();
    let plan = log.plan_charge_resolution(&decision, 7).unwrap();
    assert_eq!(log, before, "planning changes nothing");
    let record = log.resolve_charge(&decision, 7).unwrap();
    assert_eq!(record, plan);
    assert_eq!((record.was, record.was_status, record.session.as_str(), record.at_ms), (usd(5), Charge::Estimate, "a", 7));
    assert_eq!(log.charge("t", "old").unwrap().status, Charge::Actual);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(8)));
    assert_eq!(log.reported_spent("a").project.get(&Currency::USD), Some(&usd(8)));
    assert!(log.resolvable_charges().is_empty());
    assert_eq!(file(&log).open_session_ownership, 4);

    let mut log = reload(&log);
    assert_eq!(log.charge_resolutions(), [record]);
    // Final: the same decision again finds nothing to decide.
    assert_eq!(log.resolve_charge(&decision, 8).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn a_person_can_release_even_an_unresolved_charge() {
    let mut log = reconciled();
    log.settle("t", "old", usd(5), Charge::Unresolved).unwrap();
    // The automatic path refuses: an unresolved charge may have been billed.
    assert_eq!(log.clone().release("t", "old").unwrap_err().kind, ErrorKind::Conflict);
    let record = log.resolve_charge(&decide(&log, Resolution::Released), 2).unwrap();
    assert_eq!(record.was_status, Charge::Unresolved);
    assert!(log.charge("t", "old").is_none());
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(5)));
    let log = reload(&log);
    // The freed key can be charged again by a new job without making the log unreadable.
    let mut log = log;
    log.record(estimate("a", "old", 1)).unwrap();
    let log = reload(&log);
    assert!(log.resolvable_charges().is_empty(), "a resolved charge isn't offered again");
}

#[test]
fn a_stale_or_impossible_decision_changes_nothing() {
    let mut log = reconciled();
    let stale = decide(&log, Resolution::Released);
    log.settle("t", "old", usd(4), Charge::Unresolved).unwrap();
    let before = log.clone();
    assert_eq!(log.resolve_charge(&stale, 1).unwrap_err().kind, ErrorKind::Conflict);
    let euro = decide(&log, Resolution::Settled { cost: Money::new(Currency::EUR, 1) });
    assert_eq!(log.resolve_charge(&euro, 1).unwrap_err().kind, ErrorKind::CurrencyMismatch);
    let huge = decide(&log, Resolution::Settled { cost: usd(MAX_CHARGE_MICROS + 1) });
    assert_eq!(log.resolve_charge(&huge, 1).unwrap_err().kind, ErrorKind::InvalidInput);
    let mut other = decide(&log, Resolution::Released);
    other.idempotency_key = "new-actual".into();
    assert_eq!(log.resolve_charge(&other, 1).unwrap_err().kind, ErrorKind::NotFound, "only reconciled charges");
    assert_eq!(log, before);
    assert!(log.charge_resolutions().is_empty());
}

#[test]
fn quarantined_charges_wait_for_their_ownership_first() {
    let log: UsageLog = serde_json::from_str(include_str!("../fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    assert!(log.resolvable_charges().is_empty());
}

#[test]
fn a_saved_log_with_forged_resolutions_is_refused() {
    let mut log = reconciled();
    log.resolve_charge(&decide(&log, Resolution::Released), 2).unwrap();
    let good = file(&log);
    let mut cases: Vec<(&str, UsageLogFile)> = Vec::new();
    let mut f = good.clone();
    f.charge_resolutions.push(f.charge_resolutions[0].clone());
    cases.push(("twice", f));
    let mut f = good.clone();
    f.charge_resolutions[0].idempotency_key = "new-actual".into();
    cases.push(("not reconciled", f));
    let mut f = good.clone();
    f.charge_resolutions[0].was = Money::new(Currency::EUR, 5);
    cases.push(("other currency", f));
    let mut f = good.clone();
    f.charge_resolutions[0].was_status = Charge::Actual;
    cases.push(("was settled", f));
    let mut f = good.clone();
    f.charge_resolutions[0].resolution = Resolution::Settled { cost: usd(MAX_CHARGE_MICROS + 1) };
    cases.push(("too large", f));
    let mut f = good.clone();
    f.open_session_ownership = 3;
    cases.push(("version 3", f));
    let mut f = good.clone();
    f.charge_resolutions.clear();
    cases.push(("version 4 without any", f));
    for (name, f) in cases {
        assert!(UsageLog::try_from(f).is_err(), "{name}");
    }
    assert!(UsageLog::try_from(good).is_ok());
}
