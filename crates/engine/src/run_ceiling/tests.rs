use super::*;

fn target() -> ProviderTarget {
    ProviderTarget::default_instance("mock").unwrap()
}

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

#[test]
fn debits_stop_at_the_accepted_amount_exactly() {
    let ceiling = RunCeiling::new([(Currency::USD, 10)].into());
    ceiling.debit(&target(), "a", usd(6), false).unwrap();
    assert!(ceiling.debit(&target(), "b", usd(5), false).is_err(), "one over");
    assert_eq!(ceiling.status(Currency::USD), (10, 6), "a refusal changes nothing");
    ceiling.debit(&target(), "b", usd(4), false).unwrap();
    assert_eq!(ceiling.status(Currency::USD), (10, 10));
    assert!(!ceiling.debit(&target(), "a", usd(6), false).unwrap());
    assert_eq!(ceiling.status(Currency::USD), (10, 10), "a reservation is debited once");
}

#[test]
fn a_currency_without_an_accepted_amount_authorizes_nothing() {
    let ceiling = RunCeiling::new([(Currency::USD, 10)].into());
    let err = ceiling.debit(&target(), "a", Money::new(Currency::EUR, 1), false).unwrap_err();
    assert!(err.contains("nothing in EUR"), "{err}");
    assert_eq!(ceiling.status(Currency::EUR), (0, 0));
}

#[test]
fn credits_apply_once_and_only_to_this_runs_debits() {
    let ceiling = RunCeiling::new([(Currency::USD, 10)].into());
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
    let ceiling = RunCeiling::new([(Currency::USD, 10)].into());
    ceiling.debit(&target(), "a", usd(u64::MAX), true).unwrap();
    assert_eq!(ceiling.status(Currency::USD), (10, u64::MAX));
    assert!(ceiling.debit(&target(), "b", usd(1), false).is_err());
    ceiling.debit(&target(), "c", usd(1), true).unwrap();
    assert_eq!(ceiling.status(Currency::USD), (10, u64::MAX), "saturates");
}

#[test]
fn clones_share_one_ledger() {
    let ceiling = RunCeiling::new([(Currency::USD, 10)].into());
    let copy = ceiling.clone();
    copy.debit(&target(), "a", usd(7), false).unwrap();
    assert!(ceiling.debit(&target(), "b", usd(4), false).is_err());
    assert_eq!(ceiling, copy);
    assert_ne!(ceiling, RunCeiling::new(BTreeMap::new()));
}
