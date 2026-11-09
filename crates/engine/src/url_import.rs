//! Import from URL (#96): one media file fetched from a public `https` address a person typed into
//! the Assets browser, and handed to [`crate::import::import_library`] exactly as a picked file is
//! (`docs/decisions/2026-11-03-library-url-import-and-global-library.md`).
//!
//! This module is the policy and the fetch loop; the transport is the host's. The desktop app
//! runs [`fetch`] on its own thread over a transport that resolves names only to public
//! addresses and connects to the addresses it checked (`ReqwestHttp::public_only`), with no
//! proxy, no cookie store and the generic `NodeCraft/<version>` User-Agent. The browser has no
//! such transport, so it doesn't offer this at all.
//!
//! The policy, every hop:
//!
//! * `https` only, port 443, no user name or password in the address, at most
//!   [`MAX_URL_CHARS`] characters;
//! * a host that is a name, or a public address: loopback, private, link-local, shared and
//!   documentation ranges are refused here for address literals and by the transport's resolver
//!   for names (every address a name resolves to must be public);
//! * at most [`MAX_REDIRECTS`] redirects, each target checked again as above;
//! * no request headers but `Accept`; nothing the person typed besides the address is sent;
//! * the body of a reply that isn't 2xx (a redirect, a refusal) is never read;
//! * at most [`MAX_BYTES`], counted as the body streams in and aborted past it, a timeout per
//!   hop, and a cancel checked on every piece of the body;
//! * the reply's `Content-Type` must name a media type (or say nothing more than
//!   `application/octet-stream`), and the bytes must then be recognised as media of the same
//!   kind: the bytes decide, as they do for a dropped file.
//!
//! What the project keeps is the file's name, taken from the address's last path segment and
//! cleaned as a picked file's is ([`crate::import::public_name`]). The address itself (its path,
//! query and any token in it) never reaches the document, a log or an error message: messages
//! name the host at most.
use nodecraft_media::{MediaKind, sniff};
use nodecraft_providers::http::public_address;
use nodecraft_providers::{Http, HttpRequest};
use nodecraft_types::{Error, ErrorKind, Modality};
use std::sync::atomic::{AtomicBool, Ordering};

/// The largest file fetched: what a project takes as one small asset.
pub const MAX_BYTES: usize = nodecraft_format::MAX_ASSET_BYTES;
/// Redirects followed before giving up.
pub const MAX_REDIRECTS: usize = 3;
/// The longest address accepted.
pub const MAX_URL_CHARS: usize = 2048;
/// How long one hop (the request and its whole body) may take.
pub const HOP_TIMEOUT_MS: u64 = 60_000;
/// What an import asks for.
const ACCEPT: &str = "image/*, video/*, audio/*, model/*, application/pdf";

fn refused(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// An address that passed the policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked(url::Url);

impl Checked {
    /// The host, the only part of the address a message names.
    pub fn host(&self) -> String {
        self.0.host_str().unwrap_or_default().to_owned()
    }
    /// The name the file is kept under: the last path segment, decoded and cleaned.
    pub fn file_name(&self) -> String {
        let last = self.0.path_segments().and_then(|mut segments| segments.rfind(|s| !s.is_empty())).unwrap_or_default();
        let decoded = percent_decoded(last);
        let name = crate::import::public_name(&decoded);
        if decoded.trim().is_empty() || name == "Imported file" { format!("Download from {}", self.host()) } else { name }
    }
}

/// `segment` with its `%XX` escapes decoded (invalid UTF-8 replaced); a stray `%` stays.
fn percent_decoded(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let hex = bytes
            .get(at.saturating_add(1)..at.saturating_add(3))
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (byte, hex) {
            (b'%', Some(value)) => {
                out.push(value);
                at = at.saturating_add(3);
            }
            _ => {
                out.push(byte);
                at = at.saturating_add(1);
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Checks `text` against the policy: `https`, port 443, no credentials, and a host that is a
/// name or a public address.
pub fn check(text: &str) -> Result<Checked, Error> {
    let text = text.trim();
    if text.is_empty() {
        return Err(refused("type or paste the address of a file"));
    }
    if text.chars().count() > MAX_URL_CHARS {
        return Err(refused("that address is too long"));
    }
    let url = url::Url::parse(text).map_err(|_| refused("that isn't a web address; it starts with https://"))?;
    checked(url)
}

fn checked(url: url::Url) -> Result<Checked, Error> {
    if url.scheme() != "https" {
        return Err(refused("only https addresses are imported"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refused("an address with a user name or password isn't imported"));
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err(refused("only addresses on the standard https port are imported"));
    }
    match url.host() {
        None => return Err(refused("the address has no host")),
        Some(url::Host::Ipv4(ip)) if !public_address(ip.into()) => return Err(local()),
        Some(url::Host::Ipv6(ip)) if !public_address(ip.into()) => return Err(local()),
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            // Names that only ever mean this machine or its network; the resolver checks the rest.
            let local_name = !name.contains('.')
                || name == "localhost"
                || [".localhost", ".local", ".internal", ".home.arpa", ".lan"].iter().any(|suffix| name.ends_with(suffix));
            if local_name {
                return Err(local());
            }
        }
        Some(_) => {}
    }
    Ok(Checked(url))
}

fn local() -> Error {
    refused("addresses on this computer or its local network aren't imported")
}

/// Where a redirect from `from` to `location` leads, checked as the first address was.
pub fn redirect(from: &Checked, location: &str) -> Result<Checked, Error> {
    if location.chars().count() > MAX_URL_CHARS {
        return Err(refused("the site redirected to an address that is too long"));
    }
    let to = from.0.join(location.trim()).map_err(|_| refused("the site redirected to an address that can't be read"))?;
    checked(to).map_err(|error| refused(format!("the site redirected somewhere that isn't imported: {}", error.message)))
}

/// What a reply's `Content-Type` says the body is: `Some(kind)` for a media type, `None` when it
/// doesn't say (missing, or a generic binary type), so the bytes decide. Anything else (a web
/// page, JSON, text) is refused.
pub fn declared(content_type: Option<&str>) -> Result<Option<Modality>, Error> {
    let essence = content_type.unwrap_or_default().split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    if essence.is_empty() || essence == "application/octet-stream" || essence == "binary/octet-stream" {
        return Ok(None);
    }
    let mime: nodecraft_types::Mime = essence.parse().map_err(|_| refused("the site sent something that isn't a media file"))?;
    match crate::library_browse_cmds::kind_of(&mime) {
        Some(kind) if crate::library_browse_cmds::ASSET_KINDS.contains(&kind) => Ok(Some(kind)),
        _ => Err(refused("the site sent a web page or text, not an image, video, sound, 3D model or PDF")),
    }
}

/// The kind of media `bytes` are, checked against what the reply declared.
pub fn recognise(declared: Option<Modality>, bytes: &[u8]) -> Result<Modality, Error> {
    let sniffed = sniff(bytes).ok_or_else(|| refused("the download isn't a kind of file NodeCraft recognises"))?;
    let kind = match sniffed.kind {
        MediaKind::Image => Modality::Image,
        MediaKind::Video => Modality::Video,
        MediaKind::Audio => Modality::Audio,
        MediaKind::Model3d => Modality::Mesh,
        MediaKind::Document => Modality::Document,
        MediaKind::Text | MediaKind::Style => return Err(refused("the download is text, not an image, video, sound, 3D model or PDF")),
    };
    if declared.is_some_and(|declared| declared != kind) {
        return Err(refused("the download isn't the kind of file the site said it was"));
    }
    Ok(kind)
}

/// One fetched file: its public name, the host it came from, and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub name: String,
    pub host: String,
    pub kind: Modality,
    pub bytes: Vec<u8>,
}

fn cancelled() -> Error {
    Error::new(ErrorKind::Conflict, "the download was cancelled")
}

/// Fetches the file at `address` under the policy above, over `http` (which must resolve names
/// only to public addresses and follow no redirects itself). `cancel` stops it at the next piece
/// of the body.
pub async fn fetch(http: &dyn Http, address: &str, cancel: &AtomicBool) -> Result<Fetched, Error> {
    let first = check(address)?;
    let mut at = first.clone();
    for hop in 0..=MAX_REDIRECTS {
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let host = at.host();
        let request = HttpRequest::get(at.0.as_str()).header("Accept", ACCEPT).timeout_ms(HOP_TIMEOUT_MS).max_response_bytes(MAX_BYTES);
        let mut body: Vec<u8> = Vec::new();
        let mut stopped = false;
        let reply = {
            let mut sink = |chunk: &[u8]| -> Result<(), nodecraft_providers::ProviderError> {
                if cancel.load(Ordering::Acquire) {
                    stopped = true;
                    return Err(nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::Cancelled, "cancelled"));
                }
                if body.len().saturating_add(chunk.len()) > MAX_BYTES {
                    return Err(nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::Malformed, "too large"));
                }
                body.try_reserve(chunk.len()).map_err(|_| nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::Malformed, "too large"))?;
                body.extend_from_slice(chunk);
                Ok(())
            };
            // A redirect's or refusal's body is never read: only its status and headers count.
            http.send_streaming_no_redirect_bodiless_errors(request, &mut sink).await
        };
        if stopped || cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        // The transport's own message can quote the address; only the host is said.
        let reply = reply.map_err(|error| match error.kind {
            nodecraft_providers::ErrorKind::Timeout => Error::new(ErrorKind::Timeout, format!("{host} took too long to answer")),
            nodecraft_providers::ErrorKind::Malformed => {
                refused(format!("the file is larger than {} MiB, the most one import takes", MAX_BYTES / (1024 * 1024)))
            }
            _ => Error::new(ErrorKind::Network, format!("{host} couldn't be reached, or it is on a private network")),
        })?;
        match reply.status {
            200..=299 => {
                if body.is_empty() {
                    return Err(refused(format!("{host} sent an empty file")));
                }
                let kind = recognise(declared(reply.header("content-type"))?, &body)?;
                return Ok(Fetched { name: first.file_name(), host: first.host(), kind, bytes: body });
            }
            301 | 302 | 303 | 307 | 308 => {
                if hop == MAX_REDIRECTS {
                    return Err(refused(format!("{host} redirected more than {MAX_REDIRECTS} times")));
                }
                let location = reply.header("location").ok_or_else(|| refused(format!("{host} redirected without saying where")))?;
                at = redirect(&at, location)?;
            }
            401 | 403 => {
                return Err(Error::new(ErrorKind::Network, format!("{host} needs a sign-in for that file; download it in a browser and import the file")));
            }
            404 | 410 => return Err(Error::new(ErrorKind::NotFound, format!("{host} has no file at that address"))),
            status => return Err(Error::new(ErrorKind::Network, format!("{host} answered {status}; nothing was imported"))),
        }
    }
    Err(refused("too many redirects"))
}

#[cfg(test)]
mod tests;
