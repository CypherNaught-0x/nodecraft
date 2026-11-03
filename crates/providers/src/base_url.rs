//! Base URLs a person sets for a provider whose API another server can serve (#960): a LiteLLM
//! proxy, vLLM, LM Studio or a gateway in front of OpenAI or Anthropic.
//!
//! [`BaseUrl::parse`] is the one check every such address passes before a key is attached to a
//! request for it:
//! - HTTPS, or plain HTTP only when the host is local ([`BaseUrl::is_local`]): an IP literal that
//!   isn't a public unicast address ([`crate::http::public_address`]), or exactly `localhost`.
//!   Any other name counts as remote, even if it resolves to a private address today: its records
//!   can change, and the browser can't resolve names at all;
//! - a host, and no user name, password, query or fragment (keys belong in the keychain);
//! - at most [`MAX_BASE_URL_BYTES`] bytes and no control characters.
//!
//! Errors never quote the address: it may hold a pasted key, and tools that echo input fail the
//! panic hunt.

use std::net::IpAddr;

use crate::{ErrorKind, ProviderError};

/// The longest base URL accepted, in bytes.
pub const MAX_BASE_URL_BYTES: usize = 2048;

/// What every refusal says; it never contains the address itself.
pub const INVALID_BASE_URL: &str = "use an HTTPS base URL, or HTTP for a local or private IP address or localhost, \
     with no user name, password, query or fragment; keep keys in API keys";

/// A checked base URL, including any version path (such as `https://proxy.example/v1`), without
/// a trailing slash.
#[derive(Clone, PartialEq, Eq)]
pub struct BaseUrl {
    url: String,
    local: bool,
}

impl std::fmt::Debug for BaseUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BaseUrl").field("url", &crate::http::redact_url(&self.url)).field("local", &self.local).finish()
    }
}

fn invalid() -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, INVALID_BASE_URL)
}

/// Whether `host` is local in the sense above: never by DNS.
fn local_host(host: url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(name) => name == "localhost",
        url::Host::Ipv4(ip) => !crate::http::public_address(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => !crate::http::public_address(IpAddr::V6(ip)),
    }
}

impl BaseUrl {
    /// Checks and normalises `value` (surrounding space and a trailing slash are dropped).
    pub fn parse(value: &str) -> Result<Self, ProviderError> {
        let value = value.trim();
        if value.is_empty() || value.len() > MAX_BASE_URL_BYTES || value.chars().any(char::is_control) {
            return Err(invalid());
        }
        let url = url::Url::parse(value).map_err(|_| invalid())?;
        let Some(host) = url.host() else { return Err(invalid()) };
        let local = local_host(host);
        let scheme_ok = match url.scheme() {
            "https" => true,
            "http" => local,
            _ => false,
        };
        if !scheme_ok || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(invalid());
        }
        let url = url.as_str().trim_end_matches('/').to_owned();
        if url.len() > MAX_BASE_URL_BYTES {
            return Err(invalid());
        }
        Ok(Self { url, local })
    }

    pub fn as_str(&self) -> &str {
        &self.url
    }

    /// The host is a non-public IP literal or `localhost`.
    pub fn is_local(&self) -> bool {
        self.local
    }

    /// `path` under this base, such as `chat/completions`.
    pub fn join(&self, path: &str) -> String {
        format!("{}/{}", self.url, path.trim_start_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_anywhere_and_http_only_for_local_hosts() {
        for (value, normal, local) in [
            ("https://proxy.example/v1/", "https://proxy.example/v1", false),
            ("  https://proxy.example:8443/v1  ", "https://proxy.example:8443/v1", false),
            ("http://127.0.0.1:4000/v1", "http://127.0.0.1:4000/v1", true),
            ("http://localhost:1234/v1", "http://localhost:1234/v1", true),
            ("http://LOCALHOST:1234", "http://localhost:1234", true),
            ("http://192.168.1.20:8000/v1", "http://192.168.1.20:8000/v1", true),
            ("http://10.0.0.5", "http://10.0.0.5", true),
            ("http://[::1]:4000/v1", "http://[::1]:4000/v1", true),
            ("http://[fd00::1]/v1", "http://[fd00::1]/v1", true),
            ("https://10.0.0.5/v1", "https://10.0.0.5/v1", true),
        ] {
            let url = BaseUrl::parse(value).unwrap_or_else(|e| panic!("{value}: {e:?}"));
            assert_eq!(url.as_str(), normal);
            assert_eq!(url.is_local(), local, "{value}");
        }
        assert!(BaseUrl::parse("http://[::ffff:192.168.0.1]/v1").unwrap().is_local(), "a mapped private address is local");
        assert!(BaseUrl::parse("http://[::ffff:8.8.8.8]/v1").is_err(), "a mapped public address is remote");
        assert_eq!(BaseUrl::parse("https://proxy.example/v1/").unwrap().join("chat/completions"), "https://proxy.example/v1/chat/completions");
    }

    #[test]
    fn other_schemes_remote_http_credentials_and_malformed_urls_are_refused_without_echo() {
        let secret = "sk-live-0123456789";
        for value in [
            "",
            "   ",
            "ftp://proxy.example/v1",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ws://127.0.0.1:4000",
            "http://proxy.example/v1",
            "http://8.8.8.8/v1",
            "http://my-box.localhost/v1",
            "http://gpu-box/v1",
            "proxy.example/v1",
            "https://",
            "https://user@proxy.example/v1",
            &format!("https://user:{secret}@proxy.example/v1"),
            &format!("https://proxy.example/v1?key={secret}"),
            &format!("https://proxy.example/v1#{secret}"),
            &format!("https://proxy.example/{secret}\u{7}/v1"),
            &format!("https://proxy.example/{}", "a".repeat(MAX_BASE_URL_BYTES)),
            "https://proxy .example/v1",
        ] {
            let error = BaseUrl::parse(value).unwrap_err();
            assert_eq!(error.kind, ErrorKind::UnsupportedInput, "{value}");
            assert_eq!(error.message, INVALID_BASE_URL);
            assert!(!error.message.contains(secret) && !error.message.contains("proxy.example"));
        }
    }

    #[test]
    fn debug_hides_what_a_url_might_carry() {
        let url = BaseUrl::parse("https://proxy.example/v1").unwrap();
        assert!(format!("{url:?}").contains("BaseUrl"));
    }
}
