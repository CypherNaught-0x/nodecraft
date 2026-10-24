use super::*;
use nodecraft_types::Currency;

fn usd(micros: u64) -> Money {
    Money { currency: Currency::USD, micros }
}

#[test]
fn quotes_are_kept_per_routing_and_replaced_by_newer_ones() {
    let mut quotes = ProviderQuotes::default();
    quotes.keep(Some([1; 32]), 0, [([7; 32], usd(10)), ([8; 32], usd(20))]);
    assert_eq!(quotes.get(&[7; 32], Some([1; 32])), Some(usd(10)));
    assert_eq!(quotes.get(&[7; 32], Some([2; 32])), None, "another routing may be another account");
    let generation = quotes.generation;
    quotes.keep(Some([1; 32]), 0, [([7; 32], usd(30))]);
    assert_eq!(quotes.get(&[7; 32], Some([1; 32])), Some(usd(30)));
    assert_eq!(quotes.len(), 2);
    assert_ne!(quotes.generation, generation);
    quotes.keep(Some([2; 32]), 0, [([9; 32], usd(5))]);
    assert_eq!(quotes.len(), 1, "a new routing drops the old quotes");
    assert_eq!(quotes.get(&[8; 32], Some([1; 32])), None);
}

#[test]
fn the_oldest_quotes_go_first() {
    let mut quotes = ProviderQuotes::default();
    let key = |n: usize| -> QuoteKey {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&(n as u64).to_le_bytes());
        key
    };
    quotes.keep(None, 0, (0..MAX_QUOTES + 3).map(|n| (key(n), usd(n as u64))));
    assert_eq!(quotes.len(), MAX_QUOTES);
    assert_eq!(quotes.get(&key(2), None), None);
    assert_eq!(quotes.get(&key(3), None), Some(usd(3)));
    assert_eq!(quotes.get(&key(MAX_QUOTES + 2), None), Some(usd((MAX_QUOTES + 2) as u64)));
}

#[test]
fn only_generation_work_has_a_key() {
    let work = JobSpec::Source { node: NodeId::from_bytes([1; 16]), output: "text".into(), value: nodecraft_types::Value::Text("sea".into()) };
    assert_eq!(key(&work).ok(), Some(None));
}

#[test]
fn quotes_expire() {
    let mut quotes = ProviderQuotes::default();
    quotes.keep(None, 100, [([7; 32], usd(10))]);
    quotes.keep(None, 200, [([8; 32], usd(20))]);
    assert!(!quotes.has_expired(100));
    let generation = quotes.generation;
    quotes.expire(100);
    assert_eq!(quotes.generation, generation, "nothing expired");
    assert!(quotes.has_expired(150));
    quotes.expire(150);
    assert_eq!(quotes.get(&[7; 32], None), None);
    assert_eq!(quotes.get(&[8; 32], None), Some(usd(20)));
    assert_ne!(quotes.generation, generation, "a run priced with an expired quote is quoted again");
}
