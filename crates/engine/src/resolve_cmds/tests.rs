use super::*;
use crate::Session;
use crate::commands::{Registry, Resources, Services};
use nodecraft_exec::cost::{UsageEntry, UsageLogFile};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

/// One legacy group (session "a" holds 5, the earlier total 6, three open estimates), with its
/// ownership reconciled: "three" held by the earlier total, the rest by the session.
fn reconciled() -> UsageLog {
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
            status: nodecraft_exec::cost::Charge::Estimate,
        });
    }
    file.project = BTreeMap::from([(Currency::USD, usd(11))]);
    file.sessions = BTreeMap::from([("a".into(), BTreeMap::from([(Currency::USD, usd(5))])), (String::new(), BTreeMap::from([(Currency::USD, usd(6))]))]);
    let mut log = UsageLog::try_from(file).unwrap();
    let mut decide = session(&Arc::new(Mutex::new(log.clone())), false);
    let token = decide.dispatch("usage.ownership", &json!({"session": "a", "currency": "USD"})).unwrap()["group"]["token"].clone();
    let request = json!({"session": "a", "currency": "USD", "token": token, "holder": "named", "except": [{"provider": "t", "key": "three"}]});
    crate::ownership_cmds::reconcile(&mut log, &request, 1).unwrap();
    log
}

fn session(log: &Arc<Mutex<UsageLog>>, with_service: bool) -> Session {
    let mut resources = Resources::default();
    resources.insert(UsageSnapshot(Arc::new(log.lock().unwrap().clone())));
    let mut services = Services::default();
    if with_service {
        let log = Arc::clone(log);
        services.register(RESOLUTION_SERVICE, move |request| resolve(&mut log.lock().unwrap(), request, 99)).unwrap();
    }
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Resolve").unwrap();
    Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap())
        .unwrap()
        .with_resources(resources)
        .with_services(services)
}

fn token(s: &mut Session, key: &str) -> Value {
    let page = s.dispatch("usage.unsettled", &json!({})).unwrap();
    page["charges"].as_array().unwrap().iter().find(|c| c["key"] == key).unwrap()["token"].clone()
}

#[test]
fn a_person_settles_one_charge_and_releases_another() {
    let log = Arc::new(Mutex::new(reconciled()));
    let mut s = session(&log, true);
    let page = s.dispatch("usage.unsettled", &json!({"limit": 2})).unwrap();
    assert_eq!((page["total"].clone(), page["next_offset"].clone(), page["resolved"].clone()), (json!(3), json!(2), json!(0)));
    assert_eq!(page["charges"][0]["holder"], "named");
    assert_eq!(page["charges"][0]["reconciled_at_ms"], 1);

    let one = token(&mut s, "one");
    let settle = json!({"provider": "t", "key": "one", "token": one, "outcome": "settled", "currency": "USD", "micros": 7, "dry_run": true});
    let preview = s.dispatch("usage.resolve_charge", &settle).unwrap();
    assert_eq!((preview["cost"]["micros"].clone(), preview["was"]["micros"].clone(), preview["dry_run"].clone()), (json!(7), json!(2), json!(true)));
    let was = log.lock().unwrap().reported_spent("a").session[&Currency::USD].micros;
    assert_eq!(preview["session_total"]["micros"], was - 2 + 7, "the estimate is replaced by the billed amount");
    let before = log.lock().unwrap().clone();
    assert_eq!(*log.lock().unwrap(), before, "a dry run changes nothing");

    let mut confirm = settle.clone();
    confirm["dry_run"] = json!(false);
    let record = s.dispatch("usage.resolve_charge", &confirm).unwrap();
    assert_eq!((record["outcome"].clone(), record["at_ms"].clone()), (json!("settled"), json!(99)));
    assert_eq!(log.lock().unwrap().charge_resolutions().len(), 1);

    let three = token(&mut s, "three");
    let release = json!({"provider": "t", "key": "three", "token": three, "outcome": "released"});
    assert_eq!(s.dispatch("usage.resolve_charge", &release).unwrap()["outcome"], "released");
    assert_eq!(log.lock().unwrap().resolvable_charges().len(), 1);
}

#[test]
fn stale_or_bad_decisions_change_nothing() {
    let log = Arc::new(Mutex::new(reconciled()));
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
        json!({"provider": "t", "key": "nope", "token": one, "outcome": "released"}),
        json!({"provider": "t", "key": "one", "token": one, "outcome": "released", "extra": true}),
    ];
    for case in cases {
        assert!(s.dispatch("usage.resolve_charge", &case).is_err(), "{case}");
        assert_eq!(*log.lock().unwrap(), before, "{case}");
    }
    assert_eq!(s.dispatch("usage.unsettled", &json!({"limit": 0})).unwrap_err().kind, ErrorKind::InvalidInput);
    // The live log moved on after the review: the service refuses what the snapshot allowed.
    log.lock()
        .unwrap()
        .resolve_charge(
            &ResolutionDecision {
                provider: "t".into(),
                idempotency_key: "one".into(),
                token: unhex(one.as_str().unwrap()).unwrap(),
                resolution: Resolution::Released,
            },
            5,
        )
        .unwrap();
    let late = s.dispatch("usage.resolve_charge", &json!({"provider": "t", "key": "one", "token": one, "outcome": "released"}));
    assert_eq!(late.unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn hosts_without_the_log_or_the_service_report_unsupported() {
    let log = Arc::new(Mutex::new(reconciled()));
    let mut no_service = session(&log, false);
    let one = token(&mut no_service, "one");
    let decision = json!({"provider": "t", "key": "one", "token": one, "outcome": "released"});
    assert_eq!(no_service.dispatch("usage.resolve_charge", &decision).unwrap_err().kind, ErrorKind::Unsupported);
    let mut preview = decision.clone();
    preview["dry_run"] = json!(true);
    assert_eq!(no_service.dispatch("usage.resolve_charge", &preview).unwrap()["outcome"], "released", "a preview needs no service");
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Bare").unwrap();
    let mut bare = Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap();
    assert_eq!(bare.dispatch("usage.unsettled", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(bare.dispatch("usage.resolve_charge", &decision).unwrap_err().kind, ErrorKind::Unsupported);
}
