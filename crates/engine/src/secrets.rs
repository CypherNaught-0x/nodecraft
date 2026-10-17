//! Provider API keys in the operating system's keychain.
//!
//! Keys live only in the keychain (macOS Keychain, Windows Credential Manager, the Secret Service
//! on Linux), under the service `io.github.cyphernaught0x.nodecraft` with the provider's account
//! name (`openai`, `anthropic`, `comfyui:<server>`). They never go into project files, settings,
//! logs or error messages: [`Secret`] doesn't print its value, and errors never contain it.
//!
//! The browser build has no keychain; it will keep keys per origin with a notice (M10). Until then
//! it gets a [`MemoryStore`] (keys last for the session).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};

/// The keychain service name: the app id.
pub const SERVICE: &str = "io.github.cyphernaught0x.nodecraft";
/// Longest key accepted (real API keys are well under 1 KiB; JWT-style keys can be longer).
pub const MAX_SECRET_BYTES: usize = 8 * 1024;

/// A secret value. `Debug` and `Display` never show it; [`Secret::masked`] shows the last four
/// characters for the settings UI.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// A secret from user input, trimmed. Refuses empty, oversized or multi-line values (a pasted
    /// key with a newline would otherwise break HTTP headers).
    pub fn new(value: &str) -> Result<Self, SecretError> {
        let v = value.trim();
        if v.is_empty() {
            return Err(SecretError::Invalid("the key is empty".into()));
        }
        if v.len() > MAX_SECRET_BYTES {
            return Err(SecretError::Invalid(format!("the key is longer than {MAX_SECRET_BYTES} bytes")));
        }
        if v.chars().any(char::is_control) {
            return Err(SecretError::Invalid("the key contains line breaks or control characters".into()));
        }
        Ok(Self(v.to_owned()))
    }

    /// The value, for building a request header. Don't log or display it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `…abcd` (the last four characters), or `…` for short keys.
    pub fn masked(&self) -> String {
        let n = self.0.chars().count();
        if n <= 8 {
            return "…".into();
        }
        let tail: String = self.0.chars().skip(n - 4).collect();
        format!("…{tail}")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.masked())
    }
}

/// A keychain account name: the provider id, optionally with a qualifier (`comfyui:gpu-box`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Account(String);

impl Account {
    /// Lowercase ASCII letters, digits and `-_.:`, 1–128 characters.
    pub fn new(name: &str) -> Result<Self, SecretError> {
        let ok = !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.:".contains(c));
        if ok {
            Ok(Self(name.to_owned()))
        } else {
            Err(SecretError::Invalid(format!("{:?} is not a valid key name", name.chars().take(64).collect::<String>())))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a keychain operation failed. Messages are safe to show and never contain the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretError {
    /// The value or account name can't be stored.
    Invalid(String),
    /// No keychain on this system (e.g. Linux without a Secret Service such as GNOME Keyring or
    /// KWallet), or the platform isn't supported.
    Unavailable(String),
    /// The keychain is locked or the user denied access.
    Denied(String),
    /// The keychain failed.
    Failed(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(m) => write!(f, "{m}"),
            Self::Unavailable(m) => write!(f, "no keychain available: {m}"),
            Self::Denied(m) => write!(f, "keychain access denied: {m}"),
            Self::Failed(m) => write!(f, "keychain error: {m}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Where keys are kept.
pub trait SecretStore: Send + Sync {
    /// The key for `account`, or `None` when none is stored.
    fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError>;
    fn set(&self, account: &Account, secret: &Secret) -> Result<(), SecretError>;
    /// Removes the key; `Ok(false)` when there was none.
    fn delete(&self, account: &Account) -> Result<bool, SecretError>;
}

/// Keys in memory: tests, and the browser build until per-origin storage lands.
#[derive(Default)]
pub struct MemoryStore(Mutex<BTreeMap<Account, Secret>>);

impl SecretStore for MemoryStore {
    fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).get(account).cloned())
    }
    fn set(&self, account: &Account, secret: &Secret) -> Result<(), SecretError> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(account.clone(), secret.clone());
        Ok(())
    }
    fn delete(&self, account: &Account) -> Result<bool, SecretError> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(account).is_some())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use keychain::KeyringStore;

#[cfg(not(target_arch = "wasm32"))]
mod keychain {
    use super::*;

    /// The operating system keychain, through the `keyring` crate.
    #[derive(Debug, Default)]
    pub struct KeyringStore;

    fn map(e: keyring::Error) -> SecretError {
        use keyring::Error as E;
        match e {
            E::NoDefaultStore | E::NotSupportedByStore(_) => SecretError::Unavailable("install or unlock a keyring (GNOME Keyring, KWallet) on Linux".into()),
            E::NoStorageAccess(_) => SecretError::Denied("unlock the keychain and allow NodeCraft access".into()),
            // These carry the stored bytes: never echo them.
            E::BadEncoding(_) | E::BadDataFormat(..) => SecretError::Failed("the stored key can't be read; set it again".into()),
            E::Ambiguous(_) => SecretError::Failed("more than one key is stored under this name; remove and set it again".into()),
            E::TooLong(..) | E::Invalid(..) => SecretError::Invalid("the keychain refused the key".into()),
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
                Ok(v) => Secret::new(&v).map(Some).or(Ok(None)),
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

/// Checks a chat endpoint's key with a request that costs nothing (listing models).
pub async fn test_llm_key(client: &nodecraft_providers::llm::LlmClient) -> Result<usize, nodecraft_providers::ProviderError> {
    client.list_models().await.map(|models| models.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print() {
        let s = Secret::new("  sk-proj-abcdefghijklmnop1234 \n").unwrap();
        assert_eq!(s.expose(), "sk-proj-abcdefghijklmnop1234", "trimmed");
        assert_eq!(format!("{s:?}"), "Secret(…)");
        assert_eq!(s.to_string(), "…1234");
        assert_eq!(Secret::new("short").unwrap().masked(), "…");
        assert!(Secret::new("   ").is_err());
        assert!(Secret::new("sk-1\nsk-2").is_err(), "a pasted key with a line break would break headers");
        assert!(Secret::new(&"x".repeat(MAX_SECRET_BYTES + 1)).is_err());
        let err = Secret::new("sk\u{7}x").unwrap_err();
        assert!(!err.to_string().contains("sk"), "errors don't echo the value");
    }

    #[test]
    fn account_names() {
        for ok in ["openai", "anthropic", "comfyui:gpu-box.local", "fal_ai-2"] {
            assert!(Account::new(ok).is_ok(), "{ok}");
        }
        for bad in ["", "OpenAI", "a b", "x/y", &"a".repeat(129)] {
            assert!(Account::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::default();
        let acc = Account::new("openai").unwrap();
        assert_eq!(store.get(&acc).unwrap(), None);
        store.set(&acc, &Secret::new("sk-123456789").unwrap()).unwrap();
        assert_eq!(store.get(&acc).unwrap().map(|s| s.expose().to_owned()).as_deref(), Some("sk-123456789"));
        assert!(store.delete(&acc).unwrap());
        assert!(!store.delete(&acc).unwrap());
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
        assert_eq!(store.get(&acc).unwrap(), None);
    }
}
