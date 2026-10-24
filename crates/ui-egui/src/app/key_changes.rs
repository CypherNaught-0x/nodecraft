//! Browser key-service writes answer synchronously, including requests outside the keys panel.
//! The host observes successful writes without copying or recording the secret.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use nodecraft_engine::secrets::{Account, Secret, SecretError, SecretStore};

#[derive(Default)]
pub(super) struct KeyChanges(AtomicBool);

impl KeyChanges {
    pub(super) fn take(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

pub(super) struct ObservedStore {
    inner: Arc<dyn SecretStore>,
    changed: Arc<KeyChanges>,
}

impl ObservedStore {
    pub(super) fn new(inner: Arc<dyn SecretStore>, changed: Arc<KeyChanges>) -> Self {
        Self { inner, changed }
    }
}

impl SecretStore for ObservedStore {
    fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError> {
        self.inner.get(account)
    }

    fn set(&self, account: &Account, secret: &Secret) -> Result<(), SecretError> {
        self.inner.set(account, secret)?;
        self.changed.0.store(true, Ordering::Release);
        Ok(())
    }

    fn delete(&self, account: &Account) -> Result<bool, SecretError> {
        let removed = self.inner.delete(account)?;
        self.changed.0.store(true, Ordering::Release);
        Ok(removed)
    }
}
