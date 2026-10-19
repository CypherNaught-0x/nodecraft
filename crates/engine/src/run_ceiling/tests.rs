use super::*;

fn target() -> ProviderTarget {
    ProviderTarget::default_instance("mock").unwrap()
}

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

fn ceiling(usd_limit: u64, unpriced: u64) -> RunCeiling {
    RunCeiling::new([(Currency::USD, usd_limit)].into(), unpriced)
}

#[test]
fn debits_stop_at_the_accepted_amount_exactly() {
    let ceiling = ceiling(10, 0);
    assert!(ceiling.debit(&target(), "a", usd(6), false).unwrap());
    assert!(ceiling.debit(&target(), "b", usd(5), false).is_err(), "one over");
    assert_eq!(ceiling.status(Currency::USD), (10, 6), "a refusal changes nothing");
    ceiling.debit(&target(), "b", usd(4), false).unwrap();
    assert_eq!(ceiling.status(Currency::USD), (10, 10));
    assert!(!ceiling.debit(&target(), "a", usd(6), false).unwrap());
    assert_eq!(ceiling.status(Currency::USD), (10, 10), "a reservation is debited once");
}

#[test]
fn a_currency_without_an_accepted_amount_authorizes_nothing() {
    let ceiling = ceiling(10, 0);
    let err = ceiling.debit(&target(), "a", Money::new(Currency::EUR, 1), false).unwrap_err();
    assert!(err.contains("nothing in EUR"), "{err}");
    assert_eq!(ceiling.status(Currency::EUR), (0, 0));
}

#[test]
fn credits_apply_once_and_only_to_this_runs_debits() {
    let ceiling = ceiling(10, 0);
    ceiling.debit(&target(), "a", usd(10), false).unwrap();
    ceiling.credit(&target(), "other");
    assert_eq!(ceiling.status(Currency::USD), (10, 10));
    ceiling.credit(&target(), "a");
    ceiling.credit(&target(), "a");
    assert_eq!(ceiling.status(Currency::USD), (10, 0));
    ceiling.debit(&target(), "c", usd(10), false).unwrap();
}

#[test]
fn adopted_work_is_debited_past_the_ceiling_without_overflow() {
    let ceiling = ceiling(10, 0);
    ceiling.debit(&target(), "a", usd(u64::MAX), true).unwrap();
    assert_eq!(ceiling.status(Currency::USD), (10, u64::MAX));
    assert!(ceiling.debit(&target(), "b", usd(1), false).is_err());
    ceiling.debit(&target(), "c", usd(1), true).unwrap();
    assert_eq!(ceiling.status(Currency::USD), (10, u64::MAX), "saturates");
}

#[test]
fn the_record_bound_refuses_new_work_but_still_accounts_adopted_work() {
    let ceiling = ceiling(u64::MAX, u64::MAX);
    for n in 0..MAX_CEILING_RESERVATIONS {
        ceiling.debit(&target(), &format!("k{n}"), usd(1), false).unwrap();
        ceiling.admit_unpriced(&target(), &format!("u{n}"), false).unwrap();
    }
    assert!(ceiling.debit(&target(), "new", usd(1), false).is_err());
    assert!(ceiling.admit_unpriced(&target(), "new", false).is_err());
    let before = ceiling.status(Currency::USD).1;
    assert!(!ceiling.debit(&target(), "adopted", usd(5), true).unwrap(), "aggregated, never refundable");
    ceiling.credit(&target(), "adopted");
    assert_eq!(ceiling.status(Currency::USD).1, before + 5, "the adopted exposure stays");
    ceiling.admit_unpriced(&target(), "adopted", true).unwrap();
    assert_eq!(ceiling.unpriced_status().1, MAX_CEILING_RESERVATIONS as u64 + 1);
}

#[test]
fn unknown_price_jobs_use_the_accepted_count_once_per_submission() {
    let ceiling = ceiling(0, 2);
    ceiling.admit_unpriced(&target(), "a", false).unwrap();
    ceiling.admit_unpriced(&target(), "a", false).unwrap();
    assert_eq!(ceiling.unpriced_status(), (2, 1), "a same-key retry uses nothing more");
    ceiling.admit_unpriced(&target(), "b", false).unwrap();
    let err = ceiling.admit_unpriced(&target(), "c", false).unwrap_err();
    assert!(err.contains("used up"), "{err}");
    ceiling.admit_unpriced(&target(), "d", true).unwrap();
    assert_eq!(ceiling.unpriced_status(), (2, 3), "adopted work is counted past the limit");
    let none = RunCeiling::new(BTreeMap::new(), 0);
    assert!(none.admit_unpriced(&target(), "a", false).unwrap_err().contains("no jobs of unknown price"));
}

#[test]
fn clones_share_one_ledger() {
    let ceiling = ceiling(10, 0);
    let copy = ceiling.clone();
    copy.debit(&target(), "a", usd(7), false).unwrap();
    assert!(ceiling.debit(&target(), "b", usd(4), false).is_err());
    assert_eq!(ceiling, copy);
    assert_ne!(ceiling, RunCeiling::new(BTreeMap::new(), 0));
}

#[test]
fn a_saturated_total_is_never_reopened_by_credits() {
    let ceiling = ceiling(u64::MAX, 0);
    assert!(ceiling.debit(&target(), "legit", usd(100), false).unwrap());
    ceiling.debit(&target(), "h1", usd(u64::MAX - 100), true).unwrap();
    ceiling.debit(&target(), "h2", usd(u64::MAX - 100), true).unwrap();
    assert_eq!(ceiling.status(Currency::USD), (u64::MAX, u64::MAX));
    // The legitimate reservation is released without delivery: its credit can't reopen room
    // that the adopted work's lost exposure fills.
    ceiling.credit(&target(), "legit");
    assert_eq!(ceiling.status(Currency::USD), (u64::MAX, u64::MAX));
    assert!(ceiling.debit(&target(), "fresh", usd(100), false).is_err());
    assert!(ceiling.debit(&target(), "fresh", usd(1), false).is_err(), "no new work in that currency");
    // Another currency is unaffected.
    let other = RunCeiling::new([(Currency::USD, u64::MAX), (Currency::EUR, 10)].into(), 0);
    other.debit(&target(), "h", usd(u64::MAX), true).unwrap();
    other.debit(&target(), "h2", usd(1), true).unwrap();
    assert!(other.debit(&target(), "e", Money::new(Currency::EUR, 10), false).unwrap());
}

#[test]
fn exact_totals_still_refund_exactly() {
    let ceiling = ceiling(u64::MAX, 0);
    ceiling.debit(&target(), "a", usd(u64::MAX - 100), true).unwrap();
    assert!(ceiling.debit(&target(), "b", usd(100), false).unwrap());
    assert_eq!(ceiling.status(Currency::USD), (u64::MAX, u64::MAX), "exactly full, not saturated");
    ceiling.credit(&target(), "b");
    assert_eq!(ceiling.status(Currency::USD), (u64::MAX, u64::MAX - 100));
    assert!(ceiling.debit(&target(), "c", usd(100), false).unwrap());
}
