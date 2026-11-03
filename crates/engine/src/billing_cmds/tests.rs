use super::*;
use crate::Session;
use crate::commands::{Registry, Resources, Services};
use nodecraft_exec::cost::{Charge, UsageEntry};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;
use std::sync::{Arc, Mutex};

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

fn charge(key: &str, cost: u64) -> UsageEntry {
    UsageEntry {
        project: Some(ProjectId::from_bytes([1; 16])),
        at_ms: 3,
        session: "a".into(),
        node: nodecraft_types::NodeId::from_bytes([2; 16]),
        provider: "t".into(),
        instance: None,
        model: "m".into(),
        cost: usd(cost),
        idempotency_key: key.into(),
        status: Charge::Estimate,
    }
}

/// Session "a" with two unresolved charges ("one" 2, "two" 3) and a running estimate ("live" 4).
fn unresolved() -> UsageLog {
    let mut log = UsageLog::default();
    for (key, cost) in [("one", 2), ("two", 3), ("live", 4)] {
        log.record(charge(key, cost)).unwrap();
    }
    for (key, cost) in [("one", 2), ("two", 3)] {
        log.settle("t", key, usd(cost), Charge::Unresolved).unwrap();
    }
    log
}

fn session(log: &Arc<Mutex<UsageLog>>, with_service: bool) -> Session {
    let mut resources = Resources::default();
    resources.insert(UsageSnapshot(Arc::new(log.lock().unwrap().clone())));
    let mut services = Services::default();
    if with_service {
        let log = Arc::clone(log);
        services.register(BILLING_SERVICE, move |request| resolve(&mut log.lock().unwrap(), request, 99)).unwrap();
    }
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Billing").unwrap();
    Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap())
        .unwrap()
        .with_resources(resources)
        .with_services(services)
}

fn token(s: &mut Session, key: &str) -> Value {
    let page = s.dispatch("usage.unresolved", &json!({})).unwrap();
    page["charges"].as_array().unwrap().iter().find(|c| c["key"] == key).unwrap()["token"].clone()
}

#[test]
fn a_person_previews_then_settles_one_charge_and_releases_another() {
    let log = Arc::new(Mutex::new(unresolved()));
    let mut s = session(&log, true);
    let page = s.dispatch("usage.unresolved", &json!({"limit": 1})).unwrap();
    assert_eq!((page["total"].clone(), page["next_offset"].clone(), page["resolved"].clone()), (json!(2), json!(1), json!(0)), "not the running estimate");
    assert_eq!((page["charges"][0]["key"].clone(), page["charges"][0]["status"].clone()), (json!("one"), json!("unresolved")));
    assert_eq!(page["charges"][0]["instance"], Value::Null);

    let one = token(&mut s, "one");
    let settle =
        json!({"provider": "t", "key": "one", "token": one, "outcome": "settled", "currency": "USD", "micros": 7, "note": "  invoice 12  ", "dry_run": true});
    let before = log.lock().unwrap().clone();
    let preview = s.dispatch("usage.resolve_unresolved", &settle).unwrap();
    assert_eq!(*log.lock().unwrap(), before, "a dry run changes nothing");
    assert_eq!((preview["cost"]["micros"].clone(), preview["was"]["micros"].clone(), preview["dry_run"].clone()), (json!(7), json!(2), json!(true)));
    assert_eq!(preview["note"], "invoice 12", "trimmed");
    assert_eq!((preview["session_total"]["micros"].clone(), preview["project_total"]["micros"].clone()), (json!(14), json!(14)), "9 less 2 plus 7");
    assert_eq!(preview["this_project_total"]["micros"], 14);
    assert!(preview.get("at_ms").is_none());

    let mut confirm = settle.clone();
    confirm["dry_run"] = json!(false);
    let record = s.dispatch("usage.resolve_unresolved", &confirm).unwrap();
    assert_eq!((record["outcome"].clone(), record["at_ms"].clone(), record["recorded_at_ms"].clone()), (json!("settled"), json!(99), json!(3)));
    assert_eq!(log.lock().unwrap().charge("t", "one").map(|e| e.status), Some(Charge::Actual));
    // The snapshot is the host's to refresh; a second confirmation against the live log finds
    // nothing waiting, and changes nothing.
    let after = log.lock().unwrap().clone();
    assert_eq!(s.dispatch("usage.resolve_unresolved", &confirm).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(*log.lock().unwrap(), after);

    let two = token(&mut s, "two");
    let release = json!({"provider": "t", "key": "two", "token": two, "outcome": "released"});
    assert_eq!(s.dispatch("usage.resolve_unresolved", &release).unwrap()["outcome"], "released");
    let live = log.lock().unwrap();
    assert!(live.unresolved_charges().is_empty());
    assert_eq!(live.billing_resolutions().len(), 2);
    assert_eq!(live.reported_spent("a").session[&Currency::USD], usd(11), "7 billed, 4 still running");
}

#[test]
fn stale_or_bad_decisions_change_nothing() {
    let log = Arc::new(Mutex::new(unresolved()));
    let mut s = session(&log, true);
    let one = token(&mut s, "one");
    let before = log.lock().unwrap().clone();
    let mut forged = one.as_str().unwrap().to_owned();
    forged.replace_range(0..1, if forged.starts_with('0') { "1" } else { "0" });
    let cases = [
        json!({"provider": "t", "key": "one", "token": forged, "outcome": "released"}),
        json!({"provider": "t", "key": "one", "token": "nothex", "outcome": "released"}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "settled"}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "settled", "currency": "EUR", "micros": 1}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "settled", "currency": "USD", "micros": u64::MAX}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "released", "micros": 1}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "forgotten"}),
        json!({"provider": "t", "key": "live", "token": one, "outcome": "released"}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "released", "note": "x".repeat(MAX_BILLING_NOTE + 1)}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "released", "note": "bell\u{7}"}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "released", "instance": "not-a-uuid"}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "released", "instance": "01010101-0101-0101-0101-010101010101"}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "released", "extra": true}),
    ];
    for case in cases {
        assert!(s.dispatch("usage.resolve_unresolved", &case).is_err(), "{case}");
        assert_eq!(*log.lock().unwrap(), before, "{case}");
    }
    assert_eq!(s.dispatch("usage.unresolved", &json!({"limit": 0})).unwrap_err().kind, ErrorKind::InvalidInput);
    // The live log moved on after the review (a run settled the charge): the service refuses
    // what the snapshot allowed.
    log.lock().unwrap().settle("t", "one", usd(1), Charge::Actual).unwrap();
    let late = s.dispatch("usage.resolve_unresolved", &json!({"provider": "t", "key": "one", "token": one, "outcome": "released"}));
    assert_eq!(late.unwrap_err().kind, ErrorKind::NotFound);
    // Or changed it: refused as stale.
    let two = token(&mut s, "two");
    log.lock().unwrap().settle("t", "two", usd(8), Charge::Unresolved).unwrap();
    let changed = log.lock().unwrap().clone();
    let stale = s.dispatch("usage.resolve_unresolved", &json!({"provider": "t", "key": "two", "token": two, "outcome": "released"}));
    assert_eq!(stale.unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(*log.lock().unwrap(), changed);
}

#[test]
fn hosts_without_the_log_or_the_service_report_unsupported() {
    let log = Arc::new(Mutex::new(unresolved()));
    let mut no_service = session(&log, false);
    let one = token(&mut no_service, "one");
    let decision = json!({"provider": "t", "key": "one", "token": one, "outcome": "released"});
    assert_eq!(no_service.dispatch("usage.resolve_unresolved", &decision).unwrap_err().kind, ErrorKind::Unsupported);
    let mut preview = decision.clone();
    preview["dry_run"] = json!(true);
    assert_eq!(no_service.dispatch("usage.resolve_unresolved", &preview).unwrap()["outcome"], "released", "a preview needs no service");
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Bare").unwrap();
    let mut bare = Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap();
    assert_eq!(bare.dispatch("usage.unresolved", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(bare.dispatch("usage.resolve_unresolved", &decision).unwrap_err().kind, ErrorKind::Unsupported);
    // A service request that asks for a dry run is the command's to answer, never the log's.
    let mut live = log.lock().unwrap().clone();
    assert_eq!(resolve(&mut live, &preview, 1).unwrap_err().kind, ErrorKind::InvalidInput);
}
