//! Names: package IDs, entry IDs, the registered IDs the host publishes, handles and key IDs.
//!
//! Every grammar is lowercase ASCII, so two names that differ only in case can't both exist,
//! and nothing is ever rewritten to fit: a hyphen in a package label is an error, not an
//! underscore. Registered IDs fit the engine's command-ID rule (dotted lowercase, digits and
//! underscores, at most 128 bytes) and the graph's 256-byte node-kind limit.
use crate::error::{Code, Result, WireError, err};
use std::fmt;

/// Prefix of every registered ID; host-owned IDs never start with it.
pub const PLUGIN_PREFIX: &str = "plugin.";
/// Longest package ID.
pub const MAX_PACKAGE_ID: usize = 80;
/// Longest registered ID (the engine's command-ID limit).
pub const MAX_REGISTERED_ID: usize = 128;

fn label(text: &str, max: usize) -> bool {
    let mut bytes = text.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z')) && text.len() <= max && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A package's reverse-DNS-style name, such as `io.example.textpack`: 3–6 labels matching
/// `[a-z][a-z0-9_]{0,23}`, at most 80 bytes. A logical namespace, not proof of owning a domain.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId(String);

impl PackageId {
    pub fn parse(text: &str) -> Result<Self> {
        let labels = text.split('.').count();
        if text.len() <= MAX_PACKAGE_ID && (3..=6).contains(&labels) && text.split('.').all(|l| label(l, 24)) {
            Ok(Self(text.to_owned()))
        } else {
            err(Code::BadId, "a package ID is 3–6 dot-separated labels of [a-z][a-z0-9_]{0,23}, at most 80 bytes")
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An entry's name inside its package, such as `normalize` or `text.normalize`: 1–2 segments
/// matching `[a-z][a-z0-9_]{0,14}`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(String);

impl LocalId {
    pub fn parse(text: &str) -> Result<Self> {
        let segments = text.split('.').count();
        if (1..=2).contains(&segments) && text.split('.').all(|s| label(s, 15)) {
            Ok(Self(text.to_owned()))
        } else {
            err(Code::BadId, "an entry ID is 1–2 dot-separated segments of [a-z][a-z0-9_]{0,14}")
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The ID the host registers an entry under: `plugin.<package>.<entry>`, at most 128 bytes
/// (119 under the package and entry limits).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegisteredId(String);

impl RegisteredId {
    pub fn new(package: &PackageId, local: &LocalId) -> Self {
        Self(format!("{PLUGIN_PREFIX}{}.{}", package.0, local.0))
    }

    /// Parses a registered ID; which split is package and which is entry is settled by the
    /// package that declares it, so this only checks the shape.
    pub fn parse(text: &str) -> Result<Self> {
        let rest = text.strip_prefix(PLUGIN_PREFIX).unwrap_or("");
        let parts = rest.split('.').count();
        if text.len() <= MAX_REGISTERED_ID && (4..=8).contains(&parts) && rest.split('.').all(|l| label(l, 24)) {
            Ok(Self(text.to_owned()))
        } else {
            err(Code::BadId, "a registered ID is plugin.<package>.<entry>, at most 128 bytes")
        }
    }

    /// Whether this ID belongs to `package`.
    pub fn is_in(&self, package: &PackageId) -> bool {
        self.0.strip_prefix(PLUGIN_PREFIX).and_then(|rest| rest.strip_prefix(package.as_str())).is_some_and(|rest| rest.starts_with('.'))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A host-owned ID a data package may bind a descriptor to (an action or command ID): dotted
/// lowercase, digits and underscores, at most 128 bytes, never under `plugin.`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(String);

impl HostId {
    pub fn parse(text: &str) -> Result<Self> {
        let shape = text.len() <= MAX_REGISTERED_ID
            && text.contains('.')
            && text.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        if !shape {
            return err(Code::BadId, "a host ID is dotted lowercase letters, digits and underscores, at most 128 bytes");
        }
        if text.starts_with(PLUGIN_PREFIX) {
            return err(Code::BadId, "plugin. IDs belong to packages, not the host");
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A short token: request and job IDs, asset handles, key IDs. `[a-z0-9][a-z0-9_-]{0,63}`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Token(String);

impl Token {
    pub fn parse(text: &str) -> Result<Self> {
        let mut bytes = text.bytes();
        let ok = matches!(bytes.next(), Some(b'a'..=b'z' | b'0'..=b'9'))
            && text.len() <= 64
            && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if ok { Ok(Self(text.to_owned())) } else { err(Code::BadId, "a token is [a-z0-9][a-z0-9_-]{0,63}") }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! display {
    ($($t:ty),*) => {$(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    )*};
}
display!(PackageId, LocalId, RegisteredId, HostId, Token);

pub(crate) fn located<T>(result: Result<T>, at: &str) -> Result<T> {
    result.map_err(|e: WireError| e.at(at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_ids_follow_the_grammar_and_are_never_rewritten() {
        for good in ["io.example.textpack", "a.b.c", "io.example.text_pack2.x.y.z"] {
            assert!(PackageId::parse(good).is_ok(), "{good}");
        }
        for bad in ["io.example", "io.example-x.pack", "Io.example.pack", "io..pack", "io.example.pack.", "1o.example.pack", "a.b.c.d.e.f.g", ""] {
            assert_eq!(PackageId::parse(bad).unwrap_err().code, Code::BadId, "{bad}");
        }
        let long = format!("a.b.{}", "c".repeat(25));
        assert!(PackageId::parse(&long).is_err(), "a label over 24 bytes");
        let eighty_one = format!("{}.{}.{}.{}", "a".repeat(24), "b".repeat(24), "c".repeat(24), "d".repeat(6));
        assert_eq!(eighty_one.len(), 81);
        assert!(PackageId::parse(&eighty_one).is_err());
    }

    #[test]
    fn registered_ids_fit_the_engine_and_name_their_package() {
        let package = PackageId::parse(&format!("{}.{}.{}.{}", "a".repeat(24), "b".repeat(24), "c".repeat(24), "d".repeat(5))).unwrap();
        let local = LocalId::parse(&format!("{}.{}", "e".repeat(15), "f".repeat(15))).unwrap();
        let id = RegisteredId::new(&package, &local);
        assert_eq!(id.as_str().len(), 119, "the longest possible");
        assert!(RegisteredId::parse(id.as_str()).is_ok());
        assert!(id.is_in(&package));
        assert!(!id.is_in(&PackageId::parse("a.b.c").unwrap()));
        // A package that is a prefix of another can name the same ID: registration refuses
        // the collision rather than letting install order decide.
        let short = PackageId::parse("a.b.c").unwrap();
        let long = PackageId::parse("a.b.c.d").unwrap();
        let same = RegisteredId::new(&short, &LocalId::parse("d.x").unwrap());
        assert_eq!(same, RegisteredId::new(&long, &LocalId::parse("x").unwrap()));
        assert!(same.is_in(&short) && same.is_in(&long));
        for bad in ["io.example.pack.x", "plugin.io.example", "plugin.io.example.pack.X"] {
            assert!(RegisteredId::parse(bad).is_err(), "{bad}");
        }
        assert!(HostId::parse("plugin.io.example.pack.x").is_err());
        assert!(HostId::parse("text.normalize").is_ok());
    }
}
