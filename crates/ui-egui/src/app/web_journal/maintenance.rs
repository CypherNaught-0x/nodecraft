//! Bounded inventory and oldest-first recovery cleanup. A claim is deletion authority; every
//! temporary claim is released, including on errors or cancelled futures.
use super::*;

pub(super) struct Scanned {
    pub offer: Option<Offer>,
    pub inventory: Inventory,
    pub pruned: usize,
}

pub(super) struct Written {
    pub inventory: Inventory,
    pub pruned: usize,
}

struct Claims {
    shelf: Rc<RefCell<Box<dyn Shelf>>>,
    slots: Vec<String>,
}

impl Claims {
    fn new(shelf: Rc<RefCell<Box<dyn Shelf>>>) -> Self {
        Self { shelf, slots: Vec::new() }
    }

    async fn take(&mut self, slot: &str) -> Result<bool, String> {
        if self.slots.iter().any(|held| held == slot) {
            return Ok(true);
        }
        let claim = self.shelf.try_borrow_mut().map_err(|_| busy())?.claim(slot);
        if !claim.await? {
            return Ok(false);
        }
        self.slots.push(slot.to_owned());
        Ok(true)
    }

    fn keep(&mut self, slot: &str) {
        self.slots.retain(|held| held != slot);
    }

    fn release(&mut self, slot: &str) {
        if let Ok(mut shelf) = self.shelf.try_borrow_mut() {
            shelf.release(slot);
        }
        self.keep(slot);
    }
}

impl Drop for Claims {
    fn drop(&mut self) {
        if let Ok(mut shelf) = self.shelf.try_borrow_mut() {
            for slot in &self.slots {
                shelf.release(slot);
            }
        }
    }
}

async fn inventory(shelf: &Rc<RefCell<Box<dyn Shelf>>>) -> Result<Inventory, String> {
    let listed = shelf.try_borrow_mut().map_err(|_| busy())?.slots();
    listed.await
}

fn total(inventory: &Inventory) -> u64 {
    // wasm32's usize would saturate above 4 GiB, then undercount after removing a slot.
    // The bounded IndexedDB key count and u32 value lengths fit comfortably in u64.
    inventory.values().fold(0u64, |sum, slot| sum.saturating_add(slot.bytes))
}

/// Only a successfully claimed, unprotected slot may be pruned. A live tab can exceed the
/// budget; it must never lose its recovery copy to make space for another tab.
async fn prune(claims: &mut Claims, inventory: &mut Inventory, writes: &mut Vec<Write>, protected: &[String], budget: usize) -> Result<usize, String> {
    let mut bytes = total(inventory);
    let mut pruned = 0usize;
    let oldest: Vec<_> = inventory.keys().filter(|slot| !protected.contains(slot)).cloned().collect();
    for slot in oldest {
        if bytes <= budget as u64 {
            break;
        }
        if claims.take(&slot).await?
            && let Some(removed) = inventory.remove(&slot)
        {
            bytes = bytes.saturating_sub(removed.bytes);
            writes.push(Write::Remove { slot });
            pruned = pruned.saturating_add(1);
        }
    }
    Ok(pruned)
}

pub(super) async fn scan(shelf: Rc<RefCell<Box<dyn Shelf>>>, own: String, budget: usize) -> Result<Scanned, String> {
    let mut claims = Claims::new(Rc::clone(&shelf));
    if !claims.take(&own).await? {
        return Err("another tab has this tab's recovery slot".into());
    }
    claims.keep(&own);
    let mut inventory = inventory(&shelf).await?;
    let newest: Vec<_> = inventory.iter().rev().filter(|(slot, _)| **slot != own).map(|(slot, entry)| (slot.clone(), *entry)).collect();
    let mut writes = Vec::new();
    let mut offer = None;
    for (index, (slot, entry)) in newest.into_iter().enumerate() {
        // Beyond the offer window, inspect checkpoint-only slots too: a clean checkpoint
        // left by an untouched tab is not recoverable work and must not live forever.
        if (index >= MAX_SCAN && entry.dirty && entry.checkpoint) || !claims.take(&slot).await? {
            continue;
        }
        let read = shelf.try_borrow_mut().map_err(|_| busy())?.read(&slot);
        match replay(&read.await?) {
            Some(found) if index < MAX_SCAN && offer.is_none() => offer = Some(Offer { slot, found }),
            Some(_) => claims.release(&slot),
            None => {
                inventory.remove(&slot);
                writes.push(Write::Remove { slot });
            }
        }
    }
    let mut protected = vec![own];
    if let Some(offer) = &offer {
        protected.push(offer.slot.clone());
    }
    let pruned = prune(&mut claims, &mut inventory, &mut writes, &protected, budget).await?;
    if !writes.is_empty() {
        let written = shelf.try_borrow_mut().map_err(|_| busy())?.write(writes);
        written.await?;
    }
    if let Some(offer) = &offer {
        claims.keep(&offer.slot);
    }
    Ok(Scanned { offer, inventory, pruned })
}

fn project(inventory: &mut Inventory, writes: &[Write]) {
    for write in writes {
        match write {
            Write::Checkpoint { slot, bytes } => {
                inventory.insert(slot.clone(), Slot { bytes: bytes.len() as u64, hold_bytes: None, dirty: false, checkpoint: true });
            }
            Write::Frame { slot, bytes, .. } => {
                let entry = inventory.entry(slot.clone()).or_default();
                entry.bytes = entry.bytes.saturating_add(bytes.len() as u64);
                entry.dirty = true;
            }
            Write::Hold { slot } => {
                let entry = inventory.entry(slot.clone()).or_default();
                entry.bytes = entry.bytes.saturating_sub(entry.hold_bytes.unwrap_or_default()).saturating_add(1);
                entry.hold_bytes = Some(1);
                entry.dirty = true;
            }
            Write::Remove { slot } => {
                inventory.remove(slot);
            }
        }
    }
}

pub(super) async fn write(
    shelf: Rc<RefCell<Box<dyn Shelf>>>,
    mut writes: Vec<Write>,
    cached: Option<Inventory>,
    protected: Vec<String>,
    budget: usize,
) -> Result<Written, String> {
    let mut projected = match cached {
        Some(mut cached) => {
            project(&mut cached, &writes);
            if total(&cached) <= budget as u64 {
                cached
            } else {
                // Refresh before deleting: other tabs may have saved, grown or closed.
                let mut fresh = inventory(&shelf).await?;
                project(&mut fresh, &writes);
                fresh
            }
        }
        None => {
            let mut fresh = inventory(&shelf).await?;
            project(&mut fresh, &writes);
            fresh
        }
    };
    let mut claims = Claims::new(Rc::clone(&shelf));
    // Removing old copies and publishing the replacement is one transaction. On failure the
    // caller drops this inventory and retries from the shelf, without claiming any removals.
    let pruned = prune(&mut claims, &mut projected, &mut writes, &protected, budget).await?;
    if !writes.is_empty() {
        let written = shelf.try_borrow_mut().map_err(|_| busy())?.write(writes);
        written.await?;
    }
    Ok(Written { inventory: projected, pruned })
}

pub(super) fn notice(pruned: usize) -> String {
    let copies = if pruned == 1 { "copy" } else { "copies" };
    format!("Removed {pruned} old recovery {copies} to make room within this site's recovery storage budget. Work in open tabs was kept.")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MetadataShelf {
        inventory: Inventory,
        removed: Rc<RefCell<Vec<String>>>,
    }

    impl Shelf for MetadataShelf {
        fn slots(&mut self) -> PickFuture<Inventory> {
            Box::pin(std::future::ready(Ok(self.inventory.clone())))
        }
        fn read(&mut self, _: &str) -> PickFuture<Stored> {
            Box::pin(std::future::ready(Err("inventory pruning must not read payloads".into())))
        }
        fn write(&mut self, writes: Vec<Write>) -> PickFuture<()> {
            for write in writes {
                match write {
                    Write::Remove { slot } => self.removed.borrow_mut().push(slot),
                    Write::Hold { .. } => {}
                    _ => panic!("only pruning or hold markers were requested"),
                }
            }
            Box::pin(std::future::ready(Ok(())))
        }
        fn claim(&mut self, _: &str) -> PickFuture<bool> {
            Box::pin(std::future::ready(Ok(true)))
        }
        fn release(&mut self, _: &str) {}
    }

    #[test]
    fn inventory_above_four_gib_prunes_every_required_copy_without_payload_allocations() {
        let budget = MAX_RECOVERY_BYTES;
        let mut inventory: Inventory =
            (0..6).map(|n| (format!("old-{n}"), Slot { bytes: 1u64 << 30, hold_bytes: None, checkpoint: true, dirty: true })).collect();
        inventory.insert("own".into(), Slot { bytes: budget as u64, hold_bytes: None, checkpoint: true, dirty: true });
        let bytes: u64 = total(&inventory);
        assert_eq!(bytes, (6u64 << 30) + budget as u64, "accounting stays 64-bit on wasm32 too");
        let removed = Rc::new(RefCell::new(Vec::new()));
        let shelf: Rc<RefCell<Box<dyn Shelf>>> = Rc::new(RefCell::new(Box::new(MetadataShelf { inventory, removed: Rc::clone(&removed) })));
        let mut writing: PickFuture<Written> = Box::pin(write(shelf, Vec::new(), None, vec!["own".into()], budget));
        let Poll::Ready(written) = poll(&mut writing, &egui::Context::default()) else { panic!("the metadata shelf never waits") };
        let written = written.unwrap();
        assert_eq!(written.pruned, 6);
        assert_eq!(removed.borrow().len(), 6);
        assert_eq!(written.inventory.keys().cloned().collect::<Vec<_>>(), ["own"]);
        assert_eq!(total(&written.inventory), budget as u64);
    }

    #[test]
    fn replacing_a_hold_marker_never_prunes_a_copy_that_still_fits_the_exact_budget() {
        let budget = MAX_RECOVERY_BYTES;
        for old_hold in [0, 1, 17] {
            let inventory = Inventory::from([
                ("old".into(), Slot { bytes: 100, hold_bytes: None, checkpoint: true, dirty: true }),
                ("own".into(), Slot { bytes: budget as u64 - 101 + old_hold, hold_bytes: Some(old_hold), checkpoint: true, dirty: true }),
            ]);
            let removed = Rc::new(RefCell::new(Vec::new()));
            let shelf: Rc<RefCell<Box<dyn Shelf>>> =
                Rc::new(RefCell::new(Box::new(MetadataShelf { inventory: inventory.clone(), removed: Rc::clone(&removed) })));
            let writes = vec![Write::Hold { slot: "own".into() }, Write::Hold { slot: "own".into() }];
            let mut writing: PickFuture<Written> = Box::pin(write(shelf, writes, Some(inventory), vec!["own".into()], budget));
            let Poll::Ready(written) = poll(&mut writing, &egui::Context::default()) else { panic!("the metadata shelf never waits") };
            let written = written.unwrap();
            assert_eq!(written.pruned, 0, "replacement must account for an existing {old_hold}-byte hold");
            assert!(removed.borrow().is_empty());
            assert_eq!(total(&written.inventory), budget as u64);
            assert_eq!(written.inventory.get("own").unwrap().hold_bytes, Some(1));
        }
    }
}
