use super::*;

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}
fn caps(per_call: Option<u64>, session: Option<u64>, project: Option<u64>) -> Budgets {
    Budgets { per_call: per_call.map(usd), session: session.map(usd), project: project.map(usd) }
}
fn charge(key: &str, actual: Option<Money>, unresolved: bool) -> Settlement<'_> {
    Settlement { actual, unresolved, provider: "fal", model: "flux", idempotency_key: key, node: NodeId::from_bytes([1; 16]), at_ms: 1 }
}

#[test]
fn reservations_keep_parallel_jobs_within_the_session_cap() {
    let mut l = Ledger::new(caps(None, Some(100_000), None), false, "s1", UsageLog::default());
    // Three 0.04 jobs at once: the third would pass 0.10 together with the two reserved.
    l.reserve(JobId(1), Some(usd(40_000))).unwrap();
    l.reserve(JobId(2), Some(usd(40_000))).unwrap();
    assert!(matches!(l.reserve(JobId(3), Some(usd(40_000))), Err(Refusal::Session { .. })));
    assert_eq!(l.reserved().get(&Currency::USD), Some(&usd(80_000)));
    // Job 1 ends cheaper than quoted: the actual cost is charged and room frees up.
    assert_eq!(l.commit(JobId(1), charge("k1", Some(usd(10_000)), false)).unwrap(), Some(usd(10_000)));
    l.reserve(JobId(3), Some(usd(40_000))).unwrap();
    // Exactly at the cap is allowed: 0.01 spent + 0.08 reserved; one more 0.01 fits, 0.02 doesn't.
    assert!(l.reserve(JobId(4), Some(usd(10_000))).is_ok());
    assert!(l.reserve(JobId(5), Some(usd(1))).is_err());
    assert!(matches!(l.reserve(JobId(2), Some(usd(1))), Err(Refusal::AlreadyReserved(_))));
}

#[test]
fn per_call_project_unpriced_and_currency_refusals() {
    let mut usage = UsageLog::default();
    // Earlier sessions spent 0.95 of a 1.00 project cap.
    usage
        .record(UsageEntry {
            at_ms: 0,
            session: "earlier".into(),
            node: NodeId::from_bytes([1; 16]),
            provider: "fal".into(),
            model: "flux".into(),
            cost: usd(950_000),
            idempotency_key: "old".into(),
            status: Charge::Actual,
        })
        .unwrap();
    let mut l = Ledger::new(caps(Some(80_000), None, Some(1_000_000)), false, "now", usage);
    assert!(matches!(l.reserve(JobId(1), Some(usd(90_000))), Err(Refusal::PerCall { .. })));
    assert!(matches!(l.reserve(JobId(2), Some(usd(60_000))), Err(Refusal::Project { .. })));
    assert!(l.reserve(JobId(3), Some(usd(50_000))).is_ok());
    assert_eq!(l.reserve(JobId(4), None), Err(Refusal::Unpriced));
    assert!(matches!(l.reserve(JobId(5), Some(Money::new(Currency::EUR, 1))), Err(Refusal::Currency { .. })));
    // Allowing unpriced jobs lets one through, reserving nothing.
    l.set_budgets(caps(Some(80_000), None, Some(1_000_000)), true);
    assert!(l.reserve(JobId(4), None).is_ok());
    assert!(Refusal::Unpriced.job_error().starts_with(REFUSAL_PREFIX));
    assert!(is_refusal(&Refusal::Unpriced.job_error()));
}

#[test]
fn settling_charges_estimates_once_and_releases_unbilled_jobs() {
    let mut l = Ledger::new(Budgets::default(), false, "s", UsageLog::default());
    l.reserve(JobId(1), Some(usd(40_000))).unwrap();
    // No reported cost and an unknown outcome: the estimate is charged as unresolved.
    assert_eq!(l.commit(JobId(1), charge("k1", None, true)).unwrap(), Some(usd(40_000)));
    let entry = l.usage().entries().next().unwrap().clone();
    assert_eq!((entry.cost, entry.status), (usd(40_000), Charge::Unresolved));
    // A retry under the same provider key (e.g. after a resume) is the same charge.
    l.reserve(JobId(2), Some(usd(40_000))).unwrap();
    assert_eq!(l.commit(JobId(2), charge("k1", None, false)).unwrap(), None);
    assert_eq!(l.usage().spent("s").session.get(&Currency::USD), Some(&usd(40_000)));
    // A job that never reached the provider gives its reservation back.
    l.reserve(JobId(3), Some(usd(40_000))).unwrap();
    assert!(l.release(JobId(3)));
    assert!(!l.release(JobId(3)));
    assert_eq!(l.reserved().get(&Currency::USD), Some(&Money::zero(Currency::USD)));
    // Committing without a reservation is an error, not a silent charge.
    assert_eq!(l.commit(JobId(9), charge("k9", None, false)).unwrap_err().kind, ErrorKind::NotFound);
    // An unpriced job with no reported cost charges nothing.
    l.reserve(JobId(4), None).unwrap();
    assert_eq!(l.commit(JobId(4), charge("k4", None, false)).unwrap(), None);
}

#[test]
fn resumed_jobs_are_adopted_past_the_caps() {
    let mut l = Ledger::new(caps(None, Some(10_000), None), false, "s", UsageLog::default());
    // After a restart a job is already running on the provider: it is held even over the cap.
    l.adopt(JobId(1), Some(usd(40_000))).unwrap();
    l.adopt(JobId(1), Some(usd(40_000))).unwrap();
    assert_eq!(l.reserved().get(&Currency::USD), Some(&usd(40_000)));
    // New jobs see the adopted spend.
    assert!(matches!(l.reserve(JobId(2), Some(usd(1))), Err(Refusal::Session { .. })));
    assert_eq!(l.commit(JobId(1), charge("k1", Some(usd(30_000)), false)).unwrap(), Some(usd(30_000)));
}

#[test]
fn free_jobs_run_over_a_cap_and_an_unrecordable_charge_still_counts() {
    let mut l = Ledger::new(caps(None, Some(10), None), false, "s1", UsageLog::default());
    l.reserve(JobId(1), Some(usd(10))).unwrap();
    // The actual cost overshot the estimate: spend is now over the cap.
    l.commit(JobId(1), charge("k1", Some(usd(25)), false)).unwrap();
    assert!(l.reserve(JobId(2), Some(usd(1))).is_err());
    l.reserve(JobId(2), Some(usd(0))).unwrap();
    // A charge the log refuses (no key) frees the job id but still counts against the caps.
    let mut l = Ledger::new(caps(None, Some(100), None), false, "s1", UsageLog::default());
    l.reserve(JobId(3), Some(usd(60))).unwrap();
    assert!(l.commit(JobId(3), charge("", None, false)).is_err());
    assert!(!l.is_reserved(JobId(3)));
    assert_eq!(l.unrecorded().get(&Currency::USD), Some(&usd(60)));
    assert!(matches!(l.reserve(JobId(4), Some(usd(50))), Err(Refusal::Session { .. })));
}

#[test]
fn reported_costs_are_checked_and_settle_unresolved_charges() {
    let mut l = Ledger::new(Budgets::default(), false, "s", UsageLog::default());
    // Another currency than quoted, or more than any job could cost: the estimate, unresolved.
    for (job, key, reported) in [(1, "k1", Money::new(Currency::EUR, 90)), (2, "k2", usd(crate::cost::MAX_CHARGE_MICROS + 1))] {
        l.reserve(JobId(job), Some(usd(90))).unwrap();
        assert_eq!(l.commit(JobId(job), charge(key, Some(reported), false)).unwrap(), Some(usd(90)));
    }
    assert!(l.usage().entries().all(|e| (e.cost, e.status) == (usd(90), Charge::Unresolved)));
    // A later run under k1 reports a real cost: the unresolved charge settles to it.
    l.reserve(JobId(3), Some(usd(90))).unwrap();
    assert_eq!(l.commit(JobId(3), charge("k1", Some(usd(70)), false)).unwrap(), Some(usd(70)));
    let k1 = l.usage().entries().find(|e| e.idempotency_key == "k1").unwrap().clone();
    assert_eq!((k1.cost, k1.status), (usd(70), Charge::Actual));
    assert_eq!(l.usage().spent("s").session.get(&Currency::USD), Some(&usd(160)));
    // An unpriced job with an implausible report has nothing sound to charge.
    l.reserve(JobId(4), None).unwrap();
    assert!(l.commit(JobId(4), charge("k4", Some(Money::new(Currency::EUR, 1)), false)).is_ok_and(|c| c == Some(Money::new(Currency::EUR, 1))));
    l.reserve(JobId(5), None).unwrap();
    assert!(l.commit(JobId(5), charge("k5", Some(usd(u64::MAX)), false)).is_err());
}

#[test]
fn oversized_estimates_and_unrecordable_jobs_are_refused() {
    let l = Ledger::new(Budgets::default(), false, "s", UsageLog::default());
    let mut m = l.clone();
    assert_eq!(m.reserve(JobId(1), Some(usd(crate::cost::MAX_CHARGE_MICROS + 1))), Err(Refusal::Overflow));
    assert!(l.recordable("fal", "flux", "nodecraft:r:1").is_ok());
    assert_eq!(l.recordable("fal", "flux", ""), Err(Refusal::MissingKey));
    assert_eq!(l.recordable("fal", "flux", &"k".repeat(MAX_TEXT - KEY_SUFFIX_ROOM + 1)), Err(Refusal::Unrecordable));
    assert_eq!(l.recordable(&"p".repeat(MAX_TEXT + 1), "flux", "k"), Err(Refusal::Unrecordable));
}
