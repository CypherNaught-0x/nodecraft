//! The AWS Signature Version 4 algorithm (`AWS4-HMAC-SHA256`), as AWS describes it in
//! <https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html>
//! (checked 2026-11-08). Pure functions over text: [`super::SigV4Http`] feeds them a request.
//!
//! The canonical URI encodes each segment of the path *as it is sent* again, which AWS's signers
//! do for every service but S3: a path segment sent as `v1%3A0` is signed as `v1%253A0`. The
//! path is not normalised (no dot or empty segments are removed).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use crate::{ErrorKind, ProviderError};

/// The algorithm name in the string to sign and the `Authorization` header.
pub(crate) const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hmac(key: &[u8], data: &[u8]) -> Result<Vec<u8>, ProviderError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| ProviderError::new(ErrorKind::Auth, "the AWS secret access key can't sign a request"))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// AWS's `UriEncode`: every byte but the unreserved `A-Z a-z 0-9 - . _ ~` as `%XX` (uppercase
/// hex); `/` kept when `keep_slash`.
pub(crate) fn uri_encode(text: &[u8], keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for &b in text {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || (keep_slash && b == b'/') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// The canonical URI of a path as sent: each `/`-separated segment encoded (again).
pub(crate) fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".into();
    }
    path.split('/').map(|segment| uri_encode(segment.as_bytes(), false)).collect::<Vec<_>>().join("/")
}

/// `%XX` sequences decoded; anything else, an invalid sequence included, kept as it is.
fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        let pair = bytes.get(i.saturating_add(1)..i.saturating_add(3)).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b, pair) {
            (b'%', Some(decoded)) => {
                out.push(decoded);
                i = i.saturating_add(3);
            }
            _ => {
                out.push(b);
                i = i.saturating_add(1);
            }
        }
    }
    out
}

/// The canonical query string: each name and value decoded, encoded the AWS way, and sorted.
pub(crate) fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (name, value) = p.split_once('=').unwrap_or((p, ""));
            (uri_encode(&percent_decode(name), false), uri_encode(&percent_decode(value), false))
        })
        .collect();
    pairs.sort();
    pairs.into_iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("&")
}

/// A header value trimmed, with each run of spaces inside it one space.
fn header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The canonical headers block (each `name:value\n`) and the signed headers list, from the
/// headers to sign: names lowercased and sorted, a repeated name's values joined by commas.
pub(crate) fn canonical_headers(headers: &[(String, String)]) -> (String, String) {
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers {
        by_name.entry(name.trim().to_ascii_lowercase()).or_default().push(header_value(value));
    }
    let block: String = by_name.iter().map(|(n, values)| format!("{n}:{}\n", values.join(","))).collect();
    let signed = by_name.keys().cloned().collect::<Vec<_>>().join(";");
    (block, signed)
}

/// One request to sign, already reduced to what the signature covers.
pub(crate) struct Canonical<'a> {
    pub method: &'a str,
    /// The path as it is sent (already percent-encoded once).
    pub path: &'a str,
    /// The query as it is sent, without `?`.
    pub query: &'a str,
    /// Every header the signature covers, `host` and `x-amz-date` included.
    pub headers: &'a [(String, String)],
    /// Lowercase hex SHA-256 of the body.
    pub payload_hash: &'a str,
}

impl Canonical<'_> {
    /// The canonical request and its signed headers list.
    pub(crate) fn request(&self) -> (String, String) {
        let (headers, signed) = canonical_headers(self.headers);
        let text = format!("{}\n{}\n{}\n{headers}\n{signed}\n{}", self.method, canonical_uri(self.path), canonical_query(self.query), self.payload_hash);
        (text, signed)
    }
}

/// `YYYYMMDD/region/service/aws4_request`.
pub(crate) fn scope(date: &str, region: &str, service: &str) -> String {
    format!("{date}/{region}/{service}/aws4_request")
}

/// The string to sign for a canonical request made at `amz_date` (`YYYYMMDDTHHMMSSZ`).
pub(crate) fn string_to_sign(amz_date: &str, scope: &str, canonical_request: &str) -> String {
    format!("{ALGORITHM}\n{amz_date}\n{scope}\n{}", sha256_hex(canonical_request.as_bytes()))
}

/// The hex signature of `string_to_sign` with the key derived from `secret` for the day,
/// Region and service.
pub(crate) fn signature(secret: &str, date: &str, region: &str, service: &str, string_to_sign: &str) -> Result<String, ProviderError> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes())?;
    let k_region = hmac(&k_date, region.as_bytes())?;
    let k_service = hmac(&k_region, service.as_bytes())?;
    let k_signing = hmac(&k_service, b"aws4_request")?;
    Ok(hex(&hmac(&k_signing, string_to_sign.as_bytes())?))
}

/// Civil date (year, month, day) of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe.saturating_add(era.saturating_mul(400)).saturating_add(i64::from(month <= 2));
    (year, month, day)
}

/// Days since 1970-01-01 of a civil date; the inverse of [`civil_from_days`].
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year.saturating_sub(1) } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era.saturating_mul(146_097).saturating_add(doe).saturating_sub(719_468)
}

/// `x-amz-date` (`YYYYMMDDTHHMMSSZ`) and the scope's date (`YYYYMMDD`) for Unix seconds `now`.
pub(crate) fn amz_date(now: u64) -> (String, String) {
    let days = i64::try_from(now / 86_400).unwrap_or(i64::MAX);
    let secs = now % 86_400;
    let (y, m, d) = civil_from_days(days);
    let date = format!("{y:04}{m:02}{d:02}");
    (format!("{date}T{:02}{:02}{:02}Z", secs / 3_600, secs % 3_600 / 60, secs % 60), date)
}

#[cfg(test)]
mod tests;
