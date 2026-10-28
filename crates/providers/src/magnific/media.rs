use super::*;

fn bad() -> ProviderError {
    malformed("Magnific did not return a complete bounded static raster image")
}
fn four(bytes: &[u8], at: usize) -> Result<[u8; 4], ProviderError> {
    bytes.get(at..at.checked_add(4).ok_or_else(bad)?).ok_or_else(bad)?.try_into().map_err(|_| bad())
}
/// Check the complete container, including the final extent. The actual codec is checked
/// separately; these scans also refuse multi-frame PNG/WebP instead of forwarding ambiguity.
pub(super) fn complete(bytes: &[u8], mime: &str) -> Result<(), ProviderError> {
    match mime {
        "image/png" => {
            if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
                return Err(bad());
            }
            let mut at = 8usize;
            let mut image_data = false;
            for count in 0..4096 {
                let length = usize::try_from(u32::from_be_bytes(four(bytes, at)?)).map_err(|_| bad())?;
                let kind = four(bytes, at.checked_add(4).ok_or_else(bad)?)?;
                let end = at.checked_add(12).and_then(|n| n.checked_add(length)).filter(|n| *n <= bytes.len()).ok_or_else(bad)?;
                if count == 0 && (&kind != b"IHDR" || length != 13) || &kind == b"acTL" {
                    return Err(bad());
                }
                image_data |= &kind == b"IDAT" && length > 0;
                if &kind == b"IEND" {
                    return if length == 0 && end == bytes.len() && image_data { Ok(()) } else { Err(bad()) };
                }
                at = end;
            }
            Err(bad())
        }
        "image/jpeg" => {
            let end = bytes.len().checked_sub(2).ok_or_else(bad)?;
            if bytes.get(..2) == Some(b"\xff\xd8") && bytes.get(end..) == Some(b"\xff\xd9") { Ok(()) } else { Err(bad()) }
        }
        "image/webp" => {
            let length = usize::try_from(u32::from_le_bytes(four(bytes, 4)?)).map_err(|_| bad())?;
            if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") || length.checked_add(8) != Some(bytes.len()) {
                return Err(bad());
            }
            let mut at = 12usize;
            for _ in 0..4096 {
                if at == bytes.len() {
                    return Ok(());
                }
                let kind = four(bytes, at)?;
                if matches!(&kind, b"ANIM" | b"ANMF") {
                    return Err(bad());
                }
                let length = usize::try_from(u32::from_le_bytes(four(bytes, at.checked_add(4).ok_or_else(bad)?)?)).map_err(|_| bad())?;
                at = at
                    .checked_add(8)
                    .and_then(|n| n.checked_add(length))
                    .and_then(|n| n.checked_add(length % 2))
                    .filter(|n| *n <= bytes.len())
                    .ok_or_else(bad)?;
            }
            Err(bad())
        }
        _ => Err(bad()),
    }
}
pub(super) fn validate(bytes: &[u8], contract: Contract) -> Result<&'static str, ProviderError> {
    let limits = contract.limits;
    if bytes.is_empty() || bytes.len() > limits.output_bytes {
        return Err(bad());
    }
    let meta = nodecraft_media::probe_image(bytes, &limits.image(false)).map_err(|_| bad())?;
    complete(bytes, meta.mime)?;
    if meta.width != contract.width || meta.height != contract.height {
        return Err(malformed("Magnific result dimensions disagree with the requested scale"));
    }
    drop(nodecraft_media::decode_image(bytes, &limits.image(false)).map_err(|_| bad())?);
    Ok(meta.mime)
}
