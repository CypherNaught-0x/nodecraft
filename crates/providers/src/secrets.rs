//! API keys as providers see them: a [`Secret`] that never prints, the keychain [`Account`] it
//! lives under, and the [`SecretStore`] interface. The OS keychain implementation is in the engine
//! (`engine::secrets::KeyringStore`); providers only ever receive a [`Secret`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};

/// Longest key accepted. Real API keys are well under this, and Windows Credential Manager stores
/// at most 2560 bytes of UTF-16 (about 1280 characters).
pub const MAX_SECRET_BYTES: usize = 1024;

/// A secret value. `Debug` shows `Secret(…)`; `Display` and [`Secret::masked`] show at most the
/// last four characters, and only for keys long enough that this reveals little.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// A secret from user input, trimmed. It must be visible ASCII, since keys travel in HTTP
    /// headers: a pasted smart quote, zero-width space or line break is refused up front.
    pub fn new(value: &str) -> Result<Self, SecretError> {
        let v = value.trim();
        if v.is_empty() {
            return Err(SecretError::Invalid("the key is empty".into()));
        }
        if v.len() > MAX_SECRET_BYTES {
            return Err(SecretError::Invalid(format!("the key is longer than {MAX_SECRET_BYTES} characters")));
        }
        if !v.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(SecretError::Invalid("the key may only contain visible ASCII characters (no spaces, line breaks or typographic quotes)".into()));
        }
        Ok(Self(v.to_owned()))
    }

    /// The value, for building a request header. Don't log or display it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `…abcd` for keys of 16 characters or more, `…` otherwise.
    pub fn masked(&self) -> String {
        let n = self.0.len();
        match self.0.get(n.saturating_sub(4)..) {
            Some(tail) if n >= 16 => format!("…{tail}"),
            _ => "…".into(),
        }
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

/// A keychain account name: the provider id, optionally with a qualifier (`comfyui:gpu-box:8188`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Account(String);

impl Account {
    /// Lowercase ASCII letters, digits and `-_.:[]`, 1–128 characters.
    pub fn new(name: &str) -> Result<Self, SecretError> {
        let ok = !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.:[]".contains(c));
        if ok {
            Ok(Self(name.to_owned()))
        } else {
            Err(SecretError::Invalid(format!("{:?} is not a valid key name", name.chars().take(64).collect::<String>())))
        }
    }

    /// The account for a provider on a particular server: `provider:host[:port]`, readable in
    /// the OS keychain app. The host is lowercased (internationalised names become punycode) and
    /// scheme, path and credentials are dropped.
    pub fn server(provider: &str, url: &str) -> Result<Self, SecretError> {
        let parsed = url::Url::parse(url.trim()).map_err(|_| SecretError::Invalid("not a server URL".into()))?;
        let host = parsed.host_str().filter(|h| !h.is_empty()).ok_or_else(|| SecretError::Invalid("the server URL has no host".into()))?;
        let name = match parsed.port() {
            Some(port) => format!("{provider}:{host}:{port}"),
            None => format!("{provider}:{host}"),
        };
        Self::new(&name.to_ascii_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a key storage operation failed (the OS keychain, or the browser's storage). Messages are
/// safe to show and never contain the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretError {
    /// The value or account name can't be stored.
    Invalid(String),
    /// No keychain on this system, or it couldn't be opened.
    Unavailable(String),
    /// The keychain is locked or the user denied access.
    Denied(String),
    /// The keychain failed, or the stored key can't be used.
    Failed(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(m) => write!(f, "{m}"),
            Self::Unavailable(m) => write!(f, "key storage unavailable: {m}"),
            Self::Denied(m) => write!(f, "key storage access denied: {m}"),
            Self::Failed(m) => write!(f, "key storage error: {m}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Where keys are kept.
///
/// **Calls may block** until the user answers an unlock or access prompt (macOS, Linux): never
/// call them on the UI thread, and don't read every account at startup (each read can prompt).
pub trait SecretStore: Send + Sync {
    /// The key for `account`, or `None` when none is stored.
    fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError>;
    fn set(&self, account: &Account, secret: &Secret) -> Result<(), SecretError>;
    /// Removes the key; `Ok(false)` when there was none.
    fn delete(&self, account: &Account) -> Result<bool, SecretError>;
}

/// Keys in memory: tests, and hosts with nowhere to keep them (keys last for the session).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print_and_reject_unusable_pastes() {
        let s = Secret::new("  sk-proj-abcdefghijklmnop1234 \n").unwrap();
        assert_eq!(s.expose(), "sk-proj-abcdefghijklmnop1234", "trimmed");
        assert_eq!(format!("{s:?}"), "Secret(…)");
        assert_eq!(s.to_string(), "…1234");
        assert_eq!(Secret::new("sk-abcdefghi12").unwrap().masked(), "…", "short keys reveal nothing");
        for bad in ["   ", "sk-1\nsk-2", "sk-\u{201c}quoted\u{201d}", "sk-zero\u{200b}width", "has space"] {
            assert!(Secret::new(bad).is_err(), "{bad:?}");
        }
        assert!(Secret::new(&"x".repeat(MAX_SECRET_BYTES)).is_ok());
        assert!(Secret::new(&"x".repeat(MAX_SECRET_BYTES + 1)).is_err());
        let err = Secret::new("sk\u{7}x").unwrap_err();
        assert!(!err.to_string().contains("sk"), "errors don't echo the value");
    }

    #[test]
    fn account_names() {
        for ok in ["openai", "anthropic", "comfyui:gpu-box.local", "fal_ai-2", "comfyui:[::1]:8188"] {
            assert!(Account::new(ok).is_ok(), "{ok}");
        }
        for bad in ["", "OpenAI", "a b", "x/y", &"a".repeat(129)] {
            assert!(Account::new(bad).is_err(), "{bad}");
        }
        assert!(Account::new(&"a".repeat(128)).is_ok());
        assert_eq!(Account::server("comfyui", "https://user:pw@GPU-Box.local:8443/comfy?x=1").unwrap().as_str(), "comfyui:gpu-box.local:8443");
        assert_eq!(Account::server("comfyui", "http://[::1]:8188").unwrap().as_str(), "comfyui:[::1]:8188");
        assert_eq!(Account::server("comfyui", "http://bücher.example").unwrap().as_str(), "comfyui:xn--bcher-kva.example");
        assert!(Account::server("comfyui", "not a url").is_err());
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::default();
        let acc = Account::new("openai").unwrap();
        assert!(store.get(&acc).unwrap().is_none());
        store.set(&acc, &Secret::new("sk-123456789").unwrap()).unwrap();
        assert_eq!(store.get(&acc).unwrap().map(|s| s.expose().to_owned()).as_deref(), Some("sk-123456789"));
        assert!(store.delete(&acc).unwrap());
        assert!(!store.delete(&acc).unwrap());
    }
}
