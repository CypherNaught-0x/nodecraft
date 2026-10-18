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
        only: None,
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
    let decision = OwnershipDecision { session: "a".into(), currency: Currency::EUR, token, holder: Holder::Named, except: vec![], only: None };
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

fn stage(log: &UsageLog, holder: Holder, except: &[&str], only: &[&str]) -> OwnershipDecision {
    let mut decision = decide(log, "a", holder, except);
    decision.only = Some(only.iter().map(|k| ("t".to_owned(), (*k).to_owned())).collect());
    decision
}

#[test]
fn a_group_is_decided_in_stages_with_cumulative_caps() {
    // Named held 5 ("one" + "two"); "three" and "four" (7) were folded into the earlier total.
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3), ("a", "three", 4), ("a", "four", 3)], &[("a", 5)], 8);
    let first_token = log.ownership_group("a", Currency::USD).unwrap().token;
    let record = log.reconcile_ownership(&stage(&log, Holder::Named, &["three"], &["one", "three"]), 1).unwrap();
    assert!(record.partial);
    assert_eq!(record.charges.len(), 2);
    assert_eq!(record.moved_from_earlier, usd(4));
    assert_eq!(log.ownership("t", "one").unwrap(), Some(ChargeOwnership::Established));
    assert_eq!(log.ownership("t", "two").unwrap(), Some(ChargeOwnership::LegacyUncertain), "outside the stage");
    let group = log.ownership_group("a", Currency::USD).unwrap();
    assert_ne!(group.token, first_token, "every stage is reviewed afresh");
    assert_eq!(group.charges.len(), 2);
    assert_eq!((group.max_named, group.max_earlier), (usd(3), usd(4)), "the caps count earlier stages");
    assert_eq!(group.evidence.original_open, usd(12), "the evidence is kept unchanged");
    assert_eq!(marker(&log), 3);
    let mut log = reload(&log);
    // A stale token, and a named claim beyond what is left of the named cap, change nothing.
    let mut stale = stage(&log, Holder::Named, &[], &["two"]);
    stale.token = first_token;
    assert_eq!(log.reconcile_ownership(&stale, 2).unwrap_err().kind, ErrorKind::Conflict);
    let before = log.clone();
    assert_eq!(log.reconcile_ownership(&stage(&log, Holder::Named, &[], &["two", "four"]), 2).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log, before);
    // The last stage closes the group.
    let last = log.reconcile_ownership(&stage(&log, Holder::Named, &["four"], &["two", "four"]), 2).unwrap();
    assert!(!last.partial);
    assert_eq!(log.ownership_groups().count(), 0);
    assert_eq!(log.ownership_reconciliations().len(), 2);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(12)));
    assert_eq!(log.reported_spent(EARLIER_SESSIONS).session.get(&Currency::USD), Some(&usd(1)));
    assert_eq!(log.reported_spent("a").project.get(&Currency::USD), Some(&usd(13)));
    let log = reload(&log);
    assert_eq!(log.ownership_reconciliations()[1], last);
}

#[test]
fn a_whole_group_decision_after_a_stage_covers_only_what_is_left() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3), ("a", "three", 4)], &[("a", 5)], 6);
    log.reconcile_ownership(&stage(&log, Holder::Named, &[], &["one"]), 1).unwrap();
    let record = log.reconcile_ownership(&decide(&log, "a", Holder::Named, &["three"]), 2).unwrap();
    assert!(!record.partial);
    let keys: BTreeSet<&str> = record.charges.iter().map(|c| c.idempotency_key.as_str()).collect();
    assert_eq!(keys, BTreeSet::from(["two", "three"]));
    reload(&log);
}

#[test]
fn malformed_stages_change_nothing() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3), ("a", "three", 4)], &[("a", 5)], 6);
    let before = log.clone();
    let too_many: Vec<String> = (0..=MAX_STAGE_CHARGES).map(|n| n.to_string()).collect();
    let too_many: Vec<&str> = too_many.iter().map(String::as_str).collect();
    for (except, only) in [
        (vec![], vec![]),
        (vec![], vec!["one", "one"]),
        (vec![], vec!["missing"]),
        (vec!["two"], vec!["one"]), // the exception is outside the stage
        (vec![], too_many),
    ] {
        let err = log.reconcile_ownership(&stage(&log, Holder::Named, &except, &only), 1).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput, "{except:?} {}", only.len());
        assert_eq!(log, before);
    }
}

#[test]
fn a_staged_named_charge_can_be_released_while_the_group_stays_quarantined() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3)], &[("a", 5)], 6);
    log.reconcile_ownership(&stage(&log, Holder::Named, &[], &["one"]), 1).unwrap();
    log.release("t", "one").unwrap();
    let log = reload(&log);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(3)));
    assert_eq!(log.ownership_group("a", Currency::USD).unwrap().max_named, usd(3));
}

#[test]
fn tampered_stages_are_refused() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3), ("a", "three", 4)], &[("a", 5)], 6);
    log.reconcile_ownership(&stage(&log, Holder::Named, &[], &["one"]), 1).unwrap();
    let staged: UsageLogFile = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert!(UsageLog::try_from(staged.clone()).is_ok());
    log.reconcile_ownership(&decide(&log, "a", Holder::Named, &["three"]), 2).unwrap();
    let closed: UsageLogFile = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert!(UsageLog::try_from(closed.clone()).is_ok());
    let edits: Vec<(&str, &UsageLogFile, Edit)> = vec![
        ("a stage marked complete while its group is quarantined", &staged, Box::new(|f| f.ownership_reconciliations[0].partial = false)),
        ("a stage whose evidence differs from its group", &staged, Box::new(|f| f.ownership_reconciliations[0].evidence.original_named = usd(4))),
        ("a stage's amount changed", &staged, Box::new(|f| f.ownership_reconciliations[0].charges[0].cost = usd(1))),
        (
            "one charge in two stages",
            &closed,
            Box::new(|f| {
                let first = f.ownership_reconciliations[0].charges[0].clone();
                f.ownership_reconciliations[1].charges.push(first);
            }),
        ),
        (
            "a quarantined group with nothing left in it",
            &closed,
            Box::new(|f| {
                f.ownership_reconciliations[1].partial = true;
                let evidence = f.ownership_reconciliations[1].evidence.clone();
                f.legacy_groups.push(evidence);
            }),
        ),
        ("the closing record marked partial", &closed, Box::new(|f| f.ownership_reconciliations[1].partial = true)),
        ("a stage after the group closed", &closed, Box::new(|f| f.ownership_reconciliations.swap(0, 1))),
        ("stages of one group with different evidence", &closed, Box::new(|f| f.ownership_reconciliations[0].evidence.original_named = usd(4))),
        ("closed stages short of the open amount", &closed, Box::new(|f| f.ownership_reconciliations[1].charges.retain(|c| c.idempotency_key != "two"))),
    ];
    for (what, base, edit) in edits {
        let mut file = base.clone();
        edit(&mut file);
        assert!(UsageLog::try_from(file).is_err(), "{what}");
    }
}

#[test]
fn named_placements_are_capped_across_stages_on_load() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3), ("a", "three", 1)], &[("a", 5)], 6);
    log.reconcile_ownership(&stage(&log, Holder::Named, &["three"], &["one", "three"]), 1).unwrap();
    log.reconcile_ownership(&stage(&log, Holder::Named, &[], &["two"]), 2).unwrap();
    let file: UsageLogFile = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert!(UsageLog::try_from(file.clone()).is_ok());
    // Each record alone stays within the named 5; together they would place 6 there.
    let mut over = file;
    over.ownership_reconciliations[0].charges.iter_mut().for_each(|c| c.holder = Holder::Named);
    over.ownership_reconciliations[0].moved_from_earlier = usd(0);
    assert!(UsageLog::try_from(over).is_err());
}

#[test]
fn staged_charges_settle_from_either_holder_and_reload() {
    let mut log = legacy(&[("a", "one", 2), ("a", "two", 3), ("a", "three", 4)], &[("a", 5)], 6);
    log.reconcile_ownership(&stage(&log, Holder::Named, &["three"], &["one", "three"]), 1).unwrap();
    let mut log = reload(&log);
    log.settle("t", "one", usd(1), Charge::Actual).unwrap();
    log.settle("t", "three", usd(4), Charge::Unresolved).unwrap();
    let mut log = reload(&log);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(8)));
    log.settle("t", "three", usd(2), Charge::Actual).unwrap();
    let log = reload(&log);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(6)));
    assert_eq!(log.ownership_group("a", Currency::USD).unwrap().charges.len(), 1, "\"two\" is still quarantined");
}

#[test]
fn evicted_charges_can_be_staged_and_reload_from_pending() {
    let log = legacy(&[("a", "one", 2), ("a", "two", 3)], &[("a", 5)], 6);
    let mut file: UsageLogFile = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    let index = file.entries.iter().position(|e| e.idempotency_key == "one").unwrap();
    let evicted = file.entries.remove(index);
    file.uncertain_listed = file.entries.iter().enumerate().filter(|(_, e)| e.idempotency_key == "two").map(|(i, _)| i as u32).collect();
    file.quarantined.push(evicted);
    let mut log = UsageLog::try_from(file).unwrap();
    log.reconcile_ownership(&stage(&log, Holder::Named, &[], &["one"]), 1).unwrap();
    assert_eq!(log.pending().map(|e| e.idempotency_key.as_str()).collect::<Vec<_>>(), ["one"]);
    let mut log = reload(&log);
    log.settle("t", "one", usd(2), Charge::Actual).unwrap();
    let log = reload(&log);
    assert_eq!(log.pending().count(), 0);
    assert_eq!(log.ownership_group("a", Currency::USD).unwrap().charges.len(), 1);
}

#[test]
fn a_stage_of_one_group_does_not_spend_another_groups_review() {
    let mut log = legacy(&[("a", "a1", 2), ("a", "a2", 2), ("b", "b1", 2)], &[("a", 4), ("b", 2)], 4);
    let b_token = log.ownership_group("b", Currency::USD).unwrap().token;
    log.reconcile_ownership(&stage(&log, Holder::Named, &[], &["a1"]), 1).unwrap();
    assert_eq!(log.ownership_group("b", Currency::USD).unwrap().token, b_token, "named-only stages leave the earlier total alone");
}

#[test]
fn a_group_past_the_exception_limit_splits_in_bounded_stages() {
    // 1,500 charges of 1: 750 were in the named total and 750 folded, a split no default holder
    // plus 1,000 exceptions can express in one decision.
    let keys: Vec<String> = (0..1_500).map(|n| format!("k{n:04}")).collect();
    let opens: Vec<(&str, &str, u64)> = keys.iter().map(|k| ("a", k.as_str(), 1)).collect();
    let mut log = legacy(&opens, &[("a", 750)], 750);
    let named: Vec<&str> = keys.iter().take(750).map(String::as_str).collect();
    let folded: Vec<&str> = keys.iter().skip(750).map(String::as_str).collect();
    let mut first = stage(&log, Holder::Named, &[], &named);
    first.only.as_mut().unwrap().extend(folded.iter().take(250).map(|k| ("t".to_owned(), (*k).to_owned())));
    first.except = folded.iter().take(250).map(|k| ("t".to_owned(), (*k).to_owned())).collect();
    let record = log.reconcile_ownership(&first, 1).unwrap();
    assert!(record.partial);
    assert_eq!((record.charges.len(), record.moved_from_earlier), (1_000, usd(250)));
    let mut log = reload(&log);
    let rest: Vec<&str> = folded.iter().skip(250).copied().collect();
    let last = log.reconcile_ownership(&stage(&log, Holder::Earlier, &[], &rest), 2).unwrap();
    assert!(!last.partial);
    assert_eq!(last.moved_from_earlier, usd(500));
    let log = reload(&log);
    assert_eq!(log.ownership_groups().count(), 0);
    assert_eq!(log.reported_spent("a").session.get(&Currency::USD), Some(&usd(1_500)));
    assert_eq!(log.reported_spent(EARLIER_SESSIONS).session.get(&Currency::USD), Some(&usd(0)));
}
