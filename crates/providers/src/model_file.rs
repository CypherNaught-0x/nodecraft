//! Generated 3D models fetched from a provider's storage (Tripo, Meshy): the pre-signed result link
//! and the binary glTF it must hold.
use crate::{ErrorKind, ProviderError};

fn refused(provider: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, format!("{provider} result link refused"))
}

/// A result link on public HTTPS storage, nothing that could point back inside a network: no IP
/// literals, credentials, ports, single labels or names that resolve on a local network.
pub(crate) fn public_link(url: &str, provider: &str) -> Result<String, ProviderError> {
    let parsed = url::Url::parse(url).map_err(|_| ProviderError::new(ErrorKind::Malformed, format!("{provider} result link isn't a URL")))?;
    if url.len() > 8192 || parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() || parsed.port().is_some() {
        return Err(refused(provider));
    }
    let Some(url::Host::Domain(host)) = parsed.host() else {
        return Err(refused(provider));
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let internal = [".internal", ".local", ".localhost", ".localdomain", ".lan", ".home", ".corp", ".intranet"];
    if !host.contains('.') || host.starts_with("localhost") || internal.iter().any(|suffix| host.ends_with(suffix)) {
        return Err(refused(provider));
    }
    Ok(url.to_owned())
}

/// A whole binary glTF container: magic, version 2 and a length that matches the bytes.
pub(crate) fn glb(bytes: &[u8], provider: &str) -> Result<(), ProviderError> {
    let word = |at: usize| bytes.get(at..at.saturating_add(4)).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_le_bytes);
    let whole = word(8).and_then(|n| usize::try_from(n).ok()) == Some(bytes.len());
    if bytes.get(..4) != Some(b"glTF") || word(4) != Some(2) || !whole {
        return Err(ProviderError::new(ErrorKind::Malformed, format!("{provider} returned something other than a GLB model")));
    }
    Ok(())
}
