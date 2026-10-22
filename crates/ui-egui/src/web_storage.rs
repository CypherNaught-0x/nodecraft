//! The browser's `localStorage` for this origin, as the engine's [`OriginStorage`] (#118): where
//! the browser build keeps provider keys. Every call looks the storage up again, so the store holds
//! no browser object and stays `Send + Sync`.

use nodecraft_engine::secrets::{OriginStorage, SecretError};

/// Entries looked at when listing; a page with more is not one NodeCraft filled.
const MAX_ENTRIES: u32 = 4096;

#[derive(Debug, Default, Clone, Copy)]
pub struct LocalStorage;

fn storage() -> Result<web_sys::Storage, SecretError> {
    // Blocked site data and some private windows make this fail or return nothing.
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .ok_or_else(|| SecretError::Unavailable("this browser doesn't let the site store data; allow site data, then reload".into()))
}

impl OriginStorage for LocalStorage {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        storage()?.get_item(key).map_err(|_| SecretError::Failed("the browser couldn't read its storage".into()))
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        // The usual refusal is a full quota; the browser's message is not passed on.
        storage()?.set_item(key, value).map_err(|_| SecretError::Failed("this site's browser storage is full or refused the key".into()))
    }
    fn remove(&self, key: &str) -> Result<(), SecretError> {
        storage()?.remove_item(key).map_err(|_| SecretError::Failed("the browser couldn't change its storage".into()))
    }
    fn keys(&self) -> Result<Vec<String>, SecretError> {
        let storage = storage()?;
        let len = storage.length().map_err(|_| SecretError::Failed("the browser couldn't read its storage".into()))?;
        if len > MAX_ENTRIES {
            log::warn!("this site's browser storage has {len} entries; only the first {MAX_ENTRIES} are checked for keys");
        }
        Ok((0..len.min(MAX_ENTRIES)).filter_map(|i| storage.key(i).ok().flatten()).collect())
    }
}
