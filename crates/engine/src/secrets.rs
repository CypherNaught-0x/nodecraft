//! Provider API keys in the operating system's keychain.
//!
//! Keys live only in the keychain (macOS Keychain, Windows Credential Manager, the Secret Service
//! on Linux), under the service [`SERVICE`] with the provider's [`Account`] name. They never go
//! into project files, settings, logs or error messages. The value types and the store interface
//! live in `providers::secrets`, so adapters receive a [`Secret`]; this module adds the OS
//! keychain and the `keys` host service behind the `keys.*` commands.
//!
//! The browser build has no keychain; it will keep keys per origin with a notice (M10). Until then
//! hosts use a [`MemoryStore`] there (keys last for the session).

use std::sync::{Arc, Mutex, PoisonError};

pub use nodecraft_providers::secrets::{Account, MAX_SECRET_BYTES, MemoryStore, Secret, SecretError, SecretStore};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::commands::Services;

/// The keychain service name: the app id.
pub const SERVICE: &str = "io.github.cyphernaught0x.nodecraft";
/// The host-service capability the `keys.*` commands use.
pub const KEY_SERVICE: &str = "keys.store";

#[cfg(not(target_arch = "wasm32"))]
pub use keychain::KeyringStore;

#[cfg(not(target_arch = "wasm32"))]
mod keychain {
    use super::*;

    /// The operating system keychain, through the `keyring` crate.
    #[derive(Debug, Default)]
    pub struct KeyringStore;

    pub(super) fn map(e: keyring::Error) -> SecretError {
        use keyring::Error as E;
        match e {
            E::NoDefaultStore | E::NotSupportedByStore(_) => {
                // Keep the reason the keychain couldn't be opened; it is decided once per run.
                let reason = match keyring::Entry::store_status() {
                    Err(e) => e.to_string().chars().take(200).collect::<String>(),
                    Ok(()) => "not supported".into(),
                };
                let hint = if cfg!(target_os = "linux") {
                    "install or unlock a keyring (GNOME Keyring, KWallet), then restart NodeCraft"
                } else {
                    "restart NodeCraft"
                };
                SecretError::Unavailable(format!("{reason}; {hint}"))
            }
            E::NoStorageAccess(_) => SecretError::Denied("unlock the keychain and allow NodeCraft access".into()),
            // These carry the stored bytes: never echo them.
            E::BadEncoding(_) | E::BadDataFormat(..) => SecretError::Failed("the stored key can't be read; set it again".into()),
            E::Ambiguous(_) => SecretError::Failed("more than one key is stored under this name; remove and set it again".into()),
            E::TooLong(_, limit) => SecretError::Invalid(format!("the key is too long for this system's keychain (at most {limit})")),
            E::Invalid(..) => SecretError::Invalid("the keychain refused the key".into()),
            // Platform messages describe the failure, never the secret.
            other => SecretError::Failed(other.to_string().chars().take(300).collect()),
        }
    }

    fn entry(account: &Account) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, account.as_str()).map_err(map)
    }

    impl SecretStore for KeyringStore {
        fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError> {
            match entry(account)?.get_password() {
                // A value written by another tool or an older version that can't be sent: say so,
                // rather than pretending no key is set (which would surface later as a 401).
                Ok(v) => Secret::new(&v).map(Some).map_err(|_| SecretError::Failed("the stored key can't be used; set it again".into())),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(map(e)),
            }
        }
        fn set(&self, account: &Account, secret: &Secret) -> Result<(), SecretError> {
            entry(account)?.set_password(secret.expose()).map_err(map)
        }
        fn delete(&self, account: &Account) -> Result<bool, SecretError> {
            match entry(account)?.delete_credential() {
                Ok(()) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(e) => Err(map(e)),
            }
        }
    }
}

fn to_error(e: SecretError) -> Error {
    let kind = match e {
        SecretError::Invalid(_) => ErrorKind::InvalidInput,
        SecretError::Unavailable(_) => ErrorKind::Unsupported,
        // "Unlock your keychain" is something the user can fix, unlike an internal failure.
        SecretError::Denied(_) => ErrorKind::Authentication,
        _ => ErrorKind::Internal,
    };
    Error::new(kind, e.to_string())
}

/// A request to the `keys` host service (built by the `keys.*` commands).
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum KeyRequest {
    Set { account: String, key: String },
    Remove { account: String },
    Status { account: String },
}

/// Registers the `keys` host service on `services`, backed by `store`. Keychain calls can block
/// on an unlock prompt and `Session::dispatch` runs services synchronously, so the settings UI
/// must hand them to a worker thread (#205).
pub fn register_key_service(services: &mut Services, store: Arc<dyn SecretStore>) -> Result<(), Error> {
    let store = Mutex::new(store);
    services.register(KEY_SERVICE, move |request: &Value| {
        let request: KeyRequest = serde_json::from_value(request.clone()).map_err(|_| Error::new(ErrorKind::InvalidInput, "malformed key request"))?;
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        match request {
            KeyRequest::Set { account, key } => {
                let account = Account::new(&account).map_err(to_error)?;
                let secret = Secret::new(&key).map_err(to_error)?;
                store.set(&account, &secret).map_err(to_error)?;
                Ok(json!({ "stored": true, "masked": secret.masked() }))
            }
            KeyRequest::Remove { account } => {
                let account = Account::new(&account).map_err(to_error)?;
                Ok(json!({ "removed": store.delete(&account).map_err(to_error)? }))
            }
            KeyRequest::Status { account } => {
                let account = Account::new(&account).map_err(to_error)?;
                let found = store.get(&account).map_err(to_error)?;
                Ok(json!({ "stored": found.is_some(), "masked": found.map(|s| s.masked()) }))
            }
        }
    })
}

/// Checks a chat endpoint's key with a request that costs nothing (listing models). Some servers
/// list models without any key (Ollama, LM Studio, OpenRouter), so success there only says the
/// server is reachable; a rejected key is reported as `ErrorKind::Auth`.
pub async fn test_llm_key(client: &nodecraft_providers::llm::LlmClient) -> Result<usize, nodecraft_providers::ProviderError> {
    client.list_models().await.map(|models| models.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_service_round_trip_without_echoing_keys() {
        let mut services = Services::default();
        register_key_service(&mut services, Arc::new(MemoryStore::default())).unwrap();
        let set = services.invoke(KEY_SERVICE, &json!({"op": "set", "account": "openai", "key": "sk-proj-abcdefghij1234"})).unwrap();
        assert_eq!(set, json!({"stored": true, "masked": "…1234"}));
        assert!(!set.to_string().contains("abcdefghij"));
        let status = services.invoke(KEY_SERVICE, &json!({"op": "status", "account": "openai"})).unwrap();
        assert_eq!(status, json!({"stored": true, "masked": "…1234"}));
        let bad = services.invoke(KEY_SERVICE, &json!({"op": "set", "account": "openai", "key": "sk-secret with space"})).unwrap_err();
        assert!(!bad.to_string().contains("sk-secret"), "{bad}");
        assert_eq!(services.invoke(KEY_SERVICE, &json!({"op": "remove", "account": "openai"})).unwrap(), json!({"removed": true}));
        assert_eq!(services.invoke(KEY_SERVICE, &json!({"op": "status", "account": "openai"})).unwrap(), json!({"stored": false, "masked": null}));
        assert!(services.invoke(KEY_SERVICE, &json!({"op": "drop_all"})).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn keychain_errors_never_echo_stored_bytes() {
        use keyring::Error as E;
        let secret = b"sk-live-secret-bytes".to_vec();
        let platform = || -> Box<dyn std::error::Error + Send + Sync> { Box::new(std::io::Error::other("platform said no")) };
        for e in [
            E::BadEncoding(secret.clone()),
            E::BadDataFormat(secret.clone(), platform()),
            E::TooLong("password".into(), 2560),
            E::Invalid("p".into(), "r".into()),
            E::PlatformFailure(platform()),
        ] {
            let msg = keychain::map(e).to_string();
            assert!(!msg.contains("sk-live"), "{msg}");
        }
        assert!(matches!(keychain::map(E::NoStorageAccess(platform())), SecretError::Denied(_)));
        assert!(keychain::map(E::TooLong("password".into(), 2560)).to_string().contains("2560"));
    }

    /// Opt-in: touches the real keychain (`NODECRAFT_LIVE_KEYCHAIN=1`); never in CI.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn live_keychain_round_trip() {
        if std::env::var("NODECRAFT_LIVE_KEYCHAIN").ok().as_deref() != Some("1") {
            return;
        }
        let store = KeyringStore;
        let acc = Account::new("nodecraft-live-test").unwrap();
        store.set(&acc, &Secret::new("test-value-123456").unwrap()).unwrap();
        assert_eq!(store.get(&acc).unwrap().map(|s| s.expose().to_owned()).as_deref(), Some("test-value-123456"));
        assert!(store.delete(&acc).unwrap());
        assert!(store.get(&acc).unwrap().is_none());
    }
}
