//! Person-confirmed reconciliation of quarantined legacy ownership (#325).
use super::*;

fn ambiguous() -> UsageLog {
    serde_json::from_str(include_str!("../fixtures/legacy-folded-open-reused-actual.json")).unwrap()
}

fn reload(log: &UsageLog) -> UsageLog {
    let json = serde_json::to_string(log).unwrap();
    let back: UsageLog = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, log);
    back
}

fn marker(log: &UsageLog) -> u8 {
    serde_json::from_str::<UsageLogFile>(&serde_json::to_string(log).unwrap()).unwrap().open_session_ownership
}

fn decide(log: &UsageLog, session: &str, holder: Holder, except: &[&str]) -> OwnershipDecision {
    OwnershipDecision {
        session: session.into(),
        currency: Currency::USD,
        token: log.ownership_group(session, Currency::USD).unwrap().token,
        holder,
        except: except.iter().map(|k| ("t".to_owned(), (*k).to_owned())).collect(),
    }
}

/// A legacy (marker 0) log: open estimates for `session`, with named and earlier totals.
fn legacy(opens: &[(&str, &str, u64)], named: &[(&str, u64)], earlier: u64) -> UsageLog {
    let mut file = UsageLogFile::default();
    let mut project = 0;
    for (session, key, cost) in opens {
        file.entries.push(estimate(session, key, *cost));
        let _ = session;
        project += cost;
    }
    let mut sessions = BTreeMap::new();
    let mut total = earlier;
    for (name, micros) in named {
        sessions.insert((*name).to_owned(), BTreeMap::from([(Currency::USD, usd(*micros))]));
        total += micros;
    }
    sessions.insert(EARLIER_SESSIONS.to_owned(), BTreeMap::from([(Currency::USD, usd(earlier))]));
    assert!(total >= project);
    file.project = BTreeMap::from([(Currency::USD, usd(total))]);
    file.sessions = sessions;
    UsageLog::try_from(file).unwrap()
}

#[test]
fn earlier_held_charge_becomes_established_and_settles_without_touching_the_project() {
    let mut log = ambiguous();
    let group = log.ownership_group("a", Currency::USD).unwrap();
    assert_eq!(group.charges.len(), 1);
    assert_eq!((group.named_total, group.earlier_total, group.established_open), (usd(5), usd(5), usd(0)));
    assert_eq!((group.max_named, group.max_earlier), (usd(5), usd(5)));
    assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(10)), "conservative while quarantined");
    let decision = decide(&log, "a", Holder::Earlier, &[]);
    let record = log.reconcile_ownership(&decision, 42).unwrap();
    assert_eq!(record.moved_from_earlier, usd(5));
    assert_eq!(record.at_ms, 42);
    assert_eq!(record.charges, [ReconciledCharge { provider: "t".into(), idempotency_key: "old".into(), cost: usd(5), holder: Holder::Earlier }]);
    assert_eq!(log.ownership("t", "old").unwrap(), Some(ChargeOwnership::Established));
    assert_eq!(log.ownership_groups().count(), 0);
    assert!(!log.budget_view("a").has_uncertain_session_charges);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(10)));
    assert_eq!(log.reported_spent(EARLIER_SESSIONS).session.get(&Currency::USD), Some(&usd(0)));
    assert_eq!(log.reported_spent("a").project.get(&Currency::USD), Some(&usd(10)), "reconciliation moves no money in or out");
    assert_eq!(log.charge("t", "old").unwrap().status, Charge::Estimate, "reconciliation settles nothing");
    assert_eq!(marker(&log), 3);
    let mut log = reload(&log);
    assert_eq!(log.ownership_reconciliations(), [record]);
    log.settle("t", "old", usd(2), Charge::Actual).unwrap();
    let log = reload(&log);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(7)));
    assert_eq!(log.reported_spent("a").project.get(&Currency::USD), Some(&usd(7)));
}

#[test]
fn named_held_charge_moves_nothing_and_can_be_released_afterwards() {
    let mut log = legacy(&[("a", "old", 4)], &[("a", 6)], 3);
    let group = log.ownership_group("a", Currency::USD).unwrap();
    assert_eq!((group.max_named, group.max_earlier), (usd(6), usd(3)));
    let before = log.clone();
    let earlier = decide(&log, "a", Holder::Earlier, &[]);
    assert_eq!(log.reconcile_ownership(&earlier, 1).unwrap_err().kind, ErrorKind::Conflict, "the earlier total held only 3");
    assert_eq!(log, before);
    let record = log.reconcile_ownership(&decide(&log, "a", Holder::Named, &[]), 1).unwrap();
    assert_eq!(record.moved_from_earlier, usd(0));
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(6)));
    assert_eq!(log.reported_spent(EARLIER_SESSIONS).session.get(&Currency::USD), Some(&usd(3)));
    let mut log = reload(&log);
    log.release("t", "old").unwrap();
    let log = reload(&log);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(2)));
}

#[test]
fn exceptions_split_a_group_and_bad_exceptions_change_nothing() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3), ("a", "three", 4)], &[("a", 5)], 6);
    assert_eq!(log.ownership_group("a", Currency::USD).unwrap().charges.len(), 3);
    let before = log.clone();
    for (except, kind) in [
        (vec!["missing"], ErrorKind::InvalidInput),
        (vec!["one", "one"], ErrorKind::InvalidInput),
        (vec![], ErrorKind::Conflict), // all 9 in the named total, which held 5
    ] {
        assert_eq!(log.reconcile_ownership(&decide(&log, "a", Holder::Named, &except), 1).unwrap_err().kind, kind, "{except:?}");
        assert_eq!(log, before);
    }
    let too_many: Vec<String> = (0..=MAX_RECONCILE_EXCEPTIONS).map(|n| n.to_string()).collect();
    let mut decision = decide(&log, "a", Holder::Named, &[]);
    decision.except = too_many.into_iter().map(|k| ("t".to_owned(), k)).collect();
    assert_eq!(log.reconcile_ownership(&decision, 1).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(log, before);
    // "one" and "two" (5) were in the named total; "three" (4) was folded.
    let record = log.reconcile_ownership(&decide(&log, "a", Holder::Named, &["three"]), 1).unwrap();
    assert_eq!(record.moved_from_earlier, usd(4));
    let holders: BTreeMap<&str, Holder> = record.charges.iter().map(|c| (c.idempotency_key.as_str(), c.holder)).collect();
    assert_eq!(holders, BTreeMap::from([("one", Holder::Named), ("two", Holder::Named), ("three", Holder::Earlier)]));
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(9)));
    assert_eq!(log.reported_spent(EARLIER_SESSIONS).session.get(&Currency::USD), Some(&usd(2)));
    let mut log = reload(&log);
    for key in ["one", "two", "three"] {
        log.settle("t", key, usd(1), Charge::Actual).unwrap();
    }
    assert_eq!(reload(&log).reported_spent("a").project.get(&Currency::USD), Some(&usd(5)));
}

#[test]
fn a_changed_ledger_invalidates_the_reviewed_token() {
    let mut log = ambiguous();
    let stale = decide(&log, "a", Holder::Earlier, &[]);
    log.record(estimate("a", "new", 1)).unwrap();
    let before = log.clone();
    assert_eq!(log.reconcile_ownership(&stale, 1).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log, before);
    let mut forged = decide(&log, "a", Holder::Earlier, &[]);
    forged.token[0] ^= 1;
    assert_eq!(log.reconcile_ownership(&forged, 1).unwrap_err().kind, ErrorKind::Conflict);
    log.reconcile_ownership(&decide(&log, "a", Holder::Earlier, &[]), 1).unwrap();
    // A replay of the same decision finds no group.
    assert_eq!(log.reconcile_ownership(&stale, 1).unwrap_err().kind, ErrorKind::NotFound);
    let mut other = decide(&ambiguous(), "a", Holder::Earlier, &[]);
    other.currency = Currency::EUR;
    assert_eq!(log.reconcile_ownership(&other, 1).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn groups_sharing_the_earlier_total_cannot_both_claim_it_and_each_step_reloads() {
    let mut log = legacy(&[("a", "a1", 4), ("b", "b1", 4)], &[("a", 4), ("b", 4)], 5);
    assert_eq!(log.ownership_groups().collect::<Vec<_>>(), [("a", Currency::USD), ("b", Currency::USD)]);
    log.reconcile_ownership(&decide(&log, "a", Holder::Earlier, &[]), 1).unwrap();
    let mut log = reload(&log);
    let b = log.ownership_group("b", Currency::USD).unwrap();
    assert_eq!(b.max_earlier, usd(1), "a already moved 4 of the 5 the earlier total held");
    let before = log.clone();
    assert_eq!(log.reconcile_ownership(&decide(&log, "b", Holder::Earlier, &[]), 1).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log, before);
    log.reconcile_ownership(&decide(&log, "b", Holder::Named, &[]), 1).unwrap();
    let log = reload(&log);
    assert_eq!(log.ownership_reconciliations().len(), 2);
    assert_eq!(marker(&log), 3, "records keep the marker even with nothing quarantined");
    assert_eq!(log.quarantine_entries().count(), 0);
}

#[test]
fn currencies_are_reconciled_separately() {
    let eur = |micros| Money::new(Currency::EUR, micros);
    let mut file = UsageLogFile::default();
    file.entries.push(estimate("a", "usd", 3));
    file.entries.push(UsageEntry { cost: eur(2), ..estimate("a", "eur", 0) });
    file.project = BTreeMap::from([(Currency::USD, usd(8)), (Currency::EUR, eur(6))]);
    file.sessions = BTreeMap::from([
        ("a".into(), BTreeMap::from([(Currency::USD, usd(4)), (Currency::EUR, eur(3))])),
        (EARLIER_SESSIONS.into(), BTreeMap::from([(Currency::USD, usd(4)), (Currency::EUR, eur(3))])),
    ]);
    let mut log = UsageLog::try_from(file).unwrap();
    assert_eq!(log.ownership_groups().count(), 2);
    log.reconcile_ownership(&decide(&log, "a", Holder::Earlier, &[]), 1).unwrap();
    let mut log = reload(&log);
    assert_eq!(log.ownership("t", "usd").unwrap(), Some(ChargeOwnership::Established));
    assert_eq!(log.ownership("t", "eur").unwrap(), Some(ChargeOwnership::LegacyUncertain));
    assert!(log.settle("t", "eur", eur(1), Charge::Actual).is_err(), "the other currency stays quarantined");
    assert_eq!(marker(&log), 3);
    let token = log.ownership_group("a", Currency::EUR).unwrap().token;
    let decision = OwnershipDecision { session: "a".into(), currency: Currency::EUR, token, holder: Holder::Named, except: vec![] };
    log.reconcile_ownership(&decision, 2).unwrap();
    let log = reload(&log);
    assert_eq!(log.reported_spent(EARLIER_SESSIONS).session, BTreeMap::from([(Currency::USD, usd(1)), (Currency::EUR, eur(3))]));
}

#[test]
fn evicted_quarantine_returns_to_pending_and_can_settle() {
    let mut file: UsageLogFile = serde_json::from_str(&serde_json::to_string(&ambiguous()).unwrap()).unwrap();
    assert_eq!(file.uncertain_listed, [0]);
    let old = file.entries.remove(0);
    file.uncertain_listed.clear();
    file.quarantined.push(old);
    let mut log = UsageLog::try_from(file).unwrap();
    let group = log.ownership_group("a", Currency::USD).unwrap();
    assert_eq!(group.charges.first().unwrap().location, QuarantineLocation::Evicted);
    log.reconcile_ownership(&decide(&log, "a", Holder::Earlier, &[]), 1).unwrap();
    assert_eq!(log.pending().map(|e| e.idempotency_key.as_str()).collect::<Vec<_>>(), ["old"]);
    let mut log = reload(&log);
    log.settle("t", "old", usd(1), Charge::Actual).unwrap();
    assert_eq!(log.pending().count(), 0);
    assert!(!log.record(estimate("a", "old", 9)).unwrap(), "the settled key is still recognised");
    assert_eq!(reload(&log).reported_spent("a").session.get(&Currency::USD), Some(&usd(6)));
}

#[test]
fn marker_two_files_still_load_and_save_unchanged() {
    let log = ambiguous();
    let wire = serde_json::to_string(&log).unwrap();
    assert!(!wire.contains("ownership_reconciliations"));
    let back: UsageLog = serde_json::from_str(&wire).unwrap();
    assert_eq!(serde_json::to_string(&back).unwrap(), wire);
    assert_eq!(marker(&back), 2);
}

fn charge<'a>(file: &'a mut UsageLogFile, key: &str) -> &'a mut ReconciledCharge {
    file.ownership_reconciliations[0].charges.iter_mut().find(|c| c.idempotency_key == key).unwrap()
}

type Edit = Box<dyn Fn(&mut UsageLogFile)>;

#[test]
fn tampered_reconciliation_records_are_refused() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3)], &[("a", 5)], 6);
    log.reconcile_ownership(&decide(&log, "a", Holder::Named, &["two"]), 7).unwrap();
    let good: UsageLogFile = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(good.open_session_ownership, 3);
    assert!(UsageLog::try_from(good.clone()).is_ok());
    let edits: Vec<(&str, Edit)> = vec![
        ("marker 2 with records", Box::new(|f| f.open_session_ownership = 2)),
        ("marker 1 with records", Box::new(|f| f.open_session_ownership = 1)),
        ("marker 3 without records", Box::new(|f| f.ownership_reconciliations.clear())),
        ("moved mismatch", Box::new(|f| f.ownership_reconciliations[0].moved_from_earlier = usd(2))),
        (
            "named beyond the import",
            Box::new(|f| {
                // Both in the named total (5 of 5), with the moved amount kept consistent: still
                // refused, since "two" is no longer counted as earlier-held money.
                charge(f, "two").holder = Holder::Named;
                charge(f, "one").cost = usd(3);
                f.ownership_reconciliations[0].moved_from_earlier = usd(0);
            }),
        ),
        (
            "earlier beyond the import",
            Box::new(|f| {
                charge(f, "one").holder = Holder::Earlier;
                charge(f, "two").cost = usd(5);
                f.ownership_reconciliations[0].moved_from_earlier = usd(7);
            }),
        ),
        ("empty record", Box::new(|f| f.ownership_reconciliations[0].charges.clear())),
        ("zero charge", Box::new(|f| f.ownership_reconciliations[0].charges[0].cost = usd(0))),
        ("currency mismatch", Box::new(|f| f.ownership_reconciliations[0].charges[0].cost = Money::new(Currency::EUR, 2))),
        (
            "duplicate charge",
            Box::new(|f| {
                let first = f.ownership_reconciliations[0].charges[0].clone();
                f.ownership_reconciliations[0].charges.push(first);
            }),
        ),
        (
            "duplicate record",
            Box::new(|f| {
                let first = f.ownership_reconciliations[0].clone();
                f.ownership_reconciliations.push(first);
            }),
        ),
        (
            "earlier total shrank",
            Box::new(|f| {
                // Keep the project the sum of its sessions.
                f.sessions.get_mut(EARLIER_SESSIONS).unwrap().insert(Currency::USD, usd(1));
                f.project.insert(Currency::USD, usd(9));
                f.ownership_reconciliations[0].evidence.original_project = usd(9);
            }),
        ),
    ];
    for (what, edit) in edits {
        let mut file = good.clone();
        edit(&mut file);
        assert!(UsageLog::try_from(file).is_err(), "{what}");
    }
}

#[test]
fn a_record_cannot_name_a_charge_that_is_still_quarantined() {
    let mut log = legacy(&[("a", "a1", 2), ("b", "b1", 2)], &[("a", 2), ("b", 2)], 4);
    log.reconcile_ownership(&decide(&log, "a", Holder::Named, &[]), 1).unwrap();
    let mut file: UsageLogFile = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert!(UsageLog::try_from(file.clone()).is_ok());
    let mut contradictory = file.clone();
    contradictory.ownership_reconciliations[0].evidence.original_project = usd(99);
    assert!(UsageLog::try_from(contradictory).is_err(), "evidence from one import must agree with the remaining group");
    file.ownership_reconciliations[0].charges[0].idempotency_key = "b1".into();
    file.ownership_reconciliations[0].evidence.session = "b".into();
    assert!(UsageLog::try_from(file).is_err());
}

#[test]
fn earlier_money_needed_by_a_folded_sessions_listed_charges_stays_put() {
    // "b" folded after its 5 was charged: its listed row is covered by the earlier total, so
    // none of that total can have held "a"'s open charge.
    let mut file = UsageLogFile::default();
    file.entries.push(estimate("a", "old", 5));
    file.entries.push(entry("b", "x", 5));
    file.project = BTreeMap::from([(Currency::USD, usd(10))]);
    file.sessions =
        BTreeMap::from([("a".into(), BTreeMap::from([(Currency::USD, usd(5))])), (EARLIER_SESSIONS.into(), BTreeMap::from([(Currency::USD, usd(5))]))]);
    let mut log = UsageLog::try_from(file).unwrap();
    assert_eq!(log.ownership_group("a", Currency::USD).unwrap().max_earlier, usd(0));
    let before = log.clone();
    assert_eq!(log.reconcile_ownership(&decide(&log, "a", Holder::Earlier, &[]), 1).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log, before);
    log.reconcile_ownership(&decide(&log, "a", Holder::Named, &[]), 1).unwrap();
    let mut log = reload(&log);
    log.settle("t", "old", usd(1), Charge::Actual).unwrap();
    reload(&log);
}

#[test]
fn one_groups_earlier_claim_cannot_strand_another_groups_listed_shortfall() {
    let mut file = UsageLogFile::default();
    file.entries.push(estimate("a", "a-open", 4));
    file.entries.push(estimate("b", "b-open", 4));
    file.entries.push(entry("b", "b-actual", 4));
    file.project = BTreeMap::from([(Currency::USD, usd(13))]);
    file.sessions = BTreeMap::from([
        ("a".into(), BTreeMap::from([(Currency::USD, usd(4))])),
        ("b".into(), BTreeMap::from([(Currency::USD, usd(4))])),
        (EARLIER_SESSIONS.into(), BTreeMap::from([(Currency::USD, usd(5))])),
    ]);
    let mut log = UsageLog::try_from(file).unwrap();
    assert_eq!(log.ownership_groups().count(), 2);
    assert_eq!(log.ownership_group("a", Currency::USD).unwrap().max_earlier, usd(1), "b's listed rows need 4 of the earlier 5");
    let before = log.clone();
    assert_eq!(log.reconcile_ownership(&decide(&log, "a", Holder::Earlier, &[]), 1).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log, before);
    // b's own shortfall shrinks as its money arrives, so b can take what it needs.
    assert_eq!(log.ownership_group("b", Currency::USD).unwrap().max_earlier, usd(5));
    log.reconcile_ownership(&decide(&log, "b", Holder::Earlier, &[]), 1).unwrap();
    let mut log = reload(&log);
    assert_eq!(log.ownership_group("a", Currency::USD).unwrap().max_earlier, usd(1));
    log.reconcile_ownership(&decide(&log, "a", Holder::Named, &[]), 2).unwrap();
    let mut log = reload(&log);
    log.settle("t", "b-open", usd(4), Charge::Actual).unwrap();
    log.release("t", "a-open").unwrap();
    reload(&log);
}

#[test]
fn a_dry_run_reports_the_record_and_totals_without_changing_anything() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3), ("a", "three", 4)], &[("a", 5)], 6);
    let decision = decide(&log, "a", Holder::Named, &["three"]);
    let before = log.clone();
    let plan = log.plan_ownership_reconciliation(&decision, 9).unwrap();
    assert_eq!(log, before);
    assert_eq!((plan.named_total, plan.earlier_total), (usd(9), usd(2)));
    assert_eq!(plan.record.moved_from_earlier, usd(4));
    let committed = log.reconcile_ownership(&decision, 9).unwrap();
    assert_eq!(committed, plan.record);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&plan.named_total));
    assert_eq!(log.reported_spent(EARLIER_SESSIONS).session.get(&Currency::USD), Some(&plan.earlier_total));
}

#[test]
fn group_sizes_are_counted_in_one_pass() {
    let log = legacy(&[("a", "a1", 2), ("a", "a2", 2), ("b", "b1", 2)], &[("a", 4), ("b", 2)], 6);
    assert_eq!(log.ownership_group_sizes(), BTreeMap::from([(("a", Currency::USD), 2), (("b", Currency::USD), 1)]));
}
