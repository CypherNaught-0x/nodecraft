//! SVG originals are assets; previews are disposable, bounded rasters.
//!
//! Admission never loads resources or fonts. Unsupported static effects/text keep their
//! original bytes and report a preview capability error, rather than fail a paid vector result.
use crate::{Limits, MediaError};
use quick_xml::{Reader, events::Event};
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::BTreeMap;

pub const MIME: &str = "image/svg+xml";
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
const NS: &str = "http://www.w3.org/2000/svg";
const MAX_NODES: u32 = 65_536;
const MAX_DEPTH: usize = 64;
const MAX_SEGMENTS: usize = 250_000;
const MAX_DIMENSION: u32 = 16_384;
const MAX_PIXELS: u64 = 16_777_216;
const MAX_LOGICAL_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    /// Static SVG content is retained even when this renderer cannot faithfully preview it.
    pub preview_unavailable: Option<&'static str>,
}
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    /// Straight-alpha RGBA, as the other media decoders return.
    pub rgba: Vec<u8>,
}
fn bad() -> MediaError {
    MediaError::Decode("SVG is malformed, unsafe or exceeds its structural limits".into())
}
fn cap() -> MediaError {
    MediaError::LimitExceeded("SVG exceeds its bounded document or render capacity".into())
}

/// Recognition only. The full document must pass `probe` before use. Never search for an
/// inner svg element inside an HTML document, nor trust a file extension or claimed MIME.
pub fn recognizes(bytes: &[u8]) -> bool {
    let mut reader = Reader::from_reader(bytes.get(..bytes.len().min(64 * 1024)).unwrap_or_default());
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => return e.local_name().as_ref() == b"svg",
            Ok(Event::Decl(_) | Event::Comment(_) | Event::Text(_) | Event::DocType(_)) => {}
            _ => return false,
        }
    }
}

/// Validate the whole original under fixed structural caps and lower caller byte/geometry
/// and logical allocation limits. No pixel allocation, resource resolution or renderer call.
pub fn probe(bytes: &[u8], limits: &Limits) -> Result<Info, MediaError> {
    admitted(bytes, limits).map(|(_, info)| info)
}

fn preflight(text: &str) -> Result<(), MediaError> {
    let mut reader = Reader::from_str(text);
    let mut depth = 0usize;
    let mut nodes = 0u32;
    let mut attributes = 0usize;
    loop {
        let event = reader.read_event().map_err(|_| bad())?;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                if depth >= MAX_DEPTH {
                    return Err(cap());
                }
                if matches!(event, Event::Start(_)) {
                    depth = depth.checked_add(1).ok_or_else(cap)?;
                }
                nodes = nodes.checked_add(1).filter(|n| *n <= MAX_NODES).ok_or_else(cap)?;
                let count = e.attributes().take(65).collect::<Result<Vec<_>, _>>().map_err(|_| bad())?.len();
                attributes = attributes.checked_add(count).filter(|n| *n <= 262_144).ok_or_else(cap)?;
                if count > 64 {
                    return Err(cap());
                }
            }
            Event::DocType(_) | Event::PI(_) => return Err(bad()),
            Event::End(_) => depth = depth.checked_sub(1).ok_or_else(bad)?,
            Event::Eof => return if depth == 0 { Ok(()) } else { Err(bad()) },
            _ => {}
        }
    }
}

fn finite(n: f64) -> Result<f64, MediaError> {
    if !n.is_finite() || n.abs() > 1_000_000.0 { Err(cap()) } else { Ok(n) }
}
fn path_numbers(segment: svgtypes::PathSegment) -> Result<(), MediaError> {
    use svgtypes::PathSegment::*;
    let numbers = match segment {
        MoveTo { x, y, .. } | LineTo { x, y, .. } | SmoothQuadratic { x, y, .. } => [x, y, 0.0, 0.0, 0.0, 0.0],
        HorizontalLineTo { x, .. } => [x, 0.0, 0.0, 0.0, 0.0, 0.0],
        VerticalLineTo { y, .. } => [y, 0.0, 0.0, 0.0, 0.0, 0.0],
        CurveTo { x1, y1, x2, y2, x, y, .. } => [x1, y1, x2, y2, x, y],
        SmoothCurveTo { x2, y2, x, y, .. } => [x2, y2, x, y, 0.0, 0.0],
        Quadratic { x1, y1, x, y, .. } => [x1, y1, x, y, 0.0, 0.0],
        EllipticalArc { rx, ry, x_axis_rotation, x, y, .. } => [rx, ry, x_axis_rotation, x, y, 0.0],
        ClosePath { .. } => [0.0; 6],
    };
    for n in numbers {
        finite(n)?;
    }
    Ok(())
}
fn dimension(value: Option<&str>, fallback: f64, maximum: u32) -> Result<u32, MediaError> {
    let n = if let Some(value) = value {
        let length: svgtypes::Length = value.parse().map_err(|_| bad())?;
        let scale = match length.unit {
            svgtypes::LengthUnit::None | svgtypes::LengthUnit::Px => 1.0,
            svgtypes::LengthUnit::In => 96.0,
            svgtypes::LengthUnit::Cm => 96.0 / 2.54,
            svgtypes::LengthUnit::Mm => 96.0 / 25.4,
            svgtypes::LengthUnit::Pt => 96.0 / 72.0,
            svgtypes::LengthUnit::Pc => 16.0,
            svgtypes::LengthUnit::Percent if length.number == 100.0 => return dimension(None, fallback, maximum),
            _ => return Err(bad()),
        };
        length.number * scale
    } else {
        fallback
    };
    if !n.is_finite() || n <= 0.0 || n.ceil() > f64::from(maximum.min(MAX_DIMENSION)) {
        return Err(cap());
    }
    Ok(n.ceil() as u32)
}
fn local_ref(text: &str) -> Result<&str, MediaError> {
    let id = text.strip_prefix('#').ok_or_else(bad)?;
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.')) {
        return Err(bad());
    }
    Ok(id)
}
/// Strict CSS resource policy, evaluated after XML unescaping. Escape sequences, at-rules,
/// comments and legacy executable CSS are refused rather than hidden in an exported original.
fn css(text: &str) -> Result<Vec<&str>, MediaError> {
    if text.len() > 1024 * 1024
        || text.contains(['\\', '@'])
        || text.contains("/*")
        || text.bytes().any(|b| b.is_ascii_control() && !matches!(b, b'\t' | b'\n' | b'\r'))
    {
        return Err(bad());
    }
    let lower = text.to_ascii_lowercase();
    if ["expression", "behavior", "-moz-binding", "javascript:", "data:", "file:", "http:", "https:"].iter().any(|s| lower.contains(s)) {
        return Err(bad());
    }
    let mut refs = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.to_ascii_lowercase().find("url") {
        rest = rest.get(at + 3..).ok_or_else(bad)?.trim_start();
        let body = rest.strip_prefix('(').ok_or_else(bad)?;
        let (target, tail) = body.split_once(')').ok_or_else(bad)?;
        refs.push(local_ref(target.trim().trim_matches(['\'', '"']))?);
        if refs.len() > 64 {
            return Err(cap());
        }
        rest = tail;
    }
    Ok(refs)
}
fn geometry(node: Node<'_, '_>, limits: &Limits) -> Result<(u32, u32), MediaError> {
    let view = node.attribute("viewBox").map(|s| s.parse::<svgtypes::ViewBox>().map_err(|_| bad())).transpose()?;
    if let Some(v) = view {
        for n in [v.x, v.y, v.w, v.h] {
            finite(n)?;
        }
    }
    let width = dimension(node.attribute("width"), view.map_or(300.0, |v| v.w), limits.max_width)?;
    let height = dimension(node.attribute("height"), view.map_or(150.0, |v| v.h), limits.max_height)?;
    if u64::from(width).checked_mul(u64::from(height)).is_none_or(|n| n > MAX_PIXELS) {
        return Err(cap());
    }
    Ok((width, height))
}

fn admitted<'a>(bytes: &'a [u8], limits: &Limits) -> Result<(Document<'a>, Info), MediaError> {
    if bytes.len() as u64 > MAX_BYTES.min(limits.max_input_bytes) {
        return Err(cap());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| bad())?;
    preflight(text)?;
    let doc = Document::parse_with_options(text, ParsingOptions { allow_dtd: false, nodes_limit: MAX_NODES }).map_err(|_| bad())?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(NS) {
        return Err(bad());
    }
    let (width, height) = geometry(root, limits)?;
    let mut reason = None;
    let mut count = 0usize;
    let mut segments = 0usize;
    let mut ids = BTreeMap::new();
    let mut references = BTreeMap::<_, Vec<&str>>::new();
    for node in doc.descendants().filter(Node::is_element) {
        count = count.checked_add(1).ok_or_else(cap)?;
        let name = node.tag_name().name();
        if node.tag_name().namespace() != Some(NS) || matches!(name, "script" | "foreignObject" | "set" | "discard" | "a") || name.starts_with("animate") {
            return Err(bad());
        }
        if let Some(id) = node.attribute("id") {
            if id.len() > 128 || ids.insert(id, node).is_some() {
                return Err(bad());
            }
        }
        if matches!(name, "text" | "tspan" | "textPath" | "image" | "filter" | "pattern" | "marker") || name.starts_with("fe") {
            reason = Some("SVG original retained; this static feature or font has no bounded preview");
        }
        for attr in node.attributes() {
            let key = attr.name();
            let value = attr.value();
            if key.to_ascii_lowercase().starts_with("on") || key == "base" && attr.namespace() == Some("http://www.w3.org/XML/1998/namespace") {
                return Err(bad());
            }
            if key == "href" {
                let target = local_ref(value)?;
                references.entry(node.id().get()).or_default().push(target);
            }
            if matches!(key, "style" | "fill" | "stroke" | "filter" | "mask" | "clip-path" | "cursor" | "marker" | "marker-start" | "marker-mid" | "marker-end")
            {
                references.entry(node.id().get()).or_default().extend(css(value)?);
            }
            if key == "d" {
                for segment in svgtypes::PathParser::from(value) {
                    path_numbers(segment.map_err(|_| bad())?)?;
                    segments = segments.checked_add(1).filter(|n| *n <= MAX_SEGMENTS).ok_or_else(cap)?;
                }
            }
            if matches!(key, "points" | "stroke-dasharray") && value != "none" {
                for n in svgtypes::NumberListParser::from(value) {
                    finite(n.map_err(|_| bad())?)?;
                    segments = segments.checked_add(1).filter(|n| *n <= MAX_SEGMENTS).ok_or_else(cap)?;
                }
            }
            if key == "transform" || key == "gradientTransform" {
                let t: svgtypes::Transform = value.parse().map_err(|_| bad())?;
                for n in [t.a, t.b, t.c, t.d, t.e, t.f] {
                    finite(n)?;
                }
            }
        }
        if name == "style" {
            // Styles can redirect presentation to local definitions. Without a full selector
            // dependency graph they aren't sent to the renderer; originals stay available.
            let mut sheet = String::new();
            for child in node.children().filter(Node::is_text) {
                let text = child.text().unwrap_or_default();
                if sheet.len().checked_add(text.len()).is_none_or(|n| n > 1024 * 1024) {
                    return Err(cap());
                }
                sheet.push_str(text);
            }
            let _ = css(&sheet)?;
            reason = Some("SVG original retained; stylesheet preview is unavailable");
        }
    }
    if references.values().map(Vec::len).sum::<usize>() > 4096 {
        return Err(cap());
    }
    // References are admitted before usvg can expand them. Charge repeated subtrees, detect
    // cycles and cap both recursion and expansion; source XML size alone is insufficient.
    let mut expanded = 0usize;
    let mut stack = vec![(root, Vec::new())];
    while let Some((node, mut seen)) = stack.pop() {
        expanded = expanded.checked_add(1).filter(|n| *n <= MAX_NODES as usize).ok_or_else(cap)?;
        if seen.contains(&node.id()) || seen.len() >= MAX_DEPTH {
            return Err(cap());
        }
        seen.push(node.id());
        for target in references.get(&node.id().get()).into_iter().flatten() {
            if let Some(target) = ids.get(target) {
                stack.push((*target, seen.clone()));
            } else {
                reason = Some("SVG original retained; a local preview resource is missing");
            }
        }
        for child in node.children().filter(Node::is_element) {
            stack.push((child, seen.clone()));
        }
    }
    let work = (bytes.len() as u64)
        .checked_mul(4)
        .and_then(|n| n.checked_add((count as u64).checked_mul(512)?))
        .and_then(|n| n.checked_add((segments as u64).checked_mul(128)?))
        .ok_or_else(cap)?;
    if work > limits.max_alloc.min(MAX_LOGICAL_BYTES) {
        return Err(cap());
    }
    if count > 4096 || segments > 32_768 || expanded > 8192 {
        reason = Some("SVG original retained; preview complexity exceeds its work limit");
    }
    Ok((doc, Info { width, height, preview_unavailable: reason }))
}

/// Render directly at the requested bucket, never at an untrusted full-size canvas. All
/// temporary effect surfaces are avoided: filters, patterns, text and images don't render.
pub fn render(bytes: &[u8], edge: u32, limits: &Limits) -> Result<Raster, MediaError> {
    if edge == 0 || edge > 2048 {
        return Err(cap());
    }
    let (doc, info) = admitted(bytes, limits)?;
    if let Some(reason) = info.preview_unavailable {
        return Err(MediaError::UnsupportedFeature(reason.into()));
    }
    let scale = f64::from(edge) / f64::from(info.width.max(info.height));
    let width = (f64::from(info.width) * scale).ceil().max(1.0) as u32;
    let height = (f64::from(info.height) * scale).ceil().max(1.0) as u32;
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or_else(cap)?;
    if pixels.checked_mul(32).is_none_or(|n| n > limits.max_alloc.min(128 * 1024 * 1024)) {
        return Err(cap());
    }
    let mut options = resvg::usvg::Options::default();
    options.image_href_resolver = resvg::usvg::ImageHrefResolver { resolve_data: Box::new(|_, _, _| None), resolve_string: Box::new(|_, _| None) };
    let tree = resvg::usvg::Tree::from_xmltree(&doc, &options).map_err(|_| bad())?;
    let sx = width as f32 / tree.size().width();
    let sy = height as f32 / tree.size().height();
    render_budget(&tree, sx, sy, pixels, limits)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).ok_or_else(cap)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(sx, sy), &mut pixmap.as_mut());
    let mut rgba = pixmap.take();
    for pixel in rgba.chunks_exact_mut(4) {
        if let [r, g, b, a] = pixel {
            if *a == 0 {
                *r = 0;
                *g = 0;
                *b = 0;
            } else {
                let straight = |v: u8| ((u16::from(v) * 255 + u16::from(*a) / 2) / u16::from(*a)).min(255) as u8;
                *r = straight(*r);
                *g = straight(*g);
                *b = straight(*b);
            }
        }
    }
    Ok(Raster { width, height, source_width: info.width, source_height: info.height, rgba })
}

fn render_budget(tree: &resvg::usvg::Tree, sx: f32, sy: f32, pixels: u64, limits: &Limits) -> Result<(), MediaError> {
    if !sx.is_finite() || !sy.is_finite() {
        return Err(cap());
    }
    let mut groups = vec![tree.root()];
    groups.extend(tree.clip_paths().iter().map(|p| p.root()));
    groups.extend(tree.masks().iter().map(|m| m.root()));
    let mut scratch = pixels.checked_mul(8).ok_or_else(cap)?;
    let mut work = 0u64;
    let mut nodes = 0usize;
    let mut segments = 0usize;
    while let Some(group) = groups.pop() {
        if !group.filters().is_empty() {
            return Err(cap());
        }
        if group.should_isolate() {
            let bbox = group.abs_layer_bounding_box();
            let w = (bbox.width() * sx).ceil() + 8.0;
            let h = (bbox.height() * sy).ceil() + 8.0;
            if !w.is_finite() || !h.is_finite() || w < 0.0 || h < 0.0 {
                return Err(cap());
            }
            let surface = (w as u64).min(pixels.saturating_mul(5)).checked_mul((h as u64).min(pixels.saturating_mul(5))).ok_or_else(cap)?;
            scratch = scratch.checked_add(surface.checked_mul(8).ok_or_else(cap)?).ok_or_else(cap)?;
        }
        for node in group.children() {
            nodes = nodes.checked_add(1).filter(|n| *n <= 8192).ok_or_else(cap)?;
            let bbox = node.abs_stroke_bounding_box();
            let values = [bbox.x() * sx, bbox.y() * sy, bbox.width() * sx, bbox.height() * sy];
            // Protect renderer integer rectangle conversion as well as float transforms.
            if values.iter().any(|n| !n.is_finite() || n.abs() > 16_777_216.0) {
                return Err(cap());
            }
            let area = ((bbox.width() * sx).ceil().max(0.0) as u64)
                .min(pixels)
                .checked_mul(((bbox.height() * sy).ceil().max(0.0) as u64).min(pixels))
                .ok_or_else(cap)?
                .min(pixels.saturating_mul(25));
            work = work.checked_add(area).filter(|n| *n <= 64 * 1024 * 1024).ok_or_else(cap)?;
            match node {
                resvg::usvg::Node::Group(child) => {
                    groups.push(child);
                }
                resvg::usvg::Node::Path(path) => {
                    segments = segments.checked_add(path.data().segments().count()).filter(|n| *n <= 32_768).ok_or_else(cap)?;
                }
                _ => return Err(cap()),
            }
        }
    }
    if scratch > limits.max_alloc.min(64 * 1024 * 1024) {
        return Err(cap());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
