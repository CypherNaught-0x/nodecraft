//! Lexical representation admission before any owned JSON unescaping; map-only record boundaries.
use super::*;
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;
use std::{borrow::Cow, collections::BTreeMap, fmt};
pub(super) struct Object<'a>(BTreeMap<Cow<'a, str>, &'a RawValue>);
impl<'a> Object<'a> {
    pub fn values(&self) -> impl Iterator<Item = &'a RawValue> {
        self.0.values().copied()
    }
    pub fn get(&self, k: &str) -> Option<&'a RawValue> {
        self.0.get(k).copied()
    }
    pub fn text(&self, k: &str) -> Result<Cow<'a, str>, ProviderError> {
        text(self.get(k).ok_or_else(|| malformed("fal required string missing"))?)
    }
    pub fn optional_text(&self, k: &str) -> Result<Option<Cow<'a, str>>, ProviderError> {
        match self.get(k) {
            None => Ok(None),
            Some(v) if v.get() == "null" => Ok(None),
            Some(v) => text(v).map(Some),
        }
    }
    pub fn optional_u64(&self, k: &str) -> Result<Option<u64>, ProviderError> {
        match self.get(k) {
            None => Ok(None),
            Some(v) if v.get() == "null" => Ok(None),
            Some(v) => serde_json::from_str(v.get()).map(Some).map_err(|_| malformed("fal invalid numeric metadata")),
        }
    }
}
impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut m = BTreeMap::new();
                while let Some(k) = a.next_key::<Cow<'de, str>>()? {
                    if m.len() >= 4096 || m.contains_key(k.as_ref()) {
                        return Err(serde::de::Error::custom("duplicate or excess fields"));
                    }
                    m.insert(k, a.next_value::<&'de RawValue>()?);
                }
                Ok(Object(m))
            }
        }
        d.deserialize_map(V)
    }
}
pub(super) fn text<'a>(raw: &'a RawValue) -> Result<Cow<'a, str>, ProviderError> {
    // Borrow unescaped values, bounded owned decoding for escaped metadata.
    if let Ok(v) = serde_json::from_str::<&str>(raw.get()) {
        return Ok(Cow::Borrowed(v));
    }
    serde_json::from_str::<String>(raw.get()).map(Cow::Owned).map_err(|_| malformed("fal invalid string"))
}
pub(super) fn object(raw: &str) -> Result<Object<'_>, ProviderError> {
    serde_json::from_str(raw).map_err(|_| malformed("fal record must be an object with unique fields"))
}
pub(super) fn parse(bytes: &[u8], l: Limits) -> Result<Object<'_>, ProviderError> {
    admit(bytes, l)?;
    object(std::str::from_utf8(bytes).map_err(|_| malformed("fal JSON must be UTF8"))?)
}
pub(super) fn admit(bytes: &[u8], l: Limits) -> Result<(), ProviderError> {
    if bytes.len() > l.response_bytes {
        return Err(malformed("fal JSON body exceeds limit"));
    }
    struct Scan<'a> {
        b: &'a [u8],
        at: usize,
        strings: usize,
        items: usize,
        l: Limits,
    }
    impl Scan<'_> {
        fn b(&self) -> Option<u8> {
            self.b.get(self.at).copied()
        }
        fn ws(&mut self) {
            while self.b().is_some_and(|b| b.is_ascii_whitespace()) {
                self.at += 1;
            }
        }
        fn string(&mut self) -> Result<(), ProviderError> {
            self.at += 1;
            let start = self.at;
            loop {
                match self.b() {
                    Some(b'"') => {
                        let n = self.at - start;
                        self.strings = self
                            .strings
                            .checked_add(n)
                            .filter(|n| *n <= self.l.metadata_bytes)
                            .ok_or_else(|| malformed("fal JSON string budget exceeded before unescaping"))?;
                        self.at += 1;
                        return Ok(());
                    }
                    Some(b'\\') => {
                        self.at += 1;
                        match self.b() {
                            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.at += 1,
                            Some(b'u') => {
                                let hex = |at: usize| -> Result<u16, ProviderError> {
                                    let part = self
                                        .b
                                        .get(at..at.checked_add(4).ok_or_else(|| malformed("fal escape overflow"))?)
                                        .ok_or_else(|| malformed("fal short Unicode escape"))?;
                                    part.iter().try_fold(0u16, |n, b| {
                                        let d = match b {
                                            b'0'..=b'9' => b - b'0',
                                            b'a'..=b'f' => b - b'a' + 10,
                                            b'A'..=b'F' => b - b'A' + 10,
                                            _ => return Err(malformed("fal invalid Unicode hex")),
                                        };
                                        Ok((n << 4) | u16::from(d))
                                    })
                                };
                                let first = hex(self.at + 1)?;
                                if (0xd800..=0xdbff).contains(&first) {
                                    if self.b.get(self.at + 5..self.at + 7) != Some(b"\\u") || !(0xdc00..=0xdfff).contains(&hex(self.at + 7)?) {
                                        return Err(malformed("fal unpaired Unicode surrogate"));
                                    }
                                    self.at += 11;
                                } else if (0xdc00..=0xdfff).contains(&first) {
                                    return Err(malformed("fal unpaired Unicode surrogate"));
                                } else {
                                    self.at += 5;
                                }
                            }
                            _ => return Err(malformed("fal invalid JSON escape")),
                        }
                    }
                    Some(b) if b >= 32 => self.at += 1,
                    _ => return Err(malformed("fal malformed JSON string")),
                }
                if self.at - start > self.l.metadata_bytes {
                    return Err(malformed("fal JSON string exceeds metadata limit"));
                }
            }
        }
        fn value(&mut self, depth: usize) -> Result<(), ProviderError> {
            self.ws();
            self.items += 1;
            if depth > 32 || self.items > 4096 {
                return Err(malformed("fal JSON depth/item limit"));
            }
            match self.b() {
                Some(b'"') => self.string()?,
                Some(b'{' | b'[') => {
                    let object = self.b() == Some(b'{');
                    let end = if object { b'}' } else { b']' };
                    self.at += 1;
                    self.ws();
                    if self.b() == Some(end) {
                        self.at += 1;
                        return Ok(());
                    }
                    loop {
                        self.ws();
                        if object {
                            if self.b() != Some(b'"') {
                                return Err(malformed("fal invalid object key"));
                            }
                            self.string()?;
                            self.ws();
                            if self.b() != Some(b':') {
                                return Err(malformed("fal missing colon"));
                            }
                            self.at += 1;
                        }
                        self.value(depth + 1)?;
                        self.ws();
                        match self.b() {
                            Some(b) if b == end => {
                                self.at += 1;
                                break;
                            }
                            Some(b',') => self.at += 1,
                            _ => return Err(malformed("fal invalid container")),
                        }
                    }
                }
                Some(_) => {
                    let start = self.at;
                    while self.b().is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b',' | b']' | b'}')) {
                        self.at += 1;
                        if self.at - start > 64 {
                            return Err(malformed("fal scalar too long"));
                        }
                    }
                    if self.at == start {
                        return Err(malformed("fal missing value"));
                    }
                }
                None => return Err(malformed("fal missing JSON value")),
            }
            Ok(())
        }
    }
    let mut s = Scan { b: bytes, at: 0, strings: 0, items: 0, l };
    s.value(1)?;
    s.ws();
    if s.at != bytes.len() {
        return Err(malformed("fal trailing JSON"));
    }
    // RawValue performs full JSON grammar/escape validation without a Value tree.
    serde_json::from_slice::<&RawValue>(bytes).map(|_| ()).map_err(|_| malformed("fal invalid JSON grammar"))
}
pub(super) fn files(raw: &RawValue) -> Result<Vec<&RawValue>, ProviderError> {
    struct List<'a>(Vec<&'a RawValue>);
    impl<'de> Deserialize<'de> for List<'de> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = List<'de>;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("one to ten files")
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                    let mut v = Vec::new();
                    v.try_reserve_exact(10).map_err(serde::de::Error::custom)?;
                    while let Some(x) = a.next_element::<&RawValue>()? {
                        if v.len() == 10 {
                            return Err(serde::de::Error::custom("excess files"));
                        }
                        v.push(x);
                    }
                    if v.is_empty() {
                        return Err(serde::de::Error::custom("empty files"));
                    }
                    Ok(List(v))
                }
            }
            d.deserialize_seq(V)
        }
    }
    serde_json::from_str::<List<'_>>(raw.get()).map(|x| x.0).map_err(|_| malformed("fal output array must contain one to ten records"))
}
