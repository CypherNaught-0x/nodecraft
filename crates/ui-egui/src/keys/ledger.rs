//! Accepted-operation ownership is separate from row waiting/display state.
use super::{AccountChange, Op};
use nodecraft_engine::{secrets::Account, settings::KeySettings};
use std::collections::{BTreeMap, BTreeSet};
pub(super) const MAX_REQUESTS: usize = 128;
pub(super) const MAX_ROWS: usize = KeySettings::MAX_ACCOUNTS + MAX_REQUESTS + 32;
#[derive(Clone)]
pub(super) struct Request {
    pub account: Account,
    pub op: Op,
    pub seq: u64,
    pub since: f64,
    pub timed_out: bool,
    pub remote: Option<u64>,
}
#[derive(Default)]
pub(super) struct Ledger {
    next: u64,
    pub requests: BTreeMap<u64, Request>,
    pub known: BTreeSet<Account>,
    latest: BTreeMap<Account, u64>,
    // Keep successful-state tombstones while older accepted operations still exist.
    success: BTreeMap<Account, u64>,
}
impl Ledger {
    pub fn load(&mut self, settings: &KeySettings) {
        self.known = settings.accounts().into_iter().collect();
    }
    fn occupied(&self) -> BTreeSet<&Account> {
        self.known
            .iter()
            .chain(self.requests.values().filter(|r| r.op == Op::Set && self.success.get(&r.account).is_none_or(|seq| r.seq > *seq)).map(|r| &r.account))
            .collect()
    }
    pub fn begin(&mut self, op: Op, account: &Account, now: f64) -> Result<Request, String> {
        if self.requests.len() >= MAX_REQUESTS {
            return Err("The keychain has too many unresolved requests. Wait for their answers before trying again.".into());
        }
        let occupied = self.occupied();
        if op == Op::Set && !occupied.contains(account) && occupied.len() >= KeySettings::MAX_ACCOUNTS {
            return Err(format!(
                "At most {} accounts can be remembered, including pending additions. Remove an account before adding another.",
                KeySettings::MAX_ACCOUNTS
            ));
        }
        self.next = self.next.checked_add(1).ok_or("The keychain request sequence is exhausted; restart NodeCraft.")?;
        self.latest.insert(account.clone(), self.next);
        Ok(Request { account: account.clone(), op, seq: self.next, since: now, timed_out: false, remote: None })
    }
    pub fn retain(&mut self, request: Request) {
        self.requests.insert(request.seq, request);
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn take(&mut self, id: u64, account: &str) -> Option<Request> {
        let mut matches = self.requests.values().filter(|r| r.remote == Some(id) && r.account.as_str() == account);
        let seq = matches.next()?.seq;
        if matches.next().is_some() {
            return None;
        } // An ambiguous host id cannot establish an outcome.
        self.requests.remove(&seq)
    }
    pub fn latest(&self, r: &Request) -> bool {
        self.latest.get(&r.account) == Some(&r.seq)
    }
    pub fn waiting(&self) -> bool {
        self.requests.values().any(|r| !r.timed_out)
    }
    pub fn protected(&self, a: &Account) -> bool {
        self.known.contains(a) || self.requests.values().any(|r| &r.account == a)
    }
    pub fn expire(&mut self, now: f64, limit: f64) -> Vec<Request> {
        let mut expired = Vec::new();
        for r in self.requests.values_mut() {
            if !r.timed_out && now - r.since > limit {
                r.timed_out = true;
                expired.push(r.clone());
            }
        }
        expired
    }
    /// Serial worker execution order, not delivery order, determines remembered state.
    pub fn reconcile(&mut self, r: &Request, stored: bool) -> Result<Option<AccountChange>, String> {
        if self.success.get(&r.account).is_some_and(|seq| *seq >= r.seq) {
            return Ok(None);
        }
        if stored && !self.known.contains(&r.account) {
            let occupied = self.occupied();
            if !occupied.contains(&r.account) && occupied.len() >= KeySettings::MAX_ACCOUNTS {
                return Err("Key exists, but this account could not be remembered: the 64-account limit is full. Remove an account, then Check again.".into());
            }
        }
        self.success.insert(r.account.clone(), r.seq);
        let changed = if stored { self.known.insert(r.account.clone()) } else { self.known.remove(&r.account) };
        Ok(if changed { Some(if stored { AccountChange::Remember(r.account.clone()) } else { AccountChange::Forget(r.account.clone()) }) } else { None })
    }
    pub fn prune(&mut self, rows: &[(Account, super::KeyStatus)]) {
        let keep = |a: &Account| self.protected(a) || rows.iter().any(|(row, _)| row == a);
        let retained: BTreeSet<Account> = self.latest.keys().chain(self.success.keys()).filter(|a| keep(a)).cloned().collect();
        self.latest.retain(|a, _| retained.contains(a));
        self.success.retain(|a, _| retained.contains(a));
    }
}
