use super::*;
use crate::Session;
use crate::commands::{Registry, Resources, Services};
use nodecraft_exec::cost::{ChargeOwnership, UsageEntry, UsageLogFile};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;
use std::collections::BTreeMap;

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

/// One legacy group: session "a" holds 5, the earlier total 6, and three open estimates.
fn ambiguous() -> UsageLog {
    let mut file = UsageLogFile::default();
    for (key, cost) in [("one", 2), ("two", 3), ("three", 4)] {
        file.entries.push(UsageEntry {
            at_ms: 0,
            session: "a".into(),
            node: nodecraft_types::NodeId::from_bytes([2; 16]),
            provider: "t".into(),
            instance: None,
            model: "m".into(),
            cost: usd(cost),
            idempotency_key: key.into(),
            status: Charge::Estimate,
        });
    }
    file.project = BTreeMap::from([(Currency::USD, usd(11))]);
    file.sessions = BTreeMap::from([("a".into(), BTreeMap::from([(Currency::USD, usd(5))])), (String::new(), BTreeMap::from([(Currency::USD, usd(6))]))]);
    UsageLog::try_from(file).unwrap()
}

fn session(log: &Arc<Mutex<UsageLog>>, with_service: bool) -> Session {
    let mut resources = Resources::default();
    resources.insert(UsageSnapshot(Arc::new(log.lock().unwrap().clone())));
    let mut services = Services::default();
    if with_service {
        services.register(OWNERSHIP_SERVICE, ownership_service(Arc::clone(log), Arc::new(|| 1234))).unwrap();
    }
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Ownership").unwrap();
    Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap())
        .unwrap()
        .with_resources(resources)
        .with_services(services)
}

#[test]
fn review_then_confirm_reconciles_the_live_log() {
    let log = Arc::new(Mutex::new(ambiguous()));
    let mut s = session(&log, true);
    let groups = s.dispatch("usage.ownership", &json!({})).unwrap();
    assert_eq!(groups["total"], 1);
    assert_eq!(groups["reconciled"], 0);
    let summary = &groups["groups"][0];
    assert_eq!((summary["session"].clone(), summary["currency"].clone(), summary["charges_total"].clone()), (json!("a"), json!("USD"), json!(3)));
    let detail = s.dispatch("usage.ownership", &json!({"session": "a", "currency": "USD", "limit": 2})).unwrap();
    assert_eq!(detail["charges"].as_array().unwrap().len(), 2);
    assert_eq!(detail["next_offset"], 2);
    assert_eq!(detail["group"]["max_named"]["micros"], 5);
    assert_eq!(detail["group"]["max_earlier"]["micros"], 6);
    assert_eq!(detail["charges"][0]["location"], "listed");
    let rest = s.dispatch("usage.ownership", &json!({"session": "a", "currency": "USD", "offset": 2})).unwrap();
    assert_eq!((rest["charges"].as_array().unwrap().len(), rest["next_offset"].clone()), (1, Value::Null));
    assert_eq!(summary["token"], Value::Null, "summaries are cheap; a token needs the group's review");
    assert_eq!(summary["original_open"]["micros"], 9);
    let token = detail["group"]["token"].clone();
    let record = s
        .dispatch(
            "usage.reconcile_ownership",
            &json!({"session": "a", "currency": "USD", "token": token, "holder": "named", "except": [{"provider": "t", "key": "three"}]}),
        )
        .unwrap();
    assert_eq!(record["moved_from_earlier"]["micros"], 4);
    assert_eq!((record["named"].clone(), record["earlier"].clone(), record["at_ms"].clone()), (json!(2), json!(1), json!(1234)));
    let live = log.lock().unwrap();
    assert_eq!(live.ownership("t", "three").unwrap(), Some(ChargeOwnership::Established));
    assert_eq!(live.ownership_reconciliations().len(), 1);
    drop(live);
    // The host hasn't refreshed its snapshot: the stale preview passes the command's own check,
    // but the live log is authoritative and has no such group any more.
    let again = s.dispatch(
        "usage.reconcile_ownership",
        &json!({"session": "a", "currency": "USD", "token": token, "holder": "named", "except": [{"provider": "t", "key": "three"}]}),
    );
    assert_eq!(again.unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn stale_or_bad_decisions_change_nothing() {
    let log = Arc::new(Mutex::new(ambiguous()));
    let mut s = session(&log, true);
    let token = s.dispatch("usage.ownership", &json!({"session": "a", "currency": "USD"})).unwrap()["group"]["token"].clone();
    let before = log.lock().unwrap().clone();
    let mut forged = token.as_str().unwrap().to_owned();
    forged.replace_range(0..1, if forged.starts_with('0') { "1" } else { "0" });
    let cases = [
        json!({"session": "a", "currency": "USD", "token": forged, "holder": "named"}),
        json!({"session": "a", "currency": "USD", "token": "nothex", "holder": "named"}),
        json!({"session": "a", "currency": "USD", "token": token.as_str().unwrap().to_uppercase(), "holder": "named"}),
        json!({"session": "a", "currency": "usd", "token": token, "holder": "named"}),
        json!({"session": "a", "currency": "USD", "token": token, "holder": "both"}),
        json!({"session": "a", "currency": "USD", "token": token, "holder": "named", "extra": true}),
        json!({"session": "a", "currency": "USD", "token": token, "holder": "named"}), // 9 > 5 in the named total
        json!({"session": "a", "currency": "USD", "token": token, "holder": "earlier", "except": [{"provider": "t", "key": "nope"}]}),
        json!({"session": "b", "currency": "USD", "token": token, "holder": "named"}),
        json!({"session": "a", "currency": "EUR", "token": token, "holder": "named"}),
    ];
    for case in cases {
        assert!(s.dispatch("usage.reconcile_ownership", &case).is_err(), "{case}");
        assert_eq!(*log.lock().unwrap(), before, "{case}");
    }
    let too_many: Vec<Value> = (0..=MAX_RECONCILE_EXCEPTIONS).map(|n| json!({"provider": "t", "key": n.to_string()})).collect();
    let refused = s.dispatch("usage.reconcile_ownership", &json!({"session": "a", "currency": "USD", "token": token, "holder": "named", "except": too_many}));
    assert_eq!(refused.unwrap_err().kind, ErrorKind::InvalidInput);
    // The live log moved on after the preview: refused at the service, not just the snapshot.
    log.lock().unwrap().record(UsageEntry { idempotency_key: "late".into(), ..ambiguous().charge("t", "one").unwrap().clone() }).unwrap();
    let late = s.dispatch(
        "usage.reconcile_ownership",
        &json!({"session": "a", "currency": "USD", "token": token, "holder": "earlier", "except": [{"provider": "t", "key": "one"}]}),
    );
    assert_eq!(late.unwrap_err().kind, ErrorKind::Conflict);
}

#[test]
fn hosts_without_the_log_or_the_service_report_unsupported() {
    let log = Arc::new(Mutex::new(ambiguous()));
    let mut no_service = session(&log, false);
    let token = no_service.dispatch("usage.ownership", &json!({"session": "a", "currency": "USD"})).unwrap()["group"]["token"].clone();
    let decision = json!({"session": "a", "currency": "USD", "token": token, "holder": "earlier"});
    assert_eq!(no_service.dispatch("usage.reconcile_ownership", &decision).unwrap_err().kind, ErrorKind::Unsupported);
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Bare").unwrap();
    let mut bare = Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap();
    assert_eq!(bare.dispatch("usage.ownership", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(bare.dispatch("usage.reconcile_ownership", &decision).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(no_service.dispatch("usage.ownership", &json!({"session": "a"})).unwrap_err().kind, ErrorKind::InvalidInput);
    let empty = no_service.dispatch("usage.ownership", &json!({"offset": 9})).unwrap();
    assert_eq!((empty["groups"].as_array().unwrap().len(), empty["next_offset"].clone()), (0, Value::Null));
}

#[test]
fn a_group_page_stays_within_the_byte_limit() {
    let mut file = UsageLogFile::default();
    let long = "k".repeat(500);
    for n in 0..MAX_CHARGES_PER_PAGE {
        file.entries.push(UsageEntry {
            at_ms: 0,
            session: "a".into(),
            node: nodecraft_types::NodeId::from_bytes([2; 16]),
            provider: "\u{1}".repeat(500),
            instance: None,
            model: "\u{1}".repeat(500),
            cost: usd(1),
            idempotency_key: format!("{n}{long}").chars().take(500).collect(),
            status: Charge::Estimate,
        });
    }
    let open = MAX_CHARGES_PER_PAGE as u64;
    file.project = BTreeMap::from([(Currency::USD, usd(open + 1))]);
    file.sessions = BTreeMap::from([("a".into(), BTreeMap::from([(Currency::USD, usd(1))])), (String::new(), BTreeMap::from([(Currency::USD, usd(open))]))]);
    let log = Arc::new(Mutex::new(UsageLog::try_from(file).unwrap()));
    let mut s = session(&log, false);
    let page = s.dispatch("usage.ownership", &json!({"session": "a", "currency": "USD"})).unwrap();
    assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_PAGE_JSON_BYTES);
    let listed = page["charges"].as_array().unwrap().len();
    assert!(listed > 0 && listed < MAX_CHARGES_PER_PAGE, "{listed}");
    assert_eq!(page["next_offset"], listed);
}

#[test]
fn a_dry_run_reports_consequences_without_the_service_or_a_change() {
    let log = Arc::new(Mutex::new(ambiguous()));
    let mut s = session(&log, false);
    let token = s.dispatch("usage.ownership", &json!({"session": "a", "currency": "USD"})).unwrap()["group"]["token"].clone();
    let before = log.lock().unwrap().clone();
    let decision =
        json!({"session": "a", "currency": "USD", "token": token, "holder": "named", "except": [{"provider": "t", "key": "three"}], "dry_run": true});
    let preview = s.dispatch("usage.reconcile_ownership", &decision).unwrap();
    assert_eq!(preview["dry_run"], true);
    assert_eq!((preview["named_total"]["micros"].clone(), preview["earlier_total"]["micros"].clone()), (json!(9), json!(2)));
    assert_eq!(preview["moved_from_earlier"]["micros"], 4);
    assert_eq!(*log.lock().unwrap(), before);
    let impossible = json!({"session": "a", "currency": "USD", "token": token, "holder": "named", "dry_run": true});
    assert_eq!(s.dispatch("usage.reconcile_ownership", &impossible).unwrap_err().kind, ErrorKind::Conflict, "9 can't fit in a named total of 5");
    let mut service = ownership_service(Arc::clone(&log), Arc::new(|| 0));
    assert!(service(&decision).is_err(), "the service never applies a dry run");
    assert_eq!(*log.lock().unwrap(), before);
    assert_eq!(s.dispatch("usage.ownership", &json!({"limit": 0})).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn a_host_that_refreshes_its_snapshot_can_reconcile_group_after_group() {
    let mut file = UsageLogFile::default();
    for (session, key) in [("a", "a1"), ("b", "b1")] {
        file.entries.push(UsageEntry { session: session.into(), idempotency_key: key.into(), ..ambiguous().charge("t", "one").unwrap().clone() });
    }
    file.project = BTreeMap::from([(Currency::USD, usd(8))]);
    file.sessions = BTreeMap::from([
        ("a".into(), BTreeMap::from([(Currency::USD, usd(2))])),
        ("b".into(), BTreeMap::from([(Currency::USD, usd(2))])),
        (String::new(), BTreeMap::from([(Currency::USD, usd(4))])),
    ]);
    let log = Arc::new(Mutex::new(UsageLog::try_from(file).unwrap()));
    let mut s = session(&log, true);
    for group in ["a", "b"] {
        let token = s.dispatch("usage.ownership", &json!({"session": group, "currency": "USD"})).unwrap()["group"]["token"].clone();
        s.dispatch("usage.reconcile_ownership", &json!({"session": group, "currency": "USD", "token": token, "holder": "earlier"})).unwrap();
        // The host contract: save, then replace the snapshot.
        let saved: UsageLog = serde_json::from_str(&serde_json::to_string(&*log.lock().unwrap()).unwrap()).unwrap();
        s.swap_resource(Some(UsageSnapshot(Arc::new(saved))));
    }
    assert_eq!(s.dispatch("usage.ownership", &json!({})).unwrap()["reconciled"], 2);
    assert_eq!(log.lock().unwrap().ownership_groups().count(), 0);
}

#[test]
fn a_large_group_is_reconciled_in_stages_with_a_fresh_review_each() {
    let log = Arc::new(Mutex::new(ambiguous()));
    let token = |log: &Arc<Mutex<UsageLog>>| {
        let mut s = session(log, false);
        s.dispatch("usage.ownership", &json!({"session": "a", "currency": "USD"})).unwrap()["group"]["token"].clone()
    };
    let mut s = session(&log, true);
    let first = token(&log);
    let staged = json!({"session": "a", "currency": "USD", "token": first, "holder": "earlier", "only": [{"provider": "t", "key": "three"}]});
    let preview = s
        .dispatch(
            "usage.reconcile_ownership",
            &json!({"session": "a", "currency": "USD", "token": first, "holder": "earlier",
        "only": [{"provider": "t", "key": "three"}], "dry_run": true}),
        )
        .unwrap();
    assert_eq!((preview["partial"].clone(), preview["charges"].clone()), (json!(true), json!(1)));
    let record = s.dispatch("usage.reconcile_ownership", &staged).unwrap();
    assert_eq!((record["partial"].clone(), record["moved_from_earlier"]["micros"].clone()), (json!(true), json!(4)));
    assert_eq!(log.lock().unwrap().ownership_groups().count(), 1, "the rest stays quarantined");
    let mut s = session(&log, true);
    assert_eq!(s.dispatch("usage.reconcile_ownership", &staged).unwrap_err().kind, ErrorKind::Conflict, "the reviewed token is spent");
    let next = token(&log);
    let last = s.dispatch("usage.reconcile_ownership", &json!({"session": "a", "currency": "USD", "token": next, "holder": "named"})).unwrap();
    assert_eq!((last["partial"].clone(), last["charges"].clone()), (json!(false), json!(2)));
    assert_eq!(log.lock().unwrap().ownership_groups().count(), 0);
    let empty = json!({"session": "a", "currency": "USD", "token": next, "holder": "named", "only": []});
    assert_eq!(s.dispatch("usage.reconcile_ownership", &empty).unwrap_err().kind, ErrorKind::InvalidInput);
    let too_many: Vec<Value> = (0..=MAX_STAGE_CHARGES).map(|n| json!({"provider": "t", "key": n.to_string()})).collect();
    let err = decision(&json!({"session": "a", "currency": "USD", "token": next, "holder": "named", "only": too_many})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
}
