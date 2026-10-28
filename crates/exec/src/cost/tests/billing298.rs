//! Person-confirmed settlement of ordinary unresolved charges (#298).
use super::*;
use nodecraft_types::{ProviderInstanceId, ProviderTarget};

/// A log with one ordinary unresolved charge "lost" (5) and an open estimate "running" (3), both
/// in session "now".
fn unresolved() -> UsageLog {
    let mut log = UsageLog::default();
    log.record(charge("lost", 5, Charge::Estimate)).unwrap();
    log.settle("fal", "lost", usd(5), Charge::Unresolved).unwrap();
    log.record(charge("running", 3, Charge::Estimate)).unwrap();
    log.record(charge("done", 2, Charge::Actual)).unwrap();
    log
}

fn decide(log: &UsageLog, key: &str, resolution: Resolution) -> BillingDecision {
    let charge = log.unresolved_charges().into_iter().find(|c| c.entry.idempotency_key == key).unwrap();
    BillingDecision {
        provider: "fal".into(),
        instance: None,
        idempotency_key: key.into(),
        token: charge.token,
        resolution,
        note: Some("invoice line 4".into()),
    }
}

fn reload(log: &UsageLog) -> UsageLog {
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(log).unwrap()).unwrap();
    assert_eq!(&back, log);
    back
}

fn spent(log: &UsageLog) -> (u64, u64) {
    let spent = log.reported_spent("now");
    (spent.session.get(&Currency::USD).map_or(0, |m| m.micros), spent.project.get(&Currency::USD).map_or(0, |m| m.micros))
}

#[test]
fn only_unresolved_established_charges_are_offered() {
    let log = unresolved();
    let offered: Vec<_> = log.unresolved_charges().iter().map(|c| (c.entry.idempotency_key.clone(), c.location)).collect();
    assert_eq!(offered, [("lost".to_owned(), QuarantineLocation::Listed)], "not the running estimate, nor the actual charge");
    // A log without decisions writes no decision field, so older readers still load it.
    assert!(!serde_json::to_string(&log).unwrap().contains("billing_resolutions"));
}

#[test]
fn a_person_settles_an_unresolved_charge_at_the_billed_amount_once() {
    let mut log = unresolved();
    assert_eq!(spent(&log), (10, 10));
    let decision = decide(&log, "lost", Resolution::Settled { cost: usd(7) });
    let before = log.clone();
    let plan = log.plan_billing_resolution(&decision, 9).unwrap();
    assert_eq!(log, before, "planning changes nothing");
    let record = log.resolve_unresolved(&decision, 9).unwrap();
    assert_eq!(record, plan);
    assert_eq!((record.was, record.session.as_str(), record.recorded_at_ms, record.at_ms), (usd(5), "now", 1, 9));
    assert_eq!(record.note.as_deref(), Some("invoice line 4"));
    assert_eq!(log.charge("fal", "lost").map(|e| (e.cost, e.status)), Some((usd(7), Charge::Actual)));
    assert_eq!(spent(&log), (12, 12), "the estimate is replaced by the billed amount");
    assert!(log.unresolved_charges().is_empty());

    let mut log = reload(&log);
    assert_eq!(log.billing_resolutions().cloned().collect::<Vec<_>>(), [record]);
    // Final: the same review confirmed again finds nothing, and changes nothing.
    let after = log.clone();
    assert_eq!(log.resolve_unresolved(&decision, 10).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(log, after);
    // Nor can the settled charge be released or settled again some other way.
    assert_eq!(log.release("fal", "lost").unwrap_err().kind, ErrorKind::Conflict);
}

#[test]
fn a_person_releases_a_charge_that_was_never_billed_and_its_key_can_charge_again() {
    let mut log = unresolved();
    let decision = decide(&log, "lost", Resolution::Released);
    let record = log.resolve_unresolved(&decision, 9).unwrap();
    assert_eq!(record.resolution, Resolution::Released);
    assert!(log.charge("fal", "lost").is_none());
    assert_eq!(spent(&log), (5, 5), "it leaves every total");
    assert_eq!(log.resolve_unresolved(&decision, 10).unwrap_err().kind, ErrorKind::NotFound, "never twice");

    // A later job charging under the same key is a new charge with a new review token.
    log.record(UsageEntry { at_ms: 50, ..charge("lost", 5, Charge::Estimate) }).unwrap();
    log.settle("fal", "lost", usd(5), Charge::Unresolved).unwrap();
    let again = decide(&log, "lost", Resolution::Released);
    assert_ne!(again.token, decision.token);
    assert_eq!(log.resolve_unresolved(&decision, 11).unwrap_err().kind, ErrorKind::Conflict, "the old review doesn't decide the new charge");
    log.resolve_unresolved(&again, 12).unwrap();
    assert_eq!(reload(&log).billing_resolutions().len(), 2);
}

#[test]
fn a_stale_review_is_refused_and_changes_nothing() {
    let mut log = unresolved();
    let decision = decide(&log, "lost", Resolution::Released);
    // The charge changes after the review: kept unresolved at a different amount.
    log.settle("fal", "lost", usd(6), Charge::Unresolved).unwrap();
    let before = log.clone();
    assert_eq!(log.resolve_unresolved(&decision, 9).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log, before);
    // A run that settled it meanwhile leaves nothing to decide.
    log.settle("fal", "lost", usd(4), Charge::Actual).unwrap();
    let before = log.clone();
    let fresh = BillingDecision { token: [0; 32], ..decision };
    assert_eq!(log.resolve_unresolved(&fresh, 9).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(log, before);
}

#[test]
fn bad_decisions_are_refused_before_anything_changes() {
    let mut log = unresolved();
    let before = log.clone();
    let good = decide(&log, "lost", Resolution::Settled { cost: usd(1) });
    for (decision, kind) in [
        (BillingDecision { resolution: Resolution::Settled { cost: Money::new(Currency::EUR, 1) }, ..good.clone() }, ErrorKind::CurrencyMismatch),
        (BillingDecision { resolution: Resolution::Settled { cost: usd(MAX_CHARGE_MICROS + 1) }, ..good.clone() }, ErrorKind::InvalidInput),
        (BillingDecision { note: Some("x".repeat(MAX_BILLING_NOTE + 1)), ..good.clone() }, ErrorKind::InvalidInput),
        (BillingDecision { note: Some("line\nbreak".into()), ..good.clone() }, ErrorKind::InvalidInput),
        (BillingDecision { idempotency_key: String::new(), ..good.clone() }, ErrorKind::InvalidInput),
        (BillingDecision { idempotency_key: "running".into(), ..good.clone() }, ErrorKind::NotFound),
        (BillingDecision { provider: "other".into(), ..good.clone() }, ErrorKind::NotFound),
        (BillingDecision { instance: Some(ProviderInstanceId::from_bytes([1; 16])), ..good.clone() }, ErrorKind::NotFound),
    ] {
        assert_eq!(log.resolve_unresolved(&decision, 9).unwrap_err().kind, kind, "{decision:?}");
        assert_eq!(log, before);
    }
}

#[test]
fn a_charge_kept_after_leaving_the_list_and_an_instance_scoped_one_are_decided_by_identity() {
    let target = ProviderTarget::named("local", ProviderInstanceId::from_bytes([4; 16])).unwrap();
    let scoped = UsageEntry { provider: "local".into(), instance: target.instance(), ..charge("same", 2, Charge::Estimate) };
    let mut file = UsageLogFile {
        provider_identity_version: 1,
        open_session_ownership: 1,
        entries: vec![UsageEntry { status: Charge::Unresolved, ..scoped }],
        pending: vec![charge("gone", 3, Charge::Unresolved)],
        ..UsageLogFile::default()
    };
    file.project = BTreeMap::from([(Currency::USD, usd(5))]);
    file.sessions = BTreeMap::from([("now".into(), BTreeMap::from([(Currency::USD, usd(5))]))]);
    let mut log = UsageLog::try_from(file).unwrap();
    let offered: Vec<_> = log.unresolved_charges().iter().map(|c| (c.entry.idempotency_key.clone(), c.entry.instance, c.location)).collect();
    assert_eq!(offered, [("same".into(), target.instance(), QuarantineLocation::Listed), ("gone".into(), None, QuarantineLocation::Pending)]);
    let tokens: Vec<_> = log.unresolved_charges().iter().map(|c| c.token).collect();
    let scoped = BillingDecision {
        provider: "local".into(),
        instance: target.instance(),
        idempotency_key: "same".into(),
        token: tokens[0],
        resolution: Resolution::Released,
        note: None,
    };
    // The same key on the default instance is another charge.
    assert_eq!(log.resolve_unresolved(&BillingDecision { instance: None, ..scoped.clone() }, 1).unwrap_err().kind, ErrorKind::NotFound);
    log.resolve_unresolved(&scoped, 1).unwrap();
    let pending = BillingDecision {
        provider: "fal".into(),
        instance: None,
        idempotency_key: "gone".into(),
        token: tokens[1],
        resolution: Resolution::Settled { cost: usd(3) },
        note: None,
    };
    log.resolve_unresolved(&pending, 2).unwrap();
    assert!(log.pending().next().is_none(), "settled for good, off the pending list");
    assert_eq!(spent(&log), (3, 3));
    let back = reload(&log);
    assert_eq!(back.billing_resolutions().map(|r| r.instance).collect::<Vec<_>>(), [target.instance(), None]);
}

#[test]
fn reconciled_and_quarantined_charges_stay_on_their_own_path() {
    // Quarantined: never offered here.
    let legacy: UsageLog = serde_json::from_str(include_str!("../fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    assert!(legacy.unresolved_charges().is_empty());
    // Reconciled and unresolved: decided with `resolve_charge` (#332), not here.
    let mut log = legacy;
    let decision = OwnershipDecision {
        session: "a".into(),
        currency: Currency::USD,
        token: log.ownership_group("a", Currency::USD).unwrap().token,
        holder: Holder::Named,
        except: Vec::new(),
        only: None,
    };
    log.reconcile_ownership(&decision, 1).unwrap();
    log.settle("t", "old", usd(5), Charge::Unresolved).unwrap();
    assert_eq!(log.resolvable_charges().len(), 1);
    assert!(log.unresolved_charges().is_empty(), "no charge is on both paths");
}

#[test]
fn saved_decisions_are_checked_on_load() {
    let mut log = unresolved();
    let decision = decide(&log, "lost", Resolution::Settled { cost: usd(7) });
    log.resolve_unresolved(&decision, 9).unwrap();
    let file: UsageLogFile = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(file.billing_resolutions.len(), 1);
    let refuse = |change: &dyn Fn(&mut BillingResolution)| {
        let mut bad = file.clone();
        change(&mut bad.billing_resolutions[0]);
        assert!(UsageLog::try_from(bad).is_err());
    };
    refuse(&|r| r.resolution = Resolution::Settled { cost: Money::new(Currency::EUR, 7) });
    refuse(&|r| r.resolution = Resolution::Settled { cost: usd(MAX_CHARGE_MICROS + 1) });
    refuse(&|r| r.was = usd(MAX_CHARGE_MICROS + 1));
    refuse(&|r| r.idempotency_key.clear());
    refuse(&|r| r.session = "s".repeat(MAX_TEXT + 1));
    refuse(&|r| r.note = Some("\u{7}".into()));
    refuse(&|r| r.instance = Some(ProviderInstanceId::from_bytes([1; 16])));
    let mut many = file.clone();
    many.billing_resolutions = vec![file.billing_resolutions[0].clone(); MAX_BILLING_RESOLUTIONS + 1];
    assert!(UsageLog::try_from(many).is_err(), "bounded");
    // Unknown fields in a record are refused too, never dropped.
    let mut text: serde_json::Value = serde_json::to_value(&file).unwrap();
    text["billing_resolutions"][0]["extra"] = serde_json::json!(1);
    assert!(serde_json::from_value::<UsageLog>(text).is_err());
}
