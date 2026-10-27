//! Small static CSS grammar for retained originals, not a substring URL filter.
//! Unknown properties/functions are refused: browsers must not discover a resource syntax
//! that the preview renderer happens to ignore. Stylesheets are retained, never rendered.
use super::{MediaError, bad, cap, local_ref};

fn preflight(text: &str) -> Result<(), MediaError> {
    if text.len() > 1024 * 1024
        || text.contains(['\\', '@'])
        || text.contains("/*")
        || text.bytes().any(|b| b.is_ascii_control() && !matches!(b, b'\t' | b'\n' | b'\r'))
    {
        return Err(bad());
    }
    Ok(())
}

pub(super) fn value<'a>(property: &str, text: &'a str) -> Result<(Vec<&'a str>, bool), MediaError> {
    preflight(text)?;
    let property = property.trim().to_ascii_lowercase();
    if !matches!(
        property.as_str(),
        "fill"
            | "stroke"
            | "color"
            | "stop-color"
            | "flood-color"
            | "lighting-color"
            | "opacity"
            | "fill-opacity"
            | "stroke-opacity"
            | "stop-opacity"
            | "flood-opacity"
            | "fill-rule"
            | "clip-rule"
            | "stroke-width"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-miterlimit"
            | "stroke-dasharray"
            | "stroke-dashoffset"
            | "clip-path"
            | "mask"
            | "filter"
            | "marker"
            | "marker-start"
            | "marker-mid"
            | "marker-end"
            | "cursor"
            | "display"
            | "visibility"
            | "overflow"
            | "isolation"
            | "mix-blend-mode"
            | "paint-order"
            | "vector-effect"
            | "shape-rendering"
            | "image-rendering"
            | "text-rendering"
            | "font-family"
            | "font-size"
            | "font-style"
            | "font-weight"
            | "font-stretch"
            | "font-variant"
            | "text-anchor"
            | "text-decoration"
            | "dominant-baseline"
            | "alignment-baseline"
            | "baseline-shift"
            | "letter-spacing"
            | "word-spacing"
            | "white-space"
            | "writing-mode"
            | "direction"
            | "unicode-bidi"
    ) {
        return Err(bad());
    }
    let mut refs = Vec::new();
    let mut rest = text.trim();
    if rest.is_empty() {
        return Err(bad());
    }
    // Paint fallbacks (url(#paint) red) are allowed. Every function must be understood;
    // quoted/protocol-relative URLs and image-set/cross-fade/var cannot hide in tokens.
    while !rest.is_empty() {
        if rest.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("url(")) {
            if !matches!(
                property.as_str(),
                "fill" | "stroke" | "clip-path" | "mask" | "filter" | "marker" | "marker-start" | "marker-mid" | "marker-end" | "cursor"
            ) {
                return Err(bad());
            }
            let (target, tail) = rest.get(4..).ok_or_else(bad)?.split_once(')').ok_or_else(bad)?;
            let target = target.trim();
            let target = if let Some(quote) = target.chars().next().filter(|c| matches!(c, '\'' | '"')) {
                target.strip_prefix(quote).and_then(|s| s.strip_suffix(quote)).ok_or_else(bad)?
            } else {
                target
            };
            refs.push(local_ref(target)?);
            if refs.len() > 64 {
                return Err(cap());
            }
            rest = tail.trim_start();
            continue;
        }
        if let Some(open) = rest.find('(') {
            let prefix = rest.get(..open).ok_or_else(bad)?;
            if !matches!(property.as_str(), "fill" | "stroke" | "color" | "stop-color" | "flood-color" | "lighting-color")
                || !["rgb", "rgba", "hsl", "hsla"].iter().any(|name| prefix.eq_ignore_ascii_case(name))
            {
                return Err(bad());
            }
            let end = rest.find(')').ok_or_else(bad)?;
            let function = rest.get(..=end).ok_or_else(bad)?;
            let _: svgtypes::Color = function.parse().map_err(|_| bad())?;
            rest = rest.get(end + 1..).ok_or_else(bad)?.trim_start();
            continue;
        }
        // Inert CSS atoms only. No slash, colon, quotes (except family names), function,
        // declaration, selector or resource syntax can survive this branch.
        let family = property == "font-family";
        if !rest.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b.is_ascii_whitespace() || matches!(b, b'#' | b'.' | b',' | b'%' | b'-' | b'+') || family && matches!(b, b'\'' | b'"')
        }) {
            return Err(bad());
        }
        break;
    }
    Ok((refs, matches!(property.as_str(), "stroke-dasharray" | "cursor") && text.trim() != "none"))
}

pub(super) fn declarations(text: &str) -> Result<(Vec<&str>, bool), MediaError> {
    preflight(text)?;
    let mut refs = Vec::new();
    let mut unsupported = false;
    for declaration in text.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, text) = declaration.split_once(':').ok_or_else(bad)?;
        let (local, unavailable) = value(name, text)?;
        refs.extend(local);
        unsupported |= unavailable;
        if refs.len() > 64 {
            return Err(cap());
        }
    }
    Ok((refs, unsupported))
}

pub(super) fn sheet(mut text: &str) -> Result<(), MediaError> {
    preflight(text)?;
    let mut rules = 0usize;
    while !text.trim().is_empty() {
        let (selector, body) = text.split_once('{').ok_or_else(bad)?;
        if selector.trim().is_empty()
            || !selector.bytes().all(|b| {
                b.is_ascii_alphanumeric() || b.is_ascii_whitespace() || matches!(b, b'#' | b'.' | b',' | b'-' | b'_' | b'*' | b'>' | b'+' | b'~' | b':')
            })
        {
            return Err(bad());
        }
        let (body, tail) = body.split_once('}').ok_or_else(bad)?;
        let _ = declarations(body)?;
        rules = rules.checked_add(1).filter(|n| *n <= 4096).ok_or_else(cap)?;
        text = tail;
    }
    Ok(())
}
