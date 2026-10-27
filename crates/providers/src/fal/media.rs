//! Container envelope validation only for WAV/MP4/GLB; image payloads use actual codecs.
use super::*;
fn bad() -> ProviderError {
    malformed("fal output is not a bounded valid supported media envelope")
}
fn u32le(b: &[u8], at: usize) -> Result<u32, ProviderError> {
    let x: [u8; 4] = b.get(at..at.checked_add(4).ok_or_else(bad)?).ok_or_else(bad)?.try_into().map_err(|_| bad())?;
    Ok(u32::from_le_bytes(x))
}

fn u16le(b: &[u8], at: usize) -> Result<u16, ProviderError> {
    let x: [u8; 2] = b.get(at..at.checked_add(2).ok_or_else(bad)?).ok_or_else(bad)?.try_into().map_err(|_| bad())?;
    Ok(u16::from_le_bytes(x))
}
fn png_extent(b: &[u8]) -> Result<(), ProviderError> {
    if b.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err(bad());
    }
    let mut at = 8usize;
    let mut count = 0;
    let mut idat = false;
    loop {
        count += 1;
        if count > 4096 {
            return Err(bad());
        }
        let len = b
            .get(at..at.checked_add(4).ok_or_else(bad)?)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .map(u32::from_be_bytes)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(bad)?;
        let kind = b.get(at.checked_add(4).ok_or_else(bad)?..at.checked_add(8).ok_or_else(bad)?).ok_or_else(bad)?;
        let end = at.checked_add(12).and_then(|n| n.checked_add(len)).filter(|n| *n <= b.len()).ok_or_else(bad)?;
        if count == 1 && (kind != b"IHDR" || len != 13) {
            return Err(bad());
        }
        if kind == b"IDAT" && len > 0 {
            idat = true;
        }
        if kind == b"IEND" {
            return if len == 0 && end == b.len() && idat { Ok(()) } else { Err(bad()) };
        }
        at = end;
    }
}
fn wav(b: &[u8]) -> Result<(), ProviderError> {
    if b.get(..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") || usize::try_from(u32le(b, 4)?).ok().and_then(|n| n.checked_add(8)) != Some(b.len()) {
        return Err(bad());
    }
    let mut at = 12usize;
    let mut fmt = None;
    let mut data = None;
    let mut chunks = 0;
    while at < b.len() {
        chunks += 1;
        if chunks > 4096 {
            return Err(bad());
        }
        let n = usize::try_from(u32le(b, at + 4)?).map_err(|_| bad())?;
        let start = at.checked_add(8).ok_or_else(bad)?;
        let end = start.checked_add(n).filter(|n| *n <= b.len()).ok_or_else(bad)?;
        match b.get(at..at.checked_add(4).ok_or_else(bad)?) {
            Some(b"fmt ") => {
                if fmt.is_some() || n < 16 {
                    return Err(bad());
                }
                let format = u16le(b, start)?;
                let channels = u16le(b, start + 2)?;
                let rate = u32le(b, start + 4)?;
                let byte_rate = u32le(b, start + 8)?;
                let align = u16le(b, start + 12)?;
                let bits = u16le(b, start + 14)?;
                if !matches!(format, 1 | 3)
                    || !(1..=8).contains(&channels)
                    || !(1..=384_000).contains(&rate)
                    || !matches!(bits, 8 | 16 | 24 | 32)
                    || (format == 3 && bits != 32)
                    || align != channels.checked_mul(bits / 8).ok_or_else(bad)?
                    || byte_rate != rate.checked_mul(u32::from(align)).ok_or_else(bad)?
                {
                    return Err(bad());
                }
                fmt = Some(usize::from(align));
            }
            Some(b"data") if data.replace(n).is_some() || n == 0 => return Err(bad()),
            _ => {}
        }
        at = end.checked_add(n % 2).filter(|n| *n <= b.len()).ok_or_else(bad)?;
    }
    let align = fmt.ok_or_else(bad)?;
    if data.ok_or_else(bad)? % align != 0 {
        return Err(bad());
    }
    Ok(())
}
fn mp4(b: &[u8]) -> Result<(), ProviderError> {
    #[derive(Default)]
    struct Movie {
        count: usize,
        ftyp: bool,
        mvhd: bool,
        trak: bool,
        video: bool,
        mdat: bool,
    }
    fn walk(b: &[u8], start: usize, end: usize, depth: usize, m: &mut Movie) -> Result<(), ProviderError> {
        if depth > 16 {
            return Err(bad());
        }
        let mut at = start;
        while at < end {
            m.count += 1;
            if m.count > 4096 {
                return Err(bad());
            }
            let read = |pos: usize| b.get(pos..pos.checked_add(4)?).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes);
            let size = read(at).ok_or_else(bad)?;
            let kind = b.get(at.checked_add(4).ok_or_else(bad)?..at.checked_add(8).ok_or_else(bad)?).ok_or_else(bad)?;
            let (header, len) = if size == 1 {
                let n = b
                    .get(at.checked_add(8).ok_or_else(bad)?..at.checked_add(16).ok_or_else(bad)?)
                    .and_then(|s| <[u8; 8]>::try_from(s).ok())
                    .map(u64::from_be_bytes)
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(bad)?;
                (16, n)
            } else if size == 0 {
                (8, end - at)
            } else {
                (8, usize::try_from(size).map_err(|_| bad())?)
            };
            let next = at.checked_add(len).filter(|n| *n <= end).ok_or_else(bad)?;
            if len < header {
                return Err(bad());
            }
            let payload = at + header;
            match kind {
                b"ftyp" if depth == 0 => {
                    if m.ftyp || at != 0 || len < header + 8 || !(len - header).is_multiple_of(4) {
                        return Err(bad());
                    }
                    let brands = b.get(payload..next).ok_or_else(bad)?;
                    if !brands
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .enumerate()
                        .any(|(i, v)| i != 1 && matches!(v, b"isom" | b"iso2" | b"iso6" | b"mp41" | b"mp42" | b"avc1" | b"M4V "))
                    {
                        return Err(bad());
                    }
                    m.ftyp = true;
                }
                b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"dinf" => {
                    if kind == b"trak" && depth == 1 {
                        m.trak = true;
                    }
                    walk(b, payload, next, depth + 1, m)?;
                }
                b"mvhd" if depth == 1 => {
                    let version = b.get(payload).copied().ok_or_else(bad)?;
                    if m.mvhd
                        || len
                            < header
                                + if version == 0 {
                                    100
                                } else if version == 1 {
                                    112
                                } else {
                                    return Err(bad());
                                }
                    {
                        return Err(bad());
                    }
                    m.mvhd = true;
                }
                b"hdlr" => {
                    if len < header + 24 {
                        return Err(bad());
                    }
                    if b.get(payload + 8..payload + 12) == Some(b"vide") {
                        m.video = true;
                    }
                }
                b"mdat" if depth == 0 => {
                    if len <= header {
                        return Err(bad());
                    }
                    m.mdat = true;
                }
                _ => {}
            }
            at = next;
        }
        if at != end {
            return Err(bad());
        }
        Ok(())
    }
    let mut m = Movie::default();
    walk(b, 0, b.len(), 0, &mut m)?;
    if !m.ftyp || !m.mvhd || !m.trak || !m.video || !m.mdat {
        return Err(bad());
    }
    Ok(())
}
fn no_uri(raw: &serde_json::value::RawValue, depth: usize) -> Result<(), ProviderError> {
    if depth > 32 {
        return Err(bad());
    }
    let s = raw.get();
    if s.starts_with('{') {
        let o = json::object(s)?;
        if o.get("uri").is_some() {
            return Err(bad());
        }
        for v in o.values() {
            no_uri(v, depth + 1)?;
        }
    } else if s.starts_with('[') {
        let values: Vec<&serde_json::value::RawValue> = serde_json::from_str(s).map_err(|_| bad())?;
        for v in values {
            no_uri(v, depth + 1)?;
        }
    }
    Ok(())
}
fn glb(b: &[u8], l: Limits) -> Result<(), ProviderError> {
    if b.get(..4) != Some(b"glTF") || u32le(b, 4)? != 2 || usize::try_from(u32le(b, 8)?).ok() != Some(b.len()) {
        return Err(bad());
    }
    let n = usize::try_from(u32le(b, 12)?).map_err(|_| bad())?;
    let end = 20usize.checked_add(n).filter(|n| *n <= b.len()).ok_or_else(bad)?;
    if n == 0 || n % 4 != 0 || n > l.metadata_bytes || b.get(16..20) != Some(b"JSON") {
        return Err(bad());
    }
    let json = b.get(20..end).ok_or_else(bad)?;
    let o = json::parse(json, l)?;
    let asset = json::object(o.get("asset").ok_or_else(bad)?.get())?;
    if asset.text("version")? != "2.0" {
        return Err(bad());
    }
    let raw: &serde_json::value::RawValue = serde_json::from_slice(json).map_err(|_| bad())?;
    no_uri(raw, 0)?;
    if end < b.len() {
        let n = usize::try_from(u32le(b, end)?).map_err(|_| bad())?;
        if n == 0 || n % 4 != 0 || b.get(end + 4..end + 8) != Some(b"BIN\0") || end.checked_add(8).and_then(|v| v.checked_add(n)) != Some(b.len()) {
            return Err(bad());
        }
    }
    Ok(())
}
pub(super) fn validate(b: &[u8], r: Record, l: Limits, pixels: &mut u64) -> Result<&'static str, ProviderError> {
    match r.model {
        Model::Image
        | Model::Styled
        | Model::Upscale
        | Model::RemoveBackground
        | Model::QwenImage
        | Model::QwenEdit
        | Model::GrokImage
        | Model::GrokEdit
        | Model::Photon
        | Model::Ideogram => {
            let m = nodecraft_media::probe_image(b, &l.image(false)).map_err(|_| bad())?;
            if m.mime == "image/png" {
                png_extent(b)?;
            } else if !matches!(r.model, Model::QwenImage | Model::QwenEdit | Model::GrokImage | Model::GrokEdit | Model::Photon | Model::Ideogram)
                || !matches!(m.mime, "image/jpeg" | "image/webp")
            {
                return Err(bad());
            }
            let n = u64::from(m.width).checked_mul(u64::from(m.height)).ok_or_else(bad)?;
            *pixels = pixels.checked_add(n).filter(|n| *n <= l.total_pixels).ok_or_else(bad)?;
            if matches!(r.model, Model::Upscale | Model::RemoveBackground) && (m.width != r.width || m.height != r.height) {
                return Err(bad());
            }
            observe(2);
            let decoded = nodecraft_media::decode_image(b, &l.image(false)).map_err(|_| bad())?;
            drop(decoded);
            Ok(m.mime)
        }
        Model::Wan3Text
        | Model::Wan3Frames
        | Model::GrokText
        | Model::GrokFrames
        | Model::Video
        | Model::Frames
        | Model::Speech
        | Model::RayText
        | Model::RayFrames
        | Model::LtxText
        | Model::LtxFrames => {
            mp4(b)?;
            Ok("video/mp4")
        }
        Model::Audio => {
            wav(b)?;
            Ok("audio/wav")
        }
        Model::HunyuanText | Model::HunyuanImage | Model::HunyuanSketch | Model::Mesh => {
            glb(b, l)?;
            Ok("model/gltf-binary")
        }
        // A training's result is never downloaded.
        Model::Train => Err(bad()),
    }
}
