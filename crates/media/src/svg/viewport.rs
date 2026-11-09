//! Disposable root viewport normalization; original asset bytes are never changed.
use super::{MediaError, bad, cap};
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event, attributes::Attribute},
};
use std::borrow::Cow;

pub(super) const COPY_ALLOWANCE: u64 = 128;
#[cfg(test)]
thread_local! { pub(super) static ROOT_MATERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

fn value_extent(value: &[u8]) -> Result<usize, MediaError> {
    value.iter().filter(|b| **b == b'"').count().checked_mul(5).and_then(|extra| value.len().checked_add(extra)).ok_or_else(cap)
}
fn quoted(attribute: Attribute<'_>) -> Result<Attribute<'_>, MediaError> {
    if !attribute.value.contains(&b'"') {
        return Ok(attribute);
    }
    // Attribute is already XML-escaped except for literal double quotes that were valid
    // inside a single-quoted source attribute. Preserve its existing entities verbatim.
    let length = value_extent(&attribute.value)?;
    let mut escaped = Vec::new();
    escaped.try_reserve_exact(length).map_err(|_| cap())?;
    for byte in attribute.value.as_ref() {
        if *byte == b'"' {
            escaped.extend_from_slice(b"&quot;");
        } else {
            escaped.push(*byte);
        }
    }
    Ok(Attribute { key: attribute.key, value: Cow::Owned(escaped) })
}

pub(super) fn explicit(bytes: &[u8], width: f64, height: f64) -> Result<Vec<u8>, MediaError> {
    if bytes.len() as u64 > super::MAX_BYTES {
        return Err(cap());
    }
    // Track the original UTF-8 BOM explicitly: Reader positions then have one clear origin.
    let input = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let origin = bytes.len().checked_sub(input.len()).ok_or_else(bad)?;
    let mut reader = Reader::from_reader(input);
    loop {
        let start = usize::try_from(reader.buffer_position()).ok().and_then(|n| n.checked_add(origin)).ok_or_else(cap)?;
        let event = reader.read_event().map_err(|_| bad())?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                let end = usize::try_from(reader.buffer_position()).ok().and_then(|n| n.checked_add(origin)).ok_or_else(cap)?;
                let maximum = bytes.len().checked_add(COPY_ALLOWANCE as usize).ok_or_else(cap)?;
                let name = element.name();
                let root_name = std::str::from_utf8(name.as_ref()).map_err(|_| bad())?;
                let width = format!("{width:e}");
                let height = format!("{height:e}");
                // Count the COMPLETE escaped root and document BEFORE owned buffers. Raw
                // Attribute values do not escape quotes when quick-xml re-emits them.
                let mut root_length = root_name.len().checked_add(if empty { 3 } else { 2 }).ok_or_else(cap)?;
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|_| bad())?;
                    if matches!(attribute.key.as_ref(), b"width" | b"height") {
                        continue;
                    }
                    root_length = root_length
                        .checked_add(attribute.key.as_ref().len())
                        .and_then(|n| n.checked_add(4))
                        .and_then(|n| n.checked_add(value_extent(&attribute.value).ok()?))
                        .ok_or_else(cap)?;
                }
                for (key, value) in [("width", width.as_str()), ("height", height.as_str())] {
                    root_length = root_length.checked_add(key.len()).and_then(|n| n.checked_add(4)).and_then(|n| n.checked_add(value.len())).ok_or_else(cap)?;
                }
                let length =
                    start.checked_add(root_length).and_then(|n| n.checked_add(bytes.len().checked_sub(end)?)).filter(|n| *n <= maximum).ok_or_else(cap)?;
                #[cfg(test)]
                ROOT_MATERIALIZATIONS.with(|calls| calls.set(calls.get() + 1));
                let mut normalized = Vec::new();
                normalized.try_reserve_exact(length).map_err(|_| cap())?;
                normalized.extend_from_slice(bytes.get(..start).ok_or_else(bad)?);
                let mut root_buffer = String::new();
                root_buffer.try_reserve_exact(root_length).map_err(|_| cap())?;
                root_buffer.push_str(root_name);
                let mut root = BytesStart::from_content(root_buffer, root_name.len());
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|_| bad())?;
                    if !matches!(attribute.key.as_ref(), b"width" | b"height") {
                        root.push_attribute(quoted(attribute)?);
                    }
                }
                root.push_attribute(("width", width.as_str()));
                root.push_attribute(("height", height.as_str()));
                let mut writer = Writer::new(&mut normalized);
                writer.write_event(if empty { Event::Empty(root) } else { Event::Start(root) }).map_err(|_| cap())?;
                normalized.extend_from_slice(bytes.get(end..).ok_or_else(bad)?);
                if normalized.len() != length {
                    return Err(cap());
                }
                return Ok(normalized);
            }
            Event::Decl(_) | Event::Comment(_) | Event::Text(_) => {}
            _ => return Err(bad()),
        }
    }
}
