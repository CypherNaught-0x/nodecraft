//! Finite probabilistic deduplication only. Never evidence of monetary ownership.
use super::{Error, ErrorKind, MAX_RETIRED_KEYS};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    mem::size_of,
};
pub(super) const HASH_CAP: usize = 4 * MAX_RETIRED_KEYS;
pub(super) const BATCH_CAP: usize = super::MAX_USAGE_ENTRIES + super::MAX_PENDING + super::MAX_METADATA_SHELLS;
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct Key(pub(super) [u8; 33]);
fn cap() -> Error {
    Error::new(ErrorKind::Busy, "retired accounting identity capacity is unavailable")
}
#[derive(Clone, Debug, Default)]
pub(super) struct Store {
    pub(super) fifo: VecDeque<Key>,
    set: HashSet<Key>,
}
#[derive(Clone, Copy)]
struct Delta {
    removed: Option<Key>,
    inserted: Key,
}
pub(super) struct Stage {
    changes: Vec<Delta>,
    fifo: Option<VecDeque<Key>>,
    set: Option<HashSet<Key>>,
}
impl Store {
    pub(super) fn contains(&self, key: &Key) -> bool {
        self.set.contains(key)
    }
    pub(super) fn logical_bytes(&self) -> usize {
        size_of::<Self>() + 33 * (self.fifo.capacity() + self.set.capacity())
    }
    pub(super) fn prepare(&self, keys: impl Iterator<Item = Key>) -> Result<Stage, Error> {
        let mut keys = keys.peekable();
        // B=0: no table hashing, reservation or traversal, independent of N.
        if keys.peek().is_none() {
            return Ok(Stage { changes: Vec::new(), fifo: None, set: None });
        }
        // Stage is bounded by actual incoming operations, never the retained N.
        let mut incoming = Vec::new();
        for key in keys {
            if incoming.len() >= BATCH_CAP {
                return Err(cap());
            }
            if incoming.len() == incoming.capacity() {
                let target = incoming.capacity().saturating_mul(2).clamp(1, BATCH_CAP);
                allocation_gate()?;
                incoming.try_reserve_exact(target - incoming.len()).map_err(|_| cap())?;
                if incoming.capacity() > BATCH_CAP {
                    return Err(cap());
                }
            }
            incoming.push(key);
        }
        let b = incoming.len();
        if incoming.capacity() > b.saturating_mul(2) {
            return Err(cap());
        }
        // Simulation logical peak includes the bounded input owner too:
        // 33*Ci + size_of::<Delta>()*Cd + size_of::<(Key,bool)>()*Co + 33*Ct
        // plus four container headers. Ci<=2B, Cd<=B, Co<=8B, Ct<=min(B,K).
        // Private hash-table metadata/allocator overhead are not RSS claims.
        let mut changes = Vec::new();
        allocation_gate()?;
        changes.try_reserve_exact(b).map_err(|_| cap())?;
        let mut overlay = HashMap::<Key, bool>::new();
        allocation_gate()?;
        overlay.try_reserve(b.checked_mul(2).ok_or_else(cap)?).map_err(|_| cap())?;
        let mut tail = VecDeque::new();
        allocation_gate()?;
        tail.try_reserve_exact(b.min(MAX_RETIRED_KEYS)).map_err(|_| cap())?;
        if changes.capacity() > b || overlay.capacity() > 8 * b || tail.capacity() > b.min(MAX_RETIRED_KEYS) {
            return Err(cap());
        }
        let mut front = 0usize;
        let mut n = self.fifo.len();
        for key in incoming {
            if overlay.get(&key).copied().unwrap_or_else(|| self.set.contains(&key)) {
                continue;
            }
            let removed = if n == MAX_RETIRED_KEYS {
                let old = if front < self.fifo.len() {
                    note_front();
                    let old = self.fifo.get(front).copied().ok_or_else(cap)?;
                    front += 1;
                    old
                } else {
                    tail.pop_front().ok_or_else(cap)?
                };
                overlay.insert(old, false);
                Some(old)
            } else {
                n += 1;
                None
            };
            tail.push_back(key);
            overlay.insert(key, true);
            changes.push(Delta { removed, inserted: key });
        }
        let required = self.fifo.len().checked_add(changes.len()).ok_or_else(cap)?;
        let fifo = if self.fifo.capacity() < required.min(MAX_RETIRED_KEYS) {
            let target = required.min(MAX_RETIRED_KEYS).max(self.fifo.capacity().saturating_mul(2).min(MAX_RETIRED_KEYS));
            let mut replacement = VecDeque::new();
            allocation_gate()?;
            replacement.try_reserve_exact(target).map_err(|_| cap())?;
            if replacement.capacity() > MAX_RETIRED_KEYS {
                return Err(cap());
            }
            note_copy(self.fifo.len());
            replacement.extend(self.fifo.iter().copied());
            Some(replacement)
        } else {
            None
        };
        let set = if self.set.capacity() < required {
            let target = required.checked_mul(2).ok_or_else(cap)?;
            let mut replacement = HashSet::new();
            allocation_gate()?;
            replacement.try_reserve(target).map_err(|_| cap())?;
            if replacement.capacity() > HASH_CAP {
                return Err(cap());
            }
            note_copy(self.set.len());
            replacement.extend(self.set.iter().copied());
            Some(replacement)
        } else {
            None
        };
        Ok(Stage { changes, fifo, set })
    }
    pub(super) fn admit(keys: impl Iterator<Item = Key>) -> Result<Self, Error> {
        let mut all = Vec::new();
        for key in keys {
            if all.len() >= MAX_RETIRED_KEYS {
                return Err(cap());
            }
            if all.len() == all.capacity() {
                let target = all.capacity().saturating_mul(2).clamp(1, MAX_RETIRED_KEYS);
                all.try_reserve_exact(target - all.len()).map_err(|_| cap())?;
                if all.capacity() > MAX_RETIRED_KEYS {
                    return Err(cap());
                }
            }
            all.push(key);
        }
        let mut result = Self::default();
        result.fifo.try_reserve_exact(all.len()).map_err(|_| cap())?;
        result.set.try_reserve(all.len().checked_mul(2).ok_or_else(cap)?).map_err(|_| cap())?;
        if result.fifo.capacity() > MAX_RETIRED_KEYS || result.set.capacity() > HASH_CAP {
            return Err(cap());
        }
        for key in all {
            if !result.set.insert(key) {
                return Err(cap());
            }
            result.fifo.push_back(key);
        }
        Ok(result)
    }
}
impl Stage {
    /// All allocation, capacity growth and simulation completed before this point.
    pub(super) fn commit(self, store: &mut Store) {
        if let Some(fifo) = self.fifo {
            store.fifo = fifo;
        }
        if let Some(set) = self.set {
            store.set = set;
        }
        for change in self.changes {
            if let Some(old) = change.removed {
                store.fifo.pop_front();
                store.set.remove(&old);
            }
            store.fifo.push_back(change.inserted);
            store.set.insert(change.inserted);
        }
    }
}

// Test-only fault gates model each explicit fallible allocation boundary. They do not
// claim to replace the process allocator or force OS exhaustion.
fn allocation_gate() -> Result<(), Error> {
    #[cfg(test)]
    {
        let index = ALLOCATIONS.with(|n| {
            let i = n.get();
            n.set(i + 1);
            i
        });
        if FAIL.with(|f| f.get() == Some(index)) {
            return Err(cap());
        }
    }
    Ok(())
}
fn note_copy(_n: usize) {
    #[cfg(test)]
    COPIES.with(|n| n.set(n.get() + _n));
}
fn note_front() {
    #[cfg(test)]
    FRONTS.with(|n| n.set(n.get() + 1));
}
#[cfg(test)]
thread_local! {static FAIL:std::cell::Cell<Option<usize>>=const{std::cell::Cell::new(None)};static ALLOCATIONS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};static COPIES:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};static FRONTS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
pub(super) fn fail_at(index: Option<usize>) {
    FAIL.with(|n| n.set(index));
    ALLOCATIONS.with(|n| n.set(0));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(n: usize) -> Key {
        let mut k = [0u8; 33];
        k.get_mut(1..9).unwrap().copy_from_slice(&(n as u64).to_le_bytes());
        Key(k)
    }
    #[test]
    fn layout_and_resource_formula_match_the_refinement() {
        let d = size_of::<Delta>();
        let o = size_of::<(Key, bool)>();
        let headers = size_of::<Vec<Key>>() + size_of::<Vec<Delta>>() + size_of::<HashMap<Key, bool>>() + size_of::<VecDeque<Key>>();
        let replacement_headers = size_of::<VecDeque<Key>>() + size_of::<HashSet<Key>>();
        let bound = 33 * (MAX_RETIRED_KEYS + HASH_CAP) * 2
            + 33 * BATCH_CAP
            + d * BATCH_CAP
            + o * 8 * BATCH_CAP
            + 33 * BATCH_CAP
            + headers
            + size_of::<Store>()
            + replacement_headers;
        println!(
            "retirement logical policy: Key={} Delta={d} overlay_pair={o} simulation_headers={headers} Store={} replacement_headers={replacement_headers} conservative_peak={bound}",
            size_of::<Key>(),
            size_of::<Store>()
        );
        assert_eq!(size_of::<Key>(), 33);
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!((d, o, headers, size_of::<Store>(), replacement_headers), (67, 34, 128, 80, 80));
            assert_eq!(bound, 131_100_288);
        }
    }
    fn reset() {
        ALLOCATIONS.with(|n| n.set(0));
        COPIES.with(|n| n.set(0));
        FRONTS.with(|n| n.set(0));
        FAIL.with(|n| n.set(None));
    }
    fn reference(fifo: &mut VecDeque<Key>, keys: &[Key]) {
        for key in keys {
            if fifo.contains(key) {
                continue;
            }
            if fifo.len() == MAX_RETIRED_KEYS {
                fifo.pop_front();
            }
            fifo.push_back(*key);
        }
    }
    #[test]
    fn full_window_empty_one_and_large_delta_match_sequential_fifo() {
        let mut store = Store::admit((0..MAX_RETIRED_KEYS).map(key)).unwrap();
        reset();
        let empty = store.prepare(std::iter::empty()).unwrap();
        empty.commit(&mut store);
        assert_eq!(ALLOCATIONS.with(|n| n.get()), 0);
        assert_eq!(COPIES.with(|n| n.get()), 0);
        assert_eq!(FRONTS.with(|n| n.get()), 0);
        reset();
        let one = store.prepare(std::iter::once(key(MAX_RETIRED_KEYS))).unwrap();
        one.commit(&mut store);
        assert_eq!(COPIES.with(|n| n.get()), 0);
        assert_eq!(FRONTS.with(|n| n.get()), 1);
        assert_eq!(store.fifo.front(), Some(&key(1)));
        // A key evicted earlier in the SAME batch may re-enter; duplicates do not refresh.
        let batch = vec![key(MAX_RETIRED_KEYS + 1), key(1), key(1), key(MAX_RETIRED_KEYS + 2)];
        let mut expected = store.fifo.clone();
        reference(&mut expected, &batch);
        reset();
        let stage = store.prepare(batch.into_iter()).unwrap();
        stage.commit(&mut store);
        assert_eq!(store.fifo, expected);
        assert_eq!(COPIES.with(|n| n.get()), 0);
        assert!(FRONTS.with(|n| n.get()) <= 4);
        let batch = (MAX_RETIRED_KEYS + 10..MAX_RETIRED_KEYS + 10 + BATCH_CAP).map(key).collect::<Vec<_>>();
        // For unique fresh keys the reference can use an independent bounded deque directly.
        for k in &batch {
            expected.pop_front();
            expected.push_back(*k);
        }
        let stage = store.prepare(batch.into_iter()).unwrap();
        stage.commit(&mut store);
        assert_eq!(store.fifo, expected);
        assert_eq!(store.set.len(), MAX_RETIRED_KEYS);
        assert!(store.fifo.capacity() <= MAX_RETIRED_KEYS);
        assert!(store.set.capacity() <= HASH_CAP);
    }
    #[test]
    fn every_staged_allocation_failure_preserves_logical_and_backing_state() {
        let store = Store::admit((0..32).map(key)).unwrap();
        reset();
        drop(store.prepare((32..96).map(key)).unwrap());
        let boundaries = ALLOCATIONS.with(|n| n.get());
        assert!(boundaries >= 5);
        let old = store.fifo.clone();
        let capacities = (store.fifo.capacity(), store.set.capacity());
        let bytes = store.logical_bytes();
        for boundary in 0..boundaries {
            reset();
            FAIL.with(|n| n.set(Some(boundary)));
            assert!(store.prepare((32..96).map(key)).is_err());
            assert_eq!(store.fifo, old);
            assert_eq!((store.fifo.capacity(), store.set.capacity()), capacities);
            assert_eq!(store.logical_bytes(), bytes);
            assert_eq!(store.set.len(), old.len());
        }
        reset();
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn real_allocator_sees_no_commit_allocation_with_positive_control() {
        let control = allocation_counter::measure(|| drop(std::hint::black_box(vec![key(1); 32])));
        assert!(control.count_total > 0);
        let mut store = Store::admit((0..MAX_RETIRED_KEYS).map(key)).unwrap();
        let stage = store.prepare((MAX_RETIRED_KEYS..MAX_RETIRED_KEYS + 1000).map(key)).unwrap();
        let measured = allocation_counter::measure(|| stage.commit(&mut store));
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_total, 0);
        assert_eq!(store.fifo.len(), MAX_RETIRED_KEYS);
    }
    #[test]
    fn growth_and_churn_copy_only_at_amortized_capacity_boundaries() {
        let mut store = Store::default();
        reset();
        for n in 0..4096 {
            let stage = store.prepare(std::iter::once(key(n))).unwrap();
            stage.commit(&mut store);
        }
        assert!(COPIES.with(|n| n.get()) < 4096 * 8);
        let mut store = Store::admit((0..MAX_RETIRED_KEYS).map(key)).unwrap();
        reset();
        for n in MAX_RETIRED_KEYS..MAX_RETIRED_KEYS + 10_000 {
            let stage = store.prepare(std::iter::once(key(n))).unwrap();
            stage.commit(&mut store);
        }
        assert_eq!(COPIES.with(|n| n.get()), 0);
        assert_eq!(FRONTS.with(|n| n.get()), 10_000);
    }
}
